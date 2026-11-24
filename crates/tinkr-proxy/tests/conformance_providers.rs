use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, service::service_fn, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};
use tinkr_core::{provider::Provider, recall::RecallStore};
use tinkr_proxy::{adapters, spawn_proxy, ProxyConfig, Route};
use tokio::net::TcpListener;

async fn spawn_mock_upstream() -> (
    SocketAddr,
    Arc<Mutex<Vec<(String, HashMap<String, String>, Vec<u8>)>>>,
) {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = seen.clone();

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let seen = seen_clone.clone();
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(move |req: Request<Incoming>| {
                            let seen = seen.clone();
                            async move {
                                let path = req.uri().path().to_string();
                                let mut hdrs = HashMap::new();
                                for (k, v) in req.headers() {
                                    hdrs.insert(k.as_str().to_lowercase(), v.to_str().unwrap_or("").to_string());
                                }
                                let body = req.into_body().collect().await.unwrap().to_bytes().to_vec();
                                seen.lock().unwrap().push((path.clone(), hdrs, body));

                                if path.contains("/rate-limit") {
                                    return Ok::<_, hyper::Error>(
                                        Response::builder()
                                            .status(StatusCode::TOO_MANY_REQUESTS)
                                            .header("retry-after", "7")
                                            .body(Full::new(Bytes::from("{\"error\":\"rate_limited\"}")))
                                            .unwrap(),
                                    );
                                }
                                if path.contains("/stream") {
                                    let sse = "data: {\"chunk\":1}\n\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":130}}\n\ndata: [DONE]\n\n";
                                    return Ok(Response::builder()
                                        .status(StatusCode::OK)
                                        .header("content-type", "text/event-stream")
                                        .body(Full::new(Bytes::from(sse)))
                                        .unwrap());
                                }
                                Ok(Response::builder()
                                    .status(StatusCode::OK)
                                    .header("content-type", "application/json")
                                    .body(Full::new(Bytes::from("{\"ok\":true,\"usage\":{\"output_tokens\":65}}")))
                                    .unwrap())
                            }
                        }),
                    )
                    .await;
            });
        }
    });
    (addr, seen)
}

fn big_log_with_needle(needle: &str) -> String {
    let mut s = String::new();
    for i in 0..120 {
        s.push_str(&format!(
            "2026-11-23T10:00:{i:02}Z DEBUG worker tick processing batch item {i}\n"
        ));
        if i == 60 {
            s.push_str(&format!("2026-11-23T10:01:00Z ERROR {needle}\n"));
        }
    }
    s
}

#[test]
fn conformance_byte_identical_when_unmodified_and_unknown_fields_preserved() {
    let raw = br#"{"x_custom": 123,  "spaced": true}"#;
    let res = adapters::transform_request(Provider::Bedrock, "/invoke", raw, None, false);
    assert!(!res.modified);
    assert_eq!(res.body, raw);

    let raw_small = br#"{"model":"gpt-4o",  "unknown_vendor_flag": {"nested": [1,2]}, "messages":[{"role":"user","content":"hi"}]}"#;
    let res = adapters::transform_request(
        Provider::Copilot,
        "/chat/completions",
        raw_small,
        None,
        false,
    );
    assert!(!res.modified);
    assert_eq!(res.body, raw_small);
}

#[test]
fn conformance_anthropic_cache_prefix_and_needle() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecallStore::open(dir.path()).unwrap();
    let cached_big = big_log_with_needle("cached_should_not_be_touched");
    let fresh_big = big_log_with_needle("fresh_needle_src/api.rs:99");

    let req = json!({
        "model": "claude-sonnet-4-5",
        "max_tokens": 1024,
        "custom_beta_field": "preserve-me",
        "system": [
            { "type": "text", "text": "You are an assistant.", "cache_control": { "type": "ephemeral" } }
        ],
        "messages": [
            {
                "role": "user",
                "content": [
                    { "type": "tool_result", "tool_use_id": "t1", "content": cached_big, "cache_control": { "type": "ephemeral" } }
                ]
            },
            {
                "role": "assistant",
                "content": [{ "type": "text", "text": "checking" }]
            },
            {
                "role": "user",
                "content": [
                    { "type": "tool_result", "tool_use_id": "t2", "content": fresh_big }
                ]
            }
        ]
    });
    let raw = serde_json::to_vec(&req).unwrap();
    let res = adapters::transform_request(
        Provider::Anthropic,
        "/v1/messages",
        &raw,
        Some(&store),
        false,
    );
    assert!(res.modified && res.say_less_applied);
    assert!(res.after < res.before / 2);

    let out: Value = serde_json::from_slice(&res.body).unwrap();
    assert_eq!(out["custom_beta_field"], "preserve-me");
    assert_eq!(out["system"][0], req["system"][0]);
    assert!(out["system"][1]["text"]
        .as_str()
        .unwrap()
        .contains("Response style (Tinkr):"));
    assert_eq!(
        out["messages"][0]["content"][0]["content"],
        req["messages"][0]["content"][0]["content"]
    );
    let compressed = out["messages"][2]["content"][0]["content"]
        .as_str()
        .unwrap();
    assert!(compressed.contains("fresh_needle_src/api.rs:99"));
    assert!(compressed.contains("[tinkr:"));
}

#[test]
fn conformance_openai_responses_gemini_and_copilot() {
    let big = big_log_with_needle("needle_42");

    let req = json!({
        "model": "gpt-4o",
        "extra_field": 7,
        "messages": [
            { "role": "system", "content": "Be helpful." },
            { "role": "assistant", "content": "calling tool" },
            { "role": "tool", "tool_call_id": "c1", "content": big }
        ]
    });
    let res = adapters::transform_request(
        Provider::OpenAi,
        "/v1/chat/completions",
        &serde_json::to_vec(&req).unwrap(),
        None,
        false,
    );
    let out: Value = serde_json::from_slice(&res.body).unwrap();
    assert_eq!(out["extra_field"], 7);
    assert!(out["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("Response style (Tinkr):"));
    assert!(out["messages"][2]["content"]
        .as_str()
        .unwrap()
        .contains("needle_42"));
    assert!(res.after < res.before / 2);

    let req_resp = json!({
        "model": "gpt-5",
        "instructions": "Base instructions.",
        "input": [
            { "type": "function_call_output", "call_id": "c1", "output": big }
        ]
    });
    let res = adapters::transform_request(
        Provider::OpenAi,
        "/v1/responses",
        &serde_json::to_vec(&req_resp).unwrap(),
        None,
        false,
    );
    let out: Value = serde_json::from_slice(&res.body).unwrap();
    assert!(out["instructions"]
        .as_str()
        .unwrap()
        .contains("Response style (Tinkr):"));
    assert!(out["input"][0]["output"]
        .as_str()
        .unwrap()
        .contains("needle_42"));

    let res = adapters::transform_request(
        Provider::Copilot,
        "/chat/completions",
        &serde_json::to_vec(&req).unwrap(),
        None,
        false,
    );
    let out: Value = serde_json::from_slice(&res.body).unwrap();
    assert!(!res.say_less_applied);
    assert_eq!(out["messages"][0]["content"], "Be helpful.");
    assert!(out["messages"][2]["content"]
        .as_str()
        .unwrap()
        .contains("needle_42"));

    let req_gem = json!({
        "systemInstruction": { "parts": [{ "text": "Gemini sys." }] },
        "contents": [
            { "role": "user", "parts": [{ "functionResponse": { "name": "run", "response": { "output": big } } }] }
        ]
    });
    let res = adapters::transform_request(
        Provider::Gemini,
        "/v1beta/models/gemini:generateContent",
        &serde_json::to_vec(&req_gem).unwrap(),
        None,
        false,
    );
    let out: Value = serde_json::from_slice(&res.body).unwrap();
    assert!(res.say_less_applied);
    assert_eq!(out["systemInstruction"]["parts"][0]["text"], "Gemini sys.");
    assert!(out["systemInstruction"]["parts"][1]["text"]
        .as_str()
        .unwrap()
        .contains("Response style (Tinkr):"));
    assert!(
        out["contents"][0]["parts"][0]["functionResponse"]["response"]["output"]
            .as_str()
            .unwrap()
            .contains("needle_42")
    );
}

#[tokio::test]
async fn conformance_live_proxy_streaming_auth_429_and_allowlist() {
    let (up_addr, seen) = spawn_mock_upstream().await;
    let mut cfg = ProxyConfig::default();
    cfg.routes.insert(
        "mock-anthropic".into(),
        Route {
            upstream: format!("http://{up_addr}"),
            provider: "anthropic".into(),
        },
    );
    let (proxy_addr, _handle) = spawn_proxy(0, cfg).await.unwrap();
    let client = reqwest::Client::new();

    let h = client
        .get(format!("http://{proxy_addr}/healthz"))
        .send()
        .await
        .unwrap();
    assert_eq!(h.status(), StatusCode::OK);

    let bad = client
        .get(format!("http://{proxy_addr}/evil-host/v1/messages"))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::FORBIDDEN);

    let body = json!({
        "model": "claude-sonnet-4-5",
        "messages": [
            { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "1", "content": big_log_with_needle("live_needle") }] }
        ]
    });
    let r = client
        .post(format!("http://{proxy_addr}/mock-anthropic/v1/messages"))
        .header("x-api-key", "sk-ant-secret-test-key")
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&body).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    let r_stream = client
        .post(format!(
            "http://{proxy_addr}/mock-anthropic/v1/messages/stream"
        ))
        .header("x-api-key", "sk-ant-secret-test-key")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(r_stream.status(), StatusCode::OK);
    let sse = r_stream.text().await.unwrap();
    assert!(sse.contains("{\"chunk\":1}") && sse.contains("[DONE]"));

    let r_429 = client
        .post(format!("http://{proxy_addr}/mock-anthropic/rate-limit"))
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(r_429.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r_429.headers().get("retry-after").unwrap(), "7");

    let recorded = seen.lock().unwrap();
    let (_, hdrs, sent_body) = &recorded[0];
    assert_eq!(
        hdrs.get("x-api-key").map(String::as_str),
        Some("sk-ant-secret-test-key")
    );
    let sent_str = String::from_utf8_lossy(sent_body);
    assert!(sent_str.contains("live_needle"));
    assert!(sent_str.contains("Response style (Tinkr):"));
}

#[test]
fn ssrf_allowlist_and_utf8_redaction() {
    let mut cfg = ProxyConfig {
        allow_localhost_upstream: false,
        ..Default::default()
    };
    assert!(!tinkr_proxy::is_allowed_upstream(
        "http://127.0.0.1:8080/v1",
        &cfg
    ));
    assert!(!tinkr_proxy::is_allowed_upstream(
        "http://localhost:8080/v1",
        &cfg
    ));
    assert!(!tinkr_proxy::is_allowed_upstream(
        "http://[::1]:8080/v1",
        &cfg
    ));
    assert!(!tinkr_proxy::is_allowed_upstream(
        "http://169.254.169.254/latest/meta-data/",
        &cfg
    ));
    assert!(!tinkr_proxy::is_allowed_upstream(
        "http://0.0.0.0:80/",
        &cfg
    ));
    assert!(!tinkr_proxy::is_allowed_upstream(
        "https://api.openai.com.evil.com/v1",
        &cfg
    ));
    assert!(!tinkr_proxy::is_allowed_upstream(
        "file:///etc/passwd",
        &cfg
    ));
    assert!(tinkr_proxy::is_allowed_upstream(
        "https://api.openai.com/v1/chat/completions",
        &cfg
    ));
    cfg.allow_localhost_upstream = true;
    assert!(tinkr_proxy::is_allowed_upstream(
        "http://127.0.0.1:8080/v1",
        &cfg
    ));
    assert!(!tinkr_proxy::is_allowed_upstream(
        "http://169.254.169.254/latest/meta-data/",
        &cfg
    ));

    assert_eq!(tinkr_proxy::redact_secret("short"), "***");
    assert_eq!(
        tinkr_proxy::redact_secret("🔑🔑🔑🔑🔑🔑🔑🔑🔑"),
        "🔑🔑🔑🔑***"
    );
    assert_eq!(
        tinkr_proxy::redact_secret("sk-ant-1234567890"),
        "sk-a***"
    );
}

#[tokio::test]
async fn proxy_records_say_less_from_json_and_sse_usage() {
    assert_eq!(
        tinkr_proxy::extract_output_tokens(br#"{"usage":{"output_tokens":130}}"#),
        Some(130)
    );
    assert_eq!(
        tinkr_proxy::extract_output_tokens(br#"{"usage":{"completion_tokens":80}}"#),
        Some(80)
    );
    assert_eq!(
        tinkr_proxy::extract_output_tokens(
            br#"{"usageMetadata":{"candidatesTokenCount":95}}"#
        ),
        Some(95)
    );
    assert_eq!(
        tinkr_proxy::extract_output_tokens(
            b"data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"output_tokens\":1}}}\n\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":130}}\n\ndata: [DONE]\n\n"
        ),
        Some(130)
    );

    let (up_addr, _) = spawn_mock_upstream().await;
    let dir = tempfile::tempdir().unwrap();
    let paths = tinkr_core::platform::Paths {
        config: dir.path().join("cfg"),
        data: dir.path().join("data"),
        cache: dir.path().join("cache"),
    };
    let mut cfg = ProxyConfig {
        paths: Some(paths.clone()),
        ..Default::default()
    };
    cfg.routes.insert(
        "mock-anthropic".into(),
        Route {
            upstream: format!("http://{up_addr}"),
            provider: "anthropic".into(),
        },
    );
    let (proxy_addr, _handle) = spawn_proxy(0, cfg).await.unwrap();
    let client = reqwest::Client::new();

    let req_body = json!({
        "model": "claude-sonnet-4-5",
        "messages": [{ "role": "user", "content": "hi" }]
    });
    let r = client
        .post(format!("http://{proxy_addr}/mock-anthropic/v1/messages"))
        .body(serde_json::to_vec(&req_body).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let _ = r.text().await.unwrap();

    let r2 = client
        .post(format!(
            "http://{proxy_addr}/mock-anthropic/v1/messages/stream"
        ))
        .body(serde_json::to_vec(&req_body).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status(), StatusCode::OK);
    let _ = r2.text().await.unwrap();

    let ledger = tinkr_core::ledger::Ledger::open(&paths.ledger_db()).unwrap();
    let summary = ledger.summary(None).unwrap();
    let say = summary
        .by_layer
        .get("say_less")
        .expect("say_less layer must be recorded");
    assert_eq!(say.events, 2);
    assert_eq!(say.estimated_events, 2);
    assert_eq!(say.after, 195);
    assert_eq!(say.before, 300);
    assert_eq!(say.saved, 105);
}
