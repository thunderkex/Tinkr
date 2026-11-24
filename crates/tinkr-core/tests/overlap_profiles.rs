use tinkr_core::overlap::{
    load_builtin, load_with_signed_dir, validate_profile, HmacVerifier, OverlapLayer,
    ValidationIssue,
};

#[test]
fn builtin_profiles_are_valid_sourced_honest_and_fresh() {
    let loaded = load_builtin("2026-11-24");
    assert!(
        loaded.warnings.is_empty(),
        "unexpected warnings on built-in overlap profiles: {:?}",
        loaded.warnings
    );
    let ids: Vec<&str> = loaded.profiles.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, vec!["rtk", "headroom", "caveman"]);

    for p in &loaded.profiles {
        assert!(
            !p.capabilities.features.is_empty(),
            "{} has no features",
            p.id
        );
        assert!(
            !p.capabilities.does_better_than_tinkr.is_empty(),
            "{} missing honest does_better_than_tinkr",
            p.id
        );
        assert!(!p.conflicts.is_empty(), "{} missing conflict rules", p.id);
    }

    let rtk = loaded.profiles.iter().find(|p| p.id == "rtk").unwrap();
    assert!(rtk.detect.verify_args.iter().any(|a| a == "gain"));
    assert!(rtk.layers.contains(&OverlapLayer::Read));

    let headroom = loaded.profiles.iter().find(|p| p.id == "headroom").unwrap();
    assert!(headroom.detect.ports.contains(&8787));
    assert!(headroom.layers.contains(&OverlapLayer::Send));

    let caveman = loaded.profiles.iter().find(|p| p.id == "caveman").unwrap();
    assert!(caveman
        .detect
        .packages
        .npm
        .iter()
        .any(|n| n == "@caveman-ai/cli"));
    assert!(caveman.layers.contains(&OverlapLayer::Say));
}

#[test]
fn honesty_and_staleness_validator_catches_bad_profiles() {
    let loaded = load_builtin("2026-11-24");
    let mut p = loaded.profiles[0].clone();

    let stale_issues = validate_profile(&p, "2027-06-01");
    assert!(
        stale_issues
            .iter()
            .any(|i| matches!(i, ValidationIssue::Warning(w) if w.contains(">90 days"))),
        "expected staleness warning: {stale_issues:?}"
    );

    p.capabilities.does_better_than_tinkr.clear();
    let missing_honesty = validate_profile(&p, "2026-11-24");
    assert!(missing_honesty
        .iter()
        .any(|i| matches!(i, ValidationIssue::Error(e) if e.contains("does_better_than_tinkr"))));

    let mut p2 = loaded.profiles[0].clone();
    p2.capabilities.features[0].source_url = "http://unsourced.local".into();
    let bad_url = validate_profile(&p2, "2026-11-24");
    assert!(bad_url
        .iter()
        .any(|i| matches!(i, ValidationIssue::Error(e) if e.contains("source_url"))));
}

#[test]
fn signed_profile_update_verified_and_tampered_or_bad_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let verifier = HmacVerifier {
        key_id: "tinkr-profiles-v1".into(),
        key: b"test-signing-key-32-bytes-secret!".to_vec(),
    };

    let valid_toml = r#"
id = "acme-trim"
name = "Acme Trim"
homepage = "https://example.com/acme-trim"
license_note = "MIT"
doc_version_checked = "1.0.0"
doc_date_checked = "2026-11-24"
layers = ["read"]

[capabilities]
[[capabilities.features]]
text = "Compresses CLI logs"
source_url = "https://example.com/acme-trim"
date = "2026-11-24"

[[capabilities.does_better_than_tinkr]]
text = "Specialized parser for Acme build logs"
source_url = "https://example.com/acme-trim"
date = "2026-11-24"
"#;
    let valid_path = dir.path().join("acme-trim.toml");
    std::fs::write(&valid_path, valid_toml).unwrap();
    let env = verifier.sign(valid_toml.as_bytes());
    std::fs::write(
        dir.path().join("acme-trim.toml.sig"),
        serde_json::to_vec(&env).unwrap(),
    )
    .unwrap();

    let tampered_path = dir.path().join("tampered.toml");
    let mut tampered_bytes = valid_toml
        .replace("acme-trim", "tampered-tool")
        .into_bytes();
    let tampered_sig = verifier.sign(&tampered_bytes);
    tampered_bytes[0] = b'#';
    std::fs::write(&tampered_path, &tampered_bytes).unwrap();
    std::fs::write(
        dir.path().join("tampered.toml.sig"),
        serde_json::to_vec(&tampered_sig).unwrap(),
    )
    .unwrap();

    std::fs::write(dir.path().join("unsigned.toml"), valid_toml).unwrap();

    let bad_toml = b"this is not valid toml === [";
    let bad_sig = verifier.sign(bad_toml);
    std::fs::write(dir.path().join("broken.toml"), bad_toml).unwrap();
    std::fs::write(
        dir.path().join("broken.toml.sig"),
        serde_json::to_vec(&bad_sig).unwrap(),
    )
    .unwrap();

    let loaded = load_with_signed_dir(dir.path(), &verifier, "2026-11-24");
    assert!(
        loaded.profiles.iter().any(|p| p.id == "acme-trim"),
        "signed valid profile must load"
    );
    assert!(
        !loaded.profiles.iter().any(|p| p.id == "tampered-tool"),
        "tampered profile must be rejected"
    );
    assert!(loaded.warnings.iter().any(|w| w.contains("tampered")));
    assert!(loaded.warnings.iter().any(|w| w.contains("unsigned")));
    assert!(loaded.warnings.iter().any(|w| w.contains("malformed")));
}
