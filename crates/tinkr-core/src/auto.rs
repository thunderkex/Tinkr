use crate::{
    ledger::{Ledger, ProfileNetStatus},
    platform::fsx,
    Result,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

pub const BACKOFF_RECALL_THRESHOLD: u64 = 3;
pub const MIN_NET_DISABLE_SAMPLES: u64 = 20;

pub fn should_backoff(ledger: &Ledger, tool: &str) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let since = now - 86_400;
    if ledger.recall_count(tool, since).unwrap_or(0) >= BACKOFF_RECALL_THRESHOLD {
        return true;
    }
    is_net_negative_disabled(ledger, tool)
}

pub fn is_net_negative_disabled(ledger: &Ledger, tool: &str) -> bool {
    ledger
        .negative_net_profiles(MIN_NET_DISABLE_SAMPLES)
        .map(|v| v.iter().any(|p| p.tool == tool))
        .unwrap_or(false)
}

pub fn sync_net_disables(
    ledger: &Ledger,
    health: &mut HealthState,
    health_path: &Path,
) -> Vec<ProfileNetStatus> {
    let bad = ledger
        .negative_net_profiles(MIN_NET_DISABLE_SAMPLES)
        .unwrap_or_default();
    let mut changed = false;
    for p in &bad {
        let msg = format!("auto-disabled: {}", p.reason);
        if health.disabled.get(&p.key) != Some(&msg) {
            health.disabled.insert(p.key.clone(), msg);
            changed = true;
        }
    }
    if changed {
        let _ = health.save(health_path);
    }
    bad
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct HealthState {
    pub disabled: BTreeMap<String, String>,
}

impl HealthState {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        fsx::atomic_write(path, &serde_json::to_vec_pretty(self)?)
    }
    pub fn trip(&mut self, key: &str, reason: &str, path: &Path) {
        self.disabled.insert(key.to_string(), reason.to_string());
        let _ = self.save(path);
    }
    pub fn is_disabled(&self, key: &str) -> bool {
        self.disabled.contains_key(key)
    }
    pub fn reset_all(&mut self, path: &Path) -> Result<()> {
        self.disabled.clear();
        self.save(path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    Winget,
    Homebrew,
    Cargo,
    Script,
}

pub fn detect_install_method(exe: &Path) -> InstallMethod {
    let s = exe.to_string_lossy().to_ascii_lowercase();
    if s.contains("winget") || s.contains("microsoft\\winget") {
        InstallMethod::Winget
    } else if s.contains("homebrew") || s.contains("/cellar/") || s.contains("/linuxbrew/") {
        InstallMethod::Homebrew
    } else if s.contains(".cargo") {
        InstallMethod::Cargo
    } else {
        InstallMethod::Script
    }
}

pub fn upgrade_hint(exe: &Path) -> &'static str {
    match detect_install_method(exe) {
        InstallMethod::Winget => "winget upgrade thunderkex.Tinkr",
        InstallMethod::Homebrew => "brew upgrade tinkr",
        InstallMethod::Cargo => "cargo install tinkr",
        InstallMethod::Script => {
            if cfg!(windows) {
                "Re-run scripts/install.ps1 (checksum-verified)"
            } else {
                "Re-run scripts/install.sh (checksum-verified)"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ledger::{CountKind, Event, Layer},
        provider::Provider,
    };

    #[test]
    fn auto_learn_backs_off_after_three_recalls() {
        let l = Ledger::open_in_memory().unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!(!should_backoff(&l, "git-log"));
        for i in 0..10 {
            l.record(&Event {
                ts: now - i,
                layer: Layer::ReadLess,
                provider: Provider::None,
                tool: "git-log".into(),
                session: None,
                before: 500,
                after: 100,
                kind: CountKind::Measured,
                recall_id: Some(format!("abcde{i}")),
            })
            .unwrap();
        }
        assert!(!should_backoff(&l, "git-log"));
        for i in 0..3 {
            l.record_recall(&format!("abcde{i}"), now - i).unwrap();
        }
        assert!(should_backoff(&l, "git-log"));
        assert!(!should_backoff(&l, "ls"));
    }

    #[test]
    fn self_heal_circuit_breaker_and_upgrade_hint() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("health.json");
        let mut h = HealthState::load(&p);
        assert!(!h.is_disabled("read_less:git"));
        h.trip("read_less:git", "timeout >25ms", &p);
        assert!(HealthState::load(&p).is_disabled("read_less:git"));
        h.reset_all(&p).unwrap();
        assert!(!HealthState::load(&p).is_disabled("read_less:git"));

        assert_eq!(
            detect_install_method(Path::new("/opt/homebrew/Cellar/tinkr/0.1.0/bin/tinkr")),
            InstallMethod::Homebrew
        );
        assert_eq!(
            upgrade_hint(Path::new(
                "C:\\Users\\u\\AppData\\Local\\Microsoft\\WinGet\\Packages\\tinkr.exe"
            )),
            "winget upgrade thunderkex.Tinkr"
        );
    }
}
