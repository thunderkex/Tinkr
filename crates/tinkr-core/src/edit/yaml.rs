use super::EditError;

pub fn set_key(text: &str, key: &str, value: &str) -> Result<String, EditError> {
    if text.contains('\t') {
        return Err(EditError::Unparseable("tabs in YAML indentation".into()));
    }
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let prefix = format!("{key}:");
    for line in &mut lines {
        let trimmed = line.trim_start();
        if line.starts_with(&prefix) && !trimmed.starts_with('#') {
            let new_line = format!("{key}: {value}");
            if *line == new_line {
                return Ok(text.to_string());
            }
            *line = new_line;
            let mut out = lines.join(eol);
            if text.ends_with('\n') || text.is_empty() {
                out.push_str(eol);
            }
            return Ok(out);
        }
    }
    lines.push(format!("{key}: {value}"));
    let mut out = lines.join(eol);
    out.push_str(eol);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_comments_and_order() {
        let y = "# header\nfoo: bar\n# mid\nbaz: 1\n";
        let out = set_key(y, "foo", "qux").unwrap();
        assert!(out.contains("# header") && out.contains("# mid") && out.contains("foo: qux"));
        assert_eq!(set_key(&out, "foo", "qux").unwrap(), out);
    }
}
