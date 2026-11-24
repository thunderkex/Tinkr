use crate::{
    edit::{jsonc, EditError},
    manifest::ChangeSet,
    Result,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub dir_env: Option<String>,
    pub dir_default: Option<String>,
    #[serde(default)]
    pub detect: Detect,
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub limits: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Detect {
    #[serde(default)]
    pub executables: Vec<String>,
    #[serde(default)]
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    JsonHook {
        file: String,
        event: String,
        matcher: String,
        agent: String,
    },
    McpServer {
        file: String,
        key: String,
    },
    RulesBlock {
        file: String,
        comment: String,
    },
}

pub const EMBEDDED: &[(&str, &str)] = &[
    ("claude-code", include_str!("../profiles/claude-code.toml")),
    ("codex", include_str!("../profiles/codex.toml")),
    ("gemini-cli", include_str!("../profiles/gemini-cli.toml")),
    ("cursor", include_str!("../profiles/cursor.toml")),
    ("vscode", include_str!("../profiles/vscode.toml")),
    (
        "vscode-insiders",
        include_str!("../profiles/vscode-insiders.toml"),
    ),
    ("vscodium", include_str!("../profiles/vscodium.toml")),
    ("windsurf", include_str!("../profiles/windsurf.toml")),
    ("trae", include_str!("../profiles/trae.toml")),
    ("kiro", include_str!("../profiles/kiro.toml")),
    ("opencode", include_str!("../profiles/opencode.toml")),
    ("copilot-cli", include_str!("../profiles/copilot-cli.toml")),
    ("zed", include_str!("../profiles/zed.toml")),
    (
        "claude-desktop",
        include_str!("../profiles/claude-desktop.toml"),
    ),
    ("jetbrains", include_str!("../profiles/jetbrains.toml")),
    (
        "visual-studio",
        include_str!("../profiles/visual-studio.toml"),
    ),
    ("xcode", include_str!("../profiles/xcode.toml")),
    ("neovim", include_str!("../profiles/neovim.toml")),
    ("emacs", include_str!("../profiles/emacs.toml")),
    ("sublime", include_str!("../profiles/sublime.toml")),
    ("eclipse", include_str!("../profiles/eclipse.toml")),
    ("aider", include_str!("../profiles/aider.toml")),
    ("goose", include_str!("../profiles/goose.toml")),
    ("warp", include_str!("../profiles/warp.toml")),
];

pub fn builtin() -> Vec<Profile> {
    EMBEDDED
        .iter()
        .map(|(id, src)| {
            toml::from_str::<Profile>(src)
                .unwrap_or_else(|e| panic!("bad built-in profile {id}: {e}"))
        })
        .collect()
}

pub type EnvLookup = Box<dyn Fn(&str) -> Option<String>>;

pub struct Machine {
    pub home: PathBuf,
    pub env: EnvLookup,
    pub path_var: Option<std::ffi::OsString>,
    pub tinkr_exe: PathBuf,
}

impl Machine {
    pub fn real(home: PathBuf) -> Self {
        Self {
            home,
            env: Box::new(|k| std::env::var(k).ok()),
            path_var: std::env::var_os("PATH"),
            tinkr_exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("tinkr")),
        }
    }

    pub fn expand(&self, p: &str) -> PathBuf {
        match p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
            Some(rest) => self.home.join(rest),
            None if p == "~" => self.home.clone(),
            None => PathBuf::from(p),
        }
    }

    pub fn config_dir(&self, p: &Profile) -> Option<PathBuf> {
        if let Some(v) = p
            .dir_env
            .as_deref()
            .and_then(|e| (self.env)(e))
            .filter(|s| !s.is_empty())
        {
            return Some(PathBuf::from(v));
        }
        p.dir_default.as_deref().map(|d| self.expand(d))
    }

    pub fn hook_command(&self, agent: &str) -> String {
        let exe = self.tinkr_exe.to_string_lossy().replace('\\', "/");
        if exe.contains(' ') {
            format!("\"{exe}\" hook {agent}")
        } else {
            format!("{exe} hook {agent}")
        }
    }
}

pub fn detect(p: &Profile, m: &Machine) -> Option<String> {
    for e in &p.detect.executables {
        if let Some(found) = crate::runner::which(e, m.path_var.clone()) {
            return Some(format!("{} on PATH", found.display()));
        }
    }
    if let Some(d) = m.config_dir(p).filter(|d| d.is_dir()) {
        return Some(format!("config dir {}", d.display()));
    }
    for path in &p.detect.paths {
        let e = m.expand(path);
        if e.exists() {
            return Some(format!("found {}", e.display()));
        }
    }
    None
}

#[derive(Debug)]
pub struct Planned {
    pub file: PathBuf,
    pub bytes: Vec<u8>,
    pub description: String,
}

#[derive(Debug)]
pub enum ActionResult {
    Change(Planned),
    AlreadyDone(PathBuf),
    Skipped { file: PathBuf, reason: String },
}

fn is_tinkr_entry(entry: &Value, agent: &str) -> bool {
    entry["hooks"].as_array().is_some_and(|hs| {
        hs.iter().any(|h| {
            h["command"].as_str().is_some_and(|c| {
                c.contains(&format!(" hook {agent}")) && c.to_lowercase().contains("tinkr")
            })
        })
    })
}

pub fn compute(profile: &Profile, action: &Action, m: &Machine) -> ActionResult {
    let dir = m.config_dir(profile).unwrap_or_else(|| m.home.clone());
    match action {
        Action::JsonHook {
            file,
            event,
            matcher,
            agent,
        } => {
            let path = dir.join(file);
            let text = match std::fs::read(&path) {
                Ok(b) => String::from_utf8_lossy(&b).into_owned(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(e) => {
                    return ActionResult::Skipped {
                        file: path,
                        reason: e.to_string(),
                    }
                }
            };
            let current = match jsonc::get_path(&text, &["hooks", event]) {
                Ok(v) => v,
                Err(EditError::Unparseable(r)) => {
                    return ActionResult::Skipped {
                        file: path,
                        reason: format!("unparseable ({r})"),
                    }
                }
                Err(e) => {
                    return ActionResult::Skipped {
                        file: path,
                        reason: e.to_string(),
                    }
                }
            };
            let mut arr: Vec<Value> = match current {
                Some(Value::Array(a)) => a,
                None => vec![],
                Some(_) => {
                    return ActionResult::Skipped {
                        file: path,
                        reason: format!("hooks.{event} is not a list"),
                    }
                }
            };
            let entry = json!({"matcher": matcher, "hooks": [{"type": "command", "command": m.hook_command(agent)}]});
            match arr.iter().position(|e| is_tinkr_entry(e, agent)) {
                Some(i) if arr[i] == entry => return ActionResult::AlreadyDone(path),
                Some(i) => arr[i] = entry,
                None => arr.push(entry),
            }
            match jsonc::set_path(&text, &["hooks", event], &Value::Array(arr)) {
                Ok(new) => ActionResult::Change(Planned {
                    description: format!(
                        "register {event} hook for {matcher} -> `{}`",
                        m.hook_command(agent)
                    ),
                    file: path,
                    bytes: new.into_bytes(),
                }),
                Err(e) => ActionResult::Skipped {
                    file: path,
                    reason: e.to_string(),
                },
            }
        }
        Action::McpServer { file, key } => {
            let path = dir.join(file);
            let text = match std::fs::read(&path) {
                Ok(b) => String::from_utf8_lossy(&b).into_owned(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(e) => {
                    return ActionResult::Skipped {
                        file: path,
                        reason: e.to_string(),
                    }
                }
            };
            let exe = m.tinkr_exe.to_string_lossy().replace('\\', "/");
            let server = json!({ "command": exe, "args": ["mcp"] });
            match jsonc::get_path(&text, &[key, "tinkr"]) {
                Ok(Some(v)) if v == server => return ActionResult::AlreadyDone(path),
                Err(EditError::Unparseable(r)) => {
                    return ActionResult::Skipped {
                        file: path,
                        reason: format!("unparseable ({r})"),
                    }
                }
                Err(e) => {
                    return ActionResult::Skipped {
                        file: path,
                        reason: e.to_string(),
                    }
                }
                _ => {}
            }
            match jsonc::set_path(&text, &[key, "tinkr"], &server) {
                Ok(new) => ActionResult::Change(Planned {
                    description: format!("register MCP server `{key}.tinkr` -> `{exe} mcp`"),
                    file: path,
                    bytes: new.into_bytes(),
                }),
                Err(e) => ActionResult::Skipped {
                    file: path,
                    reason: e.to_string(),
                },
            }
        }
        Action::RulesBlock { file, comment } => {
            let path = dir.join(file);
            let existing = std::fs::read_to_string(&path).unwrap_or_default();
            if existing.contains(">>> tinkr >>>") && existing.contains("Response style (Tinkr):") {
                return ActionResult::AlreadyDone(path);
            }
            ActionResult::Change(Planned {
                description: format!(
                    "insert say-less rules block into {file} (comment `{comment}`)"
                ),
                file: path,
                bytes: crate::say_less::TERSE_RULE.as_bytes().to_vec(),
            })
        }
    }
}

pub fn apply(p: &Profile, m: &Machine, cs: &mut ChangeSet) -> Result<Vec<ActionResult>> {
    let mut out = Vec::new();
    for a in &p.actions {
        match compute(p, a, m) {
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

pub fn exe_dir_hint(p: &Path) -> String {
    p.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn machine(home: &Path, env: &[(&str, &str)]) -> Machine {
        let map: HashMap<String, String> = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Machine {
            home: home.to_path_buf(),
            env: Box::new(move |k| map.get(k).cloned()),
            path_var: Some(std::ffi::OsString::new()),
            tinkr_exe: PathBuf::from("C:\\Program Files\\Tinkr\\tinkr.exe"),
        }
    }

    #[test]
    fn builtin_profiles_parse() {
        let ps = builtin();
        assert!(ps.iter().any(|p| p.id == "claude-code") && ps.iter().any(|p| p.id == "codex"));
        assert!(ps
            .iter()
            .all(|p| !p.actions.is_empty() && !p.limits.is_empty()));
    }

    #[test]
    fn detection_via_dir_and_env_override() {
        let d = tempfile::tempdir().unwrap();
        let ps = builtin();
        let claude = ps.iter().find(|p| p.id == "claude-code").unwrap();
        assert!(detect(claude, &machine(d.path(), &[])).is_none());
        std::fs::create_dir_all(d.path().join(".claude")).unwrap();
        assert!(detect(claude, &machine(d.path(), &[])).is_some());
        let other = d.path().join("elsewhere");
        std::fs::create_dir_all(&other).unwrap();
        let m = machine(d.path(), &[("CLAUDE_CONFIG_DIR", other.to_str().unwrap())]);
        assert_eq!(m.config_dir(claude).unwrap(), other);
        std::fs::create_dir_all(d.path().join(".claude-code-notes")).unwrap();
        let codex = ps.iter().find(|p| p.id == "codex").unwrap();
        assert!(detect(codex, &machine(d.path(), &[])).is_none());
    }

    #[test]
    fn hook_install_preserves_settings_is_idempotent_and_undoable() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let settings = home.join(".claude").join("settings.json");
        let orig = "{\n  // my settings\n  \"model\": \"opus\",\n  \"hooks\": {\n    \"PreToolUse\": [\n      { \"matcher\": \"Write\", \"hooks\": [{ \"type\": \"command\", \"command\": \"lint.sh\" }] }\n    ]\n  }\n}\n";
        std::fs::write(&settings, orig).unwrap();
        let m = machine(&home, &[]);
        let p = builtin()
            .into_iter()
            .find(|p| p.id == "claude-code")
            .unwrap();
        let mut cs = ChangeSet::open(&d.path().join("manifest.json")).unwrap();
        let r = apply(&p, &m, &mut cs).unwrap();
        assert!(matches!(r[0], ActionResult::Change(_)));
        let after = std::fs::read_to_string(&settings).unwrap();
        assert!(
            after.contains("// my settings")
                && after.contains("lint.sh")
                && after.contains("\"opus\"")
        );
        let hooks = jsonc::get_path(&after, &["hooks", "PreToolUse"])
            .unwrap()
            .unwrap();
        assert_eq!(hooks.as_array().unwrap().len(), 2);
        assert!(hooks[1]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .starts_with("\"C:/Program Files/Tinkr/tinkr.exe\" hook claude"));
        let r2 = apply(&p, &m, &mut cs).unwrap();
        assert!(matches!(r2[0], ActionResult::AlreadyDone(_)));
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), after);
        let store = crate::platform::envpersist::EnvDFile {
            path: d.path().join("env.conf"),
        };
        cs.undo(&store).unwrap();
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), orig);
    }

    #[test]
    fn corrupt_config_is_skipped_and_untouched() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        let f = home.join(".codex").join("hooks.json");
        std::fs::write(&f, "{ this is not json").unwrap();
        let m = machine(&home, &[]);
        let p = builtin().into_iter().find(|p| p.id == "codex").unwrap();
        let mut cs = ChangeSet::open(&d.path().join("manifest.json")).unwrap();
        let r = apply(&p, &m, &mut cs).unwrap();
        assert!(matches!(r[0], ActionResult::Skipped { .. }));
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "{ this is not json");
        assert!(cs.is_empty());
    }

    #[test]
    fn drift_repair_replaces_stale_command() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        let f = home.join(".codex").join("hooks.json");
        std::fs::write(&f, r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"/old/place/tinkr hook codex"}]}]}}"#).unwrap();
        let m = machine(&home, &[]);
        let p = builtin().into_iter().find(|p| p.id == "codex").unwrap();
        let mut cs = ChangeSet::open(&d.path().join("manifest.json")).unwrap();
        apply(&p, &m, &mut cs).unwrap();
        let v = jsonc::get_path(
            &std::fs::read_to_string(&f).unwrap(),
            &["hooks", "PreToolUse"],
        )
        .unwrap()
        .unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert!(v[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("Program Files/Tinkr"));
    }
}
