use std::fs;
use tinkr_core::{
    manifest::ChangeSet,
    overlap::{
        apply_disable_plan, load_builtin, plan_disable_overlapping, restore_tool, OverlapState,
        SwitchOutcome, MAX_REPAIRS,
    },
    platform::envpersist::EnvDFile,
};

#[test]
fn choice_tinkr_disables_only_overlapping_hook_and_restores_byte_exact() {
    let profiles = load_builtin("2026-11-24").profiles;
    let rtk = profiles.iter().find(|p| p.id == "rtk").unwrap();

    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::create_dir_all(home.join(".config/rtk")).unwrap();

    let rtk_own = home.join(".config/rtk/config.toml");
    fs::write(&rtk_own, "theme = \"dark\"\n").unwrap();

    let settings = home.join(".claude/settings.json");
    let orig = "\u{feff}{\n  // keep this comment\n  \"model\": \"opus\",\n  \"hooks\": {\n    \"PreToolUse\": [\n      { \"matcher\": \"Write\", \"hooks\": [{ \"type\": \"command\", \"command\": \"lint.sh\" }] },\n      { \"matcher\": \"Bash\", \"hooks\": [{ \"type\": \"command\", \"command\": \"rtk hook claude\" }] }\n    ]\n  }\n}\n";
    fs::write(&settings, orig).unwrap();

    let plan = plan_disable_overlapping(rtk, &home, Some("claude-code"));
    assert_eq!(plan.edits.len(), 1);
    assert!(plan.edits[0].diff_preview.contains("rtk hook claude"));

    let mut cs = ChangeSet::open(&d.path().join("manifest.json")).unwrap();
    let mut state = OverlapState::default();
    let outcome = apply_disable_plan(&plan, &mut cs, &mut state, false).unwrap();
    assert!(matches!(outcome, SwitchOutcome::Applied { .. }));

    let after = fs::read_to_string(&settings).unwrap();
    assert!(after.starts_with('\u{feff}'));
    assert!(after.contains("// keep this comment"));
    assert!(after.contains("lint.sh"));
    assert!(!after.contains("rtk hook claude"));
    assert_eq!(fs::read_to_string(&rtk_own).unwrap(), "theme = \"dark\"\n");

    let rep = restore_tool("rtk", &mut cs, &mut state).unwrap();
    assert!(rep.errors.is_empty());
    assert_eq!(rep.restored.len(), 1);
    assert_eq!(fs::read_to_string(&settings).unwrap(), orig);

    let plan2 = plan_disable_overlapping(rtk, &home, Some("claude-code"));
    apply_disable_plan(&plan2, &mut cs, &mut state, false).unwrap();
    assert!(!fs::read_to_string(&settings)
        .unwrap()
        .contains("rtk hook claude"));
    let store = EnvDFile {
        path: d.path().join("env.conf"),
    };
    cs.undo(&store).unwrap();
    assert_eq!(fs::read_to_string(&settings).unwrap(), orig);
}

#[test]
fn anti_edit_war_stops_after_two_repairs() {
    let profiles = load_builtin("2026-11-24").profiles;
    let rtk = profiles.iter().find(|p| p.id == "rtk").unwrap();
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    fs::create_dir_all(home.join(".claude")).unwrap();
    let settings = home.join(".claude/settings.json");
    let rewired = r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"rtk hook claude"}]}]}}"#;

    let mut cs = ChangeSet::open(&d.path().join("manifest.json")).unwrap();
    let mut state = OverlapState::default();

    for expected_repair in 1..=MAX_REPAIRS {
        fs::write(&settings, rewired).unwrap();
        let plan = plan_disable_overlapping(rtk, &home, Some("claude-code"));
        let res = apply_disable_plan(&plan, &mut cs, &mut state, true).unwrap();
        assert!(matches!(res, SwitchOutcome::Applied { .. }));
        assert_eq!(state.choices["rtk"].repair_count, expected_repair);
    }

    fs::write(&settings, rewired).unwrap();
    let plan = plan_disable_overlapping(rtk, &home, Some("claude-code"));
    let res = apply_disable_plan(&plan, &mut cs, &mut state, true).unwrap();
    assert_eq!(
        res,
        SwitchOutcome::EditWarStopped {
            tool_id: "rtk".into(),
            repairs: MAX_REPAIRS,
        }
    );
    assert_eq!(fs::read_to_string(&settings).unwrap(), rewired);
}
