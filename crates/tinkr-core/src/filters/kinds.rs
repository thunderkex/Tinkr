use super::{
    catalog::{CatalogRule, Strategy},
    classify::Kind,
    verify,
};
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

pub fn apply_catalog(rule: &CatalogRule, text: &str, level: u8) -> String {
    if level <= 1 {
        return generic(text, false);
    }
    let prefiltered = if rule.drop_set().is_some() || rule.raw.drop_pass_lines {
        let dset = rule.drop_set();
        let kset = rule.keep_set();
        let label = if rule.raw.omitted_label.is_empty() {
            "progress lines"
        } else {
            rule.raw.omitted_label.as_str()
        };
        drop_lines(
            text,
            |l| {
                let explicitly_kept = kset.is_some_and(|ks| ks.is_match(l));
                if explicitly_kept {
                    return false;
                }
                (rule.raw.drop_pass_lines && verify::is_pass_line(l))
                    || dset.is_some_and(|ds| ds.is_match(l))
            },
            label,
        )
    } else {
        text.to_string()
    };

    match rule.raw.strategy {
        Strategy::GitStatus => generic(&git_status(&prefiltered), true),
        Strategy::GitLog => {
            if let Some((rendered, has_patches)) = git_log(&prefiltered) {
                generic(&rendered, !has_patches)
            } else {
                generic(&prefiltered, true)
            }
        }
        Strategy::GitDiff => generic(&git_diff(&prefiltered), true),
        Strategy::ListDir => {
            if let Some(rendered) = list_dir(&prefiltered) {
                let truncate = rendered.lines().count() > 1000;
                generic(&rendered, truncate)
            } else {
                generic(&prefiltered, true)
            }
        }
        Strategy::FindGroup => {
            let s = find_group(&prefiltered).unwrap_or(prefiltered);
            generic(&s, true)
        }
        Strategy::GrepGroup => {
            let s = grep_group(&prefiltered).unwrap_or(prefiltered);
            generic(&s, true)
        }
        Strategy::DropLines => generic(&prefiltered, true),
        Strategy::TableSqueeze => generic(&table_squeeze(&prefiltered), true),
        Strategy::AutoStructured => apply_auto_structured(&prefiltered),
        Strategy::FileRead => {
            if crate::outliers::looks_like_logs(&prefiltered) {
                crate::outliers::dedup_lines_preserving_outliers(&prefiltered)
            } else {
                generic(&prefiltered, false)
            }
        }
    }
}

fn apply_auto_structured(text: &str) -> String {
    let trimmed = text.trim();
    if ((trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']')))
        && serde_json::from_str::<Value>(trimmed).is_ok()
    {
        if let Some(c) = crate::compress::compress_tool_result(
            trimmed,
            crate::tokenizer::TokenizerKind::Exact,
            None,
            false,
        ) {
            return c.text;
        }
    }
    if crate::outliers::looks_like_logs(text) {
        return crate::outliers::dedup_lines_preserving_outliers(text);
    }
    generic(&table_squeeze(text), true)
}

pub fn apply(kind: Kind, text: &str, level: u8) -> String {
    if level <= 1 {
        return generic(text, false);
    }
    match kind {
        Kind::GitLog => {
            if let Some((rendered, has_patches)) = git_log(text) {
                return generic(&rendered, !has_patches);
            }
            return generic(text, true);
        }
        Kind::ListDir => {
            if let Some(rendered) = list_dir(text) {
                let truncate = rendered.lines().count() > 1000;
                return generic(&rendered, truncate);
            }
            return generic(text, true);
        }
        _ => {}
    }
    let specific: Option<String> = match kind {
        Kind::GitStatus => Some(git_status(text)),
        Kind::GitLog | Kind::ListDir => unreachable!(),
        Kind::GitDiff => Some(git_diff(text)),
        Kind::Find => find_group(text),
        Kind::Grep => grep_group(text),
        Kind::Test => Some(drop_lines(
            text,
            |l| verify::is_pass_line(l) || TEST_NOISE.is_match(l),
            "passing/progress lines",
        )),
        Kind::Build => Some(drop_lines(
            text,
            |l| BUILD_NOISE.is_match(l),
            "progress lines",
        )),
        Kind::GitOther => Some(drop_lines(
            text,
            |l| GIT_NOISE.is_match(l),
            "progress lines",
        )),
        Kind::Docker | Kind::Kube => {
            if crate::outliers::looks_like_logs(text) {
                return crate::outliers::dedup_lines_preserving_outliers(text);
            }
            Some(table_squeeze(text))
        }
        Kind::FileRead => return generic(text, false),
        Kind::Generic => {
            if crate::outliers::looks_like_logs(text) {
                return crate::outliers::dedup_lines_preserving_outliers(text);
            }
            None
        }
    };
    generic(&specific.unwrap_or_else(|| text.to_string()), true)
}

static TEST_NOISE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(running \d+ tests?|=== RUN\b.*|=== (PAUSE|CONT)\b.*|\.+|[.sxFE]{10,}|collecting \.\.\..*|platform .*|rootdir:.*|plugins:.*|cachedir:.*|configfile:.*)\s*$").unwrap()
});
static BUILD_NOISE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\s*(Compiling|Downloaded|Downloading|Fresh|Checking|Locking|Updating|Adding|Blocking|Packaging|Verifying|Unpacking|Installing|Fetching)\s|^npm (http|timing|sill|info)\b|^\s*(Collecting|Using cached|Requirement already satisfied|Obtaining|Preparing metadata|Building wheel|Installing collected)|^#\d+ (CACHED|\[|DONE|sha256|resolve|transferring|exporting)|^make\[\d+\]: (Entering|Leaving)|^\s*(Determining projects to restore|Restored\s|Restore complete)|^\s*(Progress|Download|Unpacking)\b.*\d+%|^go: (downloading|finding)\b|^\s*Compiling\b",
    )
    .unwrap()
});
static GIT_NOISE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(remote: (Counting|Compressing|Enumerating|Total)|Receiving objects|Resolving deltas|Unpacking objects|Counting objects|Compressing objects|Writing objects|Enumerating objects|Delta compression|Total \d+)").unwrap()
});

fn drop_lines(text: &str, drop: impl Fn(&str) -> bool, what: &str) -> String {
    let mut out = Vec::new();
    let mut n = 0usize;
    let mut note_at: Option<usize> = None;
    for l in text.lines() {
        if drop(l) && !verify::is_critical(l) {
            if note_at.is_none() {
                note_at = Some(out.len());
                out.push(String::new());
            }
            n += 1;
        } else {
            out.push(l.to_string());
        }
    }
    if let Some(i) = note_at {
        out[i] = format!("[{n} {what} omitted]");
    }
    out.join("\n")
}

pub fn generic(text: &str, truncate: bool) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut prev: Option<String> = None;
    let mut run = 0usize;
    let mut blank = false;
    let flush = |out: &mut Vec<String>, prev: &mut Option<String>, run: &mut usize| {
        if let Some(p) = prev.take() {
            if *run > 1 {
                out.push(format!("{p} (x{run})"));
            } else {
                out.push(p);
            }
        }
        *run = 0;
    };
    for l in text.lines() {
        let l = l.trim_end();
        if l.is_empty() {
            flush(&mut out, &mut prev, &mut run);
            if !blank && !out.is_empty() {
                out.push(String::new());
            }
            blank = true;
            continue;
        }
        blank = false;
        if prev.as_deref() == Some(l) {
            run += 1;
        } else {
            flush(&mut out, &mut prev, &mut run);
            prev = Some(l.to_string());
            run = 1;
        }
    }
    flush(&mut out, &mut prev, &mut run);
    while out.last().map(|l| l.is_empty()).unwrap_or(false) {
        out.pop();
    }
    const HEAD: usize = 80;
    const TAIL: usize = 80;
    if truncate && out.len() > HEAD + TAIL + 40 {
        return crate::outliers::dedup_lines_preserving_outliers(&out.join("\n"));
    }
    out.join("\n")
}

fn git_status(text: &str) -> String {
    let mut out = Vec::new();
    for l in text.lines() {
        let t = l.trim();
        if t.starts_with("(use \"git") || t.starts_with("(use 'git") || t.is_empty() {
            continue;
        }
        if let Some(rest) = l.strip_prefix('\t') {
            if let Some((k, v)) = rest.split_once(':') {
                let code = match k.trim() {
                    "modified" => "M",
                    "new file" => "A",
                    "deleted" => "D",
                    "renamed" => "R",
                    "copied" => "C",
                    "typechange" => "T",
                    other => other,
                };
                out.push(format!("  {code} {}", v.trim()));
                continue;
            }
            out.push(format!("  {}", rest.trim_end()));
            continue;
        }
        out.push(l.trim_end().to_string());
    }
    out.join("\n")
}

fn is_lockfile(path: &str) -> bool {
    let fname = path.rsplit(['/', '\\']).next().unwrap_or(path);
    matches!(
        fname,
        "Cargo.lock"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "go.sum"
            | "composer.lock"
            | "Gemfile.lock"
            | "poetry.lock"
            | "uv.lock"
    )
}

fn git_log(text: &str) -> Option<(String, bool)> {
    if !text.starts_with("commit ") {
        return None;
    }
    let has_patches = text.contains("\ndiff --git ")
        || text.contains("\n@@ ")
        || text.contains(" files changed")
        || text.contains(" file changed");

    let mut out = Vec::new();
    let (mut hash, mut author, mut date, mut subject): (String, String, String, Option<String>) =
        Default::default();
    let mut patch_lines: Vec<String> = Vec::new();
    let mut cur_lockfile: Option<String> = None;
    let mut lockfile_omitted = 0usize;
    let mut in_hunk = false;
    let mut have = false;

    let flush_lockfile =
        |patch_lines: &mut Vec<String>, cur_lockfile: &Option<String>, omitted: &mut usize| {
            if *omitted > 0 {
                if let Some(lf) = cur_lockfile {
                    patch_lines.push(format!("    [{} lockfile diff lines omitted in {lf}]", *omitted));
                }
                *omitted = 0;
            }
        };

    let emit = |out: &mut Vec<String>,
                h: &str,
                a: &str,
                d: &str,
                s: &Option<String>,
                patches: &mut Vec<String>| {
        let short_hash: String = h.chars().take(7).collect();
        if has_patches {
            out.push(format!(
                "{short_hash} {} ({a})",
                s.as_deref().unwrap_or("(no message)")
            ));
            out.append(patches);
        } else {
            out.push(format!(
                "{short_hash} {} ({a}, {d})",
                s.as_deref().unwrap_or("(no message)")
            ));
        }
    };

    for l in text.lines() {
        if let Some(h) = l.strip_prefix("commit ") {
            if have {
                flush_lockfile(&mut patch_lines, &cur_lockfile, &mut lockfile_omitted);
                emit(&mut out, &hash, &author, &date, &subject, &mut patch_lines);
            }
            hash = h.split_whitespace().next().unwrap_or("").to_string();
            author.clear();
            date.clear();
            subject = None;
            cur_lockfile = None;
            lockfile_omitted = 0;
            in_hunk = false;
            have = true;
        } else if let Some(a) = l.strip_prefix("Author:") {
            author = a.trim().split(" <").next().unwrap_or("").to_string();
        } else if let Some(d) = l.strip_prefix("Date:") {
            date = d.trim().to_string();
        } else if l.starts_with("    ") && subject.is_none() && !l.trim().is_empty() {
            subject = Some(l.trim().to_string());
        } else if has_patches {
            if let Some(rest) = l.strip_prefix("diff --git ") {
                flush_lockfile(&mut patch_lines, &cur_lockfile, &mut lockfile_omitted);
                let path = rest
                    .split_whitespace()
                    .nth(1)
                    .and_then(|b| b.strip_prefix("b/").or(Some(b)))
                    .unwrap_or(rest);
                cur_lockfile = is_lockfile(path).then(|| path.to_string());
                in_hunk = false;
                patch_lines.push(format!("  @ {path}"));
            } else if l.starts_with("@@ ") {
                in_hunk = true;
            } else if l.starts_with("new file")
                || l.starts_with("deleted file")
                || l.starts_with("rename ")
                || l.starts_with("Binary files")
            {
                patch_lines.push(format!("    {}", l.trim_end()));
            } else if in_hunk && (l.starts_with('+') || l.starts_with('-'))
                && !l.starts_with("+++ ")
                && !l.starts_with("--- ")
            {
                if cur_lockfile.is_some() && !verify::is_critical(l) {
                    lockfile_omitted += 1;
                } else {
                    patch_lines.push(l.trim_end().to_string());
                }
            } else if !in_hunk && (l.contains(" | ") || l.contains(" files changed") || l.contains(" file changed")) {
                patch_lines.push(format!("  {}", l.trim()));
            }
        }
    }
    if have {
        flush_lockfile(&mut patch_lines, &cur_lockfile, &mut lockfile_omitted);
        emit(&mut out, &hash, &author, &date, &subject, &mut patch_lines);
    }
    Some((out.join("\n"), has_patches))
}

fn git_diff(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut keep = vec![false; lines.len()];
    let mut in_hunk = false;
    for (i, l) in lines.iter().enumerate() {
        if l.starts_with("diff --git") || l.starts_with("@@") {
            keep[i] = true;
            in_hunk = l.starts_with("@@");
        } else if l.starts_with("--- ")
            || l.starts_with("+++ ")
            || l.starts_with("rename ")
            || l.starts_with("new file")
            || l.starts_with("deleted file")
            || l.starts_with("Binary files")
        {
            keep[i] = true;
        } else if l.starts_with("index ") || l.starts_with("similarity ") {
        } else if in_hunk && (l.starts_with('+') || l.starts_with('-') || l.starts_with('\\')) {
            keep[i] = true;
            if i > 0 && !lines[i - 1].starts_with(['+', '-', '@']) {
                keep[i - 1] = true;
            }
            if i + 1 < lines.len() && lines[i + 1].starts_with(' ') {
                keep[i + 1] = true;
            }
        } else if !in_hunk {
            keep[i] = true;
        }
    }
    lines
        .iter()
        .zip(keep)
        .filter(|(_, k)| *k)
        .map(|(l, _)| *l)
        .collect::<Vec<_>>()
        .join("\n")
}

static LS_LONG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([-dlbcps])[rwxsStT-]{9}[.+@]?\s+\d+\s+\S+\s+\S+\s+(\d+)\s+\w{3}\s+\d+\s+(?:\d{1,2}:\d{2}|\d{4})\s+(.+)$").unwrap()
});
static PS_ROW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([-darhls]{4,6})\s+\d{1,4}[-/.]\d{1,2}[-/.]\d{1,4}\s+\d{1,2}:\d{2}(?::\d{2})?(?:\s?[AP]M)?\s+(\d+\s+)?(.+)$").unwrap()
});
static DIR_ROW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\d{1,4}[-/.]\d{1,2}[-/.]\d{1,4}\s+\d{1,2}:\d{2}(?:\s?[AP]M)?\s+(<DIR>|<JUNCTION>|<SYMLINKD?>|[\d,\.]+)\s+(.+)$").unwrap()
});

fn human(n: u64) -> String {
    match n {
        0..=999 => format!("{n}B"),
        1000..=999_999 => format!("{:.1}K", n as f64 / 1000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

fn is_noisy_dir_path(dir_path: &str) -> bool {
    dir_path.split(['/', '\\']).any(|seg| {
        matches!(
            seg.trim_end_matches(':'),
            "node_modules"
                | ".git"
                | "target"
                | ".next"
                | ".nuxt"
                | "dist"
                | "build"
                | ".venv"
                | "venv"
                | "__pycache__"
                | "vendor"
                | ".turbo"
                | "coverage"
        )
    })
}

fn list_dir(text: &str) -> Option<String> {
    let mut out = Vec::new();
    let (mut parsed, mut total) = (0usize, 0usize);
    let mut in_noisy_dir = false;
    let mut noisy_omitted = 0usize;
    let mut noisy_note_idx: Option<usize> = None;

    for l in text.lines() {
        let t = l.trim_end();
        if t.is_empty() {
            continue;
        }
        if t.starts_with("total ")
            || t.starts_with("Mode ")
            || t.starts_with("----")
            || t.trim_start().starts_with("Directory:")
            || t.contains("Volume ")
            || t.contains("Directory of")
            || t.contains("File(s)")
            || t.contains("Dir(s)")
        {
            if t.trim_start().starts_with("Directory:") || t.contains("Directory of") {
                let dpath = t
                    .trim()
                    .trim_start_matches("Directory:")
                    .trim()
                    .trim_start_matches("Directory of")
                    .trim();
                in_noisy_dir = is_noisy_dir_path(dpath);
                if !in_noisy_dir {
                    out.push(format!("{dpath}:"));
                } else if noisy_note_idx.is_none() {
                    noisy_note_idx = Some(out.len());
                    out.push(String::new());
                }
            }
            continue;
        }
        if let Some(dpath) = t.strip_suffix(':') {
            if !dpath.contains(' ') && (dpath.contains('/') || dpath.contains('\\') || dpath.starts_with('.')) {
                in_noisy_dir = is_noisy_dir_path(dpath);
                if !in_noisy_dir {
                    out.push(t.to_string());
                } else if noisy_note_idx.is_none() {
                    noisy_note_idx = Some(out.len());
                    out.push(String::new());
                }
                continue;
            }
        }
        total += 1;
        let formatted = if let Some(c) = LS_LONG.captures(t) {
            parsed += 1;
            let size: u64 = c[2].parse().unwrap_or(0);
            if &c[1] == "d" {
                format!("{}/", &c[3])
            } else {
                format!("{} {}", &c[3], human(size))
            }
        } else if let Some(c) = PS_ROW.captures(t) {
            parsed += 1;
            let name = c[3].trim();
            if c[1].starts_with('d') {
                format!("{name}/")
            } else {
                let size: u64 = c
                    .get(2)
                    .and_then(|m| m.as_str().trim().parse().ok())
                    .unwrap_or(0);
                format!("{name} {}", human(size))
            }
        } else if let Some(c) = DIR_ROW.captures(t) {
            parsed += 1;
            let name = c[2].trim();
            if c[1].starts_with('<') {
                format!("{name}/")
            } else {
                let size: u64 = c[1].replace([',', '.'], "").parse().unwrap_or(0);
                format!("{name} {}", human(size))
            }
        } else {
            t.to_string()
        };
        if in_noisy_dir && !verify::is_critical(&formatted) {
            noisy_omitted += 1;
        } else {
            out.push(formatted);
        }
    }
    if let Some(idx) = noisy_note_idx {
        if noisy_omitted > 0 {
            out[idx] = format!("[{noisy_omitted} entries in third-party/build dirs omitted]");
        } else {
            out.remove(idx);
        }
    }
    if total == 0 || parsed * 100 / total.max(1) < 50 {
        return None;
    }
    Some(out.join("\n"))
}

fn find_group(text: &str) -> Option<String> {
    let paths: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .collect();
    if paths.len() < 8 || !paths.iter().all(|p| p.contains('/') || p.contains('\\')) {
        return None;
    }
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for p in paths {
        let (dir, name) = match p.rsplit_once(['/', '\\']) {
            Some((d, n)) => (d.to_string(), n.to_string()),
            None => (String::new(), p.to_string()),
        };
        match groups.last_mut() {
            Some((d, v)) if *d == dir => v.push(name),
            _ => groups.push((dir, vec![name])),
        }
    }
    let sep = if text.contains('\\') && !text.contains('/') {
        '\\'
    } else {
        '/'
    };
    Some(
        groups
            .into_iter()
            .map(|(d, v)| format!("{d}{sep}: {}", v.join(" ")))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

static GREP_HIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+?):(\d+):(?:\d+:)?(.*)$").unwrap());

fn grep_group(text: &str) -> Option<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur: Option<String> = None;
    let mut hits = 0usize;
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    for l in &lines {
        if let Some(c) = GREP_HIT.captures(l) {
            hits += 1;
            if cur.as_deref() != Some(&c[1]) {
                out.push(c[1].to_string());
                cur = Some(c[1].to_string());
            }
            let body = c[3].trim();
            let body: String = if body.chars().count() > 240 && !verify::is_critical(body) {
                format!("{}…", body.chars().take(240).collect::<String>())
            } else {
                body.to_string()
            };
            out.push(format!("  {}: {body}", &c[2]));
        } else {
            out.push((*l).to_string());
        }
    }
    if hits * 100 / lines.len().max(1) < 60 {
        return None;
    }
    Some(out.join("\n"))
}

fn table_squeeze(text: &str) -> String {
    static GAP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r" {2,}").unwrap());
    text.lines()
        .map(|l| GAP.replace_all(l.trim_end(), "\t").into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_log_default_becomes_one_line_per_commit() {
        let t = "commit 1234567890abcdef1234567890abcdef12345678\nAuthor: Ann <a@x.io>\nDate:   Mon Nov 3 10:00:00 2026 +0700\n\n    Fix thing\n\n    Longer body\n    more body\n\ncommit abcdef1234567890abcdef1234567890abcdef12\nAuthor: Bob <b@x.io>\nDate:   Sun Nov 2 09:00:00 2026 +0700\n\n    Add other\n";
        let (o, has_patches) = git_log(t).unwrap();
        assert!(!has_patches);
        assert_eq!(o.lines().count(), 2);
        assert!(o.starts_with("1234567 Fix thing (Ann,"));
    }

    #[test]
    fn git_log_with_patches_preserves_diffs_and_schema_changes() {
        let mut t = String::new();
        for i in 0..150 {
            t.push_str(&format!(
                "commit {i:040x}\nAuthor: Dev <d@x.io>\nDate: Tue Nov 24 10:00:00 2026 +0000\n\n    Change {i}\n\ndiff --git a/src/f{i}.sql b/src/f{i}.sql\nindex 111..222 100644\n--- a/src/f{i}.sql\n+++ b/src/f{i}.sql\n@@ -1,3 +1,4 @@\n SELECT 1;\n"
            ));
            if i == 75 {
                t.push_str("+ALTER TABLE accounts ADD COLUMN tax_id TEXT;\n");
            } else {
                t.push_str(&format!("+SELECT {i};\n"));
            }
        }
        let o = apply(Kind::GitLog, &t, 2);
        assert!(o.contains("+ALTER TABLE accounts ADD COLUMN tax_id TEXT;"));
        assert!(o.contains("@ src/f75.sql"));
    }

    #[test]
    fn list_dir_collapses_node_modules_and_preserves_src() {
        let mut ps = String::new();
        for i in 0..120 {
            ps.push_str(&format!(
                "\n    Directory: C:\\app\\node_modules\\pkg_{i}\n\nMode                 LastWriteTime         Length Name\n----                 -------------         ------ ----\n-a----        11/23/2026   2:41 PM           1234 index.js\n"
            ));
        }
        ps.push_str("\n    Directory: C:\\app\\src\n\nMode                 LastWriteTime         Length Name\n----                 -------------         ------ ----\n-a----        11/23/2026   2:41 PM           2048 main_entry.ts\n");
        for i in 120..240 {
            ps.push_str(&format!(
                "\n    Directory: C:\\app\\node_modules\\zpkg_{i}\n\nMode                 LastWriteTime         Length Name\n----                 -------------         ------ ----\n-a----        11/23/2026   2:41 PM           1234 index.js\n"
            ));
        }
        let o = apply(Kind::ListDir, &ps, 2);
        assert!(o.contains("main_entry.ts"), "{o}");
        assert!(o.contains("entries in third-party/build dirs omitted"), "{o}");
        assert!(!o.contains("index.js"));
    }

    #[test]
    fn ls_long_ps_and_dir_parse() {
        let ls = "total 8\ndrwxr-xr-x  2 u g 4096 Nov  3 10:00 src\n-rw-r--r--  1 u g 1234 Nov  3 10:00 a.rs\n";
        assert_eq!(list_dir(ls).unwrap(), "src/\na.rs 1.2K");
        let ps = "\n    Directory: C:\\x\n\nMode                 LastWriteTime         Length Name\n----                 -------------         ------ ----\nd-----        11/23/2026   2:41 PM                crates\n-a----        11/23/2026   2:41 PM           1234 Cargo.toml\n";
        let o = list_dir(ps).unwrap();
        assert!(
            o.contains("crates/") && o.contains("Cargo.toml 1.2K"),
            "{o}"
        );
        let dir = "11/23/2026  02:41 PM    <DIR>          crates\n11/23/2026  02:41 PM             1,234 Cargo.toml\n";
        assert_eq!(list_dir(dir).unwrap(), "crates/\nCargo.toml 1.2K");
    }

    #[test]
    fn grep_groups_by_file() {
        let t = "src/a.rs:10:foo\nsrc/a.rs:20:bar\nsrc/b.rs:5:baz\n";
        assert_eq!(
            grep_group(t).unwrap(),
            "src/a.rs\n  10: foo\n  20: bar\nsrc/b.rs\n  5: baz"
        );
    }

    #[test]
    fn test_filter_keeps_failures_verbatim() {
        let mut t = String::from("running 3 tests\ntest a ... ok\ntest b ... ok\ntest c ... FAILED\n\nfailures:\n\n---- c stdout ----\nthread 'c' panicked at src/x.rs:3:9:\nboom\n\ntest result: FAILED. 2 passed; 1 failed\n");
        let o = apply(Kind::Test, &t, 2);
        assert!(
            o.contains("panicked at src/x.rs:3:9")
                && o.contains("test c ... FAILED")
                && o.contains("2 passed; 1 failed")
        );
        assert!(!o.contains("test a ... ok"));
        t.clear();
    }

    #[test]
    fn generic_dedupes_and_truncates_keeping_errors() {
        let mut t = String::new();
        for i in 0..400 {
            if i == 200 {
                t.push_str("error: needle in the middle\n");
            }
            t.push_str(&format!("line {i}\n"));
        }
        let o = generic(&t, true);
        assert!(o.contains("error: needle in the middle"));
        assert!(o.lines().count() < 200);
        assert_eq!(generic("a\na\na\n\n\n\nb\n", false), "a (x3)\n\nb");
    }

    #[test]
    fn git_diff_drops_index_and_far_context() {
        let t = "diff --git a/x b/x\nindex 111..222 100644\n--- a/x\n+++ b/x\n@@ -1,9 +1,9 @@\n c1\n c2\n c3\n c4\n-old\n+new\n c5\n c6\n c7\n c8\n";
        let o = git_diff(t);
        assert!(
            !o.contains("index 111")
                && !o.contains("c1")
                && o.contains("-old")
                && o.contains("c4")
                && o.contains("c5")
                && !o.contains("c7")
        );
    }
}
