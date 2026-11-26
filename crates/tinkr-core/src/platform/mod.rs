pub mod envpersist;
pub mod fsx;
pub mod winpath;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use unix::Native;
#[cfg(windows)]
pub use windows::Native;

use crate::{Error, Result};
use std::{
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct Paths {
    pub config: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
}

impl Paths {
    pub fn ledger_db(&self) -> PathBuf {
        self.data.join("ledger.db")
    }
    pub fn recall_dir(&self) -> PathBuf {
        self.data.join("recall")
    }
    pub fn manifest(&self) -> PathBuf {
        self.data.join("manifest.json")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCaps {
    pub color: bool,
    pub unicode: bool,
    pub tty: bool,
}

pub trait Platform {
    fn name(&self) -> &'static str;
    fn set_private(&self, path: &Path) -> std::io::Result<()>;
    fn set_executable(&self, path: &Path) -> std::io::Result<()>;
    fn extended_path(&self, path: &Path) -> PathBuf;
    fn unicode_capable(&self, env: &dyn Fn(&str) -> Option<String>) -> bool;
    fn env_store(&self, home: &Path, paths: &Paths) -> Box<dyn envpersist::EnvStore>;
    fn home_dir(&self) -> Result<PathBuf> {
        #[cfg(windows)]
        {
            if let Some(h) = std::env::var_os("USERPROFILE").filter(|s| !s.is_empty()) {
                return Ok(PathBuf::from(h));
            }
            if let (Some(d), Some(p)) = (
                std::env::var_os("HOMEDRIVE").filter(|s| !s.is_empty()),
                std::env::var_os("HOMEPATH").filter(|s| !s.is_empty()),
            ) {
                let mut root = PathBuf::from(d);
                root.push(p);
                return Ok(root);
            }
            if let Some(h) = std::env::var_os("HOME").filter(|s| !s.is_empty()) {
                return Ok(PathBuf::from(h));
            }
            Err(Error::NoHome)
        }
        #[cfg(unix)]
        {
            std::env::var_os("HOME")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .ok_or(Error::NoHome)
        }
    }
    fn base_dirs(&self) -> Result<Paths> {
        #[cfg(windows)]
        {
            let home = self.home_dir();
            let config_base = std::env::var_os("APPDATA")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .or_else(|| home.as_ref().ok().map(|h| h.join("AppData").join("Roaming")))
                .ok_or(Error::NoHome)?;
            let local_base = std::env::var_os("LOCALAPPDATA")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .or_else(|| home.as_ref().ok().map(|h| h.join("AppData").join("Local")))
                .ok_or(Error::NoHome)?;
            let config = config_base.join("tinkr");
            let data = local_base.join("tinkr");
            let cache = data.join("cache");
            Ok(Paths {
                config,
                data,
                cache,
            })
        }
        #[cfg(target_os = "macos")]
        {
            let home = self.home_dir()?;
            let app_support = home.join("Library").join("Application Support").join("tinkr");
            let cache = home.join("Library").join("Caches").join("tinkr");
            Ok(Paths {
                config: app_support.clone(),
                data: app_support,
                cache,
            })
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let home = self.home_dir()?;
            let config = std::env::var_os("XDG_CONFIG_HOME")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"))
                .join("tinkr");
            let data = std::env::var_os("XDG_DATA_HOME")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local").join("share"))
                .join("tinkr");
            let mut cache = std::env::var_os("XDG_CACHE_HOME")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".cache"))
                .join("tinkr");
            if cache == data {
                cache = data.join("cache");
            }
            Ok(Paths {
                config,
                data,
                cache,
            })
        }
    }
}

pub fn paths(p: &dyn Platform) -> Result<Paths> {
    paths_with(p, &|k| std::env::var(k).ok())
}

pub fn paths_with(p: &dyn Platform, env: &dyn Fn(&str) -> Option<String>) -> Result<Paths> {
    match env("TINKR_HOME").filter(|s| !s.is_empty()) {
        Some(root) => {
            let root = PathBuf::from(root);
            Ok(Paths {
                config: root.join("config"),
                data: root.join("data"),
                cache: root.join("cache"),
            })
        }
        None => p.base_dirs(),
    }
}

pub fn decide_terminal(
    p: &dyn Platform,
    env: &dyn Fn(&str) -> Option<String>,
    tty: bool,
) -> TerminalCaps {
    let no_color = env("NO_COLOR").map(|v| !v.is_empty()).unwrap_or(false);
    let dumb = env("TERM").map(|t| t == "dumb").unwrap_or(false);
    TerminalCaps {
        tty,
        color: tty && !no_color && !dumb,
        unicode: tty && p.unicode_capable(env),
    }
}

pub fn terminal(p: &dyn Platform) -> TerminalCaps {
    decide_terminal(
        p,
        &|k| std::env::var(k).ok(),
        std::io::stdout().is_terminal(),
    )
}

pub fn is_case_insensitive(dir: &Path) -> bool {
    let probe = dir.join(format!(".TinkrCase{}", std::process::id()));
    let ok = std::fs::File::create(&probe)
        .and_then(|mut f| f.write_all(b"x"))
        .is_ok();
    let alt = dir.join(format!(".tinkrcase{}", std::process::id()));
    let res = ok && alt.exists();
    let _ = std::fs::remove_file(&probe);
    res
}

pub fn paths_equal(a: &Path, b: &Path, case_insensitive: bool) -> bool {
    if case_insensitive {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn tinkr_home_override() {
        let n = Native;
        let p = paths_with(&n, &env_of(&[("TINKR_HOME", "/x/y z")])).unwrap();
        assert_eq!(p.data, PathBuf::from("/x/y z").join("data"));
        assert_eq!(p.recall_dir(), p.data.join("recall"));
    }

    #[test]
    fn default_paths_end_in_tinkr() {
        let n = Native;
        let p = paths_with(&n, &env_of(&[])).unwrap();
        assert!(p.config.ends_with("tinkr"));
        assert!(p.data.ends_with("tinkr"));
        assert_ne!(p.cache, p.data);
    }

    #[test]
    fn terminal_rules() {
        let n = Native;
        let c = decide_terminal(&n, &env_of(&[("NO_COLOR", "1")]), true);
        assert!(!c.color);
        let c = decide_terminal(&n, &env_of(&[]), false);
        assert!(!c.color && !c.unicode && !c.tty);
        let c = decide_terminal(&n, &env_of(&[("TERM", "dumb")]), true);
        assert!(!c.color);
    }

    #[test]
    fn case_probe_does_not_panic() {
        let d = tempfile::tempdir().unwrap();
        let _ = is_case_insensitive(d.path());
        assert!(paths_equal(Path::new("A/b"), Path::new("a/B"), true));
        assert!(!paths_equal(Path::new("A/b"), Path::new("a/B"), false));
    }
}
