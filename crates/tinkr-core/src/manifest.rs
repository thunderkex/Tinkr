use crate::{
    io_err,
    platform::{
        envpersist::{EnvStore, EnvValue},
        fsx,
    },
    Result,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Change {
    FileEdited {
        path: PathBuf,
        backup: PathBuf,
        sha256_after: String,
    },
    FileCreated {
        path: PathBuf,
        sha256_after: String,
    },
    BlockInserted {
        path: PathBuf,
        prefix: String,
        block: String,
        file_created: bool,
    },
    EnvSet {
        name: String,
        previous: Option<EnvValue>,
    },
    DirCreated {
        path: PathBuf,
    },
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub changes: Vec<Change>,
}

#[derive(Debug, Default, Clone)]
pub struct UndoReport {
    pub restored: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    pub env_restored: Vec<String>,
    pub skipped: Vec<String>,
    pub errors: Vec<String>,
}

pub struct ChangeSet {
    path: PathBuf,
    pub manifest: Manifest,
}

fn sha(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn read_opt(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(path)(e)),
    }
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() || from > hay.len() - needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

fn find_block(content: &[u8], comment: &str) -> Option<std::ops::Range<usize>> {
    let begin = format!("{comment} >>> tinkr >>>");
    let end = format!("{comment} <<< tinkr <<<");
    let mut pos = 0;
    let start = loop {
        let i = find(content, begin.as_bytes(), pos)?;
        if i == 0 || content[i - 1] == b'\n' {
            break i;
        }
        pos = i + 1;
    };
    let e = find(content, end.as_bytes(), start)?;
    let mut stop = e + end.len();
    while stop < content.len() && content[stop] != b'\n' {
        stop += 1;
    }
    if content.get(stop) == Some(&b'\n') {
        stop += 1;
    }
    Some(start..stop)
}

fn comment_of(block: &str) -> &str {
    block.split(" >>> tinkr >>>").next().unwrap_or("#")
}

impl ChangeSet {
    pub fn open(path: &Path) -> Result<Self> {
        let manifest = Self::load(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            manifest,
        })
    }

    fn load(path: &Path) -> Result<Manifest> {
        Ok(match read_opt(path)? {
            Some(b) => serde_json::from_slice(&b)?,
            None => Manifest {
                version: 1,
                changes: vec![],
            },
        })
    }

    fn mutate<T>(&mut self, f: impl FnOnce(&mut Manifest) -> Result<T>) -> Result<T> {
        let path = self.path.clone();
        let (out, m) = fsx::with_lock(&path, || {
            let mut m = Self::load(&path)?;
            let r = f(&mut m);
            fsx::atomic_write_unlocked(&path, &serde_json::to_vec_pretty(&m)?)?;
            Ok((r, m))
        })?;
        self.manifest = m;
        out
    }

    pub fn is_empty(&self) -> bool {
        self.manifest.changes.is_empty()
    }

    pub fn remove_file_entry(&mut self, path: &Path) -> Result<()> {
        self.mutate(|m| {
            m.changes
                .retain(|c| !matches!(c, Change::FileEdited { path: p, .. } if p == path));
            Ok(())
        })
    }

    fn ensure_dirs(dir: &Path, m: &mut Manifest) -> Result<()> {
        let mut missing = Vec::new();
        let mut cur = Some(dir);
        while let Some(d) = cur {
            if d.as_os_str().is_empty() || d.exists() {
                break;
            }
            missing.push(d.to_path_buf());
            cur = d.parent();
        }
        for d in missing.into_iter().rev() {
            fs::create_dir(&d).map_err(io_err(&d))?;
            m.changes.push(Change::DirCreated { path: d });
        }
        Ok(())
    }

    pub fn edit_file(&mut self, path: &Path, new: &[u8]) -> Result<bool> {
        self.mutate(|m| {
            let existing = read_opt(path)?;
            if existing.as_deref() == Some(new) {
                return Ok(false);
            }
            let after = sha(new);
            if existing.is_none() {
                if let Some(d) = path.parent() {
                    Self::ensure_dirs(d, m)?;
                }
                fsx::atomic_write(path, new)?;
                m.changes.push(Change::FileCreated {
                    path: path.to_path_buf(),
                    sha256_after: after,
                });
                return Ok(true);
            }
            let tracked = m.changes.iter_mut().find(|c| match c {
                Change::FileEdited { path: p, .. } | Change::FileCreated { path: p, .. } => {
                    p == path
                }
                _ => false,
            });
            match tracked {
                None => {
                    let backup = fsx::backup_once(path)?.ok_or_else(|| {
                        io_err(path)(std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            "file vanished before backup",
                        ))
                    })?;
                    m.changes.push(Change::FileEdited {
                        path: path.to_path_buf(),
                        backup,
                        sha256_after: after,
                    });
                    fsx::atomic_write(path, new)?;
                }
                Some(entry) => {
                    fsx::atomic_write(path, new)?;
                    match entry {
                        Change::FileEdited { sha256_after, .. }
                        | Change::FileCreated { sha256_after, .. } => *sha256_after = after,
                        _ => {}
                    }
                }
            }
            Ok(true)
        })
    }

    pub fn insert_block(&mut self, path: &Path, comment: &str, body: &str) -> Result<bool> {
        self.mutate(|m| {
            let (comment, suffix) = comment.split_once('|').unwrap_or((comment, ""));
            let content = read_opt(path)?;
            let file_created = content.is_none();
            let mut data = content.unwrap_or_default();
            let eol = if find(&data, b"\r\n", 0).is_some() {
                "\r\n"
            } else {
                "\n"
            };
            let lines: Vec<&str> = body.trim_end_matches(['\r', '\n']).lines().collect();
            let block = format!(
                "{comment} >>> tinkr >>>{suffix}{eol}{}{eol}{comment} <<< tinkr <<<{suffix}{eol}",
                lines.join(eol)
            );
            let existing_idx = m
                .changes
                .iter()
                .position(|c| matches!(c, Change::BlockInserted { path: p, .. } if p == path));
            if let Some(range) = find_block(&data, comment) {
                if &data[range.clone()] == block.as_bytes() {
                    return Ok(false);
                }
                data.splice(range, block.bytes());
                fsx::backup_once(path)?;
                fsx::atomic_write(path, &data)?;
                if let Some(i) = existing_idx {
                    if let Change::BlockInserted { block: b, .. } = &mut m.changes[i] {
                        *b = block;
                    }
                }
                return Ok(true);
            }
            if let Some(d) = path.parent() {
                Self::ensure_dirs(d, m)?;
            }
            let prefix = if !data.is_empty() && !data.ends_with(b"\n") {
                eol.to_string()
            } else {
                String::new()
            };
            fsx::backup_once(path)?;
            data.extend_from_slice(prefix.as_bytes());
            data.extend_from_slice(block.as_bytes());
            fsx::atomic_write(path, &data)?;
            m.changes.push(Change::BlockInserted {
                path: path.to_path_buf(),
                prefix,
                block,
                file_created,
            });
            Ok(true)
        })
    }

    pub fn set_env(&mut self, store: &dyn EnvStore, name: &str, value: &EnvValue) -> Result<bool> {
        self.mutate(|m| {
            let current = store.get(name)?;
            if current.as_ref() == Some(value) {
                return Ok(false);
            }
            if !m
                .changes
                .iter()
                .any(|c| matches!(c, Change::EnvSet { name: n, .. } if n == name))
            {
                m.changes.push(Change::EnvSet {
                    name: name.to_string(),
                    previous: current,
                });
            }
            store.set(name, value)?;
            Ok(true)
        })
    }

    pub fn undo(&mut self, store: &dyn EnvStore) -> Result<UndoReport> {
        let mut report = UndoReport::default();
        self.mutate(|m| {
            let mut remaining: Vec<Change> = Vec::new();
            let mut bak_cleanup: Vec<PathBuf> = Vec::new();
            for change in m.changes.drain(..).rev() {
                match undo_one(&change, store, &mut report, &mut bak_cleanup) {
                    Ok(()) => {}
                    Err(e) => {
                        report.errors.push(e.to_string());
                        remaining.push(change);
                    }
                }
            }
            remaining.reverse();
            for bak in bak_cleanup {
                let still_used = remaining
                    .iter()
                    .any(|c| matches!(c, Change::FileEdited { backup, .. } if *backup == bak));
                if !still_used {
                    let _ = fs::remove_file(bak);
                }
            }
            m.changes = remaining;
            Ok(())
        })?;
        if self.manifest.changes.is_empty() {
            let _ = fs::remove_file(&self.path);
        }
        Ok(report)
    }
}

fn undo_one(
    change: &Change,
    store: &dyn EnvStore,
    report: &mut UndoReport,
    bak_cleanup: &mut Vec<PathBuf>,
) -> Result<()> {
    match change {
        Change::FileEdited {
            path,
            backup,
            sha256_after,
        } => {
            let original = fs::read(backup).map_err(io_err(backup))?;
            if let Some(cur) = read_opt(path)? {
                if sha(&cur) != *sha256_after {
                    let mut s = path.as_os_str().to_os_string();
                    s.push(".tinkr.modified");
                    fsx::atomic_write(&PathBuf::from(&s), &cur)?;
                    report.skipped.push(format!(
                        "{} was modified after setup; your copy kept as .tinkr.modified",
                        path.display()
                    ));
                }
            }
            fsx::atomic_write(path, &original)?;
            bak_cleanup.push(backup.clone());
            report.restored.push(path.clone());
        }
        Change::FileCreated { path, sha256_after } => match read_opt(path)? {
            None => {}
            Some(cur) if sha(&cur) == *sha256_after => {
                fs::remove_file(path).map_err(io_err(path))?;
                report.removed.push(path.clone());
            }
            Some(_) => report.skipped.push(format!(
                "{} changed since setup; left in place",
                path.display()
            )),
        },
        Change::BlockInserted {
            path,
            prefix,
            block,
            file_created,
        } => {
            if let Some(mut data) = read_opt(path)? {
                if let Some(mut range) = find_block(&data, comment_of(block)) {
                    let p = prefix.as_bytes();
                    if !p.is_empty()
                        && range.start >= p.len()
                        && &data[range.start - p.len()..range.start] == p
                    {
                        range.start -= p.len();
                    }
                    data.drain(range);
                    if *file_created && data.iter().all(|b| b.is_ascii_whitespace()) {
                        fs::remove_file(path).map_err(io_err(path))?;
                        report.removed.push(path.clone());
                    } else {
                        fsx::atomic_write(path, &data)?;
                        report.restored.push(path.clone());
                    }
                    bak_cleanup.push(fsx::backup_path(path));
                }
            }
        }
        Change::EnvSet { name, previous } => {
            match previous {
                Some(v) => store.set(name, v)?,
                None => store.unset(name)?,
            }
            report.env_restored.push(name.clone());
        }
        Change::DirCreated { path } => {
            let _ = fs::remove_dir(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::envpersist::EnvDFile;
    use std::fs;

    struct Rig {
        _d: tempfile::TempDir,
        root: PathBuf,
        cs: ChangeSet,
        env: EnvDFile,
    }

    fn rig() -> Rig {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().to_path_buf();
        let cs = ChangeSet::open(&root.join("data").join("manifest.json")).unwrap();
        let env = EnvDFile {
            path: root
                .join("home")
                .join("environment.d")
                .join("50-tinkr.conf"),
        };
        Rig {
            _d: d,
            root,
            cs,
            env,
        }
    }

    fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
        fn walk(base: &Path, d: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            for e in fs::read_dir(d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    out.push((
                        format!("{}/", p.strip_prefix(base).unwrap().display()),
                        vec![],
                    ));
                    walk(base, &p, out);
                } else {
                    out.push((
                        p.strip_prefix(base).unwrap().display().to_string(),
                        fs::read(&p).unwrap(),
                    ));
                }
            }
        }
        let mut v = Vec::new();
        walk(dir, dir, &mut v);
        v.sort();
        v
    }

    #[test]
    fn full_setup_then_undo_is_byte_exact() {
        let mut r = rig();
        let home = r.root.join("home dir \u{fc}");
        fs::create_dir_all(&home).unwrap();
        let settings = home.join("settings.json");
        let zshrc = home.join(".zshrc");
        let bashrc = home.join(".bashrc");
        let orig_settings = b"{\r\n  // comment\r\n  \"a\": 1\r\n}".to_vec();
        let orig_zshrc = b"export A=1\nalias x=y\n".to_vec();
        let orig_bashrc = b"echo hi\r\n\xffraw".to_vec();
        fs::write(&settings, &orig_settings).unwrap();
        fs::write(&zshrc, &orig_zshrc).unwrap();
        fs::write(&bashrc, &orig_bashrc).unwrap();
        let before = snapshot(&home);

        assert!(r.cs.edit_file(&settings, b"{\"a\":2}").unwrap());
        assert!(r.cs.insert_block(&zshrc, "#", "export TINKR=1").unwrap());
        assert!(r
            .cs
            .insert_block(&bashrc, "#", "export TINKR=1\nexport X=2")
            .unwrap());
        assert!(r
            .cs
            .insert_block(
                &home.join(".config").join("fish").join("conf.d.fish"),
                "#",
                "set -x T 1"
            )
            .unwrap());
        assert!(r
            .cs
            .edit_file(&home.join("new").join("deep").join("f.txt"), b"hi")
            .unwrap());
        assert!(r
            .cs
            .set_env(
                &r.env,
                "ANTHROPIC_BASE_URL",
                &EnvValue::plain("http://127.0.0.1:1")
            )
            .unwrap());
        assert_ne!(snapshot(&home), before);

        let mid = snapshot(&home);
        assert!(!r.cs.edit_file(&settings, b"{\"a\":2}").unwrap());
        assert!(!r.cs.insert_block(&zshrc, "#", "export TINKR=1").unwrap());
        assert!(!r
            .cs
            .insert_block(&bashrc, "#", "export TINKR=1\nexport X=2")
            .unwrap());
        assert!(!r
            .cs
            .set_env(
                &r.env,
                "ANTHROPIC_BASE_URL",
                &EnvValue::plain("http://127.0.0.1:1")
            )
            .unwrap());
        assert_eq!(snapshot(&home), mid);
        let b = fs::read(&bashrc).unwrap();
        assert!(String::from_utf8_lossy(&b)
            .contains("# >>> tinkr >>>\r\nexport TINKR=1\r\nexport X=2\r\n# <<< tinkr <<<\r\n"));

        let rep = r.cs.undo(&r.env).unwrap();
        assert!(rep.errors.is_empty(), "{:?}", rep.errors);
        assert_eq!(
            snapshot(&home),
            before,
            "byte-for-byte restoration incl. no residue"
        );
        assert_eq!(fs::read(&settings).unwrap(), orig_settings);
        assert_eq!(fs::read(&bashrc).unwrap(), orig_bashrc);
        assert!(r.env.get("ANTHROPIC_BASE_URL").unwrap().is_none());
        assert!(!r.root.join("data").join("manifest.json").exists());
    }

    #[test]
    fn block_update_replaces_in_place_and_keeps_user_edits_on_undo() {
        let mut r = rig();
        let p = r.root.join(".profile");
        fs::write(&p, "line1\n").unwrap();
        r.cs.insert_block(&p, "#", "v=1").unwrap();
        assert!(r.cs.insert_block(&p, "#", "v=2").unwrap());
        assert_eq!(
            fs::read_to_string(&p)
                .unwrap()
                .matches(">>> tinkr >>>")
                .count(),
            1
        );
        let mut s = fs::read_to_string(&p).unwrap();
        s.push_str("user line\n");
        fs::write(&p, &s).unwrap();
        r.cs.undo(&r.env).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "line1\nuser line\n");
    }

    #[test]
    fn env_previous_is_preserved_across_repeated_sets() {
        let mut r = rig();
        r.env
            .set("OPENAI_BASE_URL", &EnvValue::plain("https://user.example"))
            .unwrap();
        r.cs.set_env(
            &r.env,
            "OPENAI_BASE_URL",
            &EnvValue::plain("http://127.0.0.1:1"),
        )
        .unwrap();
        r.cs.set_env(
            &r.env,
            "OPENAI_BASE_URL",
            &EnvValue::plain("http://127.0.0.1:2"),
        )
        .unwrap();
        r.cs.undo(&r.env).unwrap();
        assert_eq!(
            r.env.get("OPENAI_BASE_URL").unwrap().unwrap().value,
            "https://user.example"
        );
    }

    #[test]
    fn user_modified_file_is_not_lost() {
        let mut r = rig();
        let p = r.root.join("c.json");
        fs::write(&p, "orig").unwrap();
        r.cs.edit_file(&p, b"tinkr").unwrap();
        fs::write(&p, "user edited later").unwrap();
        let rep = r.cs.undo(&r.env).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "orig");
        let mut m = p.as_os_str().to_os_string();
        m.push(".tinkr.modified");
        assert_eq!(
            fs::read_to_string(PathBuf::from(m)).unwrap(),
            "user edited later"
        );
        assert_eq!(rep.skipped.len(), 1);
    }

    #[test]
    fn crash_midway_is_still_undoable() {
        let mut r = rig();
        let p = r.root.join("a.txt");
        fs::write(&p, "orig").unwrap();
        r.cs.edit_file(&p, b"new").unwrap();
        let mut cs2 = ChangeSet::open(&r.root.join("data").join("manifest.json")).unwrap();
        assert_eq!(cs2.manifest.changes.len(), 1);
        cs2.undo(&r.env).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "orig");
    }

    #[test]
    fn concurrent_sessions_do_not_lose_manifest_entries() {
        let d = tempfile::tempdir().unwrap();
        let mpath = d.path().join("manifest.json");
        let hs: Vec<_> = (0..6)
            .map(|i| {
                let (mpath, root) = (mpath.clone(), d.path().to_path_buf());
                std::thread::spawn(move || {
                    let mut cs = ChangeSet::open(&mpath).unwrap();
                    cs.edit_file(&root.join(format!("f{i}.txt")), b"x").unwrap();
                })
            })
            .collect();
        hs.into_iter().for_each(|h| h.join().unwrap());
        assert_eq!(ChangeSet::open(&mpath).unwrap().manifest.changes.len(), 6);
    }
}
