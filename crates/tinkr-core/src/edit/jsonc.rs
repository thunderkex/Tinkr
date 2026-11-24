use super::EditError;
use serde_json::Value;

struct Member {
    key: String,
    val_start: usize,
    val_end: usize,
    key_start: usize,
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    loop {
        while i < b.len() && (b[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        if b.get(i) == Some(&b'/') && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b.get(i) == Some(&b'/') && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(b.len());
        } else {
            return i;
        }
    }
}

fn string_end(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            b'"' => return Some(j + 1),
            _ => j += 1,
        }
    }
    None
}

fn parse_value(b: &[u8], i: usize) -> Option<usize> {
    match *b.get(i)? {
        b'"' => string_end(b, i),
        b'{' => object_members(b, i).map(|(_, end)| end),
        b'[' => {
            let mut j = i + 1;
            loop {
                j = skip_ws(b, j);
                match *b.get(j)? {
                    b']' => return Some(j + 1),
                    b',' => j += 1,
                    _ => j = parse_value(b, j)?,
                }
            }
        }
        _ => {
            let mut j = i;
            while j < b.len()
                && !matches!(b[j], b',' | b']' | b'}' | b'/')
                && !(b[j] as char).is_ascii_whitespace()
            {
                j += 1;
            }
            if j == i {
                None
            } else {
                Some(j)
            }
        }
    }
}

fn object_members(b: &[u8], open: usize) -> Option<(Vec<Member>, usize)> {
    let mut out = Vec::new();
    let mut i = open + 1;
    loop {
        i = skip_ws(b, i);
        match *b.get(i)? {
            b'}' => return Some((out, i + 1)),
            b',' => i += 1,
            b'"' => {
                let ke = string_end(b, i)?;
                let key: String = serde_json::from_slice(&b[i..ke]).ok()?;
                let c = skip_ws(b, ke);
                if b.get(c) != Some(&b':') {
                    return None;
                }
                let vs = skip_ws(b, c + 1);
                let ve = parse_value(b, vs)?;
                out.push(Member {
                    key,
                    val_start: vs,
                    val_end: ve,
                    key_start: i,
                });
                i = ve;
            }
            _ => return None,
        }
    }
}

pub fn strip_jsonc(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                let e = string_end(b, i).unwrap_or(b.len());
                out.extend_from_slice(&b[i..e]);
                i = e;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(b.len());
            }
            b',' => {
                let j = skip_ws(b, i + 1);
                if !matches!(b.get(j), Some(b'}') | Some(b']')) {
                    out.push(b',');
                }
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn parse(text: &str) -> Result<Option<Value>, EditError> {
    let t = text.trim_start_matches('\u{feff}');
    if skip_ws(t.as_bytes(), 0) >= t.len() {
        return Ok(None);
    }
    serde_json::from_str(&strip_jsonc(t))
        .map(Some)
        .map_err(|e| EditError::Unparseable(e.to_string()))
}

fn root_start(text: &str) -> usize {
    let bom = if text.starts_with('\u{feff}') { 3 } else { 0 };
    skip_ws(text.as_bytes(), bom)
}

pub fn get_path(text: &str, path: &[&str]) -> Result<Option<Value>, EditError> {
    parse(text)?;
    let b = text.as_bytes();
    let mut start = root_start(text);
    for key in path {
        if b.get(start) != Some(&b'{') {
            return Ok(None);
        }
        let Some((members, _)) = object_members(b, start) else {
            return Err(EditError::Unparseable("object".into()));
        };
        match members.into_iter().find(|m| m.key == *key) {
            Some(m) => start = m.val_start,
            None => return Ok(None),
        }
    }
    let end = parse_value(b, start).ok_or_else(|| EditError::Unparseable("value".into()))?;
    serde_json::from_str(&strip_jsonc(&text[start..end]))
        .map(Some)
        .map_err(|e| EditError::Unparseable(e.to_string()))
}

fn line_start(b: &[u8], i: usize) -> usize {
    b[..i]
        .iter()
        .rposition(|&c| c == b'\n')
        .map(|p| p + 1)
        .unwrap_or(0)
}

fn leading_ws(text: &str, ls: usize, upto: usize) -> Option<&str> {
    let s = &text[ls..upto];
    s.chars().all(|c| c == ' ' || c == '\t').then_some(s)
}

fn render(v: &Value, indent: &str, eol: &str) -> String {
    let pretty = serde_json::to_string_pretty(v).unwrap_or_default();
    pretty
        .lines()
        .enumerate()
        .map(|(i, l)| {
            if i == 0 {
                l.to_string()
            } else {
                format!("{indent}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join(eol)
}

fn nested(path: &[&str], value: &Value) -> Value {
    path.iter().rev().fold(value.clone(), |acc, k| {
        let mut m = serde_json::Map::new();
        m.insert((*k).to_string(), acc);
        Value::Object(m)
    })
}

fn set_in_object(
    text: &str,
    open: usize,
    path: &[&str],
    value: &Value,
    eol: &str,
) -> Result<String, EditError> {
    let b = text.as_bytes();
    let (members, end) =
        object_members(b, open).ok_or_else(|| EditError::Unparseable("object".into()))?;
    let close = end - 1;
    if let Some(m) = members.iter().find(|m| m.key == path[0]) {
        if path.len() == 1 {
            let cur: Option<Value> =
                serde_json::from_str(&strip_jsonc(&text[m.val_start..m.val_end])).ok();
            if cur.as_ref() == Some(value) {
                return Ok(text.to_string());
            }
            let ind = leading_ws(text, line_start(b, m.key_start), m.key_start)
                .unwrap_or("")
                .to_string();
            let mut out = text.to_string();
            out.replace_range(m.val_start..m.val_end, &render(value, &ind, eol));
            return Ok(out);
        }
        if b[m.val_start] != b'{' {
            return Err(EditError::NotAnObject(path[0].to_string()));
        }
        return set_in_object(text, m.val_start, &path[1..], value, eol);
    }
    let new_val = nested(&path[1..], value);
    let cls = line_start(b, close);
    let closing_indent = leading_ws(text, cls, close)
        .map(str::to_string)
        .unwrap_or_else(|| {
            let ls = line_start(b, open);
            text[ls..open]
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect()
        });
    let unit = if closing_indent.contains('\t') {
        "\t"
    } else {
        "  "
    };
    let member_indent = match members.last() {
        Some(l) => leading_ws(text, line_start(b, l.key_start), l.key_start)
            .map(str::to_string)
            .unwrap_or_else(|| format!("{closing_indent}{unit}")),
        None => format!("{closing_indent}{unit}"),
    };
    let key_json =
        serde_json::to_string(path[0]).map_err(|e| EditError::Unparseable(e.to_string()))?;
    let member = format!(
        "{member_indent}{key_json}: {}",
        render(&new_val, &member_indent, eol)
    );
    let mut out = text.to_string();
    match members.last() {
        None => {
            out.replace_range(
                open + 1..close,
                &format!("{eol}{member}{eol}{closing_indent}"),
            );
        }
        Some(last) => {
            let after = skip_ws(b, last.val_end);
            let has_comma = b.get(after) == Some(&b',');
            let own_line = leading_ws(text, cls, close).is_some();
            if own_line {
                out.insert_str(cls, &format!("{member}{eol}"));
            } else {
                out.insert_str(close, &format!("{eol}{member}{eol}{closing_indent}"));
            }
            if !has_comma {
                out.insert(last.val_end, ',');
            }
        }
    }
    Ok(out)
}

pub fn set_path(text: &str, path: &[&str], value: &Value) -> Result<String, EditError> {
    if path.is_empty() {
        return Err(EditError::Unparseable("empty path".into()));
    }
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    if parse(text)?.is_none() {
        let v = nested(path, value);
        return Ok(format!("{}{eol}", render(&v, "", eol)));
    }
    let start = root_start(text);
    if text.as_bytes().get(start) != Some(&b'{') {
        return Err(EditError::NotAnObject("<root>".into()));
    }
    set_in_object(text, start, path, value, eol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replace_keeps_comments_and_order() {
        let t = "{\n  // keep me\n  \"a\": 1, /* inline */\n  \"b\": { \"c\": [1,2] },\n  \"z\": true // tail\n}\n";
        let o = set_path(t, &["b", "c"], &json!([9])).unwrap();
        assert!(o.contains("// keep me") && o.contains("/* inline */") && o.contains("// tail"));
        assert_eq!(get_path(&o, &["b", "c"]).unwrap().unwrap(), json!([9]));
        assert_eq!(get_path(&o, &["z"]).unwrap().unwrap(), json!(true));
    }

    #[test]
    fn insert_nested_with_crlf_and_trailing_comment() {
        let t = "{\r\n    \"a\": 1 // c\r\n}\r\n";
        let o = set_path(t, &["hooks", "X"], &json!(["v"])).unwrap();
        assert!(o.contains("// c"));
        assert!(
            !o.replace("\r\n", "").contains('\n'),
            "line endings stay CRLF: {o:?}"
        );
        assert_eq!(
            get_path(&o, &["hooks", "X"]).unwrap().unwrap(),
            json!(["v"])
        );
        assert_eq!(get_path(&o, &["a"]).unwrap().unwrap(), json!(1));
        assert!(o.contains("    \"hooks\""), "indent follows file: {o:?}");
    }

    #[test]
    fn idempotent_when_equal() {
        let t = "{\n  \"a\": {\"b\": 1}\n}";
        assert_eq!(set_path(t, &["a", "b"], &json!(1)).unwrap(), t);
    }

    #[test]
    fn empty_and_single_line_and_tabs() {
        assert_eq!(parse("  \n").unwrap(), None);
        let o = set_path("", &["a"], &json!(1)).unwrap();
        assert_eq!(parse(&o).unwrap().unwrap(), json!({"a": 1}));
        let o = set_path("{\"a\":1}", &["b"], &json!(2)).unwrap();
        assert_eq!(parse(&o).unwrap().unwrap(), json!({"a": 1, "b": 2}));
        let o = set_path("{\n\t\"a\": 1\n}\n", &["b"], &json!({"x": 1})).unwrap();
        assert!(o.contains("\t\"b\""), "{o:?}");
        let o = set_path("{}", &["b"], &json!(2)).unwrap();
        assert_eq!(parse(&o).unwrap().unwrap(), json!({"b": 2}));
    }

    #[test]
    fn trailing_comma_and_bom() {
        let t = "\u{feff}{\n  \"a\": 1,\n}\n";
        let o = set_path(t, &["b"], &json!(2)).unwrap();
        assert!(o.starts_with('\u{feff}'));
        assert_eq!(parse(&o).unwrap().unwrap(), json!({"a": 1, "b": 2}));
    }

    #[test]
    fn unparseable_is_reported_not_edited() {
        assert!(matches!(
            set_path("{\"a\": ", &["b"], &json!(1)),
            Err(EditError::Unparseable(_))
        ));
        assert!(matches!(
            set_path("[1]", &["b"], &json!(1)),
            Err(EditError::NotAnObject(_))
        ));
        assert!(matches!(
            set_path("{\"a\": 1}", &["a", "b"], &json!(1)),
            Err(EditError::NotAnObject(_))
        ));
    }

    #[test]
    fn strings_containing_comment_tokens() {
        let t = "{\"url\": \"http://x//y\", \"s\": \"/* no */\"}";
        let o = set_path(t, &["n"], &json!(1)).unwrap();
        assert_eq!(
            get_path(&o, &["url"]).unwrap().unwrap(),
            json!("http://x//y")
        );
        assert_eq!(get_path(&o, &["s"]).unwrap().unwrap(), json!("/* no */"));
    }
}
