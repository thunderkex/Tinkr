use serde_json::{json, Value};
use tinkr_core::{
    compress,
    filters::{self, Ctx, Decision},
    recall::RecallStore,
    tokenizer::TokenizerKind,
};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn range(&mut self, max: usize) -> usize {
        (self.next() as usize) % max.max(1)
    }
}

#[test]
fn property_random_needle_in_random_log_haystack_always_survives() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecallStore::open(dir.path()).unwrap();
    let ctx = Ctx {
        recall: Some(&store),
        unicode: false,
        tokenizer: TokenizerKind::Exact,
    };
    let mut rng = Rng(0x2026_1125_cafe_babe);

    let templates = [
        ("INFO", "gateway request_completed route=/v1/items status=200"),
        ("DEBUG", "scheduler worker_heartbeat active_jobs=4 queue_lag_ms=2"),
        ("INFO", "cache lru_lookup bucket=session hit=true ttl_sec=300"),
    ];

    for trial in 0..150usize {
        let len = 120 + rng.range(240);
        let needle_pos = rng.range(len);
        let needle_token = format!("UNIQUE_OUTLIER_NEEDLE_{trial}_{:08x}", rng.next() as u32);
        let needle_kind = trial % 4;

        let mut lines = Vec::with_capacity(len);
        for i in 0..len {
            if i == needle_pos {
                let needle_line = match needle_kind {
                    0 => format!(
                        "2026-11-25T12:{:02}:{:02}Z INFO schema_cutover_applied marker={needle_token} shard=7",
                        i % 60,
                        (i * 3) % 60
                    ),
                    1 => format!(
                        "2026-11-25T12:{:02}:{:02}Z NOTICE leader_switched target={needle_token}",
                        i % 60,
                        (i * 3) % 60
                    ),
                    2 => format!(
                        "2026-11-25T12:{:02}:{:02}Z INFO payload_dump {needle_token} {}",
                        i % 60,
                        (i * 3) % 60,
                        "extra_context_segment ".repeat(14)
                    ),
                    _ => format!(
                        "2026-11-25T12:{:02}:{:02}Z WARN transient_spike code={needle_token}",
                        i % 60,
                        (i * 3) % 60
                    ),
                };
                lines.push(needle_line);
            } else {
                let (lvl, body) = templates[rng.range(templates.len())];
                lines.push(format!(
                    "2026-11-25T12:{:02}:{:02}Z {lvl} req_id={:06x} {body} seq={i}",
                    i % 60,
                    (i * 7) % 60,
                    rng.next() & 0xffffff
                ));
            }
        }
        let first_line = lines[0].clone();
        let last_line = lines[len - 1].clone();
        let raw = lines.join("\n") + "\n";

        let shell_out = filters::process("kubectl logs deploy/app", 0, raw.as_bytes(), &ctx);
        assert!(matches!(shell_out.decision, Decision::Compressed { .. }));
        let shell_txt = String::from_utf8_lossy(&shell_out.output);
        assert!(
            shell_txt.contains(&needle_token),
            "shell log filter lost needle on trial {trial} (pos={needle_pos}/{len}):\n{shell_txt}"
        );
        assert!(shell_txt.contains(&first_line), "lost first line on trial {trial}");
        assert!(shell_txt.contains(&last_line), "lost last line on trial {trial}");

        let proxy_out = compress::compress_tool_result(&raw, TokenizerKind::Exact, None, false)
            .expect("proxy log compressor must compress");
        assert!(
            proxy_out.text.contains(&needle_token),
            "proxy log compressor lost needle on trial {trial} (pos={needle_pos}/{len})"
        );
        assert!(proxy_out.text.contains(&first_line));
        assert!(proxy_out.text.contains(&last_line));
    }
}

#[test]
fn property_random_needle_in_random_json_and_jsonl_haystack_always_survives() {
    let mut rng = Rng(0x9988_7766_5544_3322);

    for trial in 0..100usize {
        let len = 60 + rng.range(120);
        let needle_pos = rng.range(len);
        let needle_token = format!("JSON_OUTLIER_{trial}_{:06x}", rng.next() & 0xffffff);
        let variant = trial % 4;

        let mut items: Vec<Value> = Vec::with_capacity(len);
        for i in 0..len {
            if i == needle_pos {
                let obj = match variant {
                    0 => json!({"id": i, "status": needle_token, "latency_ms": 15, "pad": "steady_state_record"}),
                    1 => json!({"id": i, "status": "ok", "latency_ms": 15, "pad": "steady_state_record", "custom_tag": needle_token}),
                    2 => json!({"id": i, "status": "ok", "latency_ms": needle_token, "pad": "steady_state_record"}),
                    _ => json!({"id": i, "status": "ok", "latency_ms": 15, "pad": "steady_state_record", "note": needle_token}),
                };
                items.push(obj);
            } else {
                items.push(json!({
                    "id": i,
                    "status": "ok",
                    "latency_ms": 10 + (rng.next() % 20),
                    "pad": "steady_state_record_repetitive_telemetry_filler",
                    "note": Value::Null
                }));
            }
        }

        let json_str = serde_json::to_string_pretty(&items).unwrap();
        let compressed_json =
            compress::compress_tool_result(&json_str, TokenizerKind::Exact, None, false)
                .expect("JSON array must compress");
        assert!(
            compressed_json.text.contains(&needle_token),
            "JSON array lost outlier needle on trial {trial} (variant={variant}, pos={needle_pos}/{len}):\n{}",
            compressed_json.text
        );
        assert!(compressed_json.text.contains("\"id\":0"));
        assert!(compressed_json.text.contains(&format!("\"id\":{}", len - 1)));

        let jsonl_str = items
            .iter()
            .map(|v| serde_json::to_string(v).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        let compressed_jsonl =
            compress::compress_tool_result(&jsonl_str, TokenizerKind::Exact, None, false)
                .expect("JSONL must compress");
        assert!(
            compressed_jsonl.text.contains(&needle_token),
            "JSONL lost outlier needle on trial {trial} (variant={variant}, pos={needle_pos}/{len})"
        );
    }
}
