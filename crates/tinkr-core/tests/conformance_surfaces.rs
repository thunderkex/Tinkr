use std::{collections::HashMap, path::PathBuf};
use tinkr_core::{
    manifest::ChangeSet,
    platform::envpersist::EnvDFile,
    profile::{self, ActionResult, Machine},
};

fn fake_machine(home: PathBuf) -> Machine {
    let map: HashMap<String, String> = HashMap::new();
    Machine {
        home,
        env: Box::new(move |k| map.get(k).cloned()),
        path_var: Some(std::ffi::OsString::new()),
        tinkr_exe: PathBuf::from("/usr/local/bin/tinkr"),
    }
}

#[test]
fn all_24_profiles_detect_apply_idempotent_and_undo_byte_exact() {
    let profiles = profile::builtin();
    assert_eq!(profiles.len(), 24, "expected 24 built-in surface profiles");

    for p in &profiles {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let m = fake_machine(home.clone());

        std::fs::create_dir_all(home.join(format!(".{}-unrelated", p.id))).unwrap();
        if p.dir_default.as_deref() != Some("~") {
            assert!(
                profile::detect(p, &m).is_none(),
                "{} false-positive on empty home",
                p.id
            );
        }

        let cfg_dir = m.config_dir(p).unwrap();
        std::fs::create_dir_all(&cfg_dir).unwrap();
        if let Some(first_path) = p.detect.paths.first() {
            let expanded = m.expand(first_path);
            let is_file = matches!(
                expanded.extension().and_then(|e| e.to_str()),
                Some("yml" | "yaml" | "json" | "toml")
            );
            if is_file {
                if let Some(par) = expanded.parent() {
                    std::fs::create_dir_all(par).unwrap();
                }
                std::fs::write(&expanded, "# marker\n").unwrap();
            } else {
                std::fs::create_dir_all(&expanded).unwrap();
            }
        }
        assert!(
            profile::detect(p, &m).is_some(),
            "{} not detected after creating config",
            p.id
        );

        let mut cs = ChangeSet::open(&d.path().join("manifest.json")).unwrap();
        let r1 = profile::apply(p, &m, &mut cs).unwrap();
        assert!(
            r1.iter().any(|r| matches!(r, ActionResult::Change(_))),
            "{} made no changes",
            p.id
        );

        let r2 = profile::apply(p, &m, &mut cs).unwrap();
        assert!(
            r2.iter().all(|r| matches!(r, ActionResult::AlreadyDone(_))),
            "{} not idempotent: {:?}",
            p.id,
            r2
        );

        let store = EnvDFile {
            path: d.path().join("env.conf"),
        };
        let rep = cs.undo(&store).unwrap();
        assert!(
            rep.errors.is_empty(),
            "{} undo errors: {:?}",
            p.id,
            rep.errors
        );
    }
}
