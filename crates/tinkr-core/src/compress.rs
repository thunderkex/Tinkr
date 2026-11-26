use crate::{
    defaults,
    filters::{kinds, verify},
    recall::{self, RecallStore},
    tokenizer::{self, TokenizerKind},
};
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadKind {
    Json,
    Jsonl,
    Html,
    Csv,
    Logs,
    Code,
    Text,
}

pub fn detect_kind(s: &str) -> PayloadKind {
    let t = s.trim();
    if ((t.starts_with('{') && t.ends_with('}')) || (t.starts_with('[') && t.ends_with(']')))
        && serde_json::from_str::<Value>(t).is_ok()
    {
        return PayloadKind::Json;
    }
    let lower: String = t.chars().take(200).collect::<String>().to_ascii_lowercase();
    if lower.contains("<!doctype html")
        || lower.contains("<html")
        || (lower.contains("<div") && lower.contains("</div>"))
    {
        return PayloadKind::Html;
    }
    let lines: Vec<&str> = t.lines().take(20).collect();
    if lines.len() >= 5 {
        if lines.iter().all(|l| {
            let lt = l.trim();
            lt.starts_with('{') && lt.ends_with('}') && serde_json::from_str::<Value>(lt).is_ok()
        }) {
            return PayloadKind::Jsonl;
        }
        let commas: Vec<usize> = lines.iter().map(|l| l.matches(',').count()).collect();
        if commas[0] >= 2 && commas.iter().all(|&c| c == commas[0]) {
            return PayloadKind::Csv;
        }
        let log_hits = lines
            .iter()
            .filter(|l| {
                l.contains("INFO")
                    || l.contains("DEBUG")
                    || l.contains("WARN")
                    || l.contains("ERROR")
                    || l.starts_with("202")
            })
            .count();
        if log_hits * 2 >= lines.len() {
            return PayloadKind::Logs;
        }
        let code_hits = lines
            .iter()
            .filter(|l| {
                let lt = l.trim_start();
                lt.starts_with("fn ")
                    || lt.starts_with("pub ")
                    || lt.starts_with("def ")
                    || lt.starts_with("class ")
                    || lt.starts_with("function ")
                    || lt.starts_with("import ")
                    || lt.starts_with("const ")
                    || lt.ends_with('{')
                    || lt.ends_with(';')
            })
            .count();
        if code_hits * 3 >= lines.len() {
            return PayloadKind::Code;
        }
    }
    PayloadKind::Text
}

const MAX_JSON_DEPTH: usize = 64;

fn compact_json_value(v: &Value, depth: usize) -> Value {
    if depth >= MAX_JSON_DEPTH {
        return v.clone();
    }
    match v {
        Value::Array(arr) if arr.len() > 20 => {
            let mut out = Vec::with_capacity(12);
            for item in arr.iter().take(5) {
                out.push(compact_json_value(item, depth + 1));
            }
            let mut kept_errors = 0;
            for item in arr.iter().skip(5).take(arr.len().saturating_sub(8)) {
                let s = item.to_string();
                if verify::is_critical(&s) && kept_errors < 5 {
                    out.push(item.clone());
                    kept_errors += 1;
                }
            }
            out.push(Value::String(format!(
                "... [{} items omitted] ...",
                arr.len() - 8 - kept_errors
            )));
            for item in arr.iter().skip(arr.len() - 3) {
                out.push(compact_json_value(item, depth + 1));
            }
            Value::Array(out)
        }
        Value::Array(arr) => Value::Array(
            arr.iter()
                .map(|x| compact_json_value(x, depth + 1))
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                if !val.is_null() {
                    out.insert(k.clone(), compact_json_value(val, depth + 1));
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

fn compress_json(s: &str) -> Option<String> {
    let v: Value = serde_json::from_str(s).ok()?;
    let c = compact_json_value(&v, 0);
    serde_json::to_string(&c).ok()
}

fn compress_jsonl(s: &str) -> String {
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    let compact_line = |l: &str| -> String {
        if let Ok(v) = serde_json::from_str::<Value>(l) {
            serde_json::to_string(&compact_json_value(&v, 0)).unwrap_or_else(|_| l.to_string())
        } else {
            l.to_string()
        }
    };
    if lines.len() <= 30 {
        return lines
            .iter()
            .map(|l| compact_line(l))
            .collect::<Vec<_>>()
            .join("\n");
    }
    let mut out: Vec<String> = lines.iter().take(10).map(|l| compact_line(l)).collect();
    let mid = &lines[10..lines.len() - 5];
    let crit: Vec<&str> = mid
        .iter()
        .copied()
        .filter(|l| verify::is_critical(l))
        .collect();
    out.push(format!(
        "[{} JSONL records omitted]",
        mid.len() - crit.len()
    ));
    for l in crit {
        out.push(compact_line(l));
    }
    for l in &lines[lines.len() - 5..] {
        out.push(compact_line(l));
    }
    out.join("\n")
}

static SCRIPT_STYLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)<script[^>]*>.*?</script>|<style[^>]*>.*?</style>|<svg[^>]*>.*?</svg>|<noscript[^>]*>.*?</noscript>").unwrap()
});
static TAGS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]+>").unwrap());
static WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t]{2,}").unwrap());

fn compress_html(s: &str) -> String {
    let no_scripts = SCRIPT_STYLE.replace_all(s, " ");
    let no_tags = TAGS.replace_all(&no_scripts, "\n");
    let lines: Vec<String> = no_tags
        .lines()
        .map(|l| WS.replace_all(l.trim(), " ").into_owned())
        .filter(|l| !l.is_empty())
        .collect();
    kinds::generic(&lines.join("\n"), true)
}

fn compress_csv(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= 30 {
        return s.to_string();
    }
    let mut out: Vec<String> = Vec::new();
    out.push(lines[0].to_string());
    for l in lines.iter().skip(1).take(10) {
        out.push((*l).to_string());
    }
    let mid = &lines[11..lines.len() - 5];
    let crit: Vec<&str> = mid
        .iter()
        .copied()
        .filter(|l| verify::is_critical(l))
        .collect();
    out.push(format!("[{} rows omitted]", mid.len() - crit.len()));
    for l in crit {
        out.push(l.to_string());
    }
    for l in &lines[lines.len() - 5..] {
        out.push((*l).to_string());
    }
    out.join("\n")
}

fn compress_logs(s: &str) -> String {
    let mut out = Vec::new();
    let mut dropped = 0usize;
    for l in s.lines() {
        let noisy = (l.contains("DEBUG") || l.contains("TRACE")) && !verify::is_critical(l);
        if noisy {
            dropped += 1;
        } else {
            out.push(l);
        }
    }
    let mut body = kinds::generic(&out.join("\n"), true);
    if dropped > 0 {
        body = format!("[{dropped} DEBUG/TRACE lines omitted]\n{body}");
    }
    body
}

fn compress_code(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= 160 {
        return s.to_string();
    }
    let mut out: Vec<String> = lines[..60]
        .iter()
        .map(|l| l.trim_end().to_string())
        .collect();
    let mid = &lines[60..lines.len() - 40];
    let mut omitted = 0usize;
    for (idx, l) in mid.iter().enumerate() {
        let t = l.trim_start();
        let keep = verify::is_critical(l)
            || t.starts_with("pub fn ")
            || t.starts_with("pub async fn ")
            || t.starts_with("async fn ")
            || t.starts_with("fn ")
            || t.starts_with("pub struct ")
            || t.starts_with("struct ")
            || t.starts_with("pub enum ")
            || t.starts_with("enum ")
            || t.starts_with("pub trait ")
            || t.starts_with("trait ")
            || t.starts_with("pub const ")
            || t.starts_with("const ")
            || t.starts_with("pub static ")
            || t.starts_with("static ")
            || t.starts_with("pub type ")
            || t.starts_with("type ")
            || t.starts_with("pub mod ")
            || t.starts_with("mod ")
            || t.starts_with("pub(crate) ")
            || t.starts_with("impl ")
            || t.starts_with("def ")
            || t.starts_with("class ")
            || t.starts_with("interface ")
            || t.starts_with("export ");
        if keep {
            if omitted > 0 {
                out.push(format!("// ... [{omitted} lines omitted] ..."));
                omitted = 0;
            }
            out.push(format!("{}: {}", idx + 61, l.trim_end()));
        } else {
            omitted += 1;
        }
    }
    if omitted > 0 {
        out.push(format!("// ... [{omitted} lines omitted] ..."));
    }
    out.extend(
        lines[lines.len() - 40..]
            .iter()
            .map(|l| l.trim_end().to_string()),
    );
    out.join("\n")
}

pub struct CompressedPayload {
    pub text: String,
    pub before: u64,
    pub after: u64,
    pub recall_id: Option<String>,
    pub kind: PayloadKind,
}

fn verify_payload(kind: PayloadKind, orig: &str, candidate: &str) -> bool {
    match kind {
        PayloadKind::Json => {
            if let Ok(v) = serde_json::from_str::<Value>(orig) {
                fn check(v: &Value, cand: &str) -> bool {
                    match v {
                        Value::String(s) if verify::is_critical(s) => cand.contains(s.as_str()),
                        Value::Array(a) => a.iter().all(|x| check(x, cand)),
                        Value::Object(m) => m.values().all(|x| check(x, cand)),
                        _ => true,
                    }
                }
                check(&v, candidate)
            } else {
                verify::verify(orig, candidate, 0).is_ok()
            }
        }
        PayloadKind::Jsonl => orig.lines().all(|l| {
            if !verify::is_critical(l) {
                return true;
            }
            if let Ok(v) = serde_json::from_str::<Value>(l) {
                fn check(v: &Value, cand: &str) -> bool {
                    match v {
                        Value::String(s) if verify::is_critical(s) => cand.contains(s.as_str()),
                        Value::Array(a) => a.iter().all(|x| check(x, cand)),
                        Value::Object(m) => m.values().all(|x| check(x, cand)),
                        _ => true,
                    }
                }
                check(&v, candidate)
            } else {
                candidate.contains(l.trim())
            }
        }),
        PayloadKind::Html => {
            let no_scripts = SCRIPT_STYLE.replace_all(orig, " ");
            let plain = TAGS.replace_all(&no_scripts, "\n");
            verify::verify(&plain, candidate, 0).is_ok()
        }
        _ => verify::verify(orig, candidate, 0).is_ok(),
    }
}

pub fn has_tinkr_marker(text: &str) -> bool {
    text.lines().rev().take(8).any(|l| {
        let t = l.trim();
        t.starts_with("[tinkr:") && t.ends_with("recall]")
    })
}

pub fn compress_tool_result(
    text: &str,
    tok: TokenizerKind,
    recall: Option<&RecallStore>,
    unicode: bool,
) -> Option<CompressedPayload> {
    if has_tinkr_marker(text) || crate::overlap::has_foreign_marker(text.as_bytes(), &[]) {
        return None;
    }
    let before = tokenizer::count(text, tok).tokens;
    if before < defaults::MIN_PAYLOAD_TOKENS {
        return None;
    }
    let kind = detect_kind(text);
    let candidate = match kind {
        PayloadKind::Json => compress_json(text)?,
        PayloadKind::Jsonl => compress_jsonl(text),
        PayloadKind::Html => compress_html(text),
        PayloadKind::Csv => compress_csv(text),
        PayloadKind::Logs => compress_logs(text),
        PayloadKind::Code => compress_code(text),
        PayloadKind::Text => kinds::generic(text, true),
    };
    if candidate == text {
        return None;
    }
    if !verify_payload(kind, text, &candidate) {
        return None;
    }
    let after = tokenizer::count(&candidate, tok).tokens;
    if !defaults::worth_it(before, after + 14) {
        return None;
    }
    let (id, final_text, final_after) = match recall {
        Some(store) => {
            let id = store.put(text.as_bytes()).ok()?;
            let m = recall::marker(&id, before, after, unicode);
            let t = format!("{}\n{m}", candidate.trim_end());
            let fa = tokenizer::count(&t, tok).tokens;
            (Some(id), t, fa)
        }
        None => (None, candidate, after),
    };
    if !defaults::worth_it(before, final_after) {
        return None;
    }
    Some(CompressedPayload {
        text: final_text,
        before,
        after: final_after,
        recall_id: id,
        kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compresses_json_html_csv_logs_code_and_preserves_needles() {
        let items: Vec<Value> = (0..100)
            .map(|i| {
                if i == 50 {
                    serde_json::json!({"id": i, "status": "error: needle_json_50", "extra": null})
                } else {
                    serde_json::json!({"id": i, "status": "ok", "extra": null, "pad": "some repetitive data here"})
                }
            })
            .collect();
        let json_str = serde_json::to_string_pretty(&items).unwrap();
        let res = compress_tool_result(&json_str, TokenizerKind::Exact, None, false).unwrap();
        assert_eq!(res.kind, PayloadKind::Json);
        assert!(res.text.contains("needle_json_50"));
        assert!(res.after < res.before / 2);

        let html = format!(
            "<!DOCTYPE html><html><head><style>{}</style><script>{}</script></head><body><p>Error: real_html_msg</p></body></html>",
            ".a { color: red; }\n".repeat(120),
            "console.log('noise');\n".repeat(120)
        );
        let res = compress_tool_result(&html, TokenizerKind::Exact, None, false).unwrap();
        assert_eq!(res.kind, PayloadKind::Html);
        assert!(res.text.contains("real_html_msg") && !res.text.contains("console.log"));

        let mut csv = String::from("id,name,status,msg\n");
        for i in 0..100 {
            if i == 55 {
                csv.push_str("55,svc,FAILED,error: disk_full_needle\n");
            } else {
                csv.push_str(&format!(
                    "{i},svc_{i},ok,normal operation completed cleanly\n"
                ));
            }
        }
        let res = compress_tool_result(&csv, TokenizerKind::Exact, None, false).unwrap();
        assert_eq!(res.kind, PayloadKind::Csv);
        assert!(res.text.contains("disk_full_needle"));

        let mut logs = String::new();
        for i in 0..120 {
            logs.push_str(&format!(
                "2026-11-23T10:00:{i:02}Z DEBUG worker tick processing item {i}\n"
            ));
            if i == 60 {
                logs.push_str("2026-11-23T10:01:00Z ERROR db connection refused at src/db.rs:42\n");
            }
        }
        let res = compress_tool_result(&logs, TokenizerKind::Exact, None, false).unwrap();
        assert_eq!(res.kind, PayloadKind::Logs);
        assert!(res.text.contains("db connection refused at src/db.rs:42"));

        let mut code = String::new();
        for i in 0..250 {
            if i == 120 {
                code.push_str("pub fn important_entry(x: i32) -> i32 {\n");
            } else {
                code.push_str(&format!("    let var_{i} = compute_step({i}, {i} + 1);\n"));
            }
        }
        let res = compress_tool_result(&code, TokenizerKind::Exact, None, false).unwrap();
        assert_eq!(res.kind, PayloadKind::Code);
        assert!(res.text.contains("important_entry"));
    }
}
