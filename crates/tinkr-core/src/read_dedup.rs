use crate::{
    defaults,
    platform::fsx,
    recall::{self, RecallStore},
    tokenizer::{self, TokenizerKind},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_CACHE_ENTRIES: usize = 64;
const MAX_CACHE_BYTES: usize = 2 * 1024 * 1024;
const ENTRY_TTL_SECS: i64 = 2 * 3600;
const DIFF_CONTEXT: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotOrigin {
    Read,
    AgentEdit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadSnapshot {
    pub seq: u32,
    pub hash: String,
    pub text: String,
    pub range: Option<(usize, usize)>,
    pub origin: SnapshotOrigin,
    pub ts: i64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ReadCache {
    pub next_seq: u32,
    pub entries: BTreeMap<String, ReadSnapshot>,
}

#[derive(Debug, Clone)]
pub struct DedupHit {
    pub text: String,
    pub before: u64,
    pub after: u64,
    pub recall_id: Option<String>,
}

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn normalize_lf(s: &str) -> String {
    if s.contains('\r') {
        s.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        s.to_string()
    }
}

pub fn short_hash(lf_text: &str) -> String {
    let digest = Sha256::digest(lf_text.as_bytes());
    digest.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

pub fn canonical_file_key(path: &str) -> String {
    let trimmed = path.trim().trim_matches(['"', '\'']);
    let unified = trimmed.replace('\\', "/");
    let stripped = unified.strip_prefix("./").unwrap_or(&unified);
    if cfg!(windows) {
        stripped.to_ascii_lowercase()
    } else {
        stripped.to_string()
    }
}

fn make_entry_key(canon_path: &str, range: Option<(usize, usize)>) -> String {
    match range {
        Some((s, e)) => format!("{canon_path}#{s}:{e}"),
        None => canon_path.to_string(),
    }
}

pub fn is_source_code_path(path: &str) -> bool {
    let ext = path
        .rsplit(['.', '/', '\\'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "rs" | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "py"
            | "go"
            | "java"
            | "kt"
            | "kts"
            | "c"
            | "cc"
            | "cpp"
            | "h"
            | "hpp"
            | "cs"
            | "rb"
            | "swift"
            | "php"
            | "scala"
            | "zig"
            | "lua"
            | "sh"
            | "ps1"
            | "sql"
            | "toml"
            | "yaml"
            | "yml"
            | "md"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditOp {
    Equal(usize, usize),
    Delete(usize),
    Insert(usize),
}

fn diff_slice(old: &[&str], new: &[&str], old_base: usize, new_base: usize, out: &mut Vec<EditOp>) {
    let mut p = 0usize;
    while p < old.len() && p < new.len() && old[p] == new[p] {
        out.push(EditOp::Equal(old_base + p, new_base + p));
        p += 1;
    }
    let mut old_end = old.len();
    let mut new_end = new.len();
    while old_end > p && new_end > p && old[old_end - 1] == new[new_end - 1] {
        old_end -= 1;
        new_end -= 1;
    }

    let om = &old[p..old_end];
    let nm = &new[p..new_end];
    if !om.is_empty() || !nm.is_empty() {
        if om.is_empty() {
            for j in 0..nm.len() {
                out.push(EditOp::Insert(new_base + p + j));
            }
        } else if nm.is_empty() {
            for i in 0..om.len() {
                out.push(EditOp::Delete(old_base + p + i));
            }
        } else if om.len() <= 96 && nm.len() <= 96 {
            lcs_ops(om, nm, old_base + p, new_base + p, out);
        } else {
            anchor_or_block_ops(om, nm, old_base + p, new_base + p, out);
        }
    }

    let suffix_len = old.len() - old_end;
    for k in 0..suffix_len {
        out.push(EditOp::Equal(old_end + old_base + k, new_end + new_base + k));
    }
}

fn lcs_ops(om: &[&str], nm: &[&str], ob: usize, nb: usize, out: &mut Vec<EditOp>) {
    let m = om.len();
    let n = nm.len();
    let cols = n + 1;
    let mut dp = vec![0u16; (m + 1) * cols];
    for i in (0..m).rev() {
        for j in (0..n).rev() {
            dp[i * cols + j] = if om[i] == nm[j] {
                dp[(i + 1) * cols + j + 1] + 1
            } else {
                dp[(i + 1) * cols + j].max(dp[i * cols + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0usize, 0usize);
    while i < m && j < n {
        if om[i] == nm[j] {
            out.push(EditOp::Equal(ob + i, nb + j));
            i += 1;
            j += 1;
        } else if dp[(i + 1) * cols + j] >= dp[i * cols + j + 1] {
            out.push(EditOp::Delete(ob + i));
            i += 1;
        } else {
            out.push(EditOp::Insert(nb + j));
            j += 1;
        }
    }
    while i < m {
        out.push(EditOp::Delete(ob + i));
        i += 1;
    }
    while j < n {
        out.push(EditOp::Insert(nb + j));
        j += 1;
    }
}

fn anchor_or_block_ops(om: &[&str], nm: &[&str], ob: usize, nb: usize, out: &mut Vec<EditOp>) {
    let mut old_counts: HashMap<&str, (usize, usize)> = HashMap::new();
    for (idx, &l) in om.iter().enumerate() {
        if !l.trim().is_empty() {
            old_counts
                .entry(l)
                .and_modify(|e| e.0 += 1)
                .or_insert((1, idx));
        }
    }
    let mut new_counts: HashMap<&str, (usize, usize)> = HashMap::new();
    for (idx, &l) in nm.iter().enumerate() {
        if !l.trim().is_empty() {
            new_counts
                .entry(l)
                .and_modify(|e| e.0 += 1)
                .or_insert((1, idx));
        }
    }

    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for (oi, &l) in om.iter().enumerate() {
        if let (Some(&(1, _)), Some(&(1, ni))) = (old_counts.get(l), new_counts.get(l)) {
            pairs.push((oi, ni));
        }
    }

    let anchors = monotonic_anchors(&pairs);
    if anchors.is_empty() {
        for i in 0..om.len() {
            out.push(EditOp::Delete(ob + i));
        }
        for j in 0..nm.len() {
            out.push(EditOp::Insert(nb + j));
        }
        return;
    }

    let mut prev_o = 0usize;
    let mut prev_n = 0usize;
    for (ao, an) in anchors {
        diff_slice(
            &om[prev_o..ao],
            &nm[prev_n..an],
            ob + prev_o,
            nb + prev_n,
            out,
        );
        out.push(EditOp::Equal(ob + ao, nb + an));
        prev_o = ao + 1;
        prev_n = an + 1;
    }
    diff_slice(&om[prev_o..], &nm[prev_n..], ob + prev_o, nb + prev_n, out);
}

fn monotonic_anchors(pairs: &[(usize, usize)]) -> Vec<(usize, usize)> {
    if pairs.is_empty() {
        return Vec::new();
    }
    let mut tails: Vec<usize> = Vec::new();
    let mut prev_idx: Vec<Option<usize>> = vec![None; pairs.len()];
    for (k, &(_, ni)) in pairs.iter().enumerate() {
        let pos = tails.partition_point(|&idx| pairs[idx].1 < ni);
        if pos > 0 {
            prev_idx[k] = Some(tails[pos - 1]);
        }
        if pos == tails.len() {
            tails.push(k);
        } else {
            tails[pos] = k;
        }
    }
    let mut res = Vec::with_capacity(tails.len());
    let mut cur = tails.last().copied();
    while let Some(k) = cur {
        res.push(pairs[k]);
        cur = prev_idx[k];
    }
    res.reverse();
    res
}

pub fn unified_diff(old_lf: &str, new_lf: &str, line_offset: usize, context: usize) -> String {
    let old_lines: Vec<&str> = old_lf.lines().collect();
    let new_lines: Vec<&str> = new_lf.lines().collect();
    let mut ops = Vec::with_capacity(old_lines.len().max(new_lines.len()));
    diff_slice(&old_lines, &new_lines, 0, 0, &mut ops);

    let changed_indices: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter_map(|(idx, op)| (!matches!(op, EditOp::Equal(_, _))).then_some(idx))
        .collect();
    if changed_indices.is_empty() {
        return String::new();
    }

    let mut hunks: Vec<(usize, usize)> = Vec::new();
    for idx in changed_indices {
        let start = idx.saturating_sub(context);
        let end = (idx + context + 1).min(ops.len());
        match hunks.last_mut() {
            Some((_, prev_end)) if start <= *prev_end => {
                *prev_end = (*prev_end).max(end);
            }
            _ => hunks.push((start, end)),
        }
    }

    let mut out = Vec::new();
    for (hs, he) in hunks {
        let slice = &ops[hs..he];
        let mut old_start: Option<usize> = None;
        let mut new_start: Option<usize> = None;
        let mut old_count = 0usize;
        let mut new_count = 0usize;
        for &op in slice {
            match op {
                EditOp::Equal(oi, ni) => {
                    old_start.get_or_insert(oi);
                    new_start.get_or_insert(ni);
                    old_count += 1;
                    new_count += 1;
                }
                EditOp::Delete(oi) => {
                    old_start.get_or_insert(oi);
                    old_count += 1;
                }
                EditOp::Insert(ni) => {
                    new_start.get_or_insert(ni);
                    new_count += 1;
                }
            }
        }
        let os = old_start.unwrap_or(0) + 1 + line_offset;
        let ns = new_start.unwrap_or(0) + 1 + line_offset;
        out.push(format!("@@ -{os},{old_count} +{ns},{new_count} @@"));
        for &op in slice {
            match op {
                EditOp::Equal(oi, _) => out.push(format!(" {}", old_lines[oi])),
                EditOp::Delete(oi) => out.push(format!("-{}", old_lines[oi])),
                EditOp::Insert(ni) => out.push(format!("+{}", new_lines[ni])),
            }
        }
    }
    out.join("\n")
}

impl ReadCache {
    pub fn load(path: &Path) -> Self {
        let mut c: Self = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        c.prune(now_ts());
        c
    }

    pub fn save(&self, path: &Path) {
        if let Ok(bytes) = serde_json::to_vec(self) {
            let _ = fsx::atomic_write(path, &bytes);
        }
    }

    fn prune(&mut self, now: i64) {
        self.entries
            .retain(|_, v| now.saturating_sub(v.ts) <= ENTRY_TTL_SECS);
        while self.entries.len() > MAX_CACHE_ENTRIES
            || self.entries.values().map(|e| e.text.len()).sum::<usize>() > MAX_CACHE_BYTES
        {
            let Some(oldest_key) = self
                .entries
                .iter()
                .min_by_key(|(_, v)| (v.ts, v.seq))
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.entries.remove(&oldest_key);
        }
    }

    fn bump_seq(&mut self) -> u32 {
        self.next_seq = self.next_seq.saturating_add(1);
        if self.next_seq == 0 {
            self.next_seq = 1;
        }
        self.next_seq
    }

    pub fn record_baseline(
        &mut self,
        path: &str,
        content: &str,
        range: Option<(usize, usize)>,
        origin: SnapshotOrigin,
    ) {
        let canon = canonical_file_key(path);
        if canon.is_empty() {
            return;
        }
        let lf = normalize_lf(content);
        if lf.len() > MAX_CACHE_BYTES / 2 {
            return;
        }
        let seq = self.bump_seq();
        let hash = short_hash(&lf);
        let key = make_entry_key(&canon, range);
        self.entries.insert(
            key,
            ReadSnapshot {
                seq,
                hash,
                text: lf,
                range,
                origin,
                ts: now_ts(),
            },
        );
        self.prune(now_ts());
    }

    pub fn observe_read(
        &mut self,
        path: &str,
        raw_bytes: &[u8],
        range: Option<(usize, usize)>,
        tok: TokenizerKind,
        recall: Option<&RecallStore>,
        unicode: bool,
    ) -> Option<DedupHit> {
        let canon = canonical_file_key(path);
        if canon.is_empty() {
            return None;
        }
        let Ok(raw_str) = std::str::from_utf8(raw_bytes) else {
            return None;
        };
        if crate::compress::has_tinkr_marker(raw_str)
            || crate::overlap::has_foreign_marker(raw_bytes, &[])
        {
            return None;
        }

        let lf = normalize_lf(raw_str);
        let hash = short_hash(&lf);
        let cur_seq = self.bump_seq();
        let now = now_ts();
        let entry_key = make_entry_key(&canon, range);

        let before = tokenizer::count(raw_str, tok).tokens;

        if let Some(prev) = self.entries.get(&entry_key).cloned() {
            if prev.hash == hash {
                if lf.len() <= MAX_CACHE_BYTES / 2 {
                    self.entries.insert(
                        entry_key,
                        ReadSnapshot {
                            seq: prev.seq,
                            hash,
                            text: lf.clone(),
                            range,
                            origin: prev.origin,
                            ts: now,
                        },
                    );
                }
                if before < defaults::MIN_PAYLOAD_TOKENS {
                    return None;
                }
                let origin_label = match prev.origin {
                    SnapshotOrigin::Read => "read",
                    SnapshotOrigin::AgentEdit => "agent edit",
                };
                let line_count = lf.lines().count();
                let scope_note = match range {
                    Some((s, e)) => format!("lines {s}..{e} "),
                    None => String::new(),
                };
                let candidate = format!(
                    "unchanged {scope_note}since {origin_label} #{} [{}] ({line_count} lines)",
                    prev.seq, prev.hash
                );
                return finalize_hit(raw_bytes, before, &candidate, tok, recall, unicode);
            }

            let line_offset = range.map(|(s, _)| s.saturating_sub(1)).unwrap_or(0);
            let diff = unified_diff(&prev.text, &lf, line_offset, DIFF_CONTEXT);
            if lf.len() <= MAX_CACHE_BYTES / 2 {
                self.entries.insert(
                    entry_key,
                    ReadSnapshot {
                        seq: cur_seq,
                        hash: hash.clone(),
                        text: lf,
                        range,
                        origin: SnapshotOrigin::Read,
                        ts: now,
                    },
                );
                self.prune(now);
            }
            if before < defaults::MIN_PAYLOAD_TOKENS || diff.is_empty() {
                return None;
            }
            let origin_label = match prev.origin {
                SnapshotOrigin::Read => "read",
                SnapshotOrigin::AgentEdit => "agent edit",
            };
            let scope_note = match range {
                Some((s, e)) => format!("lines {s}..{e} "),
                None => String::new(),
            };
            let candidate = format!(
                "changed {scope_note}since {origin_label} #{} [{} -> {}]:\n{diff}",
                prev.seq, prev.hash, hash
            );
            return finalize_hit(raw_bytes, before, &candidate, tok, recall, unicode);
        }

        if let Some((start, end)) = range {
            if start >= 1 && end >= start {
                if let Some(full_prev) = self.entries.get(&canon).cloned() {
                    let full_lines: Vec<&str> = full_prev.text.lines().collect();
                    if start <= full_lines.len() {
                        let clamped_end = end.min(full_lines.len());
                        let expected_slice = full_lines[start - 1..clamped_end].join("\n");
                        let actual_trimmed = lf.trim_end_matches('\n');
                        if lf.len() <= MAX_CACHE_BYTES / 2 {
                            self.entries.insert(
                                entry_key,
                                ReadSnapshot {
                                    seq: cur_seq,
                                    hash: hash.clone(),
                                    text: lf.clone(),
                                    range,
                                    origin: SnapshotOrigin::Read,
                                    ts: now,
                                },
                            );
                            self.prune(now);
                        }
                        if before < defaults::MIN_PAYLOAD_TOKENS {
                            return None;
                        }
                        let origin_label = match full_prev.origin {
                            SnapshotOrigin::Read => "read",
                            SnapshotOrigin::AgentEdit => "agent edit",
                        };
                        if actual_trimmed == expected_slice {
                            let candidate = format!(
                                "unchanged lines {start}..{clamped_end} since {origin_label} #{} [{}]",
                                full_prev.seq, full_prev.hash
                            );
                            return finalize_hit(
                                raw_bytes, before, &candidate, tok, recall, unicode,
                            );
                        } else {
                            let diff = unified_diff(
                                &expected_slice,
                                actual_trimmed,
                                start - 1,
                                DIFF_CONTEXT,
                            );
                            if !diff.is_empty() {
                                let candidate = format!(
                                    "changed lines {start}..{clamped_end} since {origin_label} #{} [{} -> {}]:\n{diff}",
                                    full_prev.seq, full_prev.hash, hash
                                );
                                return finalize_hit(
                                    raw_bytes, before, &candidate, tok, recall, unicode,
                                );
                            }
                        }
                        return None;
                    }
                }
            }
        }

        if lf.len() <= MAX_CACHE_BYTES / 2 {
            self.entries.insert(
                entry_key,
                ReadSnapshot {
                    seq: cur_seq,
                    hash,
                    text: lf,
                    range,
                    origin: SnapshotOrigin::Read,
                    ts: now,
                },
            );
            self.prune(now);
        }
        None
    }
}

fn finalize_hit(
    raw_bytes: &[u8],
    before: u64,
    candidate: &str,
    tok: TokenizerKind,
    recall: Option<&RecallStore>,
    unicode: bool,
) -> Option<DedupHit> {
    let after = tokenizer::count(candidate, tok).tokens;
    if !defaults::worth_it(before, after + 14) {
        return None;
    }
    let (id, final_text, final_after) = match recall {
        Some(store) => {
            let id = store.put(raw_bytes).ok()?;
            let m = recall::marker(&id, before, after, unicode);
            let t = format!("{}\n{m}", candidate.trim_end());
            let fa = tokenizer::count(&t, tok).tokens;
            (Some(id), t, fa)
        }
        None => (None, candidate.to_string(), after),
    };
    if !defaults::worth_it(before, final_after) {
        return None;
    }
    Some(DedupHit {
        text: final_text,
        before,
        after: final_after,
        recall_id: id,
    })
}

pub fn extract_shell_read_path(cmd: &str) -> Option<String> {
    let segs = crate::filters::classify::command_segments(cmd)?;
    if segs.len() != 1 {
        return None;
    }
    let words: Vec<&str> = segs[0].split_whitespace().collect();
    if words.len() < 2 {
        return None;
    }
    let bin = words[0]
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(words[0])
        .trim_end_matches(".exe")
        .to_ascii_lowercase();
    if !matches!(
        bin.as_str(),
        "cat" | "type" | "get-content" | "gc" | "bat"
    ) {
        return None;
    }
    let mut path_arg: Option<&str> = None;
    let mut i = 1;
    while i < words.len() {
        let w = words[i];
        if w.eq_ignore_ascii_case("-path") || w.eq_ignore_ascii_case("-literalpath") {
            path_arg = words.get(i + 1).copied();
            i += 2;
            continue;
        }
        if w.eq_ignore_ascii_case("-raw") || w == "--plain" || w == "-p" {
            i += 1;
            continue;
        }
        if w.starts_with('-') {
            return None;
        }
        if path_arg.is_some() {
            return None;
        }
        path_arg = Some(w);
        i += 1;
    }
    path_arg.map(|p| p.trim_matches(['\'', '"']).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_large_file(edit_line: Option<(usize, &str)>, crlf: bool) -> String {
        let sep = if crlf { "\r\n" } else { "\n" };
        let mut lines = Vec::new();
        for i in 1..=200 {
            if let Some((ln, text)) = edit_line {
                if i == ln {
                    lines.push(text.to_string());
                    continue;
                }
            }
            lines.push(format!("    let state_step_{i:03} = compute_transition({i}, state_acc);"));
        }
        lines.join(sep) + sep
    }

    #[test]
    fn identical_reread_crlf_and_recall_byte_exact() {
        let dir = tempfile::tempdir().unwrap();
        let store = RecallStore::open(dir.path()).unwrap();
        let mut cache = ReadCache::default();

        let v1_lf = build_large_file(None, false);
        assert!(cache
            .observe_read(
                "src/engine.rs",
                v1_lf.as_bytes(),
                None,
                TokenizerKind::Exact,
                Some(&store),
                false
            )
            .is_none());

        let v1_crlf = build_large_file(None, true);
        let hit = cache
            .observe_read(
                "./src\\engine.rs",
                v1_crlf.as_bytes(),
                None,
                TokenizerKind::Exact,
                Some(&store),
                false,
            )
            .expect("CRLF re-read must deduplicate");
        assert!(hit.text.contains("unchanged since read #1 ["));
        assert!(hit.after < 40);
        let rid = hit.recall_id.unwrap();
        assert_eq!(store.get(&rid).unwrap(), v1_crlf.as_bytes());
    }

    #[test]
    fn changed_reread_emits_unified_diff_with_exact_edit() {
        let dir = tempfile::tempdir().unwrap();
        let store = RecallStore::open(dir.path()).unwrap();
        let mut cache = ReadCache::default();

        let v1 = build_large_file(None, false);
        assert!(cache
            .observe_read("src/a.rs", v1.as_bytes(), None, TokenizerKind::Exact, Some(&store), false)
            .is_none());

        let v2 = build_large_file(Some((105, "    let retry_limit = 999; // needle_body_edit")), false);
        let hit = cache
            .observe_read("src/a.rs", v2.as_bytes(), None, TokenizerKind::Exact, Some(&store), false)
            .expect("changed re-read must emit unified diff");
        assert!(hit.text.contains("changed since read #1 ["));
        assert!(hit.text.contains("+    let retry_limit = 999; // needle_body_edit"));
        assert!(hit.text.contains("@@ -103,5 +103,5 @@"));
        assert!(hit.after < hit.before / 5);
    }

    #[test]
    fn partial_slice_reread_and_agent_write_dedup() {
        let dir = tempfile::tempdir().unwrap();
        let store = RecallStore::open(dir.path()).unwrap();
        let mut cache = ReadCache::default();

        let v1 = build_large_file(None, false);
        assert!(cache
            .observe_read("src/lib.rs", v1.as_bytes(), None, TokenizerKind::Exact, Some(&store), false)
            .is_none());

        let slice_lines: Vec<&str> = v1.lines().skip(49).take(61).collect();
        let slice_text = slice_lines.join("\n");
        let slice_hit = cache
            .observe_read(
                "src/lib.rs",
                slice_text.as_bytes(),
                Some((50, 110)),
                TokenizerKind::Exact,
                Some(&store),
                false,
            )
            .expect("covered partial slice must deduplicate");
        assert!(slice_hit.text.contains("unchanged lines 50..110 since read #1"));

        let written = build_large_file(Some((10, "pub fn brand_new() {}")), false);
        cache.record_baseline("src/new_mod.rs", &written, None, SnapshotOrigin::AgentEdit);
        let write_hit = cache
            .observe_read(
                "src/new_mod.rs",
                written.as_bytes(),
                None,
                TokenizerKind::Exact,
                Some(&store),
                false,
            )
            .expect("immediate read after agent write must deduplicate");
        assert!(write_hit.text.contains("unchanged since agent edit #"));
    }
}
