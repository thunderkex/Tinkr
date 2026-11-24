use super::{classify::Kind, verify};
use regex::Regex;
use std::sync::LazyLock;

pub fn apply(kind: Kind, text: &str, level: u8) -> String {
    if level <= 1 {
        return generic(text, false);
    }
    let specific: Option<String> = match kind {
        Kind::GitStatus => Some(git_status(text)),
        Kind::GitLog => git_log(text),
        Kind::GitDiff => Some(git_diff(text)),
        Kind::ListDir => list_dir(text),
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
        Kind::Docker | Kind::Kube => Some(table_squeeze(text)),
        Kind::FileRead => return generic(text, false),
        Kind::Generic => None,
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
        let mid = &out[HEAD..out.len() - TAIL];
        let keep: Vec<&String> = mid
            .iter()
            .filter(|l| verify::is_critical(l) || !verify::critical_lines(l).is_empty())
            .collect();
        let omitted = mid.len() - keep.len();
        let mut r: Vec<String> = out[..HEAD].to_vec();
        r.push(format!("[{omitted} lines omitted]"));
        r.extend(keep.into_iter().cloned());
        r.extend_from_slice(&out[out.len() - TAIL..]);
        return r.join("\n");
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
                out.push(format!("  {}: {}", k.trim(), v.trim()));
                continue;
            }
            out.push(format!("  {}", rest.trim_end()));
            continue;
        }
        out.push(l.trim_end().to_string());
    }
    out.join("\n")
}

fn git_log(text: &str) -> Option<String> {
    if !text.starts_with("commit ") {
        return None;
    }
    let mut out = Vec::new();
    let (mut hash, mut author, mut date, mut subject): (String, String, String, Option<String>) =
        Default::default();
    let mut have = false;
    let emit = |out: &mut Vec<String>, h: &str, a: &str, d: &str, s: &Option<String>| {
        let short_hash: String = h.chars().take(7).collect();
        out.push(format!(
            "{short_hash} {} ({a}, {d})",
            s.as_deref().unwrap_or("(no message)")
        ));
    };
    for l in text.lines() {
        if let Some(h) = l.strip_prefix("commit ") {
            if have {
                emit(&mut out, &hash, &author, &date, &subject);
            }
            hash = h.split_whitespace().next().unwrap_or("").to_string();
            author.clear();
            date.clear();
            subject = None;
            have = true;
        } else if let Some(a) = l.strip_prefix("Author:") {
            author = a.trim().split(" <").next().unwrap_or("").to_string();
        } else if let Some(d) = l.strip_prefix("Date:") {
            date = d.trim().to_string();
        } else if l.starts_with("    ") && subject.is_none() && !l.trim().is_empty() {
            subject = Some(l.trim().to_string());
        }
    }
    if have {
        emit(&mut out, &hash, &author, &date, &subject);
    }
    Some(out.join("\n"))
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

fn list_dir(text: &str) -> Option<String> {
    let mut out = Vec::new();
    let (mut parsed, mut total) = (0usize, 0usize);
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
                out.push(format!(
                    "{}:",
                    t.trim()
                        .trim_start_matches("Directory:")
                        .trim()
                        .trim_start_matches("Directory of")
                        .trim()
                ));
            }
            continue;
        }
        total += 1;
        if let Some(c) = LS_LONG.captures(t) {
            parsed += 1;
            let size: u64 = c[2].parse().unwrap_or(0);
            if &c[1] == "d" {
                out.push(format!("{}/", &c[3]))
            } else {
                out.push(format!("{} {}", &c[3], human(size)))
            }
        } else if let Some(c) = PS_ROW.captures(t) {
            parsed += 1;
            let name = c[3].trim();
            if c[1].starts_with('d') {
                out.push(format!("{name}/"))
            } else {
                let size: u64 = c
                    .get(2)
                    .and_then(|m| m.as_str().trim().parse().ok())
                    .unwrap_or(0);
                out.push(format!("{name} {}", human(size)))
            }
        } else if let Some(c) = DIR_ROW.captures(t) {
            parsed += 1;
            let name = c[2].trim();
            if c[1].starts_with('<') {
                out.push(format!("{name}/"))
            } else {
                let size: u64 = c[1].replace([',', '.'], "").parse().unwrap_or(0);
                out.push(format!("{name} {}", human(size)))
            }
        } else {
            out.push(t.to_string());
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
        let o = git_log(t).unwrap();
        assert_eq!(o.lines().count(), 2);
        assert!(o.starts_with("1234567 Fix thing (Ann,"));
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
