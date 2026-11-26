use crate::{defaults, io_err, platform, Error, Result};
use regex::Regex;
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
        r"(?x)
        \bsk-ant-[A-Za-z0-9_-]{12,}
        | \bsk-proj-[A-Za-z0-9_-]{12,}
        | \bsk-[A-Za-z0-9]{20,}
        | \bAIza[0-9A-Za-z_-]{20,}
        | \bgh[pousr]_[A-Za-z0-9_]{16,}
        | \bAKIA[0-9A-Z]{16}\b
        | (?i)(?:bearer\s+)[A-Za-z0-9._\-+/=]{16,}
        | (?i)(?:x-api-key|x-goog-api-key|api-key)\s*[:=]\s*[^\s\x22',;}{]{8,}
        ",
    )
    .unwrap()
});

pub fn redact_secrets(data: &[u8]) -> Cow<'_, [u8]> {
    let Ok(text) = std::str::from_utf8(data) else {
        return Cow::Borrowed(data);
    };
    if !SECRET_PATTERNS.is_match(text) {
        return Cow::Borrowed(data);
    }
    Cow::Owned(
        SECRET_PATTERNS
            .replace_all(text, "[REDACTED]")
            .into_owned()
            .into_bytes(),
    )
}

pub fn redact_str(s: &str) -> Cow<'_, str> {
    if !SECRET_PATTERNS.is_match(s) {
        Cow::Borrowed(s)
    } else {
        SECRET_PATTERNS.replace_all(s, "[REDACTED]")
    }
}

pub struct RecallStore {
    dir: PathBuf,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}

impl RecallStore {
    pub fn open(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir).map_err(io_err(dir))?;
        Ok(Self {
            dir: dir.to_path_buf(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
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
        self.prune(
            SystemTime::now(),
            defaults::RECALL_RETENTION,
            defaults::RECALL_MAX_BYTES,
        )
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
