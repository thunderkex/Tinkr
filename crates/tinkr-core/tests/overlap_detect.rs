use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
use tinkr_core::{
    overlap::{load_builtin, scan, EvidenceKind, OverlapLayer, ScanEnv},
    remote::RemoteKind,
};

fn snapshot_tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(base: &Path, d: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        if let Ok(rd) = fs::read_dir(d) {
            for e in rd.flatten() {
                let p = e.path();
                let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                if p.is_dir() {
                    out.push((format!("{rel}/"), vec![]));
                    walk(base, &p, out);
                } else if let Ok(bytes) = fs::read(&p) {
                    out.push((rel, bytes));
                }
            }
        }
    }
    let mut v = Vec::new();
    walk(root, root, &mut v);
    v.sort();
    v
}

fn base_env(home: PathBuf, os: &str) -> ScanEnv {
    ScanEnv {
        os: os.into(),
        home,
        remote: vec![],
        env_vars: BTreeMap::new(),
        path_var: Some(std::ffi::OsString::new()),
        extra_bin_dirs: vec![],
        packages: BTreeMap::new(),
        processes: vec![],
        listening_ports: BTreeSet::new(),
        services: vec![],
        tinkr_proxy_port: Some(19191),
        verify_exec: Box::new(|_, _| None),
    }
}

#[test]
fn detects_rtk_headroom_caveman_all_three_and_proves_zero_writes() {
    let profiles = load_builtin("2026-11-24").profiles;
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let bin = d.path().join("bin");
    fs::create_dir_all(home.join(".claude/plugins/caveman")).unwrap();
    fs::create_dir_all(home.join(".codex")).unwrap();
    fs::create_dir_all(home.join(".cursor")).unwrap();
    fs::create_dir_all(&bin).unwrap();

    fs::write(bin.join("rtk"), b"#!/bin/sh\n").unwrap();
    fs::write(bin.join("headroom"), b"#!/bin/sh\n").unwrap();

    fs::write(
        home.join(".claude/settings.json"),
        r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"rtk hook claude"}]}]}}"#,
    )
    .unwrap();
    fs::write(
        home.join(".cursor/mcp.json"),
        r#"{"mcpServers":{"headroom":{"command":"headroom","args":["mcp"]},"serena":{"command":"serena"}}}"#,
    )
    .unwrap();

    let before = snapshot_tree(d.path());

    let mut env = base_env(home, "linux");
    env.remote = vec![RemoteKind::Wsl {
        distro: "Ubuntu".into(),
    }];
    env.extra_bin_dirs.push(bin);
    env.listening_ports.insert(8787);
    env.env_vars
        .insert("ANTHROPIC_BASE_URL".into(), "http://127.0.0.1:8787".into());
    env.packages
        .insert("npm".into(), vec!["@caveman-ai/cli".into()]);
    env.verify_exec = Box::new(|p, args| {
        let name = p.file_name()?.to_string_lossy();
        if name == "rtk" && args.first().map(String::as_str) == Some("gain") {
            Some("rtk 0.24.0".into())
        } else if name == "headroom" {
            Some("headroom 1.2.0".into())
        } else {
            None
        }
    });

    let report = scan(&profiles, &env);
    assert_eq!(snapshot_tree(d.path()), before, "detection modified disk!");

    assert_eq!(report.remote_context, vec!["WSL (Ubuntu)"]);
    let ids: Vec<&str> = report.detected.iter().map(|d| d.id.as_str()).collect();
    assert!(ids.contains(&"rtk"), "RTK missing: {ids:?}");
    assert!(ids.contains(&"headroom"), "Headroom missing: {ids:?}");
    assert!(ids.contains(&"caveman"), "Caveman missing: {ids:?}");
    assert!(
        !ids.contains(&"unknown-optimizer"),
        "false unknown: {ids:?}"
    );

    let rtk = report.detected.iter().find(|d| d.id == "rtk").unwrap();
    assert_eq!(rtk.version.as_deref(), Some("rtk 0.24.0"));
    assert!(rtk.wired_agents.contains("claude-code"));
    assert!(rtk.active_layers.contains(&OverlapLayer::Read));

    let hr = report.detected.iter().find(|d| d.id == "headroom").unwrap();
    assert!(hr.wired_agents.contains("cursor"));
    assert!(hr.active_layers.contains(&OverlapLayer::Send));

    assert_eq!(report.companions.len(), 1);
    assert_eq!(report.companions[0].name, "serena");
}

#[test]
fn unrelated_same_name_rtk_binary_is_not_flagged() {
    let profiles = load_builtin("2026-11-24").profiles;
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let bin = d.path().join("bin");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("rtk.exe"), b"MZ").unwrap();

    let mut env = base_env(home, "windows");
    env.extra_bin_dirs.push(bin);
    env.verify_exec = Box::new(|_, _| None);

    let report = scan(&profiles, &env);
    assert!(
        report.detected.is_empty(),
        "unrelated same-name rtk binary must not be detected: {:?}",
        report.detected
    );
}

#[test]
fn detects_unknown_optimizer_heuristics_across_hook_proxy_mcp_and_rules() {
    let profiles = load_builtin("2026-11-24").profiles;
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::create_dir_all(home.join(".cursor")).unwrap();

    fs::write(
        home.join(".claude/settings.json"),
        r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"my-token-trimmer hook"}]}]}}"#,
    )
    .unwrap();
    fs::write(
        home.join(".claude/CLAUDE.md"),
        "# >>> custom-shrink >>>\nAlways give terse replies to save tokens.\n# <<< custom-shrink <<<\n",
    )
    .unwrap();
    fs::write(
        home.join(".cursor/mcp.json"),
        r#"{"mcpServers":{"super-shrink-mcp":{"command":"shrink-mcp"}}}"#,
    )
    .unwrap();

    let before = snapshot_tree(d.path());
    let mut env = base_env(home, "macos");
    env.env_vars
        .insert("OPENAI_BASE_URL".into(), "http://127.0.0.1:9444/v1".into());

    let report = scan(&profiles, &env);
    assert_eq!(snapshot_tree(d.path()), before, "detection modified disk!");

    let unk = report
        .detected
        .iter()
        .find(|t| t.is_unknown_heuristic)
        .expect("unknown optimizer should be detected");
    assert_eq!(unk.name, "Unknown token optimizer detected");
    assert!(unk.evidence.iter().any(|e| e.kind == EvidenceKind::EnvVar));
    assert!(unk
        .evidence
        .iter()
        .any(|e| e.kind == EvidenceKind::AgentHook));
    assert!(unk
        .evidence
        .iter()
        .any(|e| e.kind == EvidenceKind::McpEntry));
    assert!(unk
        .evidence
        .iter()
        .any(|e| e.kind == EvidenceKind::RulesMarker));
    assert!(unk.active_layers.contains(&OverlapLayer::Read));
    assert!(unk.active_layers.contains(&OverlapLayer::Send));
    assert!(unk.active_layers.contains(&OverlapLayer::Say));
}
