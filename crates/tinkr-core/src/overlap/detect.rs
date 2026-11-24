use super::schema::{OverlapLayer, OverlapProfile, COMPANION_MCP_NAMES};
use crate::{edit::jsonc, remote::RemoteKind};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub const VERIFY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Executable,
    Package,
    Process,
    Port,
    Service,
    AgentHook,
    EnvVar,
    McpEntry,
    SkillOrPlugin,
    RulesMarker,
    StateDir,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub kind: EvidenceKind,
    pub agent: Option<String>,
    pub layer: Option<OverlapLayer>,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedTool {
    pub id: String,
    pub name: String,
    pub is_unknown_heuristic: bool,
    pub version: Option<String>,
    pub evidence: Vec<Evidence>,
    pub wired_agents: BTreeSet<String>,
    pub active_layers: BTreeSet<OverlapLayer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompanionTool {
    pub name: String,
    pub agent: String,
    pub source: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanReport {
    pub os: String,
    pub remote_context: Vec<String>,
    pub detected: Vec<DetectedTool>,
    pub companions: Vec<CompanionTool>,
}

pub type VerifyExecFn = Box<dyn Fn(&Path, &[String]) -> Option<String>>;

pub struct ScanEnv {
    pub os: String,
    pub home: PathBuf,
    pub remote: Vec<RemoteKind>,
    pub env_vars: BTreeMap<String, String>,
    pub path_var: Option<std::ffi::OsString>,
    pub extra_bin_dirs: Vec<PathBuf>,
    pub packages: BTreeMap<String, Vec<String>>,
    pub processes: Vec<String>,
    pub listening_ports: BTreeSet<u16>,
    pub services: Vec<String>,
    pub tinkr_proxy_port: Option<u16>,
    pub verify_exec: VerifyExecFn,
}

pub fn run_verify_with_timeout(exe: &Path, args: &[String]) -> Option<String> {
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let out = child.wait_with_output().ok()?;
                let s = String::from_utf8_lossy(&out.stdout);
                let first = s.lines().next().unwrap_or("").trim().to_string();
                return Some(first);
            }
            Ok(None) => {
                if start.elapsed() >= VERIFY_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(_) => return None,
        }
    }
}

impl ScanEnv {
    pub fn real(home: PathBuf, tinkr_proxy_port: Option<u16>) -> Self {
        let env_vars: BTreeMap<String, String> = std::env::vars()
            .filter(|(k, _)| is_safe_env_key(k))
            .collect();
        let mut listening_ports = BTreeSet::new();
        if crate::lifecycle::is_proxy_alive(8787) {
            listening_ports.insert(8787);
        }
        let os = if cfg!(windows) {
            "windows"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else {
            "linux"
        };
        Self {
            os: os.to_string(),
            home,
            remote: crate::remote::detect_current(),
            env_vars,
            path_var: std::env::var_os("PATH"),
            extra_bin_dirs: Vec::new(),
            packages: BTreeMap::new(),
            processes: Vec::new(),
            listening_ports,
            services: Vec::new(),
            tinkr_proxy_port,
            verify_exec: Box::new(run_verify_with_timeout),
        }
    }

    fn expand(&self, p: &str) -> PathBuf {
        match p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
            Some(rest) => self.home.join(rest),
            None if p == "~" => self.home.clone(),
            None => PathBuf::from(p),
        }
    }

    fn find_executable(&self, name: &str) -> Option<PathBuf> {
        if let Some(p) = crate::runner::which(name, self.path_var.clone()) {
            return Some(p);
        }
        let candidates = if self.os == "windows" {
            vec![
                format!("{name}.exe"),
                format!("{name}.cmd"),
                name.to_string(),
            ]
        } else {
            vec![name.to_string()]
        };
        for dir in &self.extra_bin_dirs {
            for c in &candidates {
                let p = dir.join(c);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
        None
    }
}

fn is_safe_env_key(k: &str) -> bool {
    let u = k.to_ascii_uppercase();
    if u.contains("KEY") || u.contains("TOKEN") || u.contains("SECRET") || u.contains("PASS") {
        return false;
    }
    u.ends_with("_BASE_URL")
        || u.ends_with("_API_BASE")
        || u.ends_with("_PROXY_URL")
        || u.starts_with("RTK_")
        || u.starts_with("HEADROOM_")
        || u.starts_with("CAVEMAN_")
        || u == "OLLAMA_HOST"
}

const BASE_URL_VARS: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "OPENAI_BASE_URL",
    "OPENAI_API_BASE",
    "GEMINI_BASE_URL",
    "GOOGLE_GEMINI_BASE_URL",
];

fn extract_loopback_port(url: &str) -> Option<u16> {
    let lower = url.trim().to_ascii_lowercase();
    let rest = lower
        .strip_prefix("http://")
        .or_else(|| lower.strip_prefix("https://"))?;
    let host_port = rest.split('/').next()?;
    let (host, port_s) = host_port.rsplit_once(':')?;
    if host == "127.0.0.1" || host == "localhost" || host == "[::1]" {
        port_s.parse::<u16>().ok()
    } else {
        None
    }
}

struct AgentConfigSpec {
    agent: &'static str,
    rel_path: &'static str,
    kind: ConfigFileKind,
}

#[derive(Clone, Copy)]
enum ConfigFileKind {
    HooksJson,
    McpJson,
    RulesText,
    PluginDir,
}

const AGENT_CONFIGS: &[AgentConfigSpec] = &[
    AgentConfigSpec {
        agent: "claude-code",
        rel_path: ".claude/settings.json",
        kind: ConfigFileKind::HooksJson,
    },
    AgentConfigSpec {
        agent: "claude-code",
        rel_path: ".claude/CLAUDE.md",
        kind: ConfigFileKind::RulesText,
    },
    AgentConfigSpec {
        agent: "claude-code",
        rel_path: ".claude/plugins",
        kind: ConfigFileKind::PluginDir,
    },
    AgentConfigSpec {
        agent: "codex",
        rel_path: ".codex/hooks.json",
        kind: ConfigFileKind::HooksJson,
    },
    AgentConfigSpec {
        agent: "codex",
        rel_path: ".codex/AGENTS.md",
        kind: ConfigFileKind::RulesText,
    },
    AgentConfigSpec {
        agent: "gemini-cli",
        rel_path: ".gemini/settings.json",
        kind: ConfigFileKind::McpJson,
    },
    AgentConfigSpec {
        agent: "gemini-cli",
        rel_path: ".gemini/GEMINI.md",
        kind: ConfigFileKind::RulesText,
    },
    AgentConfigSpec {
        agent: "antigravity",
        rel_path: ".gemini/config/plugins",
        kind: ConfigFileKind::PluginDir,
    },
    AgentConfigSpec {
        agent: "cursor",
        rel_path: ".cursor/mcp.json",
        kind: ConfigFileKind::McpJson,
    },
    AgentConfigSpec {
        agent: "vscode",
        rel_path: ".vscode/mcp.json",
        kind: ConfigFileKind::McpJson,
    },
    AgentConfigSpec {
        agent: "copilot-cli",
        rel_path: ".copilot/mcp-config.json",
        kind: ConfigFileKind::McpJson,
    },
    AgentConfigSpec {
        agent: "claude-desktop",
        rel_path: ".config/Claude/claude_desktop_config.json",
        kind: ConfigFileKind::McpJson,
    },
    AgentConfigSpec {
        agent: "aider",
        rel_path: ".aider.conf.yml",
        kind: ConfigFileKind::RulesText,
    },
];

fn read_text_safe(p: &Path) -> Option<String> {
    let md = std::fs::metadata(p).ok()?;
    if !md.is_file() || md.len() > 2 * 1024 * 1024 {
        return None;
    }
    std::fs::read_to_string(p).ok()
}

fn is_tinkr_text(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    l.contains("tinkr")
}

pub fn scan(profiles: &[OverlapProfile], env: &ScanEnv) -> ScanReport {
    let mut report = ScanReport {
        os: env.os.clone(),
        remote_context: env.remote.iter().map(|r| r.label()).collect(),
        detected: Vec::new(),
        companions: Vec::new(),
    };

    let mut claimed_hooks: BTreeSet<String> = BTreeSet::new();
    let mut claimed_mcps: BTreeSet<String> = BTreeSet::new();
    let mut claimed_ports: BTreeSet<u16> = BTreeSet::new();
    if let Some(tp) = env.tinkr_proxy_port {
        claimed_ports.insert(tp);
    }

    for p in profiles {
        let mut ev = Vec::new();
        let mut version: Option<String> = None;
        let mut wired_agents = BTreeSet::new();
        let mut active_layers = BTreeSet::new();

        for exe_name in &p.detect.executables {
            if let Some(found) = env.find_executable(exe_name) {
                if !p.detect.verify_args.is_empty() && exe_name == p.detect.executables[0].as_str()
                {
                    if let Some(v) = (env.verify_exec)(&found, &p.detect.verify_args) {
                        if version.is_none() {
                            let ver_probe = if v.chars().any(|c| c.is_ascii_digit()) {
                                Some(v)
                            } else {
                                (env.verify_exec)(&found, &["--version".to_string()])
                                    .filter(|s| s.chars().any(|c| c.is_ascii_digit()))
                            };
                            version = ver_probe;
                        }
                        ev.push(Evidence {
                            kind: EvidenceKind::Executable,
                            agent: None,
                            layer: None,
                            detail: found.display().to_string(),
                        });
                    }
                } else {
                    ev.push(Evidence {
                        kind: EvidenceKind::Executable,
                        agent: None,
                        layer: None,
                        detail: found.display().to_string(),
                    });
                }
            }
        }

        let pkg_lists: &[(&str, &[String])] = &[
            ("winget", &p.detect.packages.winget),
            ("brew", &p.detect.packages.brew),
            ("cargo", &p.detect.packages.cargo),
            ("pip", &p.detect.packages.pip),
            ("npm", &p.detect.packages.npm),
            ("scoop", &p.detect.packages.scoop),
        ];
        for (mgr, wanted) in pkg_lists {
            if let Some(installed) = env.packages.get(*mgr) {
                for w in *wanted {
                    if installed.iter().any(|i| i.eq_ignore_ascii_case(w)) {
                        ev.push(Evidence {
                            kind: EvidenceKind::Package,
                            agent: None,
                            layer: None,
                            detail: format!("{mgr}:{w}"),
                        });
                    }
                }
            }
        }

        for proc_name in &p.detect.processes {
            if env
                .processes
                .iter()
                .any(|r| r.eq_ignore_ascii_case(proc_name))
            {
                ev.push(Evidence {
                    kind: EvidenceKind::Process,
                    agent: None,
                    layer: p.layers.first().copied(),
                    detail: format!("process {proc_name}"),
                });
                if let Some(l) = p.layers.first().copied() {
                    active_layers.insert(l);
                }
            }
        }
        for &port in &p.detect.ports {
            if env.listening_ports.contains(&port) {
                claimed_ports.insert(port);
                ev.push(Evidence {
                    kind: EvidenceKind::Port,
                    agent: None,
                    layer: Some(OverlapLayer::Send),
                    detail: format!("127.0.0.1:{port}"),
                });
                active_layers.insert(OverlapLayer::Send);
            }
        }
        for svc in &p.detect.services {
            if env
                .services
                .iter()
                .any(|s| s.to_ascii_lowercase().contains(&svc.to_ascii_lowercase()))
            {
                ev.push(Evidence {
                    kind: EvidenceKind::Service,
                    agent: None,
                    layer: p.layers.first().copied(),
                    detail: format!("service {svc}"),
                });
            }
        }

        for var in &p.detect.env_vars {
            if env.env_vars.contains_key(var) {
                ev.push(Evidence {
                    kind: EvidenceKind::EnvVar,
                    agent: None,
                    layer: Some(OverlapLayer::Send),
                    detail: format!("env {var}"),
                });
                active_layers.insert(OverlapLayer::Send);
            }
        }
        for base_var in BASE_URL_VARS {
            if let Some(val) = env.env_vars.get(*base_var) {
                if let Some(port) = extract_loopback_port(val) {
                    if p.detect.ports.contains(&port) {
                        claimed_ports.insert(port);
                        ev.push(Evidence {
                            kind: EvidenceKind::EnvVar,
                            agent: None,
                            layer: Some(OverlapLayer::Send),
                            detail: format!("{base_var} -> 127.0.0.1:{port}"),
                        });
                        active_layers.insert(OverlapLayer::Send);
                    }
                }
            }
        }

        for sd in &p.detect.state_dirs {
            let expanded = env.expand(sd);
            if expanded.is_dir() {
                ev.push(Evidence {
                    kind: EvidenceKind::StateDir,
                    agent: None,
                    layer: None,
                    detail: expanded.display().to_string(),
                });
            }
        }

        for spec in AGENT_CONFIGS {
            let path = env.home.join(spec.rel_path);
            match spec.kind {
                ConfigFileKind::HooksJson => {
                    if let Some(text) = read_text_safe(&path) {
                        let lower = text.to_ascii_lowercase();
                        for pat in &p.detect.agent_hooks {
                            if lower.contains(&pat.to_ascii_lowercase()) {
                                claimed_hooks.insert(format!("{}:{}", spec.agent, pat));
                                wired_agents.insert(spec.agent.to_string());
                                active_layers.insert(OverlapLayer::Read);
                                ev.push(Evidence {
                                    kind: EvidenceKind::AgentHook,
                                    agent: Some(spec.agent.to_string()),
                                    layer: Some(OverlapLayer::Read),
                                    detail: format!("{} ({pat})", path.display()),
                                });
                                break;
                            }
                        }
                    }
                }
                ConfigFileKind::McpJson => {
                    if let Some(text) = read_text_safe(&path) {
                        let lower = text.to_ascii_lowercase();
                        for pat in &p.detect.mcp_entries {
                            if lower.contains(&pat.to_ascii_lowercase()) {
                                claimed_mcps.insert(format!("{}:{}", spec.agent, pat));
                                wired_agents.insert(spec.agent.to_string());
                                active_layers.insert(OverlapLayer::Send);
                                ev.push(Evidence {
                                    kind: EvidenceKind::McpEntry,
                                    agent: Some(spec.agent.to_string()),
                                    layer: Some(OverlapLayer::Send),
                                    detail: format!("{} ({pat})", path.display()),
                                });
                                break;
                            }
                        }
                    }
                }
                ConfigFileKind::RulesText => {
                    if let Some(text) = read_text_safe(&path) {
                        let lower = text.to_ascii_lowercase();
                        for marker in &p.detect.rules_markers {
                            if lower.contains(&marker.to_ascii_lowercase()) {
                                wired_agents.insert(spec.agent.to_string());
                                let layer = if p.layers.contains(&OverlapLayer::Say) {
                                    OverlapLayer::Say
                                } else {
                                    OverlapLayer::Read
                                };
                                active_layers.insert(layer);
                                ev.push(Evidence {
                                    kind: EvidenceKind::RulesMarker,
                                    agent: Some(spec.agent.to_string()),
                                    layer: Some(layer),
                                    detail: format!("{} ({marker})", path.display()),
                                });
                                break;
                            }
                        }
                    }
                }
                ConfigFileKind::PluginDir => {
                    if let Ok(rd) = std::fs::read_dir(&path) {
                        for ent in rd.flatten() {
                            let fname = ent.file_name().to_string_lossy().to_ascii_lowercase();
                            if p.detect
                                .skills_or_plugins
                                .iter()
                                .any(|sp| fname == sp.to_ascii_lowercase())
                            {
                                wired_agents.insert(spec.agent.to_string());
                                if let Some(l) = p.layers.first().copied() {
                                    active_layers.insert(l);
                                }
                                ev.push(Evidence {
                                    kind: EvidenceKind::SkillOrPlugin,
                                    agent: Some(spec.agent.to_string()),
                                    layer: p.layers.first().copied(),
                                    detail: ent.path().display().to_string(),
                                });
                            }
                        }
                    }
                }
            }
        }

        if !ev.is_empty() {
            report.detected.push(DetectedTool {
                id: p.id.clone(),
                name: p.name.clone(),
                is_unknown_heuristic: false,
                version,
                evidence: ev,
                wired_agents,
                active_layers,
            });
        }
    }

    let mut unknown_ev = Vec::new();
    let mut unknown_agents = BTreeSet::new();
    let mut unknown_layers = BTreeSet::new();

    for base_var in BASE_URL_VARS {
        if let Some(val) = env.env_vars.get(*base_var) {
            if let Some(port) = extract_loopback_port(val) {
                if !claimed_ports.contains(&port) {
                    unknown_layers.insert(OverlapLayer::Send);
                    unknown_ev.push(Evidence {
                        kind: EvidenceKind::EnvVar,
                        agent: None,
                        layer: Some(OverlapLayer::Send),
                        detail: format!("{base_var} points to loopback port {port}"),
                    });
                }
            }
        }
    }

    for spec in AGENT_CONFIGS {
        let path = env.home.join(spec.rel_path);
        match spec.kind {
            ConfigFileKind::HooksJson => {
                if let Some(text) = read_text_safe(&path) {
                    if let Ok(Some(val)) = jsonc::parse(&text) {
                        if let Some(pre) = val["hooks"]["PreToolUse"].as_array() {
                            for entry in pre {
                                if let Some(hooks) = entry["hooks"].as_array() {
                                    for h in hooks {
                                        if let Some(cmd) = h["command"].as_str() {
                                            let lower = cmd.to_ascii_lowercase();
                                            let known = is_tinkr_text(&lower)
                                                || profiles.iter().any(|p| {
                                                    p.detect.agent_hooks.iter().any(|pat| {
                                                        lower.contains(&pat.to_ascii_lowercase())
                                                    })
                                                });
                                            let looks_like_rewriter = lower.contains("hook")
                                                || lower.contains("compress")
                                                || lower.contains("trim")
                                                || lower.contains("token")
                                                || lower.contains("shrink");
                                            if !known && looks_like_rewriter {
                                                unknown_agents.insert(spec.agent.to_string());
                                                unknown_layers.insert(OverlapLayer::Read);
                                                unknown_ev.push(Evidence {
                                                    kind: EvidenceKind::AgentHook,
                                                    agent: Some(spec.agent.to_string()),
                                                    layer: Some(OverlapLayer::Read),
                                                    detail: format!(
                                                        "{}: PreToolUse command `{cmd}`",
                                                        path.display()
                                                    ),
                                                });
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            ConfigFileKind::McpJson => {
                if let Some(text) = read_text_safe(&path) {
                    if let Ok(Some(val)) = jsonc::parse(&text) {
                        for key in ["mcpServers", "servers"] {
                            if let Some(map) = val[key].as_object() {
                                for (srv_name, srv_val) in map {
                                    let lower_name = srv_name.to_ascii_lowercase();
                                    if is_tinkr_text(&lower_name) {
                                        continue;
                                    }
                                    if COMPANION_MCP_NAMES
                                        .iter()
                                        .any(|c| lower_name == *c || lower_name.contains(c))
                                    {
                                        report.companions.push(CompanionTool {
                                            name: srv_name.clone(),
                                            agent: spec.agent.to_string(),
                                            source: path.display().to_string(),
                                        });
                                        continue;
                                    }
                                    let known = profiles.iter().any(|p| {
                                        p.detect
                                            .mcp_entries
                                            .iter()
                                            .any(|m| lower_name.contains(&m.to_ascii_lowercase()))
                                    });
                                    let serialized = srv_val.to_string().to_ascii_lowercase();
                                    let looks_like_opt = ["compress", "shrink", "trim", "token"]
                                        .iter()
                                        .any(|w| lower_name.contains(w) || serialized.contains(w));
                                    if !known && looks_like_opt {
                                        unknown_agents.insert(spec.agent.to_string());
                                        unknown_layers.insert(OverlapLayer::Send);
                                        unknown_ev.push(Evidence {
                                            kind: EvidenceKind::McpEntry,
                                            agent: Some(spec.agent.to_string()),
                                            layer: Some(OverlapLayer::Send),
                                            detail: format!(
                                                "{}: MCP server `{srv_name}`",
                                                path.display()
                                            ),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
            ConfigFileKind::RulesText => {
                if let Some(text) = read_text_safe(&path) {
                    let lower = text.to_ascii_lowercase();
                    let has_managed_block =
                        lower.contains(">>> ") && !lower.contains(">>> tinkr >>>");
                    let mentions_opt = lower.contains("terse")
                        || lower.contains("compress")
                        || lower.contains("token");
                    let known = profiles.iter().any(|p| {
                        p.detect
                            .rules_markers
                            .iter()
                            .any(|m| lower.contains(&m.to_ascii_lowercase()))
                    });
                    if has_managed_block && mentions_opt && !known {
                        unknown_agents.insert(spec.agent.to_string());
                        unknown_layers.insert(OverlapLayer::Say);
                        unknown_ev.push(Evidence {
                            kind: EvidenceKind::RulesMarker,
                            agent: Some(spec.agent.to_string()),
                            layer: Some(OverlapLayer::Say),
                            detail: format!(
                                "{}: managed block mentioning compression/terse/tokens",
                                path.display()
                            ),
                        });
                    }
                }
            }
            ConfigFileKind::PluginDir => {}
        }
    }

    if !unknown_ev.is_empty() {
        report.detected.push(DetectedTool {
            id: "unknown-optimizer".into(),
            name: "Unknown token optimizer detected".into(),
            is_unknown_heuristic: true,
            version: None,
            evidence: unknown_ev,
            wired_agents: unknown_agents,
            active_layers: unknown_layers,
        });
    }

    report
}
