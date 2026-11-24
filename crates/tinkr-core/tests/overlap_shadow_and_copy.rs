use std::collections::{BTreeMap, BTreeSet};
use tinkr_core::overlap::{
    analyze, append_sample, format_plain, format_shadow_report, load_builtin, load_samples, scan,
    shadow_evaluate, summarize, ScanEnv, MIN_SHADOW_SAMPLES,
};

#[test]
fn shadow_mode_leaves_agent_output_byte_identical_and_flags_dropped_needles() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("shadow-rtk.jsonl");

    let mut raw = String::from("running 101 tests\n");
    for i in 0..100 {
        raw.push_str(&format!("test suite::case_{i:03} ... ok\n"));
    }
    raw.push_str(
        "test suite::auth_check ... FAILED\n\
         thread 'suite::auth_check' panicked at src/auth.rs:42:9:\n\
         error: invalid signature\n\
         test result: FAILED. 100 passed; 1 failed\n",
    );

    let their_out = b"100 passed; 1 failed (suite::auth_check)\n";

    let (returned, sample) =
        shadow_evaluate("rtk", "cargo test", 1, raw.as_bytes(), their_out, 1000);
    assert_eq!(returned, their_out);
    assert_eq!(sample.count_kind, "measured");
    assert!(sample.raw_tokens > sample.tinkr_tokens);
    assert!(
        sample
            .theirs_dropped_that_tinkr_kept
            .iter()
            .any(|l| l.contains("invalid signature")),
        "shadow check must flag critical line dropped by theirs: {:?}",
        sample.theirs_dropped_that_tinkr_kept
    );
    assert!(sample.tinkr_dropped_that_theirs_kept.is_empty());

    append_sample(&log_path, &sample).unwrap();
    let few = load_samples(&log_path);
    let sum_few = summarize("rtk", &few);
    assert!(!sum_few.enough_data);
    let rep_few = format_shadow_report(&sum_few);
    assert!(rep_few.contains("too little data"));

    for i in 1..MIN_SHADOW_SAMPLES {
        let mut s = sample.clone();
        s.ts += i as i64;
        append_sample(&log_path, &s).unwrap();
    }
    let all = load_samples(&log_path);
    let sum_all = summarize("rtk", &all);
    assert!(sum_all.enough_data);
    assert_eq!(sum_all.samples, MIN_SHADOW_SAMPLES);
    let rep_all = format_shadow_report(&sum_all);
    assert!(rep_all.contains("measured on your machine"));
    assert!(rep_all.contains("Critical lines dropped"));
}

#[test]
fn rendered_overlap_copy_is_neutral_and_has_no_dark_patterns() {
    let profiles = load_builtin("2026-11-24").profiles;
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(
        home.join(".claude/settings.json"),
        r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"rtk hook claude"}]}]}}"#,
    )
    .unwrap();

    let env = ScanEnv {
        os: "windows".into(),
        home,
        remote: vec![],
        env_vars: BTreeMap::new(),
        path_var: Some(std::ffi::OsString::new()),
        extra_bin_dirs: vec![],
        packages: BTreeMap::new(),
        processes: vec![],
        listening_ports: BTreeSet::new(),
        services: vec![],
        tinkr_proxy_port: None,
        verify_exec: Box::new(|_, _| Some("rtk 1.2.1".into())),
    };
    let sr = scan(&profiles, &env);
    let a = analyze(&sr, &profiles, &["claude-code".into()], &BTreeMap::new());
    let copy = format_plain(&a, true).to_ascii_lowercase();

    for banned in [
        "danger",
        "inferior",
        "obsolete",
        "uninstall now",
        "must replace",
        "urgent",
    ] {
        assert!(
            !copy.contains(banned),
            "dark-pattern / non-neutral phrase '{banned}' found in overlap copy"
        );
    }
}
