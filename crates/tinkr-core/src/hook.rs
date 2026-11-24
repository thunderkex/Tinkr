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
        let kind = classify::classify(seg)?;
        if perms.denied(seg) || perms.denied(trimmed) {
            return None;
        }
        let ok = kind.read_only()
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
                .all(|s| classify::classify(s).is_some_and(Kind::read_only))
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
}
