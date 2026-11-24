use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, service::service_fn, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tinkr_core::{lifecycle, platform::Paths, provider::Provider, recall::RecallStore};
use tinkr_proxy::{adapters, spawn_proxy, ProxyConfig, Route, MAX_BODY_BYTES};
use tokio::net::TcpListener;

async fn spawn_echo_upstream() -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_c = hits.clone();

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let hits = hits_c.clone();
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(move |req: Request<Incoming>| {
                            let hits = hits.clone();
                            async move {
                                hits.fetch_add(1, Ordering::Relaxed);
                                let path = req.uri().path().to_string();
                                let tag = req
                                    .headers()
                                    .get("x-trace-id")
                                    .and_then(|v| v.to_str().ok())
                                    .unwrap_or("")
                                    .to_string();
                                let _ = req.into_body().collect().await;

                                if path.contains("/redirect-ssrf") {
                                    return Ok::<_, hyper::Error>(
                                        Response::builder()
                                            .status(StatusCode::FOUND)
                                            .header(
                                                "location",
                                                "http://169.254.169.254/latest/meta-data/",
                                            )
                                            .body(Full::new(Bytes::from("redirect")))
                                            .unwrap(),
                                    );
                                }
                                if path.contains("/err503") {
                                    return Ok(Response::builder()
                                        .status(StatusCode::SERVICE_UNAVAILABLE)
                                        .body(Full::new(Bytes::from("upstream overloaded")))
                                        .unwrap());
                                }
                                Ok(Response::builder()
                                    .status(StatusCode::OK)
                                    .header("x-echo-path", path)
                                    .header("x-echo-trace", tag)
                                    .body(Full::new(Bytes::from("{\"ok\":true}")))
                                    .unwrap())
                            }
                        }),
                    )
                    .await;
            });
        }
    });
    (addr, hits)
}

#[test]
fn safety_exceptions_in_provider_inputs() {
    let anth = json!({
        "model": "claude-sonnet-4-5",
        "system": "Base sys.",
        "messages": [
            { "role": "user", "content": [{ "type": "text", "text": "I'm confused, walk me through this step by step" }] }
        ]
    });
    let res = adapters::transform_request(
        Provider::Anthropic,
        "/v1/messages",
        &serde_json::to_vec(&anth).unwrap(),
        None,
        false,
    );
    assert!(
        !res.say_less_applied,
        "Anthropic array user confusion must skip say-less"
    );
    assert!(
        !res.modified,
        "unmodified Anthropic request must be byte-identical"
    );

    let resp = json!({
        "model": "gpt-5",
        "instructions": "Base instructions.",
        "input": [
            { "role": "user", "content": [{ "type": "input_text", "text": "I do not understand, explain in detail" }] }
        ]
    });
    let res = adapters::transform_request(
        Provider::OpenAi,
        "/v1/responses",
        &serde_json::to_vec(&resp).unwrap(),
        None,
        false,
    );
    assert!(
        !res.say_less_applied,
        "Responses API confusion must skip say-less"
    );
    assert!(!res.modified);

    let weird = br#"{"foo":  1, "bar": [true, false]}"#;
    let res = adapters::transform_request(Provider::Unknown, "/weird", weird, None, false);
    assert!(!res.modified);
    assert_eq!(res.body, weird);
}

#[tokio::test]
async fn parallel_provider_streams_no_crosstalk() {
    let (up_addr, _) = spawn_echo_upstream().await;
    let mut cfg = ProxyConfig::default();
    for (route, prov) in [
        ("r-anth", "anthropic"),
        ("r-oai", "openai"),
        ("r-gem", "gemini"),
        ("r-cop", "copilot"),
    ] {
        cfg.routes.insert(
            route.into(),
            Route {
                upstream: format!("http://{up_addr}"),
                provider: prov.into(),
            },
        );
    }
    let (proxy_addr, handle) = spawn_proxy(0, cfg).await.unwrap();
    let client = Arc::new(reqwest::Client::new());

    let mut tasks = Vec::with_capacity(200);
    let routes = ["r-anth", "r-oai", "r-gem", "r-cop"];
    for i in 0..200usize {
        let client = client.clone();
        let route = routes[i % routes.len()];
        let trace = format!("trace-{i}-{route}");
        tasks.push(tokio::spawn(async move {
            let r = client
                .post(format!("http://{proxy_addr}/{route}/req/{i}"))
                .header("x-trace-id", &trace)
                .body(r#"{"ping":true}"#)
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::OK);
            let echo_path = r.headers().get("x-echo-path").unwrap().to_str().unwrap();
            let echo_trace = r.headers().get("x-echo-trace").unwrap().to_str().unwrap();
            assert_eq!(echo_path, format!("/req/{i}"), "cross-talk on path!");
            assert_eq!(echo_trace, trace, "cross-talk on trace header!");
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    handle.abort();
}

#[tokio::test]
async fn proxy_redirect_fallback_and_no_leaks() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths {
        config: dir.path().join("cfg"),
        data: dir.path().join("data"),
        cache: dir.path().join("cache"),
    };
    let (up_addr, hits) = spawn_echo_upstream().await;
    let mut cfg = ProxyConfig {
        paths: Some(paths.clone()),
        ..Default::default()
    };
    cfg.routes.insert(
        "mock".into(),
        Route {
            upstream: format!("http://{up_addr}"),
            provider: "anthropic".into(),
        },
    );
    let (proxy_addr, handle) = spawn_proxy(0, cfg).await.unwrap();
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    let r = client
        .get(format!("http://{proxy_addr}/mock/redirect-ssrf"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FOUND);

    let before_hits = hits.load(Ordering::Relaxed);
    let r503 = client
        .post(format!("http://{proxy_addr}/mock/err503"))
        .header("x-api-key", "sk-ant-PLANTED_SECRET_9999999999")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(r503.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        hits.load(Ordering::Relaxed),
        before_hits + 1,
        "proxy must not retry 5xx"
    );

    let r413 = client
        .post(format!("http://{proxy_addr}/mock/v1/messages"))
        .header("content-length", (MAX_BODY_BYTES + 1).to_string())
        .body("x")
        .send()
        .await;
    let _ = r413;

    assert!(lifecycle::is_proxy_alive(proxy_addr.port()));
    assert_eq!(
        lifecycle::resolve_endpoint(Some(proxy_addr.port()), "mock", "https://api.anthropic.com"),
        format!("http://127.0.0.1:{}/mock", proxy_addr.port())
    );
    handle.abort();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        lifecycle::resolve_endpoint(Some(1), "mock", "https://api.anthropic.com"),
        "https://api.anthropic.com"
    );

    let _store = RecallStore::open(&paths.recall_dir()).unwrap();
    fn check_dir(d: &std::path::Path, needle: &str) {
        if let Ok(rd) = std::fs::read_dir(d) {
            for ent in rd.flatten() {
                let p = ent.path();
                if p.is_dir() {
                    check_dir(&p, needle);
                } else if let Ok(bytes) = std::fs::read(&p) {
                    assert!(
                        !String::from_utf8_lossy(&bytes).contains(needle),
                        "SECRET LEAKED in {}",
                        p.display()
                    );
                }
            }
        }
    }
    check_dir(dir.path(), "PLANTED_SECRET_9999999999");

    let _: Value = json!(null);
}
