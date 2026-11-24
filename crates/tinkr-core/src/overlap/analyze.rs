use super::{
    detect::{DetectedTool, EvidenceKind, ScanReport},
    schema::{ConflictVerdict, OverlapLayer, OverlapProfile},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolState {
    NotFound,
    InstalledInactive,
    Active,
    PartialDrifted,
    TinkrManaged,
}

impl ToolState {
    pub fn label(self) -> &'static str {
        match self {
            ToolState::NotFound => "not found",
            ToolState::InstalledInactive => "installed (inactive)",
            ToolState::Active => "active",
            ToolState::PartialDrifted => "partial/drifted",
            ToolState::TinkrManaged => "managed by choice",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserChoice {
    Keep,
    Tinkr,
    Both,
    Compare,
}

impl UserChoice {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "keep" | "1" => Some(UserChoice::Keep),
            "tinkr" | "2" => Some(UserChoice::Tinkr),
            "both" | "3" => Some(UserChoice::Both),
            "compare" | "4" => Some(UserChoice::Compare),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            UserChoice::Keep => "keep",
            UserChoice::Tinkr => "tinkr",
            UserChoice::Both => "both",
            UserChoice::Compare => "compare",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolAssessment {
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub state: ToolState,
    pub is_unknown_heuristic: bool,
    pub wired_agents: Vec<String>,
    pub active_layers: Vec<OverlapLayer>,
    pub current_choice: Option<UserChoice>,
    pub summary_does: String,
    pub does_better_than_tinkr: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictFinding {
    pub tool_id: String,
    pub tool_name: String,
    pub agent: String,
    pub layer: OverlapLayer,
    pub verdict: ConflictVerdict,
    pub explanation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlapAnalysis {
    pub os: String,
    pub remote_context: Vec<String>,
    pub tools: Vec<ToolAssessment>,
    pub conflicts: Vec<ConflictFinding>,
    pub companions: Vec<String>,
    pub uncovered_agents: Vec<String>,
    pub has_active_overlap: bool,
}

fn is_active_evidence(k: EvidenceKind) -> bool {
    matches!(
        k,
        EvidenceKind::Process
            | EvidenceKind::Port
            | EvidenceKind::AgentHook
            | EvidenceKind::EnvVar
            | EvidenceKind::McpEntry
            | EvidenceKind::SkillOrPlugin
            | EvidenceKind::RulesMarker
    )
}

fn classify_tool(
    det: &DetectedTool,
    detected_agents: &[String],
    saved_choice: Option<UserChoice>,
) -> ToolState {
    if saved_choice.is_some() {
        return ToolState::TinkrManaged;
    }
    let has_active = det.evidence.iter().any(|e| is_active_evidence(e.kind));
    if !has_active {
        return ToolState::InstalledInactive;
    }
    if !det.wired_agents.is_empty()
        && detected_agents.len() > 1
        && det.wired_agents.len() < detected_agents.len()
    {
        return ToolState::PartialDrifted;
    }
    ToolState::Active
}

fn default_verdict(layer: OverlapLayer) -> (ConflictVerdict, &'static str) {
    match layer {
        OverlapLayer::Read => (
            ConflictVerdict::Redundant,
            "running two shell-output rewriters on the same agent is redundant and risks double-lossy output",
        ),
        OverlapLayer::Send => (
            ConflictVerdict::Harmful,
            "two loopback proxies competing for the same base URL can break streaming or prompt caching",
        ),
        OverlapLayer::Say => (
            ConflictVerdict::Redundant,
            "two terse-output style rules in the system prompt waste input tokens and can conflict",
        ),
        OverlapLayer::Memory | OverlapLayer::Routing | OverlapLayer::Other => (
            ConflictVerdict::NoConflict,
            "Tinkr does not modify this layer; no conflict",
        ),
    }
}

pub fn analyze(
    scan: &ScanReport,
    profiles: &[OverlapProfile],
    detected_agents: &[String],
    choices: &BTreeMap<String, UserChoice>,
) -> OverlapAnalysis {
    let mut tools = Vec::new();
    let mut conflicts = Vec::new();
    let mut covered_agents: BTreeSet<String> = BTreeSet::new();

    for det in &scan.detected {
        let profile = profiles.iter().find(|p| p.id == det.id);
        let choice = choices.get(&det.id).copied();
        let state = classify_tool(det, detected_agents, choice);

        let does = if det.is_unknown_heuristic {
            "unknown local token/context optimizer".to_string()
        } else if let Some(p) = profile {
            p.layers
                .iter()
                .map(|l| l.label())
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            "token optimization".to_string()
        };

        let better: Vec<String> = profile
            .map(|p| {
                p.capabilities
                    .does_better_than_tinkr
                    .iter()
                    .filter(|c| !c.unverified)
                    .map(|c| format!("{} (source: {}, {})", c.text, c.source_url, c.date))
                    .collect()
            })
            .unwrap_or_default();

        for a in &det.wired_agents {
            covered_agents.insert(a.clone());
        }

        if matches!(
            state,
            ToolState::Active | ToolState::PartialDrifted | ToolState::TinkrManaged
        ) {
            let agents: Vec<String> = if det.wired_agents.is_empty() {
                vec!["global".into()]
            } else {
                det.wired_agents.iter().cloned().collect()
            };
            for agent in &agents {
                for &layer in &det.active_layers {
                    let (verdict, explanation) = if let Some(rule) =
                        profile.and_then(|p| p.conflicts.iter().find(|r| r.layer == layer))
                    {
                        (rule.verdict, rule.summary.clone())
                    } else {
                        let (v, s) = default_verdict(layer);
                        (v, s.to_string())
                    };
                    if verdict != ConflictVerdict::NoConflict {
                        conflicts.push(ConflictFinding {
                            tool_id: det.id.clone(),
                            tool_name: det.name.clone(),
                            agent: agent.clone(),
                            layer,
                            verdict,
                            explanation,
                        });
                    }
                }
            }
        }

        tools.push(ToolAssessment {
            id: det.id.clone(),
            name: det.name.clone(),
            version: det.version.clone(),
            state,
            is_unknown_heuristic: det.is_unknown_heuristic,
            wired_agents: det.wired_agents.iter().cloned().collect(),
            active_layers: det.active_layers.iter().copied().collect(),
            current_choice: choice,
            summary_does: does,
            does_better_than_tinkr: better,
        });
    }

    let uncovered_agents: Vec<String> = detected_agents
        .iter()
        .filter(|a| !covered_agents.contains(*a))
        .cloned()
        .collect();
    let has_active_overlap = tools.iter().any(|t| {
        matches!(
            t.state,
            ToolState::Active | ToolState::PartialDrifted | ToolState::TinkrManaged
        )
    });

    OverlapAnalysis {
        os: scan.os.clone(),
        remote_context: scan.remote_context.clone(),
        tools,
        conflicts,
        companions: scan
            .companions
            .iter()
            .map(|c| format!("{} (in {})", c.name, c.agent))
            .collect(),
        uncovered_agents,
        has_active_overlap,
    }
}

pub fn format_plain(a: &OverlapAnalysis, include_choices: bool) -> String {
    let mut out = String::new();
    if a.tools.is_empty() {
        out.push_str("No other token-saving or context-compression tools detected.\n");
        if !a.companions.is_empty() {
            out.push_str(&format!(
                "Companion MCP servers (no conflict): {}\n",
                a.companions.join(", ")
            ));
        }
        return out;
    }

    let active: Vec<&ToolAssessment> = a
        .tools
        .iter()
        .filter(|t| t.state != ToolState::InstalledInactive)
        .collect();
    let inactive: Vec<&ToolAssessment> = a
        .tools
        .iter()
        .filter(|t| t.state == ToolState::InstalledInactive)
        .collect();

    if !active.is_empty() {
        out.push_str("Tinkr found token-saving tools already active on this machine:\n\n");
        for t in &active {
            let ver = t
                .version
                .as_deref()
                .map(|v| format!(" ({v})"))
                .unwrap_or_default();
            let where_s = if t.wired_agents.is_empty() {
                "global".to_string()
            } else {
                t.wired_agents.join(", ")
            };
            let choice_s = t
                .current_choice
                .map(|c| format!(" [choice: {}]", c.as_str()))
                .unwrap_or_default();
            out.push_str(&format!(
                "  {}{}  state: {}  in: {}  does: {}{}\n",
                t.name,
                ver,
                t.state.label(),
                where_s,
                t.summary_does,
                choice_s
            ));
        }
    }

    if !inactive.is_empty() {
        out.push_str("\nInstalled but inactive (no conflict):\n");
        for t in &inactive {
            out.push_str(&format!("  {}  ({})\n", t.name, t.summary_does));
        }
    }

    if !a.conflicts.is_empty() {
        out.push_str("\nWhere they overlap with Tinkr:\n");
        for c in &a.conflicts {
            out.push_str(&format!(
                "  {:<24} {} in {} -> {} ({})\n",
                c.layer.label(),
                c.tool_name,
                c.agent,
                c.verdict.as_str(),
                c.explanation
            ));
        }
    }

    if !a.uncovered_agents.is_empty() {
        out.push_str(&format!(
            "\nWhat Tinkr can add without overlapping:\n  Agents not covered by the tools above: {}\n",
            a.uncovered_agents.join(", ")
        ));
    }

    let better_lines: Vec<String> = active
        .iter()
        .flat_map(|t| {
            t.does_better_than_tinkr
                .iter()
                .map(move |b| format!("  - {}: {b}", t.name))
        })
        .collect();
    if !better_lines.is_empty() {
        out.push_str("\nWhat your current tools do that Tinkr does NOT:\n");
        for l in better_lines {
            out.push_str(&l);
            out.push('\n');
        }
    }

    if !a.companions.is_empty() {
        out.push_str(&format!(
            "\nCompanion MCP servers (informational, no conflict): {}\n",
            a.companions.join(", ")
        ));
    }

    if include_choices && !active.is_empty() {
        out.push_str(
            "\nChoose (you can change this any time with `tinkr overlap choose <tool> keep|tinkr|both|compare`):\n\
             \x20 [1] Keep my current tools (default). Tinkr turns off only its overlapping parts.\n\
             \x20 [2] Use Tinkr instead. Tinkr turns off the other tool's hooks in the agents it manages (reversible; nothing is uninstalled).\n\
             \x20 [3] Use both where it is safe. Tinkr covers only what the other tool does not.\n\
             \x20 [4] Compare first. Run Tinkr in shadow mode for 7 days and show real numbers.\n",
        );
    }

    out
}
