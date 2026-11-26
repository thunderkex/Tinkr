use crate::filters::classify::{self, Kind};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    Antigravity,
}

impl Agent {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(Agent::Claude),
            "codex" => Some(Agent::Codex),
            "antigravity" | "agy" => Some(Agent::Antigravity),
            _ => None,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Permissions {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

fn rule_matches(rule: &str, cmd: &str) -> bool {
    let rule = rule.trim();
    if rule == "Bash" || rule == "Bash(*)" {
        return true;
    }
    let Some(pat) = rule.strip_prefix("Bash(").and_then(|r| r.strip_suffix(')')) else {
        return false;
    };
    let wild = pat
        .strip_suffix(":*")
        .or_else(|| pat.strip_suffix(" *"))
        .or_else(|| pat.strip_suffix('*'));
    match wild {
        Some(prefix) => cmd == prefix.trim_end() || cmd.starts_with(prefix),
        None => cmd == pat,
    }
}

impl Permissions {
    pub fn merge_from(&mut self, settings_text: &str) {
        if let Ok(Some(v)) = crate::edit::jsonc::parse(settings_text) {
            for (key, dst) in [("allow", &mut self.allow), ("deny", &mut self.deny)] {
                if let Some(a) = v["permissions"][key].as_array() {
                    dst.extend(a.iter().filter_map(|x| x.as_str().map(str::to_string)));
                }
            }
        }
    }
    fn allowed(&self, seg: &str) -> bool {
        self.allow.iter().any(|r| rule_matches(r, seg))
    }
    fn denied(&self, seg: &str) -> bool {
        self.deny.iter().any(|r| rule_matches(r, seg))
    }
}

fn shell_quote(shell: &str, s: &str) -> String {
    match shell {
        "pwsh" | "powershell" => format!("'{}'", s.replace('\'', "''")),
        _ => format!("'{}'", s.replace('\'', "'\\''")),
    }
}

pub fn rewrite_command(
    agent: Agent,
    tool: &str,
    cmd: &str,
    exe: &str,
    perms: &Permissions,
) -> Option<String> {
    let shell = match tool {
        "Bash" => "bash",
        "PowerShell" => "pwsh",
        _ => return None,
    };
    let trimmed = cmd.trim();
    if trimmed.is_empty()
        || (trimmed.contains("tinkr") && trimmed.contains(" run "))
        || trimmed.starts_with("rtk ")
    {
        return None;
    }
    classify::classify(trimmed)?;
    let segs = classify::command_segments(trimmed)?;
    for seg in &segs {
        let ro = classify::is_segment_read_only(seg)?;
        if perms.denied(seg) || perms.denied(trimmed) {
            return None;
        }
        let ok = ro
            || agent == Agent::Antigravity
            || (agent == Agent::Claude && (perms.allowed(seg) || perms.allowed(trimmed)));
        if !ok {
            return None;
        }
    }
    let exe = exe.replace('\\', "/");
    let q = |s: &str| shell_quote(shell, s);
    Some(match shell {
        "pwsh" => format!("& {} run --shell pwsh -- {}", q(&exe), q(trimmed)),
        _ => format!("{} run --shell bash -- {}", q(&exe), q(trimmed)),
    })
}

pub fn handle(agent: Agent, input: &str, exe: &str, perms: &Permissions) -> Option<String> {
    let v: Value = serde_json::from_str(input).ok()?;
    if agent == Agent::Antigravity {
        if let Some(ev) = v["hookEventName"].as_str() {
            if ev != "PreToolUse" {
                return None;
            }
        }
        if v["toolCall"]["name"].as_str()? != "run_command" {
            return None;
        }
        let cmd = v["toolCall"]["args"]["CommandLine"].as_str()?;
        let tool = if cfg!(windows) { "PowerShell" } else { "Bash" };
        let new_cmd = rewrite_command(Agent::Antigravity, tool, cmd, exe, perms)?;
        let all_read_only = classify::command_segments(cmd.trim()).is_some_and(|segs| {
            segs.iter()
                .all(|s| classify::is_segment_read_only(s).unwrap_or(false))
        });
        let decision = if all_read_only { "allow" } else { "ask" };
        return Some(
            json!({
                "decision": decision,
                "reason": "tinkr: compact command output",
                "overwrite": {
                    "CommandLine": new_cmd
                }
            })
            .to_string(),
        );
    }
    if let Some(ev) = v["hook_event_name"].as_str() {
        if ev != "PreToolUse" {
            return None;
        }
    }
    let tool = v["tool_name"].as_str()?;
    let cmd = v["tool_input"]["command"].as_str()?;
    let new_cmd = rewrite_command(agent, tool, cmd, exe, perms)?;
    let mut updated = v["tool_input"].clone();
    updated["command"] = Value::String(new_cmd);
    Some(
        json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "permissionDecisionReason": "tinkr: compact command output",
            "updatedInput": updated
        }})
        .to_string(),
    )
}

pub fn kind_of(cmd: &str) -> Kind {
    classify::classify(cmd).unwrap_or(Kind::Generic)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AntigravityStopEvent {
    pub session_key: String,
    pub provider: crate::provider::Provider,
    pub before: u64,
    pub after: u64,
}

fn read_tail_utf8(path: &std::path::Path, max_bytes: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    if len > max_bytes {
        f.seek(SeekFrom::End(-(max_bytes as i64))).ok()?;
    }
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

pub fn extract_antigravity_stop(input: &str) -> Option<AntigravityStopEvent> {
    let v: Value = serde_json::from_str(input).ok()?;
    if v["hookEventName"].as_str() != Some("Stop") {
        return None;
    }
    if let Some(reason) = v["terminationReason"].as_str() {
        if !reason.is_empty() && reason != "NO_TOOL_CALL" {
            return None;
        }
    }
    if let Some(user_input) = v["lastUserInput"].as_str() {
        if crate::say_less::user_needs_full_explanation(user_input) {
            return None;
        }
    }
    let transcript_path = v["transcriptPath"].as_str()?;
    let tail = read_tail_utf8(std::path::Path::new(transcript_path), 512 * 1024)?;
    let conv_id = v["conversationId"].as_str().unwrap_or("unknown");
    let fallback_model = v["modelName"].as_str().unwrap_or("");

    for line in tail.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.starts_with('{') {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if entry["type"].as_str() != Some("PLANNER_RESPONSE") {
            continue;
        }
        let Some(content) = entry["content"].as_str().filter(|s| !s.trim().is_empty()) else {
            continue;
        };
        let after = crate::tokenizer::count(content, crate::tokenizer::TokenizerKind::Exact).tokens;
        let before = crate::say_less::estimate_before_tokens(after)?;
        let model = entry["model"].as_str().unwrap_or(fallback_model);
        let provider = crate::say_less::infer_provider_from_model(model);
        let step_idx = entry["step_index"].as_i64().unwrap_or(-1);
        return Some(AntigravityStopEvent {
            session_key: format!("agy:{conv_id}:{step_idx}"),
            provider,
            before,
            after,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn perms(allow: &[&str], deny: &[&str]) -> Permissions {
        Permissions {
            allow: allow.iter().map(|s| s.to_string()).collect(),
            deny: deny.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn read_only_commands_are_wrapped_without_any_rule() {
        let r = rewrite_command(
            Agent::Claude,
            "Bash",
            "git status",
            "C:\\t\\tinkr.exe",
            &perms(&[], &[]),
        )
        .unwrap();
        assert_eq!(r, "'C:/t/tinkr.exe' run --shell bash -- 'git status'");
        let r = rewrite_command(
            Agent::Claude,
            "PowerShell",
            "Get-ChildItem -Recurse",
            "/bin/tinkr",
            &perms(&[], &[]),
        )
        .unwrap();
        assert_eq!(
            r,
            "& '/bin/tinkr' run --shell pwsh -- 'Get-ChildItem -Recurse'"
        );
    }

    #[test]
    fn never_grants_new_permissions() {
        assert!(
            rewrite_command(Agent::Claude, "Bash", "cargo test", "t", &perms(&[], &[])).is_none()
        );
        assert!(rewrite_command(
            Agent::Claude,
            "Bash",
            "cargo test",
            "t",
            &perms(&["Bash(cargo test:*)"], &[])
        )
        .is_some());
        assert!(rewrite_command(
            Agent::Claude,
            "Bash",
            "cargo test",
            "t",
            &perms(&["Bash(cargo test:*)"], &["Bash(cargo test:*)"])
        )
        .is_none());
        assert!(rewrite_command(
            Agent::Codex,
            "Bash",
            "cargo test",
            "t",
            &perms(&["Bash(cargo test:*)"], &[])
        )
        .is_none());
        assert!(rewrite_command(Agent::Codex, "Bash", "git log", "t", &perms(&[], &[])).is_some());
        assert!(rewrite_command(
            Agent::Claude,
            "Bash",
            "cd x && cargo test",
            "t",
            &perms(&["Bash(cargo test:*)"], &[])
        )
        .is_some());
        assert!(rewrite_command(
            Agent::Claude,
            "Bash",
            "git status && cargo test",
            "t",
            &perms(&[], &[])
        )
        .is_none());
    }

    #[test]
    fn quoting_survives_single_quotes() {
        let r = rewrite_command(
            Agent::Claude,
            "Bash",
            "grep 'a b' file",
            "t",
            &perms(&[], &[]),
        )
        .unwrap();
        assert!(r.ends_with("-- 'grep '\\''a b'\\'' file'"), "{r}");
    }

    #[test]
    fn full_handler_roundtrip_and_failopen() {
        let input = r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git diff","description":"d","timeout":5}}"#;
        let out = handle(Agent::Claude, input, "t", &perms(&[], &[])).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["hookSpecificOutput"]["updatedInput"]["description"], "d");
        assert_eq!(v["hookSpecificOutput"]["updatedInput"]["timeout"], 5);
        assert!(v["hookSpecificOutput"]["updatedInput"]["command"]
            .as_str()
            .unwrap()
            .contains("run --shell bash"));
        for bad in [
            "",
            "not json",
            r#"{"tool_name":"Write","tool_input":{}}"#,
            r#"{"tool_name":"Bash","tool_input":{"command":"git log | head"}}"#,
            r#"{"hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"git status"}}"#,
        ] {
            assert!(
                handle(Agent::Claude, bad, "t", &perms(&[], &[])).is_none(),
                "{bad}"
            );
        }
    }

    #[test]
    fn permissions_from_settings() {
        let mut p = Permissions::default();
        p.merge_from("{ // c\n \"permissions\": { \"allow\": [\"Bash(npm test:*)\"], \"deny\": [\"Bash(rm:*)\"] } }");
        assert_eq!(p.allow, vec!["Bash(npm test:*)"]);
        assert!(
            p.denied("rm -rf x") && p.allowed("npm test --silent") && !p.allowed("npm install")
        );
    }

    #[test]
    fn antigravity_hook_rewrites_and_respects_permissions() {
        let p = Permissions::default();
        let ro = r#"{"hookEventName":"PreToolUse","toolCall":{"id":"1","name":"run_command","args":{"CommandLine":"git status"}}}"#;
        let out = handle(Agent::Antigravity, ro, "C:/t/tinkr.exe", &p).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["decision"], "allow");
        assert!(v["overwrite"]["CommandLine"]
            .as_str()
            .unwrap()
            .contains("run --shell"));

        let test_cmd = r#"{"hookEventName":"PreToolUse","toolCall":{"id":"2","name":"run_command","args":{"CommandLine":"cargo test"}}}"#;
        let out = handle(Agent::Antigravity, test_cmd, "C:/t/tinkr.exe", &p).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["decision"], "ask");

        let rtk = r#"{"hookEventName":"PreToolUse","toolCall":{"id":"3","name":"run_command","args":{"CommandLine":"rtk git status"}}}"#;
        assert!(handle(Agent::Antigravity, rtk, "C:/t/tinkr.exe", &p).is_none());
    }

    #[test]
    fn antigravity_stop_extracts_say_less_and_skips_exceptions() {
        let dir = tempfile::tempdir().unwrap();
        let tp = dir.path().join("transcript.jsonl");
        let reply = "Fixed the recall backoff bug in ledger.rs and wired SayLess and MCP events into the SQLite ledger. All workspace unit and integration tests pass.";
        let line1 = json!({
            "step_index": 10,
            "type": "PLANNER_RESPONSE",
            "model": "gemini-4-neon-high",
            "tool_calls": [{"name": "run_command"}]
        });
        let line2 = json!({
            "step_index": 11,
            "type": "PLANNER_RESPONSE",
            "model": "gemini-4-neon-high",
            "content": reply,
            "output_tokens": 500
        });
        std::fs::write(&tp, format!("{line1}\n{line2}\n")).unwrap();

        let payload = json!({
            "hookEventName": "Stop",
            "conversationId": "conv-123",
            "terminationReason": "NO_TOOL_CALL",
            "lastUserInput": "fix everything",
            "transcriptPath": tp.to_string_lossy()
        })
        .to_string();
        let ev = extract_antigravity_stop(&payload).expect("expected stop event");
        assert_eq!(ev.session_key, "agy:conv-123:11");
        assert_eq!(ev.provider, crate::provider::Provider::Gemini);
        assert!(ev.before > ev.after && ev.after >= 12);

        let detailed = json!({
            "hookEventName": "Stop",
            "conversationId": "conv-123",
            "terminationReason": "NO_TOOL_CALL",
            "lastUserInput": "please explain in detail",
            "transcriptPath": tp.to_string_lossy()
        })
        .to_string();
        assert!(extract_antigravity_stop(&detailed).is_none());

        let canceled = json!({
            "hookEventName": "Stop",
            "conversationId": "conv-123",
            "terminationReason": "USER_CANCELED",
            "lastUserInput": "fix everything",
            "transcriptPath": tp.to_string_lossy()
        })
        .to_string();
        assert!(extract_antigravity_stop(&canceled).is_none());
    }
}
