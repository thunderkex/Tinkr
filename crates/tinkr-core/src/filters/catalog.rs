use super::classify::Kind;
use regex::RegexSet;
use serde::Deserialize;
use std::sync::OnceLock;

struct CatalogShard {
    name: &'static str,
    src: &'static str,
    rules: OnceLock<Vec<CatalogRule>>,
}

impl CatalogShard {
    const fn new(name: &'static str, src: &'static str) -> Self {
        Self {
            name,
            src,
            rules: OnceLock::new(),
        }
    }

    fn rules(&self) -> &[CatalogRule] {
        self.rules.get_or_init(|| {
            let parsed: CatalogFile = toml::from_str(self.src)
                .unwrap_or_else(|e| panic!("invalid catalog/{}.toml: {e}", self.name));
            parsed.rules.into_iter().map(CatalogRule::new).collect()
        })
    }
}

static SHARDS: [CatalogShard; 7] = [
    CatalogShard::new("git", include_str!("../../catalog/git.toml")),
    CatalogShard::new("cargo", include_str!("../../catalog/cargo.toml")),
    CatalogShard::new("python", include_str!("../../catalog/python.toml")),
    CatalogShard::new("js-ts", include_str!("../../catalog/js-ts.toml")),
    CatalogShard::new(
        "go-ruby-dotnet",
        include_str!("../../catalog/go-ruby-dotnet.toml"),
    ),
    CatalogShard::new(
        "containers-cloud",
        include_str!("../../catalog/containers-cloud.toml"),
    ),
    CatalogShard::new("shell-pwsh", include_str!("../../catalog/shell-pwsh.toml")),
];

#[derive(Debug, Deserialize)]
struct CatalogFile {
    rules: Vec<RawRule>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawRule {
    pub id: String,
    pub kind: Kind,
    pub read_only: bool,
    pub executables: Vec<String>,
    #[serde(default)]
    pub flags_with_arg: Vec<String>,
    #[serde(default)]
    pub subcommands: Vec<String>,
    #[serde(default)]
    pub two_word_subcommands: Vec<String>,
    #[serde(default)]
    pub second_word_prefixes: Vec<String>,
    #[serde(default)]
    pub arg_pairs: Vec<String>,
    #[serde(default)]
    pub arg_contains: Vec<String>,
    #[serde(default)]
    pub any_subcommand: bool,
    #[serde(default)]
    pub any_subcommand_for: Vec<String>,
    pub strategy: Strategy,
    #[serde(default)]
    pub omitted_label: String,
    #[serde(default)]
    pub drop_pass_lines: bool,
    #[serde(default)]
    pub drop_regex: Vec<String>,
    #[serde(default)]
    pub keep_regex: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    GitStatus,
    GitLog,
    GitDiff,
    ListDir,
    FindGroup,
    GrepGroup,
    DropLines,
    TableSqueeze,
    AutoStructured,
    FileRead,
}

pub struct CatalogRule {
    pub raw: RawRule,
    drop_set: OnceLock<Option<RegexSet>>,
    keep_set: OnceLock<Option<RegexSet>>,
}

impl CatalogRule {
    fn new(raw: RawRule) -> Self {
        Self {
            raw,
            drop_set: OnceLock::new(),
            keep_set: OnceLock::new(),
        }
    }

    pub fn drop_set(&self) -> Option<&RegexSet> {
        self.drop_set
            .get_or_init(|| {
                if self.raw.drop_regex.is_empty() {
                    None
                } else {
                    Some(
                        RegexSet::new(&self.raw.drop_regex)
                            .unwrap_or_else(|e| panic!("invalid drop_regex in {}: {e}", self.raw.id)),
                    )
                }
            })
            .as_ref()
    }

    pub fn keep_set(&self) -> Option<&RegexSet> {
        self.keep_set
            .get_or_init(|| {
                if self.raw.keep_regex.is_empty() {
                    None
                } else {
                    Some(
                        RegexSet::new(&self.raw.keep_regex)
                            .unwrap_or_else(|e| panic!("invalid keep_regex in {}: {e}", self.raw.id)),
                    )
                }
            })
            .as_ref()
    }
}

pub fn rules() -> Vec<&'static CatalogRule> {
    SHARDS.iter().flat_map(|s| s.rules().iter()).collect()
}

fn base_exe(word: &str) -> String {
    let w = word
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(word)
        .to_ascii_lowercase();
    w.strip_suffix(".exe")
        .or_else(|| w.strip_suffix(".cmd"))
        .or_else(|| w.strip_suffix(".ps1"))
        .map(str::to_string)
        .unwrap_or(w)
}

fn unwrap_runners(words: &[String]) -> &[String] {
    let mut i = 0usize;
    while i < words.len() {
        let w = &words[i];
        if w.contains('=') && !w.starts_with('-') {
            i += 1;
            continue;
        }
        let b = base_exe(w);
        if matches!(b.as_str(), "env" | "sudo" | "time" | "command" | "call") {
            i += 1;
            continue;
        }
        if matches!(b.as_str(), "npx" | "pnpx" | "bunx") {
            i += 1;
            while i < words.len() && (words[i] == "--yes" || words[i] == "-y" || words[i] == "--no-install") {
                i += 1;
            }
            continue;
        }
        if b == "bundle" && words.get(i + 1).is_some_and(|n| n.eq_ignore_ascii_case("exec")) {
            i += 2;
            continue;
        }
        if matches!(b.as_str(), "pnpm" | "yarn" | "npm")
            && words.get(i + 1).is_some_and(|n| n.eq_ignore_ascii_case("exec"))
        {
            i += 2;
            if words.get(i).map(String::as_str) == Some("--") {
                i += 1;
            }
            continue;
        }
        break;
    }
    &words[i..]
}

fn positional_words<'a>(args: &'a [String], flags_with_arg: &[String]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut j = 0usize;
    while j < args.len() {
        let a = args[j].as_str();
        if a == "--" {
            break;
        }
        if flags_with_arg.iter().any(|f| f == a) {
            j += 2;
            continue;
        }
        if a.starts_with('-') || a.starts_with('+') {
            j += 1;
            continue;
        }
        out.push(a);
        j += 1;
    }
    out
}

pub fn match_words(words: &[String]) -> Option<&'static CatalogRule> {
    let stripped = unwrap_runners(words);
    let first = base_exe(stripped.first()?);
    let args = &stripped[1..];
    let quoted_exe = format!("\"{first}\"");

    for shard in &SHARDS {
        if shard.rules.get().is_none() && !shard.src.contains(&quoted_exe) {
            continue;
        }
        for rule in shard.rules() {
            if !rule.raw.executables.iter().any(|e| e == &first) {
                continue;
            }
            if !rule.raw.arg_pairs.is_empty() {
                let matched_pair = rule.raw.arg_pairs.iter().any(|pair| {
                    let mut parts = pair.split_whitespace();
                    let (Some(a), Some(b)) = (parts.next(), parts.next()) else {
                        return false;
                    };
                    args.windows(2)
                        .any(|w| w[0].eq_ignore_ascii_case(a) && w[1].eq_ignore_ascii_case(b))
                });
                if matched_pair {
                    return Some(rule);
                }
                continue;
            }
            if !rule.raw.arg_contains.is_empty() {
                if args
                    .iter()
                    .any(|a| rule.raw.arg_contains.iter().any(|c| a.eq_ignore_ascii_case(c)))
                {
                    return Some(rule);
                }
                continue;
            }
            if rule.raw.any_subcommand || rule.raw.any_subcommand_for.iter().any(|e| e == &first) {
                return Some(rule);
            }

            let pos = positional_words(args, &rule.raw.flags_with_arg);
            let Some(p0) = pos.first().map(|s| s.to_ascii_lowercase()) else {
                continue;
            };
            let p1 = pos.get(1).map(|s| s.to_ascii_lowercase());

            if let Some(ref second) = p1 {
                let two = format!("{p0} {second}");
                if rule.raw.two_word_subcommands.iter().any(|tw| tw == &two) {
                    return Some(rule);
                }
                if rule
                    .raw
                    .second_word_prefixes
                    .iter()
                    .any(|pfx| second.starts_with(pfx.as_str()))
                {
                    return Some(rule);
                }
            }
            if rule.raw.subcommands.iter().any(|sc| sc == &p0) {
                return Some(rule);
            }
        }
    }
    None
}
