use super::EditError;

pub fn set_key(text: &str, section: &str, key: &str, value: &str) -> Result<String, EditError> {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let want_sec = format!("[{section}]");
    let mut in_sec = section.is_empty();
    let mut sec_end: Option<usize> = if in_sec { Some(lines.len()) } else { None };
    let mut found_sec = in_sec;

    for i in 0..lines.len() {
        let t = lines[i].trim();
        if t.starts_with('[') && t.ends_with(']') {
            if in_sec {
                sec_end = Some(i);
                in_sec = false;
            }
            if t.eq_ignore_ascii_case(&want_sec) {
                in_sec = true;
                found_sec = true;
                sec_end = Some(lines.len());
            }
            continue;
        }
        if in_sec && !t.starts_with(';') && !t.starts_with('#') {
            if let Some((k, _)) = t.split_once('=') {
                if k.trim().eq_ignore_ascii_case(key) {
                    let new_line = format!("{key}={value}");
                    if lines[i] == new_line {
                        return Ok(text.to_string());
                    }
                    lines[i] = new_line;
                    let mut out = lines.join(eol);
                    if text.ends_with('\n') {
                        out.push_str(eol);
                    }
                    return Ok(out);
                }
            }
        }
    }
    if found_sec {
        let pos = sec_end.unwrap_or(lines.len());
        lines.insert(pos, format!("{key}={value}"));
    } else {
        if lines.last().is_some_and(|l| !l.is_empty()) {
            lines.push(String::new());
        }
        lines.push(want_sec);
        lines.push(format!("{key}={value}"));
    }
    let mut out = lines.join(eol);
    out.push_str(eol);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_comments_and_sections() {
        let ini = "; top\n[agent]\nurl=old\n[other]\nx=1\n";
        let o = set_key(ini, "agent", "url", "http://127.0.0.1:9").unwrap();
        assert!(
            o.contains("; top") && o.contains("url=http://127.0.0.1:9") && o.contains("[other]")
        );
        assert_eq!(
            set_key(&o, "agent", "url", "http://127.0.0.1:9").unwrap(),
            o
        );
    }
}
