use super::{
    analyze::{OverlapAnalysis, ToolState, UserChoice},
    schema::OverlapLayer,
};
use crate::{
    manifest::ChangeSet,
    platform::fsx,
    profile::{self, Action, ActionResult, Machine, Profile},
    Result,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChoiceRecord {
    pub choice: UserChoice,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub snapshot_agents: Vec<String>,
    #[serde(default)]
    pub snapshot_layers: Vec<OverlapLayer>,
    #[serde(default)]
    pub repair_count: u32,
    #[serde(default)]
    pub disabled_files: Vec<std::path::PathBuf>,
    #[serde(default)]
    pub shadow_started_ts: Option<i64>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct OverlapState {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub choices: BTreeMap<String, ChoiceRecord>,
}

impl OverlapState {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Self {
                version: 1,
                choices: BTreeMap::new(),
            })
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let mut s = self.clone();
        if s.version == 0 {
            s.version = 1;
        }
        fsx::atomic_write(path, &serde_json::to_vec_pretty(&s)?)
    }

    pub fn key(tool_id: &str, agent: Option<&str>) -> String {
        match agent {
            Some(a) => format!("{tool_id}@{a}"),
            None => tool_id.to_string(),
        }
    }

    pub fn set_choice(
        &mut self,
        tool_id: &str,
        agent: Option<&str>,
        choice: UserChoice,
        snapshot_agents: Vec<String>,
        snapshot_layers: Vec<OverlapLayer>,
    ) {
        let k = Self::key(tool_id, agent);
        let shadow_started_ts = if choice == UserChoice::Compare {
            Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64,
            )
        } else {
            None
        };
        self.choices.insert(
            k,
            ChoiceRecord {
                choice,
                agent: agent.map(str::to_string),
                snapshot_agents,
                snapshot_layers,
                repair_count: 0,
                disabled_files: Vec::new(),
                shadow_started_ts,
            },
        );
    }

    pub fn effective_choice(&self, tool_id: &str, agent: &str) -> UserChoice {
        if let Some(r) = self.choices.get(&Self::key(tool_id, Some(agent))) {
            return r.choice;
        }
        if let Some(r) = self.choices.get(tool_id) {
            return r.choice;
        }
        UserChoice::Keep
    }

    pub fn global_choices(&self) -> BTreeMap<String, UserChoice> {
        self.choices
            .iter()
            .filter(|(_, r)| r.agent.is_none())
            .map(|(k, r)| (k.clone(), r.choice))
            .collect()
    }

    pub fn should_yield(
        &self,
        analysis: &OverlapAnalysis,
        agent: &str,
        layer: OverlapLayer,
    ) -> bool {
        for c in &analysis.conflicts {
            let agent_matches = c.agent == agent || c.agent == "global";
            if !agent_matches || c.layer != layer {
                continue;
            }
            let eff = self.effective_choice(&c.tool_id, agent);
            match eff {
                UserChoice::Keep | UserChoice::Compare => return true,
                UserChoice::Tinkr => continue,
                UserChoice::Both => {
                    if matches!(layer, OverlapLayer::Say | OverlapLayer::Send) {
                        return true;
                    }
                }
            }
        }
        false
    }

    pub fn needs_reprompt(&self, analysis: &OverlapAnalysis) -> bool {
        for t in &analysis.tools {
            if !matches!(
                t.state,
                ToolState::Active | ToolState::PartialDrifted | ToolState::TinkrManaged
            ) {
                continue;
            }
            match self.choices.get(&t.id) {
                None => return true,
                Some(rec) => {
                    if rec.snapshot_agents != t.wired_agents
                        || rec.snapshot_layers != t.active_layers
                    {
                        return true;
                    }
                }
            }
        }
        false
    }
}

pub fn action_layer(a: &Action) -> OverlapLayer {
    match a {
        Action::JsonHook { .. } => OverlapLayer::Read,
        Action::McpServer { .. } => OverlapLayer::Send,
        Action::RulesBlock { .. } => OverlapLayer::Say,
    }
}

pub fn apply_with_yield(
    p: &Profile,
    m: &Machine,
    cs: &mut ChangeSet,
    analysis: &OverlapAnalysis,
    state: &OverlapState,
) -> Result<Vec<ActionResult>> {
    let mut out = Vec::new();
    for a in &p.actions {
        let layer = action_layer(a);
        if state.should_yield(analysis, &p.id, layer) {
            let dir = m.config_dir(p).unwrap_or_else(|| m.home.clone());
            let file = match a {
                Action::JsonHook { file, .. }
                | Action::McpServer { file, .. }
                | Action::RulesBlock { file, .. } => dir.join(file),
            };
            out.push(ActionResult::Skipped {
                file,
                reason: format!(
                    "auto-yielded {} layer to existing tool (run `tinkr overlap` to change)",
                    layer.label()
                ),
            });
            continue;
        }
        match profile::compute(p, a, m) {
            ActionResult::Change(pl) => {
                if let Action::RulesBlock { comment, .. } = a {
                    cs.insert_block(&pl.file, comment, crate::say_less::TERSE_RULE)?;
                } else {
                    cs.edit_file(&pl.file, &pl.bytes)?;
                }
                out.push(ActionResult::Change(pl));
            }
            other => out.push(other),
        }
    }
    Ok(out)
}
