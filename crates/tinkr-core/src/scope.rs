use crate::{io_err, platform::fsx, Result};
use std::path::{Path, PathBuf};

pub fn global_pause_file(config_dir: &Path) -> PathBuf {
    config_dir.join("paused")
}

pub fn project_pause_file(dir: &Path) -> PathBuf {
    dir.join(".tinkr").join("paused")
}

pub fn set_paused(pause_file: &Path, paused: bool) -> Result<()> {
    if paused {
        fsx::atomic_write(pause_file, b"paused\n")
    } else {
        match std::fs::remove_file(pause_file) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_err(pause_file)(e)),
        }
    }
}

pub fn is_paused(config_dir: &Path, cwd: &Path) -> bool {
    if global_pause_file(config_dir).exists() {
        return true;
    }
    let mut cur = Some(cwd);
    while let Some(d) = cur {
        if project_pause_file(d).exists() {
            return true;
        }
        cur = d.parent();
    }
    false
}

pub fn is_ignored(cwd: &Path, cmd_or_path: &str) -> bool {
    let mut cur = Some(cwd);
    while let Some(d) = cur {
        let p = d.join(".tinkrignore");
        if let Ok(text) = std::fs::read_to_string(&p) {
            for line in text.lines() {
                let pat = line.trim();
                if pat.is_empty() || pat.starts_with('#') {
                    continue;
                }
                if cmd_or_path.contains(pat) {
                    return true;
                }
            }
        }
        cur = d.parent();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pause_and_ignore_work() {
        let d = tempfile::tempdir().unwrap();
        let cfg = d.path().join("cfg");
        let proj = d.path().join("repo").join("sub");
        std::fs::create_dir_all(&proj).unwrap();
        assert!(!is_paused(&cfg, &proj));
        set_paused(&project_pause_file(&d.path().join("repo")), true).unwrap();
        assert!(is_paused(&cfg, &proj));
        set_paused(&project_pause_file(&d.path().join("repo")), false).unwrap();
        assert!(!is_paused(&cfg, &proj));

        std::fs::write(
            d.path().join("repo").join(".tinkrignore"),
            "# skip secrets\nsecret.env\ncargo bench\n",
        )
        .unwrap();
        assert!(is_ignored(&proj, "cat secret.env"));
        assert!(!is_ignored(&proj, "git status"));
    }
}
