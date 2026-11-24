use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlapLayer {
    Read,
    Send,
    Say,
    Memory,
    Routing,
    Other,
}

impl OverlapLayer {
    pub fn label(self) -> &'static str {
        match self {
            OverlapLayer::Read => "Shell output (read-less)",
            OverlapLayer::Send => "API traffic (send-less)",
            OverlapLayer::Say => "Short replies (say-less)",
            OverlapLayer::Memory => "Cross-agent memory",
            OverlapLayer::Routing => "Model/effort routing",
            OverlapLayer::Other => "Other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictVerdict {
    NoConflict,
    Redundant,
    Harmful,
    SafeToCombineWithRules,
}

impl ConflictVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            ConflictVerdict::NoConflict => "no conflict",
            ConflictVerdict::Redundant => "redundant",
            ConflictVerdict::Harmful => "harmful",
            ConflictVerdict::SafeToCombineWithRules => "safe to combine with rules",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoexistSupport {
    Yes,
    No,
    Conditional,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PackageSpec {
    #[serde(default)]
    pub winget: Vec<String>,
    #[serde(default)]
    pub brew: Vec<String>,
    #[serde(default)]
    pub cargo: Vec<String>,
    #[serde(default)]
    pub pip: Vec<String>,
    #[serde(default)]
    pub npm: Vec<String>,
    #[serde(default)]
    pub scoop: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OverlapDetect {
    #[serde(default)]
    pub executables: Vec<String>,
    #[serde(default)]
    pub verify_args: Vec<String>,
    #[serde(default)]
    pub packages: PackageSpec,
    #[serde(default)]
    pub processes: Vec<String>,
    #[serde(default)]
    pub ports: Vec<u16>,
    #[serde(default)]
    pub services: Vec<String>,
    #[serde(default)]
    pub agent_hooks: Vec<String>,
    #[serde(default)]
    pub env_vars: Vec<String>,
    #[serde(default)]
    pub mcp_entries: Vec<String>,
    #[serde(default)]
    pub skills_or_plugins: Vec<String>,
    #[serde(default)]
    pub rules_markers: Vec<String>,
    #[serde(default)]
    pub state_dirs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourcedClaim {
    pub text: String,
    pub source_url: String,
    pub date: String,
    #[serde(default)]
    pub unverified: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Capabilities {
    #[serde(default)]
    pub commands_covered: Vec<String>,
    #[serde(default)]
    pub agents_supported: Vec<String>,
    #[serde(default)]
    pub features: Vec<SourcedClaim>,
    #[serde(default)]
    pub does_better_than_tinkr: Vec<SourcedClaim>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictRule {
    pub layer: OverlapLayer,
    pub verdict: ConflictVerdict,
    pub summary: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Compat {
    #[serde(default)]
    pub can_coexist_with_tinkr: BTreeMap<OverlapLayer, CoexistSupport>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub output_markers: Vec<String>,
    #[serde(default)]
    pub off_switch_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlapProfile {
    pub id: String,
    pub name: String,
    pub homepage: String,
    pub license_note: String,
    pub doc_version_checked: String,
    pub doc_date_checked: String,
    #[serde(default)]
    pub layers: Vec<OverlapLayer>,
    #[serde(default)]
    pub detect: OverlapDetect,
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default)]
    pub conflicts: Vec<ConflictRule>,
    #[serde(default)]
    pub compat: Compat,
}

pub const COMPANION_MCP_NAMES: &[&str] = &["serena", "context7", "memory", "filesystem", "github"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationIssue {
    Error(String),
    Warning(String),
}

fn parse_iso_date(s: &str) -> Option<(i32, u32, u32)> {
    let mut parts = s.split('-');
    let y: i32 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some((y, m, d))
}

fn approx_days(y: i32, m: u32, d: u32) -> i64 {
    (y as i64) * 365 + (m as i64) * 30 + (d as i64)
}

pub fn validate_profile(p: &OverlapProfile, today_iso: &str) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    if p.id.trim().is_empty() || p.name.trim().is_empty() {
        issues.push(ValidationIssue::Error("missing id or name".into()));
    }
    if !p.homepage.starts_with("https://") {
        issues.push(ValidationIssue::Error(format!(
            "{}: homepage must be an https:// URL",
            p.id
        )));
    }
    if p.layers.is_empty() {
        issues.push(ValidationIssue::Error(format!(
            "{}: must declare at least one layer",
            p.id
        )));
    }
    if p.capabilities.does_better_than_tinkr.is_empty() {
        issues.push(ValidationIssue::Error(format!(
            "{}: missing honest 'does_better_than_tinkr' entries",
            p.id
        )));
    }
    let check_claim = |kind: &str, c: &SourcedClaim, issues: &mut Vec<ValidationIssue>| {
        if c.unverified {
            return;
        }
        if !c.source_url.starts_with("https://") {
            issues.push(ValidationIssue::Error(format!(
                "{}: {kind} claim '{}' missing https:// source_url",
                p.id, c.text
            )));
        }
        if parse_iso_date(&c.date).is_none() {
            issues.push(ValidationIssue::Error(format!(
                "{}: {kind} claim '{}' has invalid date '{}'",
                p.id, c.text, c.date
            )));
        }
    };
    for c in &p.capabilities.features {
        check_claim("feature", c, &mut issues);
    }
    for c in &p.capabilities.does_better_than_tinkr {
        check_claim("does_better_than_tinkr", c, &mut issues);
    }
    match (
        parse_iso_date(&p.doc_date_checked),
        parse_iso_date(today_iso),
    ) {
        (Some((y1, m1, d1)), Some((y2, m2, d2))) => {
            let age = approx_days(y2, m2, d2) - approx_days(y1, m1, d1);
            if age > 90 {
                issues.push(ValidationIssue::Warning(format!(
                    "{}: profile doc_date_checked ({}) is >90 days old",
                    p.id, p.doc_date_checked
                )));
            }
        }
        (None, _) => {
            issues.push(ValidationIssue::Error(format!(
                "{}: invalid doc_date_checked '{}'",
                p.id, p.doc_date_checked
            )));
        }
        _ => {}
    }
    issues
}
