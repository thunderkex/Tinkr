use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};
use tinkr_core::{
    manifest::ChangeSet,
    overlap::{
        analyze, apply_with_yield, load_builtin, scan, OverlapLayer, OverlapState, ScanEnv,
        UserChoice,
    },
    profile::{self, ActionResult, Machine},
};

fn test_machine(home: PathBuf) -> Machine {
    Machine {
        home,
        env: Box::new(|_| None),
        path_var: Some(std::ffi::OsString::new()),
        tinkr_exe: PathBuf::from("/usr/local/bin/tinkr"),
    }
}

fn test_scan_env(home: PathBuf) -> ScanEnv {
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
fn auto_yield_leaves_foreign_agent_config_byte_identical_while_covering_other_agents() {
    let overlap_profiles = load_builtin("2026-11-24").profiles;
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::create_dir_all(home.join(".codex")).unwrap();

    let claude_settings = home.join(".claude/settings.json");
    let orig_claude = r#"{
  // user's existing RTK hook
  "hooks": {
    "PreToolUse": [
      { "matcher": "Bash", "hooks": [{ "type": "command", "command": "rtk hook claude" }] }
    ]
  }
}
"#;
    fs::write(&claude_settings, orig_claude).unwrap();

    let sr = scan(&overlap_profiles, &test_scan_env(home.clone()));
    let state = OverlapState::default();
    let analysis = analyze(
        &sr,
        &overlap_profiles,
        &["claude-code".into(), "codex".into()],
        &state.global_choices(),
    );

    assert!(state.should_yield(&analysis, "claude-code", OverlapLayer::Read));
    assert!(!state.should_yield(&analysis, "codex", OverlapLayer::Read));

    let m = test_machine(home.clone());
    let mut cs = ChangeSet::open(&d.path().join("manifest.json")).unwrap();
    let claude_prof = profile::builtin()
        .into_iter()
        .find(|p| p.id == "claude-code")
        .unwrap();
    let codex_prof = profile::builtin()
        .into_iter()
        .find(|p| p.id == "codex")
        .unwrap();

    let r_claude = apply_with_yield(&claude_prof, &m, &mut cs, &analysis, &state).unwrap();
    assert!(matches!(r_claude[0], ActionResult::Skipped { .. }));
    assert_eq!(
        fs::read_to_string(&claude_settings).unwrap(),
        orig_claude,
        "foreign agent config must remain byte-identical under auto-yield"
    );

    let r_codex = apply_with_yield(&codex_prof, &m, &mut cs, &analysis, &state).unwrap();
    assert!(matches!(r_codex[0], ActionResult::Change(_)));
}

#[test]
fn reprompt_triggers_only_on_material_change_not_version_bump() {
    let overlap_profiles = load_builtin("2026-11-24").profiles;
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::write(
        home.join(".claude/settings.json"),
        r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"rtk hook claude"}]}]}}"#,
    )
    .unwrap();

    let sr1 = scan(&overlap_profiles, &test_scan_env(home.clone()));
    let mut state = OverlapState::default();
    let a1 = analyze(
        &sr1,
        &overlap_profiles,
        &["claude-code".into()],
        &state.global_choices(),
    );
    assert!(
        state.needs_reprompt(&a1),
        "must prompt when no choice saved"
    );

    let state_path = d.path().join("overlap-state.json");
    state.set_choice(
        "rtk",
        None,
        UserChoice::Keep,
        a1.tools[0].wired_agents.clone(),
        a1.tools[0].active_layers.clone(),
    );
    state.save(&state_path).unwrap();
    let reloaded = OverlapState::load(&state_path);

    let mut sr2 = sr1.clone();
    sr2.detected[0].version = Some("rtk 9.9.9".into());
    let a2 = analyze(
        &sr2,
        &overlap_profiles,
        &["claude-code".into()],
        &reloaded.global_choices(),
    );
    assert!(
        !reloaded.needs_reprompt(&a2),
        "version bump alone must NEVER trigger re-prompt"
    );

    fs::create_dir_all(home.join(".codex")).unwrap();
    fs::write(
        home.join(".codex/hooks.json"),
        r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"rtk hook codex"}]}]}}"#,
    )
    .unwrap();
    let sr3 = scan(&overlap_profiles, &test_scan_env(home));
    let a3 = analyze(
        &sr3,
        &overlap_profiles,
        &["claude-code".into(), "codex".into()],
        &reloaded.global_choices(),
    );
    assert!(
        reloaded.needs_reprompt(&a3),
        "newly wired agent must trigger re-prompt"
    );
}
