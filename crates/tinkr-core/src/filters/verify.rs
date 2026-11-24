use regex::Regex;
use std::sync::LazyLock;

static CRIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?ix)
        (?:^|[^a-zA-Z0-9])(error|errors|fail|failed|failure|failures|panic|panicked|exception|traceback|fatal|denied|
           cannot|unable|segfault|timeout|timed\ out|warning|warn|assertion|aborted|abort|refused|
           unresolved|undefined|conflict|rejected|not\ found|no\ such|critical|anomaly|corrupt|
           invalid|oom|oomkilled|crashloopbackoff|unauthorized|forbidden|unexpected|mismatch|broken)(?:$|[^a-zA-Z0-9])
        | \bE\d{4}\b
        | ^\s*at\s+\S+.*:\d+
        | ^\s*File\s+".*",\ line\ \d+
        | ✗ | ✘ | ❌
        "#,
    )
    .unwrap()
});

static FILE_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:[A-Za-z]:)?[\w./\\@-]+\.\w{1,6}:\d+(?::\d+)?").unwrap());

static PASS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(\.\.\. ok$|\bPASSED\b|^\s*PASS\b|--- PASS|^\s*ok\s+\S+|✓|✔|^\s*test .* \.\.\. ok)",
    )
    .unwrap()
});

static GREP_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+?):(\d+):(?:\d+:)?(.*)$").unwrap());

pub fn is_pass_line(l: &str) -> bool {
    PASS.is_match(l) && !l.contains("FAILED") && !l.to_lowercase().contains("fail")
}

pub fn is_critical(l: &str) -> bool {
    if l.trim().is_empty() || is_pass_line(l) {
        return false;
    }
    CRIT.is_match(l)
}

pub fn critical_lines(text: &str) -> Vec<&str> {
    let mut v: Vec<&str> = text
        .lines()
        .filter(|l| is_critical(l))
        .map(str::trim)
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

fn present(out: &str, line: &str) -> bool {
    if out.contains(line) {
        return true;
    }
    if let Some(c) = GREP_LINE.captures(line) {
        let (file, n, text) = (&c[1], &c[2], c[3].trim());
        return out.contains(file) && out.contains(&format!("{n}: {text}"));
    }
    static GAP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r" {2,}").unwrap());
    let squeezed = GAP.replace_all(line, "\t");
    squeezed != line && out.contains(squeezed.as_ref())
}

pub fn verify(orig: &str, out: &str, exit_code: i32) -> Result<(), Vec<String>> {
    let mut missing: Vec<String> = Vec::new();
    for l in critical_lines(orig) {
        if !present(out, l) {
            missing.push(l.to_string());
        }
    }
    for m in FILE_LINE.find_iter(orig) {
        let s = m.as_str();
        if !out.contains(s) {
            let (file, rest) = s.rsplit_once(':').unwrap_or((s, ""));
            let file = file
                .rsplit_once(':')
                .filter(|(_, n)| n.chars().all(|c| c.is_ascii_digit()))
                .map(|(f, _)| f)
                .unwrap_or(file);
            let _ = rest;
            let line_no = s
                .split(':')
                .find(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
                .unwrap_or("");
            if !(out.contains(file) && out.contains(&format!("{line_no}: ")))
                && (exit_code != 0 || orig.lines().any(|l| l.contains(s) && is_critical(l)))
            {
                missing.push(s.to_string());
            }
        }
    }
    if exit_code != 0 {
        if let Some(last) = orig.lines().rev().map(str::trim).find(|l| !l.is_empty()) {
            if !present(out, last) {
                missing.push(last.to_string());
            }
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passing_lines_are_not_critical_but_failures_are() {
        assert!(!is_critical("test parser::error_handling ... ok"));
        assert!(is_critical("test parser::x ... FAILED"));
        assert!(is_critical("error[E0308]: mismatched types"));
    }

    #[test]
    fn detects_dropped_error_and_final_line() {
        let orig = "a\nerror: boom\nsrc/x.rs:12 bad\nlast words\n";
        assert!(verify(orig, "a\nerror: boom\nsrc/x.rs:12 bad\nlast words\n", 1).is_ok());
        assert!(verify(orig, "a\nsrc/x.rs:12 bad\nlast words\n", 1).is_err());
        assert!(verify(orig, "a\nerror: boom\nsrc/x.rs:12 bad\n", 1).is_err());
    }

    #[test]
    fn grouped_grep_form_counts_as_present() {
        let orig = "src/a.rs:12:panic: oops\n";
        let out = "src/a.rs\n  12: panic: oops\n";
        assert!(verify(orig, out, 0).is_ok());
        assert!(verify(orig, "nothing\n", 0).is_err());
    }
}
