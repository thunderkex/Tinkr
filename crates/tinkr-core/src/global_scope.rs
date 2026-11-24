use crate::{
    manifest::ChangeSet,
    platform::envpersist::{EnvStore, EnvValue},
    Result,
};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellProfile {
    pub shell: &'static str,
    pub path: PathBuf,
    pub snippet: String,
}

pub fn detect_shell_profiles(home: &Path, bin_dir: &Path) -> Vec<ShellProfile> {
    let dir_posix = bin_dir.to_string_lossy().replace('\\', "/");
    let dir_win = bin_dir.to_string_lossy().into_owned();
    let mut out = Vec::new();

    for (shell, rel) in [("bash", ".bashrc"), ("zsh", ".zshrc")] {
        let p = home.join(rel);
        if p.exists() {
            out.push(ShellProfile {
                shell,
                path: p,
                snippet: format!("export TINKR_ACTIVE=1\nexport PATH=\"{dir_posix}:$PATH\""),
            });
        }
    }
    let fish = home.join(".config").join("fish").join("config.fish");
    if fish.exists() || home.join(".config").join("fish").is_dir() {
        out.push(ShellProfile {
            shell: "fish",
            path: home
                .join(".config")
                .join("fish")
                .join("conf.d")
                .join("tinkr.fish"),
            snippet: format!("set -gx TINKR_ACTIVE 1\nfish_add_path \"{dir_posix}\""),
        });
    }
    let nu = home.join(".config").join("nushell").join("env.nu");
    if nu.exists() {
        out.push(ShellProfile {
            shell: "nushell",
            path: nu,
            snippet: format!(
                "$env.TINKR_ACTIVE = \"1\"\n$env.PATH = ($env.PATH | prepend \"{dir_posix}\")"
            ),
        });
    }
    for (shell, rel) in [
        (
            "pwsh",
            PathBuf::from("Documents")
                .join("PowerShell")
                .join("Microsoft.PowerShell_profile.ps1"),
        ),
        (
            "powershell",
            PathBuf::from("Documents")
                .join("WindowsPowerShell")
                .join("Microsoft.PowerShell_profile.ps1"),
        ),
    ] {
        let p = home.join(rel);
        if p.exists() || p.parent().is_some_and(|d| d.is_dir()) {
            out.push(ShellProfile {
                shell,
                path: p,
                snippet: format!("$env:TINKR_ACTIVE = \"1\"\nif ($env:Path -notlike \"*{dir_win}*\") {{ $env:Path = \"{dir_win};$env:Path\" }}"),
            });
        }
    }
    out
}

pub fn apply_global(
    home: &Path,
    bin_dir: &Path,
    store: &dyn EnvStore,
    cs: &mut ChangeSet,
) -> Result<usize> {
    let mut changed = 0;
    if cs.set_env(store, "TINKR_ACTIVE", &EnvValue::plain("1"))? {
        changed += 1;
    }
    for sp in detect_shell_profiles(home, bin_dir) {
        if cs.insert_block(&sp.path, "#", &sp.snippet)? {
            changed += 1;
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::envpersist::EnvDFile;

    #[test]
    fn shell_blocks_and_env_apply_and_undo_cleanly() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        std::fs::create_dir_all(home.join(".config").join("fish")).unwrap();
        std::fs::write(home.join(".bashrc"), "export FOO=1\n").unwrap();
        std::fs::write(home.join(".zshrc"), "export BAR=2\n").unwrap();
        let store = EnvDFile {
            path: d.path().join("env.conf"),
        };
        let mut cs = ChangeSet::open(&d.path().join("manifest.json")).unwrap();

        let n = apply_global(&home, &d.path().join("bin"), &store, &mut cs).unwrap();
        assert_eq!(n, 4);
        assert_eq!(
            apply_global(&home, &d.path().join("bin"), &store, &mut cs).unwrap(),
            0
        );
        cs.undo(&store).unwrap();
        assert_eq!(
            std::fs::read_to_string(home.join(".bashrc")).unwrap(),
            "export FOO=1\n"
        );
        assert_eq!(
            std::fs::read_to_string(home.join(".zshrc")).unwrap(),
            "export BAR=2\n"
        );
        assert!(!home
            .join(".config")
            .join("fish")
            .join("conf.d")
            .join("tinkr.fish")
            .exists());
        assert!(store.get("TINKR_ACTIVE").unwrap().is_none());
    }
}
