pub use tinkr_core::adapters;

use bytes::Bytes;
use futures_util::{Stream, TryStreamExt};
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::{
    body::{Frame, Incoming},
    service::service_fn,
    Request, Response, StatusCode,
};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{SystemTime, UNIX_EPOCH},
};
use tinkr_core::{
    ledger::{CountKind, Event, Layer, Ledger},
    platform::{fsx, Paths},
    provider::Provider,
    recall::RecallStore,
    say_less,
};
use tokio::net::TcpListener;

pub const ALLOWED_HOSTS: &[&str] = &[
    "api.anthropic.com",
    "api.openai.com",
    "generativelanguage.googleapis.com",
    "api.githubcopilot.com",
    "openrouter.ai",
    "api.together.xyz",
    "api.groq.com",
    "api.fireworks.ai",
    "api.deepseek.com",
    "api.mistral.ai",
    "api.x.ai",
    "api.perplexity.ai",
    "api.cohere.com",
    "api.cerebras.ai",
    "api.sambanova.ai",
    "api.moonshot.cn",
    "dashscope.aliyuncs.com",
    "open.bigmodel.cn",
];

pub const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    pub upstream: String,
    pub provider: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyState {
    pub pid: u32,
    pub port: u16,
    pub routes: HashMap<String, Route>,
}

pub struct ProxyConfig {
    pub routes: HashMap<String, Route>,
    pub allow_localhost_upstream: bool,
    pub paths: Option<Paths>,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        let mut routes = HashMap::new();
        routes.insert(
            "anthropic".into(),
            Route {
                upstream: "https://api.anthropic.com".into(),
                provider: "anthropic".into(),
            },
        );
        routes.insert(
            "openai".into(),
            Route {
                upstream: "https://api.openai.com".into(),
                provider: "openai".into(),
            },
        );
        routes.insert(
            "gemini".into(),
            Route {
                upstream: "https://generativelanguage.googleapis.com".into(),
                provider: "gemini".into(),
            },
        );
        routes.insert(
            "copilot".into(),
            Route {
                upstream: "https://api.githubcopilot.com".into(),
                provider: "copilot".into(),
            },
        );
        routes.insert(
            "openrouter".into(),
            Route {
                upstream: "https://openrouter.ai/api".into(),
                provider: "openai_compatible".into(),
            },
        );
        Self {
            routes,
            allow_localhost_upstream: true,
            paths: None,
        }
    }
}

fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    match bare.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => false,
    }
}

fn is_blocked_ssrf_ip(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let Ok(ip) = bare.parse::<std::net::IpAddr>() else {
        return false;
    };
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast() || v4.is_multicast()
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_unspecified() || v6.is_multicast() || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

fn origin_key(u: &reqwest::Url) -> Option<(String, String, Option<u16>)> {
    Some((
        u.scheme().to_ascii_lowercase(),
        u.host_str()?.to_ascii_lowercase(),
        u.port_or_known_default(),
    ))
}

pub fn is_allowed_upstream(url: &str, cfg: &ProxyConfig) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    if is_blocked_ssrf_ip(host) {
        return false;
    }
    if is_loopback_host(host) {
        return cfg.allow_localhost_upstream;
    }
    if let Some(target_origin) = origin_key(&parsed) {
        if cfg.routes.values().any(|r| {
            reqwest::Url::parse(&r.upstream)
                .ok()
                .and_then(|u| origin_key(&u))
                == Some(target_origin.clone())
        }) {
            return true;
        }
    }
    let lower = host.to_ascii_lowercase();
    ALLOWED_HOSTS.contains(&lower.as_str())
        || lower.ends_with(".openai.azure.com")
        || lower.ends_with("-aiplatform.googleapis.com")
}

fn parse_provider(s: &str) -> Provider {
    match s {
        "anthropic" => Provider::Anthropic,
        "openai" => Provider::OpenAi,
        "gemini" => Provider::Gemini,
        "vertex" => Provider::Vertex,
        "azure_openai" => Provider::AzureOpenAi,
        "copilot" => Provider::Copilot,
        "openai_compatible" => Provider::OpenAiCompatible,
        "bedrock" => Provider::Bedrock,
        _ => Provider::Unknown,
    }
}

pub fn redact_secret(s: &str) -> String {
    if s.chars().count() <= 8 {
        "***".into()
    } else {
        let prefix: String = s.chars().take(4).collect();
        format!("{prefix}***")
    }
}

type BoxBody = http_body_util::combinators::BoxBody<Bytes, std::io::Error>;

fn full_body(b: impl Into<Bytes>) -> BoxBody {
    Full::new(b.into()).map_err(|never| match never {}).boxed()
}

fn usage_tokens_from_json(v: &serde_json::Value) -> Option<u64> {
    v["usage"]["output_tokens"]
        .as_u64()
        .or_else(|| v["usage"]["completion_tokens"].as_u64())
        .or_else(|| v["response"]["usage"]["output_tokens"].as_u64())
        .or_else(|| v["message"]["usage"]["output_tokens"].as_u64())
        .or_else(|| v["usageMetadata"]["candidatesTokenCount"].as_u64())
}

pub fn extract_output_tokens(bytes: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(bytes).ok()?;
    let trimmed = text.trim();
    if trimmed.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(n) = usage_tokens_from_json(&v) {
                return Some(n);
            }
        }
    }
    let mut best: Option<u64> = None;
    for line in text.lines() {
        let payload = line.trim().strip_prefix("data:").map(str::trim);
        let Some(p) = payload else {
            continue;
        };
        if p.is_empty() || p == "[DONE]" || !p.starts_with('{') {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(p) {
            if let Some(n) = usage_tokens_from_json(&v) {
                best = Some(best.map_or(n, |cur| cur.max(n)));
            }
        }
    }
    best
}

const SAY_LESS_TAIL_CAP: usize = 64 * 1024;

struct SayLessTapMeta {
    ledger_db: PathBuf,
    provider: Provider,
    tool: String,
    tail: Vec<u8>,
    recorded: bool,
}

impl SayLessTapMeta {
    fn push_bytes(&mut self, chunk: &[u8]) {
        if chunk.len() >= SAY_LESS_TAIL_CAP {
            self.tail.clear();
            self.tail
                .extend_from_slice(&chunk[chunk.len() - SAY_LESS_TAIL_CAP..]);
        } else {
            let overflow = (self.tail.len() + chunk.len()).saturating_sub(SAY_LESS_TAIL_CAP);
            if overflow > 0 {
                self.tail.drain(..overflow);
            }
            self.tail.extend_from_slice(chunk);
        }
    }

    fn flush(&mut self) {
        if self.recorded {
            return;
        }
        self.recorded = true;
        let Some(after) = extract_output_tokens(&self.tail) else {
            return;
        };
        let before = say_less::estimate_before_tokens(after).unwrap_or(after);
        if let Ok(ledger) = Ledger::open(&self.ledger_db) {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let _ = ledger.record(&Event {
                ts,
                layer: Layer::SayLess,
                provider: self.provider,
                tool: self.tool.clone(),
                session: None,
                before,
                after,
                kind: CountKind::Estimated,
                recall_id: None,
            });
        }
    }
}

struct SayLessTapStream<S> {
    inner: Pin<Box<S>>,
    meta: SayLessTapMeta,
}

impl<S> Stream for SayLessTapStream<S>
where
    S: Stream<Item = Result<Frame<Bytes>, std::io::Error>>,
{
    type Item = Result<Frame<Bytes>, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.as_mut().get_mut();
        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    this.meta.push_bytes(data);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(None) => {
                this.meta.flush();
                Poll::Ready(None)
            }
            other => other,
        }
    }
}

impl<S> Drop for SayLessTapStream<S> {
    fn drop(&mut self) {
        self.meta.flush();
    }
}

fn plain_response(status: StatusCode, msg: impl Into<Bytes>) -> Response<BoxBody> {
    let mut resp = Response::new(full_body(msg));
    *resp.status_mut() = status;
    resp
}

pub struct ServerState {
    pub cfg: ProxyConfig,
    pub client: reqwest::Client,
}

impl ServerState {
    pub fn new(cfg: ProxyConfig) -> Result<Self, reqwest::Error> {
        let mut builder = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
        if let Ok(ca_path) = std::env::var("TINKR_CA_BUNDLE") {
            if let Ok(pem) = std::fs::read(&ca_path) {
                if let Ok(cert) = reqwest::Certificate::from_pem(&pem) {
                    builder = builder.add_root_certificate(cert);
                }
            }
        }
        Ok(Self {
            cfg,
            client: builder.build()?,
        })
    }
}

async fn proxy_request(
    req: Request<Incoming>,
    state: Arc<ServerState>,
) -> Result<Response<BoxBody>, hyper::Error> {
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/")
        .to_string();
    if path_and_query == "/healthz" || path_and_query == "/_tinkr/health" {
        return Ok(plain_response(StatusCode::OK, "ok\n"));
    }

    let trimmed = path_and_query.trim_start_matches('/');
    let (route_name, rest) = match trimmed.split_once('/') {
        Some((r, rest)) => (r, format!("/{rest}")),
        None => (trimmed, "/".to_string()),
    };
    let Some(route) = state.cfg.routes.get(route_name) else {
        return Ok(plain_response(
            StatusCode::FORBIDDEN,
            format!("tinkr-proxy: unknown route '{route_name}'"),
        ));
    };
    let target_url = format!("{}{}", route.upstream.trim_end_matches('/'), rest);
    let same_origin = reqwest::Url::parse(&route.upstream)
        .ok()
        .and_then(|u| origin_key(&u))
        .zip(
            reqwest::Url::parse(&target_url)
                .ok()
                .and_then(|u| origin_key(&u)),
        )
        .is_some_and(|(a, b)| a == b);
    if !same_origin || !is_allowed_upstream(&target_url, &state.cfg) {
        return Ok(plain_response(
            StatusCode::FORBIDDEN,
            "tinkr-proxy: upstream host not in allow-list",
        ));
    }

    let method = req.method().clone();
    let headers = req.headers().clone();
    if let Some(len) = headers
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok())
    {
        if len > MAX_BODY_BYTES {
            return Ok(plain_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "tinkr-proxy: request body exceeds limit",
            ));
        }
    }
    let limited = http_body_util::Limited::new(req.into_body(), MAX_BODY_BYTES);
    let raw_bytes = match limited.collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => {
            return Ok(plain_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "bad or oversized request body",
            ));
        }
    };

    let provider_hint = parse_provider(&route.provider);
    let recall = state
        .cfg
        .paths
        .as_ref()
        .and_then(|p| RecallStore::open(&p.recall_dir()).ok());
    let transformed = if method == hyper::Method::POST && !raw_bytes.is_empty() {
        adapters::transform_request(provider_hint, &rest, &raw_bytes, recall.as_ref(), false)
    } else {
        adapters::TransformResult {
            body: raw_bytes.to_vec(),
            provider: provider_hint,
            before: 0,
            after: 0,
            modified: false,
            say_less_applied: false,
        }
    };

    if transformed.before > transformed.after {
        if let Some(paths) = &state.cfg.paths {
            if let Ok(ledger) = Ledger::open(&paths.ledger_db()) {
                let ts = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                let _ = ledger.record(&Event {
                    ts,
                    layer: Layer::SendLess,
                    provider: transformed.provider,
                    tool: route_name.to_string(),
                    session: None,
                    before: transformed.before,
                    after: transformed.after,
                    kind: if transformed.provider == Provider::OpenAi {
                        CountKind::Measured
                    } else {
                        CountKind::Estimated
                    },
                    recall_id: None,
                });
            }
        }
    }

    let mut out_req = state
        .client
        .request(method, &target_url)
        .body(transformed.body);
    for (k, v) in headers.iter() {
        let name = k.as_str();
        if matches!(
            name,
            "host" | "content-length" | "transfer-encoding" | "connection"
        ) {
            continue;
        }
        out_req = out_req.header(k, v);
    }

    let resp = match out_req.send().await {
        Ok(r) => r,
        Err(e) => {
            return Ok(plain_response(
                StatusCode::BAD_GATEWAY,
                format!("tinkr-proxy upstream error: {e}"),
            ));
        }
    };

    let resp_ok = resp.status().is_success();
    let mut builder = Response::builder().status(resp.status());
    for (k, v) in resp.headers() {
        let name = k.as_str();
        if matches!(name, "transfer-encoding" | "connection" | "content-length") {
            continue;
        }
        builder = builder.header(k, v);
    }
    let stream = resp
        .bytes_stream()
        .map_ok(Frame::data)
        .map_err(std::io::Error::other);
    let body: BoxBody = if transformed.say_less_applied && resp_ok {
        if let Some(paths) = &state.cfg.paths {
            let tapped = SayLessTapStream {
                inner: Box::pin(stream),
                meta: SayLessTapMeta {
                    ledger_db: paths.ledger_db(),
                    provider: transformed.provider,
                    tool: route_name.to_string(),
                    tail: Vec::new(),
                    recorded: false,
                },
            };
            BodyExt::boxed(StreamBody::new(tapped))
        } else {
            BodyExt::boxed(StreamBody::new(stream))
        }
    } else {
        BodyExt::boxed(StreamBody::new(stream))
    };
    Ok(builder.body(body).unwrap_or_else(|_| {
        plain_response(
            StatusCode::BAD_GATEWAY,
            "tinkr-proxy: invalid upstream response",
        )
    }))
}

pub async fn spawn_proxy(
    port: u16,
    cfg: ProxyConfig,
) -> std::io::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await?;
    let addr = listener.local_addr()?;
    let state = Arc::new(ServerState::new(cfg).map_err(std::io::Error::other)?);
    let handle = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let st = state.clone();
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service_fn(move |req| proxy_request(req, st.clone())))
                    .await;
            });
        }
    });
    Ok((addr, handle))
}

pub fn write_state(path: &Path, state: &ProxyState) -> tinkr_core::Result<()> {
    let bytes = serde_json::to_vec_pretty(state)?;
    fsx::atomic_write(path, &bytes)
}
