use crate::{
    defaults, io_err,
    platform::{self, Native, Platform},
    Error, Result,
};
use regex::Regex;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    fs,
    path::{Path, PathBuf},
    sync::LazyLock,
    time::{Duration, SystemTime},
};

static SECRET_PATTERNS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?x)
        (?s)-----BEGIN\ [A-Z0-9\ ]*PRIVATE\ KEY-----.*?-----END\ [A-Z0-9\ ]*PRIVATE\ KEY-----
        | \bsk-ant-[A-Za-z0-9_-]{12,}
        | \bsk-proj-[A-Za-z0-9_-]{12,}
        | \b(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{12,}
        | \bsk-[A-Za-z0-9]{20,}
        | \bAIza[0-9A-Za-z_-]{20,}
        | \bgh[pousr]_[A-Za-z0-9_]{16,}
        | \bgithub_pat_[A-Za-z0-9_]{20,}
        | \b(?:AKIA|ASIA)[0-9A-Z]{16}\b
        | \bxox[baprs]-[A-Za-z0-9-]{16,}
        | \bnpm_[A-Za-z0-9]{20,}
        | \bpypi-[A-Za-z0-9_-]{20,}
        | \beyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}
        | (?i)(?:bearer\s+)[A-Za-z0-9._\-+/=]{16,}
        | (?i)(?:x-api-key|x-goog-api-key|api[_-]?key|client[_-]?secret|private[_-]?key|access[_-]?token|auth[_-]?token|webhook[_-]?secret|password|passwd|pwd)\s*[:=]\s*["']?[^\s"',;}{]{8,}["']?
        "#,
    )
    .unwrap()
});

static URI_USERINFO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(://[^:/@\s"']+:)[^@\s"']+(@)"#).unwrap());

pub fn shannon_entropy(s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    let mut counts = [0u32; 256];
    let bytes = s.as_bytes();
    for &b in bytes {
        counts[b as usize] += 1;
    }
    let len = bytes.len() as f64;
    let mut ent = 0.0f64;
    for &c in &counts {
        if c > 0 {
            let p = (c as f64) / len;
            ent -= p * p.log2();
        }
    }
    ent
}

fn is_high_entropy_secret_token(tok: &str) -> bool {
    let trimmed = tok.trim_matches(|c: char| {
        matches!(
            c,
            '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '.'
        )
    });
    if trimmed.len() < 24 || trimmed.len() > 256 {
        return false;
    }
    if trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains("::")
        || trimmed.contains("->")
        || trimmed.starts_with("tinkr:")
        || trimmed == "REDACTED"
    {
        return false;
    }
    if trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    let mut has_upper = false;
    let mut has_lower = false;
    let mut has_digit = false;
    for b in trimmed.bytes() {
        if !b.is_ascii_alphanumeric() && !matches!(b, b'_' | b'-' | b'+' | b'=') {
            return false;
        }
        if b.is_ascii_uppercase() {
            has_upper = true;
        } else if b.is_ascii_lowercase() {
            has_lower = true;
        } else if b.is_ascii_digit() {
            has_digit = true;
        }
    }
    if !(has_upper && has_lower && has_digit) {
        return false;
    }
    shannon_entropy(trimmed) >= 4.30
}

fn redact_entropy_tokens(input: &str) -> Cow<'_, str> {
    let bytes = input.as_bytes();
    let mut out: Option<String> = None;
    let mut i = 0usize;
    let is_delim = |b: u8| {
        b.is_ascii_whitespace()
            || matches!(
                b,
                b'=' | b':' | b'"' | b'\'' | b',' | b';' | b'&' | b'(' | b')' | b'[' | b']' | b'{' | b'}'
            )
    };
    while i < bytes.len() {
        if is_delim(bytes[i]) {
            if let Some(ref mut s) = out {
                let ch = input[i..].chars().next().unwrap();
                s.push(ch);
                i += ch.len_utf8();
            } else {
                i += 1;
            }
            continue;
        }
        let start = i;
        while i < bytes.len() && !is_delim(bytes[i]) {
            let ch = input[i..].chars().next().unwrap();
            i += ch.len_utf8();
        }
        let tok = &input[start..i];
        if is_high_entropy_secret_token(tok) {
            let s = out.get_or_insert_with(|| String::with_capacity(input.len()));
            if s.is_empty() && start > 0 {
                s.push_str(&input[..start]);
            }
            s.push_str("[REDACTED]");
        } else if let Some(ref mut s) = out {
            s.push_str(tok);
        }
    }
    match out {
        Some(s) => Cow::Owned(s),
        None => Cow::Borrowed(input),
    }
}

pub fn redact_str(s: &str) -> Cow<'_, str> {
    let step1: Cow<'_, str> = if SECRET_PATTERNS.is_match(s) {
        Cow::Owned(SECRET_PATTERNS.replace_all(s, "[REDACTED]").into_owned())
    } else {
        Cow::Borrowed(s)
    };
    let step2: Cow<'_, str> = if URI_USERINFO.is_match(&step1) {
        Cow::Owned(URI_USERINFO.replace_all(&step1, "${1}[REDACTED]${2}").into_owned())
    } else {
        step1
    };
    match redact_entropy_tokens(&step2) {
        Cow::Borrowed(_) => step2,
        Cow::Owned(redacted) => Cow::Owned(redacted),
    }
}

pub fn redact_secrets(data: &[u8]) -> Cow<'_, [u8]> {
    let Ok(text) = std::str::from_utf8(data) else {
        return Cow::Borrowed(data);
    };
    match redact_str(text) {
        Cow::Borrowed(_) => Cow::Borrowed(data),
        Cow::Owned(s) => Cow::Owned(s.into_bytes()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecallConfig {
    pub retention: Duration,
    pub max_bytes: u64,
}

impl Default for RecallConfig {
    fn default() -> Self {
        Self {
            retention: defaults::RECALL_RETENTION,
            max_bytes: defaults::RECALL_MAX_BYTES,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct RecallToml {
    retention_days: Option<u64>,
    max_mb: Option<u64>,
}

pub fn load_recall_config(recall_dir: &Path) -> RecallConfig {
    let mut cfg = RecallConfig::default();

    let candidates = [
        Some(recall_dir.join("recall.toml")),
        recall_dir
            .parent()
            .and_then(|p| p.parent())
            .map(|r| r.join("config").join("recall.toml")),
    ];
    for cand in candidates.into_iter().flatten() {
        if let Ok(src) = fs::read_to_string(&cand) {
            if let Ok(parsed) = toml::from_str::<RecallToml>(&src) {
                if let Some(days) = parsed.retention_days {
                    cfg.retention = Duration::from_secs(days.saturating_mul(86_400));
                }
                if let Some(mb) = parsed.max_mb {
                    cfg.max_bytes = mb.saturating_mul(1024 * 1024);
                }
                break;
            }
        }
    }

    if let Ok(days_s) = std::env::var("TINKR_RECALL_RETENTION_DAYS") {
        if let Ok(days) = days_s.trim().parse::<u64>() {
            cfg.retention = Duration::from_secs(days.saturating_mul(86_400));
        }
    }
    if let Ok(mb_s) = std::env::var("TINKR_RECALL_MAX_MB") {
        if let Ok(mb) = mb_s.trim().parse::<u64>() {
            cfg.max_bytes = mb.saturating_mul(1024 * 1024);
        }
    }
    cfg
}

pub struct RecallStore {
    dir: PathBuf,
    cfg: RecallConfig,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}

const PRUNE_CHECK_INTERVAL: Duration = Duration::from_secs(60);

impl RecallStore {
    pub fn open(dir: &Path) -> Result<Self> {
        let cfg = load_recall_config(dir);
        Self::open_with_config(dir, cfg)
    }

    pub fn open_with_config(dir: &Path, cfg: RecallConfig) -> Result<Self> {
        let newly_created = !dir.exists();
        fs::create_dir_all(dir).map_err(io_err(dir))?;
        if newly_created {
            let _ = Native.set_private(dir);
        }
        let store = Self {
            dir: dir.to_path_buf(),
            cfg,
        };
        let _ = store.prune_if_due();
        Ok(store)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn config(&self) -> RecallConfig {
        self.cfg
    }

    fn prune_if_due(&self) -> Result<usize> {
        let stamp = self.dir.join(".last_prune");
        let now = SystemTime::now();
        if let Ok(md) = fs::metadata(&stamp) {
            if let Ok(mtime) = md.modified() {
                if now.duration_since(mtime).unwrap_or_default() < PRUNE_CHECK_INTERVAL {
                    return Ok(0);
                }
            }
        }
        let removed = self.prune(now, self.cfg.retention, self.cfg.max_bytes)?;
        let _ = platform::fsx::atomic_write(&stamp, b"ok\n");
        Ok(removed)
    }

    fn list(&self) -> Result<Vec<(String, PathBuf)>> {
        let mut out = Vec::new();
        for ent in fs::read_dir(&self.dir).map_err(io_err(&self.dir))? {
            let ent = ent.map_err(io_err(&self.dir))?;
            let name = ent.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_suffix(".bin") {
                if stem.len() == 64 && is_hex(stem) {
                    out.push((stem.to_string(), ent.path()));
                }
            }
        }
        Ok(out)
    }

    pub fn put(&self, data: &[u8]) -> Result<String> {
        let safe = redact_secrets(data);
        let full = hex(&Sha256::digest(&safe));
        if self.cfg.retention.is_zero() || self.cfg.max_bytes == 0 {
            return Ok(full[..defaults::RECALL_ID_LEN].to_string());
        }
        let path = self.dir.join(format!("{full}.bin"));
        if path.exists() {
            if let Ok(f) = fs::OpenOptions::new().write(true).open(&path) {
                let _ = f.set_modified(SystemTime::now());
            }
        } else {
            platform::fsx::atomic_write(&path, &safe)?;
        }
        let others: Vec<String> = self
            .list()?
            .into_iter()
            .map(|(h, _)| h)
            .filter(|h| *h != full)
            .collect();
        let mut n = defaults::RECALL_ID_LEN;
        while n < full.len() && others.iter().any(|h| h.starts_with(&full[..n])) {
            n += 1;
        }
        Ok(full[..n].to_string())
    }

    pub fn get(&self, id: &str) -> Result<Vec<u8>> {
        let id = id.trim().to_ascii_lowercase();
        if id.len() < defaults::RECALL_ID_LEN || !is_hex(&id) {
            return Err(Error::RecallInvalid(id));
        }
        let mut hits: Vec<_> = self
            .list()?
            .into_iter()
            .filter(|(h, _)| h.starts_with(&id))
            .collect();
        match hits.len() {
            0 => Err(Error::RecallNotFound(id)),
            1 => {
                let p = hits.remove(0).1;
                fs::read(&p).map_err(io_err(p))
            }
            _ => Err(Error::RecallAmbiguous(id)),
        }
    }

    pub fn purge(&self) -> Result<usize> {
        let mut removed = 0usize;
        for (_, p) in self.list()? {
            if fs::remove_file(&p).is_ok() {
                removed += 1;
            }
        }
        let _ = fs::remove_file(self.dir.join("read-cache.json"));
        let _ = fs::remove_file(self.dir.join(".last_prune"));
        Ok(removed)
    }

    pub fn prune(&self, now: SystemTime, max_age: Duration, max_bytes: u64) -> Result<usize> {
        let mut entries: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
        for (_, p) in self.list()? {
            if let Ok(md) = fs::metadata(&p) {
                entries.push((md.modified().unwrap_or(now), md.len(), p));
            }
        }
        entries.sort_by_key(|e| e.0);
        let mut removed = 0;
        let mut total: u64 = entries.iter().map(|e| e.1).sum();
        for (mtime, len, p) in entries {
            let age = now.duration_since(mtime).unwrap_or_default();
            if (age > max_age || total > max_bytes) && fs::remove_file(&p).is_ok() {
                removed += 1;
                total = total.saturating_sub(len);
            }
        }
        Ok(removed)
    }

    pub fn prune_default(&self) -> Result<usize> {
        self.prune(SystemTime::now(), self.cfg.retention, self.cfg.max_bytes)
    }
}

pub fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn marker(id: &str, before: u64, after: u64, unicode: bool) -> String {
    let (sep, arrow) = if unicode {
        (" \u{b7} ", "\u{2192}")
    } else {
        (" | ", "->")
    };
    format!(
        "[tinkr:{id}{sep}{}{arrow}{} tok{sep}recall]",
        group_digits(before),
        group_digits(after)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_dedupe_and_binary() {
        let d = tempfile::tempdir().unwrap();
        let s = RecallStore::open(d.path()).unwrap();
        let data = vec![0xffu8, 0, 1, 2, b'\r', b'\n'];
        let id = s.put(&data).unwrap();
        assert_eq!(id.len(), 6);
        assert_eq!(s.put(&data).unwrap(), id);
        assert_eq!(s.get(&id).unwrap(), data);
        assert_eq!(s.get(&id.to_uppercase()).unwrap(), data);
        assert_eq!(s.list().unwrap().len(), 1);
    }

    #[test]
    fn errors() {
        let d = tempfile::tempdir().unwrap();
        let s = RecallStore::open(d.path()).unwrap();
        assert!(matches!(s.get("zz"), Err(Error::RecallInvalid(_))));
        assert!(matches!(s.get("abcdef"), Err(Error::RecallNotFound(_))));
    }

    #[test]
    fn prune_by_age_and_size() {
        let d = tempfile::tempdir().unwrap();
        let s = RecallStore::open(d.path()).unwrap();
        let now = SystemTime::now();
        let mut ids = Vec::new();
        for i in 0..3u8 {
            let id = s.put(&vec![i; 1000]).unwrap();
            let p = s
                .list()
                .unwrap()
                .into_iter()
                .find(|(h, _)| h.starts_with(&id))
                .unwrap()
                .1;
            let f = fs::OpenOptions::new().write(true).open(p).unwrap();
            let age = [8, 2, 0][i as usize];
            f.set_modified(now - Duration::from_secs(age * 86400))
                .unwrap();
            ids.push(id);
        }
        let week = Duration::from_secs(7 * 86400);
        assert_eq!(s.prune(now, week, u64::MAX).unwrap(), 1);
        assert!(s.get(&ids[0]).is_err());
        assert_eq!(s.prune(now, week, 1500).unwrap(), 1);
        assert!(s.get(&ids[1]).is_err());
        assert!(s.get(&ids[2]).is_ok());
    }

    #[test]
    fn marker_format() {
        assert_eq!(group_digits(4210), "4,210");
        assert_eq!(group_digits(312), "312");
        assert_eq!(group_digits(1234567), "1,234,567");
        assert_eq!(
            marker("a1b2c3", 4210, 312, true),
            "[tinkr:a1b2c3 \u{b7} 4,210\u{2192}312 tok \u{b7} recall]"
        );
        assert_eq!(
            marker("a1b2c3", 4210, 312, false),
            "[tinkr:a1b2c3 | 4,210->312 tok | recall]"
        );
    }
}
