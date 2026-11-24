use super::{
    envpersist::{EnvDFile, EnvStore, PlistEnv, PLIST_LABEL},
    Paths, Platform,
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

#[derive(Debug, Default, Clone, Copy)]
pub struct Native;

impl Platform for Native {
    fn name(&self) -> &'static str {
        if cfg!(target_os = "macos") {
            "macos"
        } else {
            "linux"
        }
    }
    fn set_private(&self, path: &Path) -> std::io::Result<()> {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
    }
    fn set_executable(&self, path: &Path) -> std::io::Result<()> {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
    }
    fn env_store(&self, home: &Path, paths: &Paths) -> Box<dyn EnvStore> {
        if cfg!(target_os = "macos") {
            Box::new(PlistEnv {
                plist_path: home
                    .join("Library")
                    .join("LaunchAgents")
                    .join(format!("{PLIST_LABEL}.plist")),
                state_path: paths.data.join("env-state.json"),
                launchctl: true,
            })
        } else {
            let cfg = std::env::var_os("XDG_CONFIG_HOME")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"));
            Box::new(EnvDFile {
                path: cfg.join("environment.d").join("50-tinkr.conf"),
            })
        }
    }
    fn extended_path(&self, path: &Path) -> PathBuf {
        path.to_path_buf()
    }
    fn unicode_capable(&self, env: &dyn Fn(&str) -> Option<String>) -> bool {
        ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .filter_map(|k| env(k).filter(|v| !v.is_empty()))
            .next()
            .map(|v| {
                let v = v.to_ascii_lowercase();
                v.contains("utf-8") || v.contains("utf8")
            })
            .unwrap_or(false)
    }
}
