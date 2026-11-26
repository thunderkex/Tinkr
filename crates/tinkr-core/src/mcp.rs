use crate::{
    compress,
    ledger::{CountKind, Event, Layer, Ledger},
    provider::Provider,
    recall::RecallStore,
    tokenizer::TokenizerKind,
};
use serde_json::{json, Value};
use std::{
    io::{BufRead, Write},
    time::{SystemTime, UNIX_EPOCH},
};

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn handle_request(req: &Value, recall: &RecallStore, unicode: bool) -> Option<Value> {
    handle_request_with_ledger(req, recall, None, unicode)
}

pub fn handle_request_with_ledger(
    req: &Value,
    recall: &RecallStore,
    ledger: Option<&Ledger>,
    unicode: bool,
) -> Option<Value> {
    let id = req.get("id").cloned();
    let method = req["method"].as_str()?;
    if id.is_none() && method.starts_with("notifications/") {
        return None;
    }
    let result = match method {
        "initialize" => json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "tinkr", "version": env!("CARGO_PKG_VERSION") }
        }),
        "tools/list" => json!({
            "tools": [
                {
                    "name": "tinkr_retrieve",
                    "description": "Retrieve the original uncompressed output for a Tinkr recall id.",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "id": { "type": "string", "description": "Recall id from [tinkr:<id> ...]" } },
                        "required": ["id"]
                    }
                },
                {
                    "name": "tinkr_compress",
                    "description": "Compress a large text, JSON, log, CSV, HTML, or code payload while preserving errors and storing the original for recall.",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "text": { "type": "string" } },
                        "required": ["text"]
                    }
                },
                {
                    "name": "tinkr_read",
                    "description": "Read a local file and return a compact or deduplicated view when large or re-read, with a recall id for the full original.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" },
                            "start_line": { "type": "integer", "description": "Optional 1-based start line (inclusive)" },
                            "end_line": { "type": "integer", "description": "Optional 1-based end line (inclusive)" }
                        },
                        "required": ["path"]
                    }
                }
            ]
        }),
        "tools/call" => {
            let name = req["params"]["name"].as_str().unwrap_or("");
            let args = &req["params"]["arguments"];
            match name {
                "tinkr_retrieve" => {
                    let rid = args["id"].as_str().unwrap_or("");
                    match recall.get(rid) {
                        Ok(bytes) => {
                            if let Some(l) = ledger {
                                let _ = l.record_recall(rid, now_ts());
                            }
                            json!({ "content": [{ "type": "text", "text": String::from_utf8_lossy(&bytes) }] })
                        }
                        Err(e) => {
                            json!({ "isError": true, "content": [{ "type": "text", "text": e.to_string() }] })
                        }
                    }
                }
                "tinkr_compress" => {
                    let text = args["text"].as_str().unwrap_or("");
                    let out = match compress::compress_tool_result(
                        text,
                        TokenizerKind::Exact,
                        Some(recall),
                        unicode,
                    ) {
                        Some(c) => {
                            if c.before > c.after {
                                if let Some(l) = ledger {
                                    let _ = l.record(&Event {
                                        ts: now_ts(),
                                        layer: Layer::ReadLess,
                                        provider: Provider::None,
                                        tool: "mcp-compress".to_string(),
                                        session: None,
                                        before: c.before,
                                        after: c.after,
                                        kind: CountKind::Measured,
                                        recall_id: c.recall_id,
                                    });
                                }
                            }
                            c.text
                        }
                        None => text.to_string(),
                    };
                    json!({ "content": [{ "type": "text", "text": out }] })
                }
                "tinkr_read" => {
                    let p = args["path"].as_str().unwrap_or("");
                    let start_line = args["start_line"].as_u64().map(|n| (n as usize).max(1));
                    let end_line = args["end_line"].as_u64().map(|n| (n as usize).max(1));
                    match std::fs::read(p) {
                        Ok(full_bytes) => {
                            let full_str = String::from_utf8_lossy(&full_bytes);
                            let (slice_cow, range_opt): (std::borrow::Cow<'_, [u8]>, Option<(usize, usize)>) =
                                if start_line.is_some() || end_line.is_some() {
                                    let lines: Vec<&str> = full_str.lines().collect();
                                    let s_ln = start_line.unwrap_or(1).min(lines.len().max(1));
                                    let e_ln = end_line.unwrap_or(lines.len()).max(s_ln).min(lines.len());
                                    let sliced = if lines.is_empty() {
                                        String::new()
                                    } else {
                                        lines[s_ln - 1..e_ln].join("\n")
                                    };
                                    (std::borrow::Cow::Owned(sliced.into_bytes()), Some((s_ln, e_ln)))
                                } else {
                                    (std::borrow::Cow::Borrowed(&full_bytes), None)
                                };

                            let cache_path = recall.dir().join("read-cache.json");
                            let mut cache = crate::read_dedup::ReadCache::load(&cache_path);
                            let dedup_hit = cache.observe_read(
                                p,
                                &slice_cow,
                                range_opt,
                                TokenizerKind::Exact,
                                Some(recall),
                                unicode,
                            );
                            cache.save(&cache_path);

                            let out = if let Some(hit) = dedup_hit {
                                if hit.before > hit.after {
                                    if let Some(l) = ledger {
                                        let _ = l.record(&Event {
                                            ts: now_ts(),
                                            layer: Layer::ReadLess,
                                            provider: Provider::None,
                                            tool: "mcp-read".to_string(),
                                            session: None,
                                            before: hit.before,
                                            after: hit.after,
                                            kind: CountKind::Measured,
                                            recall_id: hit.recall_id,
                                        });
                                    }
                                }
                                hit.text
                            } else {
                                let s = String::from_utf8_lossy(&slice_cow);
                                let keep_lossless_first_read =
                                    crate::read_dedup::is_source_code_path(p)
                                        && s.lines().count() <= 2500;
                                if keep_lossless_first_read {
                                    s.into_owned()
                                } else {
                                    match compress::compress_tool_result(
                                        &s,
                                        TokenizerKind::Exact,
                                        Some(recall),
                                        unicode,
                                    ) {
                                        Some(c) => {
                                            if c.before > c.after {
                                                if let Some(l) = ledger {
                                                    let _ = l.record(&Event {
                                                        ts: now_ts(),
                                                        layer: Layer::ReadLess,
                                                        provider: Provider::None,
                                                        tool: "mcp-read".to_string(),
                                                        session: None,
                                                        before: c.before,
                                                        after: c.after,
                                                        kind: CountKind::Measured,
                                                        recall_id: c.recall_id,
                                                    });
                                                }
                                            }
                                            c.text
                                        }
                                        None => s.into_owned(),
                                    }
                                }
                            };
                            json!({ "content": [{ "type": "text", "text": out }] })
                        }
                        Err(e) => {
                            json!({ "isError": true, "content": [{ "type": "text", "text": e.to_string() }] })
                        }
                    }
                }
                _ => {
                    return Some(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32601, "message": format!("unknown tool '{name}'") }
                    }));
                }
            }
        }
        _ => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("method not found: {method}") }
            }));
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

pub fn serve_stdio<R: BufRead, W: Write>(
    reader: R,
    writer: W,
    recall: &RecallStore,
    unicode: bool,
) -> std::io::Result<()> {
    serve_stdio_with_ledger(reader, writer, recall, None, unicode)
}

pub fn serve_stdio_with_ledger<R: BufRead, W: Write>(
    reader: R,
    mut writer: W,
    recall: &RecallStore,
    ledger: Option<&Ledger>,
    unicode: bool,
) -> std::io::Result<()> {
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(req) = serde_json::from_str::<Value>(&line) {
            if let Some(resp) = handle_request_with_ledger(&req, recall, ledger, unicode) {
                writeln!(
                    writer,
                    "{}",
                    serde_json::to_string(&resp).unwrap_or_default()
                )?;
                writer.flush()?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_initialize_list_and_retrieve() {
        let d = tempfile::tempdir().unwrap();
        let store = RecallStore::open(d.path()).unwrap();
        let id = store.put(b"original payload").unwrap();

        let init = handle_request(
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
            &store,
            false,
        )
        .unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "tinkr");

        let list = handle_request(
            &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            &store,
            false,
        )
        .unwrap();
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 3);

        let got = handle_request(
            &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tinkr_retrieve","arguments":{"id":id}}}),
            &store,
            false,
        )
        .unwrap();
        assert_eq!(got["result"]["content"][0]["text"], "original payload");
    }

    #[test]
    fn mcp_compress_read_and_retrieve_record_to_ledger() {
        let d = tempfile::tempdir().unwrap();
        let store = RecallStore::open(&d.path().join("recall")).unwrap();
        let ledger = Ledger::open(&d.path().join("ledger.db")).unwrap();

        let mut logs = String::new();
        for i in 0..120 {
            logs.push_str(&format!(
                "2026-11-24T10:00:{i:02}Z DEBUG worker tick processing item {i}\n"
            ));
            if i == 60 {
                logs.push_str("2026-11-24T10:01:00Z ERROR needle_mcp_err at src/a.rs:10\n");
            }
        }
        let resp = handle_request_with_ledger(
            &json!({
                "jsonrpc": "2.0",
                "id": 10,
                "method": "tools/call",
                "params": { "name": "tinkr_compress", "arguments": { "text": logs } }
            }),
            &store,
            Some(&ledger),
            false,
        )
        .unwrap();
        let out = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(out.contains("needle_mcp_err") && out.contains("[tinkr:"));

        let file_path = d.path().join("big.log");
        std::fs::write(&file_path, &logs).unwrap();
        let read_resp = handle_request_with_ledger(
            &json!({
                "jsonrpc": "2.0",
                "id": 11,
                "method": "tools/call",
                "params": { "name": "tinkr_read", "arguments": { "path": file_path.to_string_lossy() } }
            }),
            &store,
            Some(&ledger),
            false,
        )
        .unwrap();
        let read_out = read_resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(read_out.contains("[tinkr:"));

        let summary = ledger.summary(None).unwrap();
        assert_eq!(summary.events, 2);
        assert!(summary.saved > 0);
        assert!(summary.by_tool.contains_key("mcp-compress"));
        assert!(summary.by_tool.contains_key("mcp-read"));

        let id = out
            .split("[tinkr:")
            .nth(1)
            .unwrap()
            .split([' ', '|'])
            .next()
            .unwrap();
        let _ = handle_request_with_ledger(
            &json!({
                "jsonrpc": "2.0",
                "id": 12,
                "method": "tools/call",
                "params": { "name": "tinkr_retrieve", "arguments": { "id": id } }
            }),
            &store,
            Some(&ledger),
            false,
        )
        .unwrap();
        assert_eq!(ledger.recall_count("mcp-read", 0).unwrap(), 1);
    }
}
