use crate::{compress, recall::RecallStore, tokenizer::TokenizerKind};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

pub fn handle_request(req: &Value, recall: &RecallStore, unicode: bool) -> Option<Value> {
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
                    "description": "Read a local file and return a compact view when large, with a recall id for the full original.",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "path": { "type": "string" } },
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
                            json!({ "content": [{ "type": "text", "text": String::from_utf8_lossy(&bytes) }] })
                        }
                        Err(e) => {
                            json!({ "isError": true, "content": [{ "type": "text", "text": e.to_string() }] })
                        }
                    }
                }
                "tinkr_compress" => {
                    let text = args["text"].as_str().unwrap_or("");
                    let out = compress::compress_tool_result(
                        text,
                        TokenizerKind::Exact,
                        Some(recall),
                        unicode,
                    )
                    .map(|c| c.text)
                    .unwrap_or_else(|| text.to_string());
                    json!({ "content": [{ "type": "text", "text": out }] })
                }
                "tinkr_read" => {
                    let p = args["path"].as_str().unwrap_or("");
                    match std::fs::read(p) {
                        Ok(bytes) => {
                            let s = String::from_utf8_lossy(&bytes);
                            let out = compress::compress_tool_result(
                                &s,
                                TokenizerKind::Exact,
                                Some(recall),
                                unicode,
                            )
                            .map(|c| c.text)
                            .unwrap_or_else(|| s.into_owned());
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
    mut writer: W,
    recall: &RecallStore,
    unicode: bool,
) -> std::io::Result<()> {
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(req) = serde_json::from_str::<Value>(&line) {
            if let Some(resp) = handle_request(&req, recall, unicode) {
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
}
