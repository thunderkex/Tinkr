use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};
use tinkr_core::{
    filters::{self, Ctx, Decision},
    overlap::{
        analyze, build_plan, load_builtin, scan, should_skip_command, OverlapState, ScanEnv,
        UserChoice,
    },
    say_less::{self, Billing},
    tokenizer::TokenizerKind,
};

fn base_env(home: PathBuf) -> ScanEnv {
    ScanEnv {
        os: "linux".into(),
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
    }
}

#[test]
fn choice_both_skips_rtk_covered_commands_and_never_double_compresses_foreign_markers() {
    let profiles = load_builtin("2026-11-24").profiles;
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::write(
        home.join(".claude/settings.json"),
        r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"rtk hook claude"}]}]}}"#,
    )
    .unwrap();

    let sr = scan(&profiles, &base_env(home));
    let mut state = OverlapState::default();
    state.set_choice(
        "rtk",
        None,
        UserChoice::Both,
        vec!["claude-code".into()],
        vec![],
    );
    let analysis = analyze(
        &sr,
        &profiles,
        &["claude-code".into()],
        &state.global_choices(),
    );

    let plan = build_plan("claude-code", &analysis, &profiles, &state);
    assert!(plan.allow_read_less);
    assert!(should_skip_command("git status", &plan));
    assert!(should_skip_command("cargo test", &plan));
    assert!(!should_skip_command("go test ./...", &plan));
    assert!(!should_skip_command("dotnet test", &plan));

    let mut rtk_output = String::new();
    for i in 0..250 {
        rtk_output.push_str(&format!("line {i} summary entry\n"));
    }
    rtk_output.push_str("[full output: rtk recall a1b2c3d4e5f6]\n");
    let ctx = Ctx {
        recall: None,
        unicode: false,
        tokenizer: TokenizerKind::Exact,
    };
    let out = filters::process("go test ./...", 0, rtk_output.as_bytes(), &ctx);
    assert_eq!(
        out.decision,
        Decision::Passthrough("foreign-optimizer-marker")
    );
    assert_eq!(out.output, rtk_output.as_bytes());
}

#[test]
fn choice_both_enforces_single_proxy_fallback_and_single_voice_rule() {
    let profiles = load_builtin("2026-11-24").profiles;
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::write(
        home.join(".claude/CLAUDE.md"),
        "Use /caveman caveman-speak for short replies.\n",
    )
    .unwrap();

    let mut env = base_env(home);
    env.listening_ports.insert(8787);
    env.env_vars
        .insert("ANTHROPIC_BASE_URL".into(), "http://127.0.0.1:8787".into());

    let sr = scan(&profiles, &env);
    let mut state = OverlapState::default();
    state.set_choice("headroom", None, UserChoice::Both, vec![], vec![]);
    state.set_choice(
        "caveman",
        None,
        UserChoice::Both,
        vec!["claude-code".into()],
        vec![],
    );
    let analysis = analyze(
        &sr,
        &profiles,
        &["claude-code".into()],
        &state.global_choices(),
    );

    let plan = build_plan("claude-code", &analysis, &profiles, &state);
    assert!(!plan.allow_send_less);
    assert!(plan.proxy_chain_note.contains("falling back to Keep"));
    assert!(!plan.allow_say_less);
    assert!(plan.say_less_note.contains("single voice rule"));

    let sys = "You are a helper. Reply in caveman-speak.";
    assert_eq!(
        say_less::append_to_system(sys, Billing::PerToken, None),
        sys
    );
}
