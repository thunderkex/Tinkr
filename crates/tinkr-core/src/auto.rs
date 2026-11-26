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
pub const LOOSEN_RECALL_RATE_PCT: f64 = 15.0;
pub const LOOSEN_MIN_SAMPLES: u64 = 3;
pub const TIGHTEN_RECALL_RATE_PCT: f64 = 2.0;
pub const TIGHTEN_MIN_SAMPLES: u64 = 50;
pub const MAX_TUNING_PROFILES: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProfileTuning {
    pub profile_id: String,
    pub level: u8,
    pub events: u64,
    pub recalls: u64,
    pub recall_rate_pct: f64,
    pub net_saved: i64,
    pub reason: String,
    pub updated_ts: i64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct TuningState {
    pub profiles: BTreeMap<String, ProfileTuning>,
}

impl TuningState {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        fsx::atomic_write(path, &serde_json::to_vec_pretty(self)?)
    }

    pub fn level_for(&self, profile_id: &str) -> u8 {
        self.profiles
            .get(profile_id)
            .map(|p| p.level)
            .unwrap_or(2)
    }

    fn enforce_bound(&mut self) {
        while self.profiles.len() > MAX_TUNING_PROFILES {
            let Some(oldest) = self
                .profiles
                .iter()
                .min_by_key(|(_, v)| (v.updated_ts, v.events))
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.profiles.remove(&oldest);
        }
    }
}

pub fn compute_profile_tuning(
    profile_id: &str,
    events: u64,
    recalls: u64,
    recall_rate_pct: f64,
    net_saved: i64,
    disabled_reason: Option<&str>,
    now_ts: i64,
) -> ProfileTuning {
    if let Some(d_reason) = disabled_reason {
        return ProfileTuning {
            profile_id: profile_id.to_string(),
            level: 0,
            events,
            recalls,
            recall_rate_pct,
            net_saved,
            reason: format!("disabled (level 0): {d_reason}"),
            updated_ts: now_ts,
        };
    }
    if events >= MIN_NET_DISABLE_SAMPLES && net_saved <= 0 {
        return ProfileTuning {
            profile_id: profile_id.to_string(),
            level: 0,
            events,
            recalls,
            recall_rate_pct,
            net_saved,
            reason: format!(
                "auto-disabled (level 0): NET {net_saved:+} tok <= 0 across {events} samples"
            ),
            updated_ts: now_ts,
        };
    }
    if events >= LOOSEN_MIN_SAMPLES && recall_rate_pct > LOOSEN_RECALL_RATE_PCT {
        return ProfileTuning {
            profile_id: profile_id.to_string(),
            level: 1,
            events,
            recalls,
            recall_rate_pct,
            net_saved,
            reason: format!(
                "loosened to level 1 (gentle dedup only): recall rate {recall_rate_pct:.1}% > {LOOSEN_RECALL_RATE_PCT:.1}% across {events} samples"
            ),
            updated_ts: now_ts,
        };
    }
    if events >= TIGHTEN_MIN_SAMPLES && recall_rate_pct < TIGHTEN_RECALL_RATE_PCT && net_saved > 0 {
        return ProfileTuning {
            profile_id: profile_id.to_string(),
            level: 3,
            events,
            recalls,
            recall_rate_pct,
            net_saved,
            reason: format!(
                "tightened to level 3: recall rate {recall_rate_pct:.1}% < {TIGHTEN_RECALL_RATE_PCT:.1}% across {events} samples (NET {net_saved:+} tok)"
            ),
            updated_ts: now_ts,
        };
    }
    let remaining = TIGHTEN_MIN_SAMPLES.saturating_sub(events);
    let reason = if events == 0 {
        "standard level 2 (default): no samples recorded yet".to_string()
    } else if remaining > 0 {
        format!(
            "standard level 2 (default): recall rate {recall_rate_pct:.1}% <= {LOOSEN_RECALL_RATE_PCT:.1}% ({events}/{TIGHTEN_MIN_SAMPLES} samples toward tightening)"
        )
    } else {
        format!(
            "standard level 2: recall rate {recall_rate_pct:.1}% is between {TIGHTEN_RECALL_RATE_PCT:.1}% and {LOOSEN_RECALL_RATE_PCT:.1}% across {events} samples"
        )
    };
    ProfileTuning {
        profile_id: profile_id.to_string(),
        level: 2,
        events,
        recalls,
        recall_rate_pct,
        net_saved,
        reason,
        updated_ts: now_ts,
    }
}

pub fn sync_tuning(
    ledger: &Ledger,
    health: &HealthState,
    tuning_path: Option<&Path>,
) -> TuningState {
    let mut state = tuning_path.map(TuningState::load).unwrap_or_default();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    if let Ok(summary) = ledger.summary(None) {
        for (tool, ls) in &summary.by_tool {
            let dis = health
                .disabled
                .get(&format!("read_less:{tool}"))
                .or_else(|| health.disabled.get(&format!("send_less:{tool}")))
                .or_else(|| health.disabled.get(&format!("say_less:{tool}")))
                .map(String::as_str);
            let pt = compute_profile_tuning(
                tool,
                ls.events,
                ls.recalls,
                ls.recall_rate_pct,
                ls.net_saved,
                dis,
                now,
            );
            state.profiles.insert(tool.clone(), pt);
        }
    }
    state.enforce_bound();
    if let Some(p) = tuning_path {
        let _ = state.save(p);
    }
    state
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhyReport {
    pub query: String,
    pub profile_id: String,
    pub matched_catalog_rule: Option<String>,
    pub strategy: Option<String>,
    pub read_only: Option<bool>,
    pub level: u8,
    pub level_label: String,
    pub events: u64,
    pub before_tokens: u64,
    pub after_tokens: u64,
    pub net_saved: i64,
    pub recalls: u64,
    pub recall_rate_pct: f64,
    pub reason: String,
}

pub fn explain_command(
    query: &str,
    ledger: &Ledger,
    health: &HealthState,
) -> WhyReport {
    let trimmed = query.trim();
    let summary = ledger.summary(None).unwrap_or_default();
    let cat_rule = crate::filters::classify::match_catalog(trimmed).or_else(|| {
        crate::filters::catalog::rules()
            .into_iter()
            .find(|r| r.raw.id == trimmed || r.raw.executables.iter().any(|e| e == trimmed))
    });

    let profile_id = if summary.by_tool.contains_key(trimmed) {
        trimmed.to_string()
    } else if let Some(r) = cat_rule {
        r.raw.id.clone()
    } else {
        trimmed.to_string()
    };

    let ls = summary.by_tool.get(&profile_id).cloned().unwrap_or_default();
    let dis = health
        .disabled
        .get(&format!("read_less:{profile_id}"))
        .or_else(|| health.disabled.get(&format!("send_less:{profile_id}")))
        .or_else(|| health.disabled.get(&format!("say_less:{profile_id}")))
        .map(String::as_str);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let tuning = compute_profile_tuning(
        &profile_id,
        ls.events,
        ls.recalls,
        ls.recall_rate_pct,
        ls.net_saved,
        dis,
        now,
    );
    let level_label = match tuning.level {
        0 => "disabled (passthrough)",
        1 => "loosened (gentle run-length dedup only)",
        2 => "standard (catalog filter + outlier needles)",
        _ => "tightened (zero-context diffs & strict rarity)",
    }
    .to_string();

    WhyReport {
        query: trimmed.to_string(),
        profile_id,
        matched_catalog_rule: cat_rule.map(|r| r.raw.id.clone()),
        strategy: cat_rule.map(|r| format!("{:?}", r.raw.strategy)),
        read_only: cat_rule.map(|r| r.raw.read_only),
        level: tuning.level,
        level_label,
        events: ls.events,
        before_tokens: ls.before,
        after_tokens: ls.after,
        net_saved: ls.net_saved,
        recalls: ls.recalls,
        recall_rate_pct: ls.recall_rate_pct,
        reason: tuning.reason,
    }
}

pub fn format_why(w: &WhyReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("Explanation for `{}`:\n", w.query));
    out.push_str(&format!("  profile:     {}\n", w.profile_id));
    if let (Some(strat), Some(ro)) = (&w.strategy, w.read_only) {
        out.push_str(&format!("  catalog:     strategy={strat}, read_only={ro}\n"));
    } else {
        out.push_str("  catalog:     (non-shell or unmatched command -> passthrough unless MCP/proxy)\n");
    }
    out.push_str(&format!("  level:       {} — {}\n", w.level, w.level_label));
    out.push_str(&format!(
        "  samples:     {} ({} -> {} tok, NET {:+} tok)\n",
        w.events, w.before_tokens, w.after_tokens, w.net_saved
    ));
    out.push_str(&format!(
        "  recalls:     {} within 3 turns ({:.1}%)\n",
        w.recalls, w.recall_rate_pct
    ));
    out.push_str(&format!("  why:         {}\n", w.reason));
    out
}

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
