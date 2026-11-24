use super::EditError;
use toml_edit::{DocumentMut, Item, Table};

fn parse(text: &str) -> Result<DocumentMut, EditError> {
    text.parse::<DocumentMut>()
        .map_err(|e| EditError::Unparseable(e.to_string()))
}

pub fn get_path(text: &str, path: &[&str]) -> Result<Option<String>, EditError> {
    let doc = parse(text)?;
    let mut cur: &Item = doc.as_item();
    for k in path {
        match cur.get(*k) {
            Some(n) => cur = n,
            None => return Ok(None),
        }
    }
    Ok(Some(cur.to_string().trim().to_string()))
}

pub fn set_path(text: &str, path: &[&str], value_toml: &str) -> Result<String, EditError> {
    let mut doc = parse(text)?;
    let new: toml_edit::Value = value_toml
        .parse()
        .map_err(|e: toml_edit::TomlError| EditError::Unparseable(e.to_string()))?;
    let mut tbl: &mut Table = doc.as_table_mut();
    for k in &path[..path.len() - 1] {
        let item = tbl.entry(k).or_insert_with(|| {
            let mut t = Table::new();
            t.set_implicit(true);
            Item::Table(t)
        });
        tbl = item
            .as_table_mut()
            .ok_or_else(|| EditError::NotAnObject((*k).to_string()))?;
    }
    let last = path[path.len() - 1];
    if let Some(Item::Value(cur)) = tbl.get(last) {
        if cur.to_string().trim() == new.to_string().trim() {
            return Ok(text.to_string());
        }
    }
    tbl.insert(last, Item::Value(new));
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_and_order_survive() {
        let t = "# top\nmodel = \"x\" # trailing\n\n[features]\nhooks = false\n";
        let o = set_path(t, &["features", "hooks"], "true").unwrap();
        assert!(o.contains("# top") && o.contains("# trailing"));
        assert_eq!(
            get_path(&o, &["features", "hooks"]).unwrap().unwrap(),
            "true"
        );
        let o2 = set_path(&o, &["mcp_servers", "tinkr", "command"], "\"tinkr\"").unwrap();
        assert!(o2.contains("[mcp_servers.tinkr]") || o2.contains("mcp_servers"));
        assert_eq!(set_path(&o2, &["features", "hooks"], "true").unwrap(), o2);
    }

    #[test]
    fn unparseable_is_reported() {
        assert!(matches!(
            set_path("a = ", &["b"], "1"),
            Err(EditError::Unparseable(_))
        ));
    }
}
