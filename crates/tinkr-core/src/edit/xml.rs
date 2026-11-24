use super::EditError;

fn xml_attr_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn set_jetbrains_option(
    text: &str,
    component: &str,
    option: &str,
    value: &str,
) -> Result<String, EditError> {
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let val_esc = xml_attr_escape(value);
    let new_tag = format!("<option name=\"{option}\" value=\"{val_esc}\" />");
    if text.trim().is_empty() {
        return Ok(format!(
            "<application>{eol}  <component name=\"{component}\">{eol}    {new_tag}{eol}  </component>{eol}</application>{eol}"
        ));
    }
    if !text.contains('<') || !text.contains('>') {
        return Err(EditError::Unparseable("not XML".into()));
    }
    let comp_open = format!("<component name=\"{component}\">");
    if let Some(ci) = text.find(&comp_open) {
        let comp_end = text[ci..]
            .find("</component>")
            .ok_or_else(|| EditError::Unparseable("unclosed component".into()))?
            + ci;
        let opt_needle = format!("<option name=\"{option}\"");
        if let Some(rel) = text[ci..comp_end].find(&opt_needle) {
            let start = ci + rel;
            let end = text[start..comp_end]
                .find("/>")
                .ok_or_else(|| EditError::Unparseable("unclosed option".into()))?
                + start
                + 2;
            if text[start..end] == new_tag {
                return Ok(text.to_string());
            }
            let mut out = text.to_string();
            out.replace_range(start..end, &new_tag);
            return Ok(out);
        }
        let mut out = text.to_string();
        out.insert_str(comp_end, &format!("  {new_tag}{eol}  "));
        return Ok(out);
    }
    if let Some(app_end) = text.rfind("</application>") {
        let mut out = text.to_string();
        out.insert_str(
            app_end,
            &format!(
                "  <component name=\"{component}\">{eol}    {new_tag}{eol}  </component>{eol}"
            ),
        );
        return Ok(out);
    }
    Err(EditError::Unparseable("missing <application> root".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn updates_and_inserts_jetbrains_options() {
        let xml = "<application>\n  <!-- keep -->\n  <component name=\"AiSettings\">\n    <option name=\"other\" value=\"1\" />\n  </component>\n</application>\n";
        let o = set_jetbrains_option(xml, "AiSettings", "endpoint", "http://127.0.0.1:9").unwrap();
        assert!(
            o.contains("<!-- keep -->") && o.contains("other") && o.contains("http://127.0.0.1:9")
        );
        assert_eq!(
            set_jetbrains_option(&o, "AiSettings", "endpoint", "http://127.0.0.1:9").unwrap(),
            o
        );
    }
}
