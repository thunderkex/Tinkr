use crate::filters::verify;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

pub fn line_level(line: &str) -> &'static str {
    for tok in line.split(|c: char| !c.is_ascii_alphabetic()).filter(|s| !s.is_empty()).take(10) {
        if tok.len() < 3 || tok.len() > 9 {
            continue;
        }
        let upper = tok.to_ascii_uppercase();
        match upper.as_str() {
            "TRACE" | "TRC" => return "TRACE",
            "DEBUG" | "DBG" => return "DEBUG",
            "INFO" | "INF" => return "INFO",
            "NOTICE" => return "NOTICE",
            "WARN" | "WARNING" | "WRN" => return "WARN",
            "ERROR" | "ERR" => return "ERROR",
            "FATAL" | "FTL" => return "FATAL",
            "CRITICAL" | "CRIT" => return "CRIT",
            "ALERT" => return "ALERT",
            "EMERG" | "PANIC" => return "PANIC",
            _ => {}
        }
    }
    "NONE"
}

pub fn line_shape(line: &str) -> String {
    let trimmed = line.trim();
    let bytes = trimmed.as_bytes();
    let mut out = String::with_capacity(trimmed.len().min(256));
    let mut i = 0usize;
    while i < bytes.len() && out.len() < 320 {
        let b = bytes[i];
        if b.is_ascii_alphanumeric() {
            let start = i;
            let mut has_digit = false;
            let mut all_digits = true;
            let mut all_hex = true;
            while i < bytes.len() && bytes[i].is_ascii_alphanumeric() {
                let c = bytes[i];
                if c.is_ascii_digit() {
                    has_digit = true;
                } else {
                    all_digits = false;
                    if !c.is_ascii_hexdigit() {
                        all_hex = false;
                    }
                }
                i += 1;
            }
            let tok_len = i - start;
            if all_digits {
                out.push('#');
            } else if all_hex && has_digit && tok_len >= 6 {
                out.push_str("<H>");
            } else if has_digit {
                let mut prev_digit = false;
                for ch in trimmed[start..i].chars() {
                    if ch.is_ascii_digit() {
                        if !prev_digit {
                            out.push('#');
                            prev_digit = true;
                        }
                    } else {
                        out.push(ch);
                        prev_digit = false;
                    }
                }
            } else {
                out.push_str(&trimmed[start..i]);
            }
        } else if b == b' ' || b == b'\t' {
            while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
                i += 1;
            }
            out.push(' ');
        } else {
            let ch = trimmed[i..].chars().next().unwrap_or(' ');
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

pub fn looks_like_logs(text: &str) -> bool {
    let mut total = 0usize;
    let mut hits = 0usize;
    for l in text.lines().filter(|l| !l.trim().is_empty()).take(25) {
        total += 1;
        if line_level(l) != "NONE" || l.starts_with("202") || l.starts_with('[') {
            hits += 1;
        }
    }
    total >= 5 && hits * 2 >= total
}

pub fn dedup_lines_preserving_outliers(text: &str) -> String {
    dedup_lines_with_rarity(text, 3)
}

pub fn dedup_lines_with_rarity(text: &str, max_rare_freq: usize) -> String {
    let raw_lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    if raw_lines.len() <= 24 {
        return simple_run_dedup(&raw_lines);
    }

    let mut non_empty_indices: Vec<usize> = Vec::with_capacity(raw_lines.len());
    let mut meta: Vec<(&'static str, String)> = Vec::with_capacity(raw_lines.len());
    let mut freq: HashMap<(&'static str, String), usize> = HashMap::new();
    let mut lengths: Vec<usize> = Vec::with_capacity(raw_lines.len());

    for (idx, &l) in raw_lines.iter().enumerate() {
        if l.is_empty() {
            meta.push(("EMPTY", String::new()));
            continue;
        }
        non_empty_indices.push(idx);
        lengths.push(l.len());
        let lvl = line_level(l);
        let shp = line_shape(l);
        *freq.entry((lvl, shp.clone())).or_insert(0) += 1;
        meta.push((lvl, shp));
    }

    if non_empty_indices.len() <= 20 {
        return simple_run_dedup(&raw_lines);
    }

    lengths.sort_unstable();
    let median_len = lengths[lengths.len() / 2].max(1);
    let len_outlier_threshold = median_len.saturating_mul(3).max(200);
    let rare_threshold = if max_rare_freq <= 1 {
        1
    } else if non_empty_indices.len() >= 500 {
        (non_empty_indices.len() / 200).max(max_rare_freq)
    } else {
        max_rare_freq
    };

    let first_ne = *non_empty_indices.first().unwrap();
    let last_ne = *non_empty_indices.last().unwrap();

    let mut keep = vec![false; raw_lines.len()];
    keep[first_ne] = true;
    keep[last_ne] = true;

    let mut seen_templates: HashSet<(&'static str, String)> = HashSet::new();

    for (pos_in_ne, &line_idx) in non_empty_indices.iter().enumerate() {
        let l = raw_lines[line_idx];
        let (lvl, ref shp) = meta[line_idx];
        let key = (lvl, shp.clone());
        let count = freq.get(&key).copied().unwrap_or(0);

        if verify::is_critical(l) || !verify::critical_lines(l).is_empty() {
            keep[line_idx] = true;
        }
        if seen_templates.insert(key) {
            keep[line_idx] = true;
        }
        if count <= rare_threshold {
            keep[line_idx] = true;
        }
        if lvl != "NONE" && pos_in_ne > 0 && pos_in_ne + 1 < non_empty_indices.len() {
            let prev_lvl = meta[non_empty_indices[pos_in_ne - 1]].0;
            let next_lvl = meta[non_empty_indices[pos_in_ne + 1]].0;
            if lvl != prev_lvl && lvl != next_lvl {
                keep[line_idx] = true;
            }
        }
        if l.len() > len_outlier_threshold {
            keep[line_idx] = true;
        }
    }

    let mut out: Vec<String> = Vec::new();
    let mut omitted_total = 0usize;
    let mut omitted_by_level: HashMap<&'static str, usize> = HashMap::new();
    let mut prev_was_blank = false;

    let flush_omitted = |out: &mut Vec<String>,
                         total: &mut usize,
                         by_lvl: &mut HashMap<&'static str, usize>| {
        if *total == 0 {
            return;
        }
        if by_lvl.len() == 1 {
            let (&lvl, _) = by_lvl.iter().next().unwrap();
            if lvl == "NONE" {
                out.push(format!("[{} similar lines omitted]", *total));
            } else {
                out.push(format!("[{} similar {lvl} lines omitted]", *total));
            }
        } else {
            let mut parts: Vec<(&'static str, usize)> = by_lvl.drain().collect();
            parts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
            let detail = parts
                .into_iter()
                .map(|(k, n)| {
                    if k == "NONE" {
                        format!("{n} text")
                    } else {
                        format!("{n} {k}")
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            out.push(format!("[{} similar lines omitted ({detail})]", *total));
        }
        *total = 0;
        by_lvl.clear();
    };

    for (idx, &l) in raw_lines.iter().enumerate() {
        if l.is_empty() {
            if omitted_total == 0 && !prev_was_blank && !out.is_empty() {
                out.push(String::new());
                prev_was_blank = true;
            }
            continue;
        }
        if keep[idx] {
            flush_omitted(&mut out, &mut omitted_total, &mut omitted_by_level);
            out.push(l.to_string());
            prev_was_blank = false;
        } else {
            omitted_total += 1;
            *omitted_by_level.entry(meta[idx].0).or_insert(0) += 1;
        }
    }
    flush_omitted(&mut out, &mut omitted_total, &mut omitted_by_level);
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out.join("\n")
}

fn simple_run_dedup(lines: &[&str]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut prev: Option<&str> = None;
    let mut run = 0usize;
    let mut blank = false;
    let flush = |out: &mut Vec<String>, prev: &mut Option<&str>, run: &mut usize| {
        if let Some(p) = prev.take() {
            if *run > 1 {
                out.push(format!("{p} (x{run})"));
            } else {
                out.push(p.to_string());
            }
        }
        *run = 0;
    };
    for &l in lines {
        if l.is_empty() {
            flush(&mut out, &mut prev, &mut run);
            if !blank && !out.is_empty() {
                out.push(String::new());
            }
            blank = true;
            continue;
        }
        blank = false;
        if prev == Some(l) {
            run += 1;
        } else {
            flush(&mut out, &mut prev, &mut run);
            prev = Some(l);
            run = 1;
        }
    }
    flush(&mut out, &mut prev, &mut run);
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out.join("\n")
}

fn is_discriminator_key(k: &str) -> bool {
    matches!(
        k.to_ascii_lowercase().as_str(),
        "status"
            | "level"
            | "severity"
            | "state"
            | "type"
            | "kind"
            | "outcome"
            | "code"
            | "http_code"
            | "status_code"
            | "error"
            | "err"
            | "error_code"
            | "ok"
            | "success"
            | "healthy"
            | "event"
            | "phase"
            | "reason"
    )
}

pub fn json_value_shape(v: &Value, depth: usize) -> String {
    if depth >= 4 {
        return match v {
            Value::Null => "0".into(),
            Value::Bool(_) => "b".into(),
            Value::Number(_) => "n".into(),
            Value::String(_) => "s".into(),
            Value::Array(_) => "a".into(),
            Value::Object(_) => "o".into(),
        };
    }
    match v {
        Value::Null => "0".into(),
        Value::Bool(b) => format!("b:{b}"),
        Value::Number(_) => "n".into(),
        Value::String(_) => "s".into(),
        Value::Array(a) => {
            if a.is_empty() {
                "a:0".into()
            } else {
                format!("a:{}", json_value_shape(&a[0], depth + 1))
            }
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            let mut parts = Vec::with_capacity(keys.len());
            for k in keys {
                let val = &map[k];
                if is_discriminator_key(k) {
                    match val {
                        Value::String(s) if s.len() <= 48 => {
                            parts.push(format!("{k}={s}"));
                            continue;
                        }
                        Value::Number(n) => {
                            parts.push(format!("{k}={n}"));
                            continue;
                        }
                        Value::Bool(b) => {
                            parts.push(format!("{k}={b}"));
                            continue;
                        }
                        Value::Null => {
                            parts.push(format!("{k}=null"));
                            continue;
                        }
                        _ => {}
                    }
                }
                parts.push(format!("{k}:{}", json_value_shape(val, depth + 1)));
            }
            format!("{{{}}}", parts.join(","))
        }
    }
}

pub fn json_contains_critical(v: &Value) -> bool {
    match v {
        Value::String(s) => verify::is_critical(s),
        Value::Array(a) => a.iter().any(json_contains_critical),
        Value::Object(m) => m
            .iter()
            .any(|(k, val)| verify::is_critical(k) || json_contains_critical(val)),
        _ => false,
    }
}

pub fn compact_json_array<F>(arr: &[Value], depth: usize, compact_elem: &F) -> Vec<Value>
where
    F: Fn(&Value, usize) -> Value,
{
    let len = arr.len();
    if len <= 12 {
        return arr.iter().map(|x| compact_elem(x, depth + 1)).collect();
    }

    let shapes: Vec<String> = arr.iter().map(|v| json_value_shape(v, 0)).collect();
    let mut freq: HashMap<&str, usize> = HashMap::new();
    for shp in &shapes {
        *freq.entry(shp.as_str()).or_insert(0) += 1;
    }

    let mut seen_shapes: HashSet<&str> = HashSet::new();
    let mut keep = vec![false; len];
    keep[0] = true;
    keep[len - 1] = true;

    for i in 0..len {
        let shp = shapes[i].as_str();
        if seen_shapes.insert(shp)
            || freq.get(shp).copied().unwrap_or(0) <= 3
            || json_contains_critical(&arr[i])
        {
            keep[i] = true;
        }
    }

    let mut out = Vec::new();
    let mut omitted = 0usize;
    for i in 0..len {
        if keep[i] {
            if omitted > 0 {
                out.push(Value::String(format!(
                    "... [{omitted} similar items omitted] ..."
                )));
                omitted = 0;
            }
            out.push(compact_elem(&arr[i], depth + 1));
        } else {
            omitted += 1;
        }
    }
    if omitted > 0 {
        out.push(Value::String(format!(
            "... [{omitted} similar items omitted] ..."
        )));
    }
    out
}

pub fn compress_jsonl_preserving_outliers<F>(s: &str, compact_line: F) -> String
where
    F: Fn(&str) -> String,
{
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    let len = lines.len();
    if len <= 15 {
        return lines.iter().map(|l| compact_line(l)).collect::<Vec<_>>().join("\n");
    }

    let parsed: Vec<Option<Value>> = lines
        .iter()
        .map(|l| serde_json::from_str::<Value>(l).ok())
        .collect();
    let shapes: Vec<String> = lines
        .iter()
        .zip(&parsed)
        .map(|(l, pv)| match pv {
            Some(v) => json_value_shape(v, 0),
            None => line_shape(l),
        })
        .collect();

    let mut freq: HashMap<&str, usize> = HashMap::new();
    for shp in &shapes {
        *freq.entry(shp.as_str()).or_insert(0) += 1;
    }

    let mut seen_shapes: HashSet<&str> = HashSet::new();
    let mut keep = vec![false; len];
    keep[0] = true;
    keep[len - 1] = true;

    for i in 0..len {
        let shp = shapes[i].as_str();
        let crit = match &parsed[i] {
            Some(v) => json_contains_critical(v),
            None => verify::is_critical(lines[i]),
        };
        if seen_shapes.insert(shp) || freq.get(shp).copied().unwrap_or(0) <= 3 || crit {
            keep[i] = true;
        }
    }

    let mut out = Vec::new();
    let mut omitted = 0usize;
    for i in 0..len {
        if keep[i] {
            if omitted > 0 {
                out.push(format!("[{omitted} similar JSONL records omitted]"));
                omitted = 0;
            }
            out.push(compact_line(lines[i]));
        } else {
            omitted += 1;
        }
    }
    if omitted > 0 {
        out.push(format!("[{omitted} similar JSONL records omitted]"));
    }
    out.join("\n")
}
