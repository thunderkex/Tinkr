use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};
use tinkr_core::overlap::{
    analyze, format_plain, load_builtin, scan, ConflictVerdict, OverlapLayer, ScanEnv, ToolState,
    UserChoice,
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
        verify_exec: Box::new(|_, _| Some("0.24.0".into())),
    }
}

#[test]
fn inactive_tool_has_no_conflicts_and_no_active_overlap() {
    let profiles = load_builtin("2026-11-24").profiles;
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let bin = d.path().join("bin");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("rtk"), b"#!/bin/sh\n").unwrap();

    let mut env = base_env(home);
    env.extra_bin_dirs.push(bin);
    let sr = scan(&profiles, &env);
    let analysis = analyze(&sr, &profiles, &["claude-code".into()], &BTreeMap::new());

    assert!(!analysis.has_active_overlap);
    assert!(analysis.conflicts.is_empty());
    assert_eq!(analysis.tools.len(), 1);
    assert_eq!(analysis.tools[0].state, ToolState::InstalledInactive);
    let txt = format_plain(&analysis, true);
    assert!(txt.contains("Installed but inactive (no conflict)"));
    assert!(!txt.contains("[1] Keep my current tools"));
}

#[test]
fn per_agent_per_layer_split_and_honest_neutral_copy() {
    let profiles = load_builtin("2026-11-24").profiles;
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::create_dir_all(home.join(".codex")).unwrap();

    fs::write(
        home.join(".claude/settings.json"),
        r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"rtk hook claude"}]}]}}"#,
    )
    .unwrap();
    fs::write(
        home.join(".codex/AGENTS.md"),
        "Use /caveman mode for concise output.\n",
    )
    .unwrap();

    let env = base_env(home);
    let sr = scan(&profiles, &env);
    let agents = vec![
        "claude-code".to_string(),
        "codex".to_string(),
        "gemini-cli".to_string(),
    ];
    let analysis = analyze(&sr, &profiles, &agents, &BTreeMap::new());

    assert!(analysis.has_active_overlap);
    assert_eq!(analysis.uncovered_agents, vec!["gemini-cli"]);

    let rtk = analysis.tools.iter().find(|t| t.id == "rtk").unwrap();
    assert_eq!(rtk.state, ToolState::PartialDrifted);
    assert_eq!(rtk.wired_agents, vec!["claude-code"]);

    let cav = analysis.tools.iter().find(|t| t.id == "caveman").unwrap();
    assert_eq!(cav.state, ToolState::PartialDrifted);
    assert_eq!(cav.wired_agents, vec!["codex"]);

    assert!(analysis.conflicts.iter().any(|c| c.tool_id == "rtk"
        && c.agent == "claude-code"
        && c.layer == OverlapLayer::Read
        && c.verdict == ConflictVerdict::Redundant));
    assert!(analysis.conflicts.iter().any(|c| c.tool_id == "caveman"
        && c.agent == "codex"
        && c.layer == OverlapLayer::Say
        && c.verdict == ConflictVerdict::Redundant));

    let txt = format_plain(&analysis, true);
    let p1 = txt.find("[1] Keep my current tools (default)").unwrap();
    let p2 = txt.find("[2] Use Tinkr instead").unwrap();
    let p3 = txt.find("[3] Use both where it is safe").unwrap();
    let p4 = txt.find("[4] Compare first").unwrap();
    assert!(p1 < p2 && p2 < p3 && p3 < p4);
    assert!(txt.contains("What your current tools do that Tinkr does NOT:"));
    assert!(txt.contains("https://github.com/rtk-ai/rtk"));
    assert!(txt.contains("gemini-cli"));

    let json_s = serde_json::to_string_pretty(&analysis).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json_s).unwrap();
    assert_eq!(v["has_active_overlap"], true);
}

#[test]
fn saved_choice_marks_tool_as_tinkr_managed() {
    let profiles = load_builtin("2026-11-24").profiles;
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::write(
        home.join(".claude/settings.json"),
        r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"rtk hook claude"}]}]}}"#,
    )
    .unwrap();

    let env = base_env(home);
    let sr = scan(&profiles, &env);
    let mut choices = BTreeMap::new();
    choices.insert("rtk".into(), UserChoice::Keep);
    let analysis = analyze(&sr, &profiles, &["claude-code".into()], &choices);
    assert_eq!(analysis.tools[0].state, ToolState::TinkrManaged);
    assert_eq!(analysis.tools[0].current_choice, Some(UserChoice::Keep));
}
