use tinkr_core::{
    auto::{self, HealthState, MAX_TUNING_PROFILES},
    filters::{self, Ctx, Decision},
    ledger::{CountKind, Event, Layer, Ledger},
    provider::Provider,
    recall::RecallStore,
    tokenizer::TokenizerKind,
};

#[test]
fn golden_self_tuning_loosens_tightens_bounds_state_and_explains_why() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecallStore::open(&dir.path().join("recall")).unwrap();
    let ledger = Ledger::open(&dir.path().join("ledger.db")).unwrap();
    let tuning_path = dir.path().join("tuning.json");
    let health = HealthState::default();

    for i in 0..8 {
        let rid = format!("py11{:02x}", i);
        ledger
            .record(&Event {
                ts: 1_000 + i,
                layer: Layer::ReadLess,
                provider: Provider::None,
                tool: "pytest".into(),
                session: None,
                before: 2500,
                after: 300,
                kind: CountKind::Measured,
                recall_id: Some(rid.clone()),
            })
            .unwrap();
        if i < 2 {
            ledger.record_recall(&rid, 1_001 + i).unwrap();
        }
    }

    for i in 0..52 {
        ledger
            .record(&Event {
                ts: 2_000 + i,
                layer: Layer::ReadLess,
                provider: Provider::None,
                tool: "git-diff".into(),
                session: None,
                before: 3000,
                after: 700,
                kind: CountKind::Measured,
                recall_id: Some(format!("df22{:02x}", i)),
            })
            .unwrap();
    }

    for i in 0..75 {
        ledger
            .record(&Event {
                ts: 500 + i,
                layer: Layer::ReadLess,
                provider: Provider::None,
                tool: format!("synthetic-tool-{i:03}"),
                session: None,
                before: 600,
                after: 200,
                kind: CountKind::Measured,
                recall_id: None,
            })
            .unwrap();
    }

    let state = auto::sync_tuning(&ledger, &health, Some(&tuning_path));
    assert!(
        state.profiles.len() <= MAX_TUNING_PROFILES,
        "tuning state exceeded bound: {}",
        state.profiles.len()
    );
    assert_eq!(state.level_for("pytest"), 1);
    assert_eq!(state.level_for("git-diff"), 3);

    let why_py = auto::explain_command("python -m pytest -q", &ledger, &health);
    assert_eq!(why_py.profile_id, "pytest");
    assert_eq!(why_py.level, 1);
    assert!(why_py.reason.contains("loosened to level 1"));
    let rendered = auto::format_why(&why_py);
    assert!(rendered.contains("profile:     pytest") && rendered.contains("recalls:     2"));

    let why_diff = auto::explain_command("git diff --cached", &ledger, &health);
    assert_eq!(why_diff.profile_id, "git-diff");
    assert_eq!(why_diff.level, 3);
    assert!(why_diff.reason.contains("tightened to level 3"));

    let mut py_raw = String::new();
    for _ in 0..40 {
        py_raw.push_str("============================= repeated banner =============================\n");
    }
    for i in 0..30 {
        py_raw.push_str(&format!("tests/test_u.py::test_keep_when_loosened_{i} PASSED\n"));
    }
    let ctx = Ctx {
        recall: Some(&store),
        unicode: false,
        tokenizer: TokenizerKind::Exact,
    };
    let out_l1 = filters::process_tuned("pytest", 0, py_raw.as_bytes(), &ctx, 1);
    assert_eq!(out_l1.decision, Decision::Compressed { level: 1 });
    let txt_l1 = String::from_utf8_lossy(&out_l1.output);
    assert!(txt_l1.contains("test_keep_when_loosened_15 PASSED"));
}
