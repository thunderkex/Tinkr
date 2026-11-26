use serde_json::{json, Value};
use tinkr_core::{
    adapters,
    filters::{self, Ctx, Decision},
    mcp,
    provider::Provider,
    recall::RecallStore,
    tokenizer::TokenizerKind,
};

fn sample_rust_file(override_line: Option<(usize, &str)>, crlf: bool) -> String {
    let sep = if crlf { "\r\n" } else { "\n" };
    let mut lines = Vec::with_capacity(260);
    lines.push("pub mod service {".to_string());
    for i in 1..=250 {
        if let Some((ln, custom)) = override_line {
            if i == ln {
                lines.push(custom.to_string());
                continue;
            }
        }
        lines.push(format!(
            "    pub fn compute_slot_{i:03}(seed: u64) -> u64 {{ seed.wrapping_add({i}) }}"
        ));
    }
    lines.push("}".to_string());
    lines.join(sep) + sep
}

#[test]
fn golden_mcp_shell_and_openai_read_dedup_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecallStore::open(&dir.path().join("recall")).unwrap();
    let fpath = dir.path().join("service.rs");

    let v1 = sample_rust_file(None, false);
    std::fs::write(&fpath, &v1).unwrap();

    let req1 = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "tinkr_read", "arguments": { "path": fpath.to_string_lossy() } }
    });
    let r1 = mcp::handle_request(&req1, &store, false).unwrap();
    assert_eq!(r1["result"]["content"][0]["text"].as_str().unwrap(), v1);

    let v1_crlf = sample_rust_file(None, true);
    std::fs::write(&fpath, &v1_crlf).unwrap();
    let r2 = mcp::handle_request(&req1, &store, false).unwrap();
    let r2_txt = r2["result"]["content"][0]["text"].as_str().unwrap();
    assert!(r2_txt.contains("unchanged since read #1 ["), "{r2_txt}");
    let recall_id = r2_txt
        .split("[tinkr:")
        .nth(1)
        .unwrap()
        .split([' ', '|'])
        .next()
        .unwrap();
    assert_eq!(store.get(recall_id).unwrap(), v1_crlf.as_bytes());

    let req_slice = json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {
            "name": "tinkr_read",
            "arguments": { "path": fpath.to_string_lossy(), "start_line": 40, "end_line": 110 }
        }
    });
    let r3 = mcp::handle_request(&req_slice, &store, false).unwrap();
    let r3_txt = r3["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        r3_txt.contains("unchanged lines 40..110 since read #1 ["),
        "{r3_txt}"
    );

    let v2 = sample_rust_file(
        Some((120, "    pub fn compute_slot_120(seed: u64) -> u64 { seed ^ 0xdead_beef /* golden_edit */ }")),
        false,
    );
    std::fs::write(&fpath, &v2).unwrap();
    let r4 = mcp::handle_request(&req1, &store, false).unwrap();
    let r4_txt = r4["result"]["content"][0]["text"].as_str().unwrap();
    assert!(r4_txt.contains("changed since read #1 ["), "{r4_txt}");
    assert!(r4_txt.contains("golden_edit"), "{r4_txt}");

    let ctx = Ctx {
        recall: Some(&store),
        unicode: false,
        tokenizer: TokenizerKind::Exact,
    };
    let cmd = format!("cat {}", fpath.to_string_lossy());
    let shell_out = filters::process(&cmd, 0, v2.as_bytes(), &ctx);
    assert!(matches!(shell_out.decision, Decision::Compressed { .. }));
    let shell_txt = String::from_utf8_lossy(&shell_out.output);
    assert!(shell_txt.contains("unchanged since read #"), "{shell_txt}");

    let oai_req = json!({
        "model": "gpt-5",
        "messages": [
            { "role": "system", "content": "Response style (Tinkr): active" },
            {
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": "read_file", "arguments": "{\"path\":\"src/mod.rs\"}" }
                }]
            },
            { "role": "tool", "tool_call_id": "call_1", "content": v1 },
            {
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_2",
                    "type": "function",
                    "function": { "name": "read_file", "arguments": "{\"path\":\"src/mod.rs\"}" }
                }]
            },
            { "role": "tool", "tool_call_id": "call_2", "content": v2 }
        ]
    });
    let tr = adapters::transform_request(
        Provider::OpenAi,
        "/v1/chat/completions",
        &serde_json::to_vec(&oai_req).unwrap(),
        Some(&store),
        false,
    );
    assert!(tr.modified);
    let out_oai: Value = serde_json::from_slice(&tr.body).unwrap();
    assert_eq!(out_oai["messages"][2]["content"].as_str().unwrap(), v1);
    let tail_oai = out_oai["messages"][4]["content"].as_str().unwrap();
    assert!(tail_oai.contains("changed since read #1") && tail_oai.contains("golden_edit"));
}
