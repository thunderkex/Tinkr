use tinkr_core::{
    auto::{self, HealthState},
    ledger::{CountKind, Event, Layer, Ledger, NetMeta},
    mcp,
    provider::Provider,
    recall::RecallStore,
};

#[test]
fn golden_net_accounting_layers_agents_and_autodisable_mcp() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecallStore::open(&dir.path().join("recall")).unwrap();
    let ledger = Ledger::open(&dir.path().join("ledger.db")).unwrap();
    let health_path = dir.path().join("health.json");
    let mut health = HealthState::load(&health_path);

    for i in 0..22 {
        let rid = format!("aaaa{:02x}", i);
        ledger
            .record_net(
                &Event {
                    ts: 1_000 + i,
                    layer: Layer::ReadLess,
                    provider: Provider::None,
                    tool: "mcp-compress".into(),
                    session: Some("sess-1".into()),
                    before: 300,
                    after: 240,
                    kind: CountKind::Measured,
                    recall_id: Some(rid.clone()),
                },
                &NetMeta {
                    agent: "mcp".into(),
                    marker_tokens: 15,
                    rule_tokens: 0,
                },
            )
            .unwrap();
        if i % 3 == 0 {
            ledger.record_recall(&rid, 1_001 + i).unwrap();
        }
    }

    ledger
        .record_net(
            &Event {
                ts: 2_000,
                layer: Layer::ReadLess,
                provider: Provider::None,
                tool: "git-log".into(),
                session: Some("sess-2".into()),
                before: 4000,
                after: 615,
                kind: CountKind::Measured,
                recall_id: Some("bbbb01".into()),
            },
            &NetMeta {
                agent: "claude".into(),
                marker_tokens: 15,
                rule_tokens: 0,
            },
        )
        .unwrap();
    ledger
        .record(&Event {
            ts: 2_005,
            layer: Layer::SayLess,
            provider: Provider::Anthropic,
            tool: "anthropic".into(),
            session: Some("sess-2".into()),
            before: 400,
            after: 220,
            kind: CountKind::Estimated,
            recall_id: None,
        })
        .unwrap();

    let s = ledger.summary(None).unwrap();
    assert!(s.by_tool["mcp-compress"].net_saved <= 0);
    assert!(s.by_tool["git-log"].net_saved > 3000);
    assert!(s.by_agent["claude"].net_saved > 3000);
    assert!(s.by_layer["say_less"].rule_tokens > 0);
    assert_eq!(s.auto_disabled.len(), 1);
    assert_eq!(s.auto_disabled[0].key, "read_less:mcp-compress");

    let synced = auto::sync_net_disables(&ledger, &mut health, &health_path);
    assert_eq!(synced.len(), 1);
    assert!(health.is_disabled("read_less:mcp-compress"));

    let mut big_logs = String::new();
    for i in 0..100 {
        big_logs.push_str(&format!("2026-11-25T10:00:{:02}Z DEBUG noisy line {i}\n", i % 60));
    }
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 99,
        "method": "tools/call",
        "params": { "name": "mcp-compress-check" }
    });
    let _ = req;
    let compress_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 100,
        "method": "tools/call",
        "params": { "name": "tinkr_compress", "arguments": { "text": big_logs } }
    });
    let resp = mcp::handle_request_with_ledger(&compress_req, &store, Some(&ledger), false).unwrap();
    assert_eq!(
        resp["result"]["content"][0]["text"].as_str().unwrap(),
        big_logs,
        "negative-NET profile must auto-disable and pass through unchanged"
    );
}
