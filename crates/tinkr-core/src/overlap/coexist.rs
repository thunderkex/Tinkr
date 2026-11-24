use super::{
    analyze::{OverlapAnalysis, UserChoice},
    schema::{CoexistSupport, OverlapLayer, OverlapProfile},
    state::OverlapState,
};
use crate::filters::classify;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const KNOWN_FOREIGN_MARKERS: &[&str] = &[
    "[full output: rtk recall",
    "[headroom:",
    "headroom_retrieve",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoexistencePlan {
    pub agent: String,
    pub allow_read_less: bool,
    pub skip_commands: BTreeSet<String>,
    pub foreign_markers: Vec<String>,
    pub allow_send_less: bool,
    pub proxy_chain_note: String,
    pub allow_say_less: bool,
    pub say_less_note: String,
}

pub fn build_plan(
    agent: &str,
    analysis: &OverlapAnalysis,
    profiles: &[OverlapProfile],
    state: &OverlapState,
) -> CoexistencePlan {
    let mut plan = CoexistencePlan {
        agent: agent.to_string(),
        allow_read_less: true,
        skip_commands: BTreeSet::new(),
        foreign_markers: KNOWN_FOREIGN_MARKERS
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        allow_send_less: true,
        proxy_chain_note: "no competing proxy active".into(),
        allow_say_less: true,
        say_less_note: "no competing voice rule active".into(),
    };

    for c in &analysis.conflicts {
        if c.agent != agent && c.agent != "global" {
            continue;
        }
        let choice = state.effective_choice(&c.tool_id, agent);
        let profile = profiles.iter().find(|p| p.id == c.tool_id);

        if let Some(p) = profile {
            for m in &p.compat.output_markers {
                if !plan.foreign_markers.contains(m) {
                    plan.foreign_markers.push(m.clone());
                }
            }
        }

        match (c.layer, choice) {
            (OverlapLayer::Read, UserChoice::Keep | UserChoice::Compare) => {
                plan.allow_read_less = false;
            }
            (OverlapLayer::Read, UserChoice::Both) => {
                if let Some(p) = profile {
                    let support = p
                        .compat
                        .can_coexist_with_tinkr
                        .get(&OverlapLayer::Read)
                        .copied()
                        .unwrap_or(CoexistSupport::No);
                    match support {
                        CoexistSupport::Yes | CoexistSupport::Conditional => {
                            for cmd in &p.capabilities.commands_covered {
                                plan.skip_commands.insert(cmd.to_ascii_lowercase());
                            }
                        }
                        CoexistSupport::No => {
                            plan.allow_read_less = false;
                        }
                    }
                } else {
                    plan.allow_read_less = false;
                }
            }
            (OverlapLayer::Send, UserChoice::Keep | UserChoice::Compare) => {
                plan.allow_send_less = false;
                plan.proxy_chain_note = format!(
                    "yielding send-less proxy to {} (choice: {})",
                    c.tool_name,
                    choice.as_str()
                );
            }
            (OverlapLayer::Send, UserChoice::Both) => {
                let support = profile
                    .and_then(|p| p.compat.can_coexist_with_tinkr.get(&OverlapLayer::Send))
                    .copied()
                    .unwrap_or(CoexistSupport::No);
                if support != CoexistSupport::Yes {
                    plan.allow_send_less = false;
                    plan.proxy_chain_note = format!(
                        "proxy chaining with {} is not enabled (single proxy per agent); falling back to Keep for send-less",
                        c.tool_name
                    );
                }
            }
            (OverlapLayer::Say, UserChoice::Keep | UserChoice::Both | UserChoice::Compare) => {
                plan.allow_say_less = false;
                plan.say_less_note = format!(
                    "single voice rule enforced: skipping Tinkr say-less where {} is active",
                    c.tool_name
                );
            }
            (_, UserChoice::Tinkr) => {}
            _ => {}
        }
    }

    plan
}

pub fn should_skip_command(cmd: &str, plan: &CoexistencePlan) -> bool {
    if !plan.allow_read_less {
        return true;
    }
    if let Some(first) = classify::first_word(cmd) {
        if plan.skip_commands.contains(&first) {
            return true;
        }
    }
    false
}

pub fn has_foreign_marker(raw: &[u8], extra_markers: &[String]) -> bool {
    let Ok(s) = std::str::from_utf8(raw) else {
        return false;
    };
    KNOWN_FOREIGN_MARKERS.iter().any(|m| s.contains(m))
        || extra_markers
            .iter()
            .any(|m| !m.is_empty() && s.contains(m.as_str()))
}

pub fn has_foreign_voice_rule(system: &str) -> bool {
    let lower = system.to_ascii_lowercase();
    lower.contains("caveman-speak") || lower.contains("/caveman")
}
