use std::{
    fs,
    time::{Duration, SystemTime},
};
use tinkr_core::recall::{load_recall_config, redact_str, RecallConfig, RecallStore};

#[test]
fn golden_recall_safety_redaction_retention_purge_and_os_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let rdir = dir.path().join("recall");

    fs::create_dir_all(&rdir).unwrap();
    fs::write(rdir.join("recall.toml"), "retention_days = 2\nmax_mb = 1\n").unwrap();
    let loaded_cfg = load_recall_config(&rdir);
    assert_eq!(loaded_cfg.retention, Duration::from_secs(2 * 86_400));
    assert_eq!(loaded_cfg.max_bytes, 1024 * 1024);

    let store = RecallStore::open_with_config(&rdir, loaded_cfg).unwrap();

    let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEAtestkeymaterial\n-----END RSA PRIVATE KEY-----";
    let unlabelled_high_entropy = "7kP9mW2xQ5vL8nR3zT6yB4cJ1fH0dG9sA2uE";
    let payload = format!(
        "commit 0123456789abcdef0123456789abcdef01234567\n\
         error at crates/api/src/handler.rs:108:5\n\
         {pem}\n\
         raw_token={unlabelled_high_entropy}\n\
         REDIS_URL=redis://default:MyRedisPass99!@cache.local:6379/0\n"
    );
    let id = store.put(payload.as_bytes()).unwrap();
    let stored = String::from_utf8_lossy(&store.get(&id).unwrap()).into_owned();
    assert!(stored.contains("0123456789abcdef0123456789abcdef01234567"));
    assert!(stored.contains("crates/api/src/handler.rs:108:5"));
    assert!(!stored.contains("MIIEpAIBAAKCAQEAtestkeymaterial"));
    assert!(!stored.contains(unlabelled_high_entropy));
    assert!(!stored.contains("MyRedisPass99!"));
    assert!(stored.contains("[REDACTED]"));

    let normal = "pub fn calculate_monthly_invoice_summary(account_id: u64) -> Result<Invoice, BillingError>";
    assert_eq!(redact_str(normal).as_ref(), normal);

    let bin_file = fs::read_dir(&rdir)
        .unwrap()
        .filter_map(|e| e.ok().map(|x| x.path()))
        .find(|p| p.extension().and_then(|s| s.to_str()) == Some("bin"))
        .expect("recall .bin file exists");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&bin_file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "recall blob must be 0600 on Unix");
    }

    #[cfg(windows)]
    {
        let out = std::process::Command::new("icacls")
            .arg(&bin_file)
            .output()
            .expect("icacls available on Windows");
        let acl_text = String::from_utf8_lossy(&out.stdout);
        assert!(
            !acl_text.contains("(I)"),
            "inherited ACEs must be stripped on recall blob, got:\n{acl_text}"
        );
        assert!(
            !acl_text.contains("Everyone:") && !acl_text.contains("BUILTIN\\Users:"),
            "Everyone/Users must not have access to recall blob, got:\n{acl_text}"
        );
        assert!(
            acl_text.contains("(F)"),
            "current user must retain full control on recall blob, got:\n{acl_text}"
        );
    }

    {
        let f = fs::OpenOptions::new().write(true).open(&bin_file).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(3 * 86_400))
            .unwrap();
    }
    let _ = fs::remove_file(rdir.join(".last_prune"));
    let reopened = RecallStore::open(&rdir).unwrap();
    assert!(
        reopened.get(&id).is_err(),
        "expired >2d blob must be automatically pruned on open"
    );

    let id2 = reopened
        .put(&vec![b'x'; 512])
        .unwrap();
    assert!(reopened.get(&id2).is_ok());
    assert_eq!(reopened.purge().unwrap(), 1);
    assert!(reopened.get(&id2).is_err());
    let zero_cfg = RecallConfig {
        retention: Duration::ZERO,
        max_bytes: 0,
    };
    let zero_store = RecallStore::open_with_config(&dir.path().join("zero"), zero_cfg).unwrap();
    let zid = zero_store.put(b"ephemeral").unwrap();
    assert!(zero_store.get(&zid).is_err());
}
