use super::{
    analyze::UserChoice,
    schema::OverlapProfile,
    state::{ChoiceRecord, OverlapState},
};
use crate::{edit::jsonc, manifest::ChangeSet, platform::fsx, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const MAX_REPAIRS: u32 = 2;

#[derive(Debug, Clone)]
pub struct PlannedFileEdit {
    pub agent: String,
    pub file: PathBuf,
    pub before: String,
    pub after: String,
    pub diff_preview: String,
}

#[derive(Debug, Clone)]
pub struct PlannedSwitch {
    pub tool_id: String,
    pub edits: Vec<PlannedFileEdit>,
    pub manual_hints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitchOutcome {
    Applied {
        files_modified: Vec<PathBuf>,
        manual_hints: Vec<String>,
    },
    NothingToEdit {
        manual_hints: Vec<String>,
    },
    EditWarStopped {
        tool_id: String,
        repairs: u32,
    },
}

#[derive(Debug, Default, Clone)]
pub struct RestoreReport {
    pub restored: Vec<PathBuf>,
    pub errors: Vec<String>,
}

fn simple_diff(file: &Path, before: &str, after: &str) -> String {
    let mut out = format!("--- {}\n+++ {}\n", file.display(), file.display());
    let b_lines: Vec<&str> = before.lines().collect();
    let a_lines: Vec<&str> = after.lines().collect();
    for l in &b_lines {
        if !a_lines.contains(l) {
            out.push_str(&format!("- {l}\n"));
        }
    }
    for l in &a_lines {
        if !b_lines.contains(l) {
            out.push_str(&format!("+ {l}\n"));
        }
    }
    out
}

fn entry_matches_tool(entry: &Value, patterns: &[String]) -> bool {
    entry["hooks"].as_array().is_some_and(|hs| {
        hs.iter().any(|h| {
            h["command"].as_str().is_some_and(|cmd| {
                let lower = cmd.to_ascii_lowercase();
                patterns
                    .iter()
                    .any(|p| lower.contains(&p.to_ascii_lowercase()))
            })
        })
    })
}

pub fn plan_disable_overlapping(
    profile: &OverlapProfile,
    home: &Path,
    target_agent: Option<&str>,
) -> PlannedSwitch {
    let mut edits = Vec::new();
    let mut manual_hints = Vec::new();

    let hook_configs: &[(&str, &str)] = &[
        ("claude-code", ".claude/settings.json"),
        ("codex", ".codex/hooks.json"),
    ];

    for (agent, rel) in hook_configs {
        if target_agent.is_some_and(|t| t != *agent) {
            continue;
        }
        let path = home.join(rel);
        let Ok(before) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(Some(Value::Array(arr))) = jsonc::get_path(&before, &["hooks", "PreToolUse"]) else {
            continue;
        };
        let filtered: Vec<Value> = arr
            .iter()
            .filter(|e| !entry_matches_tool(e, &profile.detect.agent_hooks))
            .cloned()
            .collect();
        if filtered.len() == arr.len() {
            continue;
        }
        if let Ok(after) =
            jsonc::set_path(&before, &["hooks", "PreToolUse"], &Value::Array(filtered))
        {
            if after != before {
                let diff_preview = simple_diff(&path, &before, &after);
                edits.push(PlannedFileEdit {
                    agent: (*agent).to_string(),
                    file: path,
                    before,
                    after,
                    diff_preview,
                });
            }
        }
    }

    if let Some(hint) = &profile.compat.off_switch_hint {
        manual_hints.push(hint.clone());
    }

    PlannedSwitch {
        tool_id: profile.id.clone(),
        edits,
        manual_hints,
    }
}

pub fn apply_disable_plan(
    plan: &PlannedSwitch,
    cs: &mut ChangeSet,
    state: &mut OverlapState,
    is_repair: bool,
) -> Result<SwitchOutcome> {
    let rec = state
        .choices
        .entry(plan.tool_id.clone())
        .or_insert_with(|| ChoiceRecord {
            choice: UserChoice::Tinkr,
            agent: None,
            snapshot_agents: Vec::new(),
            snapshot_layers: Vec::new(),
            repair_count: 0,
            disabled_files: Vec::new(),
            shadow_started_ts: None,
        });
    if is_repair && rec.repair_count >= MAX_REPAIRS {
        return Ok(SwitchOutcome::EditWarStopped {
            tool_id: plan.tool_id.clone(),
            repairs: rec.repair_count,
        });
    }
    if plan.edits.is_empty() {
        return Ok(SwitchOutcome::NothingToEdit {
            manual_hints: plan.manual_hints.clone(),
        });
    }

    let mut modified = Vec::new();
    for edit in &plan.edits {
        if cs.edit_file(&edit.file, edit.after.as_bytes())? {
            modified.push(edit.file.clone());
            if !rec.disabled_files.contains(&edit.file) {
                rec.disabled_files.push(edit.file.clone());
            }
        }
    }
    rec.choice = UserChoice::Tinkr;
    if is_repair && !modified.is_empty() {
        rec.repair_count += 1;
    }

    Ok(SwitchOutcome::Applied {
        files_modified: modified,
        manual_hints: plan.manual_hints.clone(),
    })
}

pub fn restore_tool(
    tool_id: &str,
    cs: &mut ChangeSet,
    state: &mut OverlapState,
) -> Result<RestoreReport> {
    let mut rep = RestoreReport::default();
    let Some(rec) = state.choices.get_mut(tool_id) else {
        return Ok(rep);
    };
    let targets: Vec<PathBuf> = std::mem::take(&mut rec.disabled_files);
    for path in targets {
        let bak = fsx::backup_path(&path);
        match std::fs::read(&bak) {
            Ok(orig) => {
                fsx::atomic_write(&path, &orig)?;
                let _ = std::fs::remove_file(&bak);
                cs.remove_file_entry(&path)?;
                rep.restored.push(path);
            }
            Err(e) => {
                rep.errors.push(format!("{}: {e}", path.display()));
            }
        }
    }
    rec.choice = UserChoice::Keep;
    rec.repair_count = 0;
    Ok(rep)
}
