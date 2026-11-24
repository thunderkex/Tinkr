use crate::{io_err, platform::fsx, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::PathBuf, process::Command};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvValue {
    pub value: String,
    #[serde(default)]
    pub expand: bool,
}

impl EnvValue {
    pub fn plain(v: impl Into<String>) -> Self {
        Self {
            value: v.into(),
            expand: false,
        }
    }
}

pub trait EnvStore {
    fn name(&self) -> &'static str;
    fn get(&self, name: &str) -> Result<Option<EnvValue>>;
    fn set(&self, name: &str, value: &EnvValue) -> Result<()>;
    fn unset(&self, name: &str) -> Result<()>;
}

pub struct EnvDFile {
    pub path: PathBuf,
}

fn envd_quote(v: &str) -> String {
    let mut s = String::from("\"");
    for c in v.chars() {
        match c {
            '\\' | '"' | '$' => {
                s.push('\\');
                s.push(c);
            }
            '\n' => s.push_str("\\n"),
            _ => s.push(c),
        }
    }
    s.push('"');
    s
}

fn envd_unquote(v: &str) -> String {
    let v = v.trim();
    let inner = v
        .strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .or_else(|| v.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')));
    let Some(inner) = inner else {
        return v.to_string();
    };
    let mut out = String::new();
    let mut it = inner.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => out.push('\n'),
                Some(o) => out.push(o),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

impl EnvDFile {
    fn read_map(&self) -> Result<BTreeMap<String, String>> {
        let text = match fs::read(&self.path) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(io_err(&self.path)(e)),
        };
        let mut m = BTreeMap::new();
        for line in text.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = l.split_once('=') {
                m.insert(k.trim().to_string(), envd_unquote(v));
            }
        }
        Ok(m)
    }

    fn write_map(&self, m: &BTreeMap<String, String>) -> Result<()> {
        if m.is_empty() {
            return match fs::remove_file(&self.path) {
                Ok(()) => {
                    if let Some(d) = self.path.parent() {
                        let _ = fs::remove_dir(d);
                    }
                    Ok(())
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(io_err(&self.path)(e)),
            };
        }
        let mut s = String::from("# managed by tinkr; removed by `tinkr undo`\n");
        for (k, v) in m {
            s.push_str(&format!("{k}={}\n", envd_quote(v)));
        }
        fsx::atomic_write(&self.path, s.as_bytes())
    }
}

impl EnvStore for EnvDFile {
    fn name(&self) -> &'static str {
        "environment.d"
    }
    fn get(&self, name: &str) -> Result<Option<EnvValue>> {
        Ok(self
            .read_map()?
            .get(name)
            .map(|v| EnvValue::plain(v.clone())))
    }
    fn set(&self, name: &str, value: &EnvValue) -> Result<()> {
        let mut m = self.read_map()?;
        m.insert(name.to_string(), value.value.clone());
        self.write_map(&m)
    }
    fn unset(&self, name: &str) -> Result<()> {
        let mut m = self.read_map()?;
        m.remove(name);
        self.write_map(&m)
    }
}

pub struct PlistEnv {
    pub plist_path: PathBuf,
    pub state_path: PathBuf,
    pub launchctl: bool,
}

pub const PLIST_LABEL: &str = "dev.tinkr.env";

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn plist_xml(m: &BTreeMap<String, String>) -> String {
    let script: Vec<String> = m
        .iter()
        .map(|(k, v)| format!("/bin/launchctl setenv {} {}", sh_quote(k), sh_quote(v)))
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>{PLIST_LABEL}</string>\n\
  <key>ProgramArguments</key>\n  <array>\n    <string>/bin/sh</string>\n    <string>-c</string>\n    <string>{}</string>\n  </array>\n\
  <key>RunAtLoad</key>\n  <true/>\n</dict>\n</plist>\n",
        xml_escape(&script.join("; "))
    )
}

impl PlistEnv {
    fn read_map(&self) -> Result<BTreeMap<String, String>> {
        match fs::read(&self.state_path) {
            Ok(b) => Ok(serde_json::from_slice(&b).unwrap_or_default()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(io_err(&self.state_path)(e)),
        }
    }

    fn write_map(&self, m: &BTreeMap<String, String>) -> Result<()> {
        if m.is_empty() {
            for p in [&self.plist_path, &self.state_path] {
                match fs::remove_file(p) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(io_err(p)(e)),
                }
            }
            return Ok(());
        }
        fsx::atomic_write(&self.state_path, &serde_json::to_vec_pretty(m)?)?;
        fsx::atomic_write(&self.plist_path, plist_xml(m).as_bytes())
    }

    fn launchctl(&self, args: &[&str]) {
        if self.launchctl {
            let _ = Command::new("/bin/launchctl").args(args).status();
        }
    }
}

impl EnvStore for PlistEnv {
    fn name(&self) -> &'static str {
        "launchagent"
    }
    fn get(&self, name: &str) -> Result<Option<EnvValue>> {
        Ok(self
            .read_map()?
            .get(name)
            .map(|v| EnvValue::plain(v.clone())))
    }
    fn set(&self, name: &str, value: &EnvValue) -> Result<()> {
        let mut m = self.read_map()?;
        m.insert(name.to_string(), value.value.clone());
        self.write_map(&m)?;
        self.launchctl(&["setenv", name, &value.value]);
        Ok(())
    }
    fn unset(&self, name: &str) -> Result<()> {
        let mut m = self.read_map()?;
        m.remove(name);
        self.write_map(&m)?;
        self.launchctl(&["unsetenv", name]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envd_roundtrip_special_chars_and_cleanup() {
        let d = tempfile::tempdir().unwrap();
        let s = EnvDFile {
            path: d.path().join("environment.d").join("50-tinkr.conf"),
        };
        let weird = "http://127.0.0.1:1234/a b\"c$d\\e\u{fc}";
        s.set("ANTHROPIC_BASE_URL", &EnvValue::plain(weird))
            .unwrap();
        s.set("TINKR_ACTIVE", &EnvValue::plain("1")).unwrap();
        assert_eq!(s.get("ANTHROPIC_BASE_URL").unwrap().unwrap().value, weird);
        s.unset("ANTHROPIC_BASE_URL").unwrap();
        assert!(s.get("ANTHROPIC_BASE_URL").unwrap().is_none());
        assert!(s.path.exists());
        s.unset("TINKR_ACTIVE").unwrap();
        assert!(!s.path.exists());
        assert!(
            !d.path().join("environment.d").exists(),
            "empty dir removed"
        );
        s.unset("NOPE").unwrap();
    }

    #[test]
    fn plist_is_escaped_and_cleaned_up() {
        let d = tempfile::tempdir().unwrap();
        let s = PlistEnv {
            plist_path: d.path().join("LaunchAgents").join("dev.tinkr.env.plist"),
            state_path: d.path().join("state.json"),
            launchctl: false,
        };
        s.set("K", &EnvValue::plain("it's <&>")).unwrap();
        let xml = fs::read_to_string(&s.plist_path).unwrap();
        assert!(xml.contains("setenv &apos;K&apos;") || xml.contains("setenv 'K'"));
        assert!(xml.contains("&lt;&amp;&gt;"));
        assert!(xml.contains("'\\''"));
        assert_eq!(s.get("K").unwrap().unwrap().value, "it's <&>");
        s.unset("K").unwrap();
        assert!(!s.plist_path.exists() && !s.state_path.exists());
    }
}
