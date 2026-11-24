use std::{
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub fn which(name: &str, path_var: Option<std::ffi::OsString>) -> Option<PathBuf> {
    let exts: &[&str] = if cfg!(windows) {
        &["", ".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };
    let path_var = path_var.or_else(|| std::env::var_os("PATH"))?;
    for dir in std::env::split_paths(&path_var) {
        for e in exts {
            let p = dir.join(format!("{name}{e}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

fn resolve(shell: &str) -> PathBuf {
    if let Some(p) = which(shell, None) {
        return p;
    }
    if cfg!(windows) {
        if shell == "pwsh" {
            if let Some(p) = which("powershell", None) {
                return p;
            }
        }
        if shell == "bash" {
            for p in [
                r"C:\Program Files\Git\bin\bash.exe",
                r"C:\Program Files (x86)\Git\bin\bash.exe",
            ] {
                if Path::new(p).is_file() {
                    return PathBuf::from(p);
                }
            }
        }
    }
    PathBuf::from(shell)
}

fn build(shell: &str, cmd: &str) -> Command {
    match shell {
        "pwsh" | "powershell" => {
            let mut c = Command::new(resolve(shell));
            c.args(["-NoProfile", "-NonInteractive", "-Command", cmd]);
            c
        }
        "cmd" => {
            let mut c = Command::new("cmd");
            c.args(["/D", "/S", "/C", cmd]);
            c
        }
        other => {
            let mut c = Command::new(resolve(other));
            c.args(["-c", cmd]);
            c
        }
    }
}

pub fn default_shell() -> &'static str {
    if cfg!(windows) {
        if which("pwsh", None).is_some() {
            "pwsh"
        } else {
            "powershell"
        }
    } else {
        "sh"
    }
}

pub fn run(shell: &str, cmd: &str) -> std::io::Result<(i32, Vec<u8>)> {
    let mut tmp = tempfile::tempfile()?;
    let (out, err) = (tmp.try_clone()?, tmp.try_clone()?);
    let status = build(shell, cmd)
        .stdin(Stdio::inherit())
        .stdout(out)
        .stderr(err)
        .status()?;
    tmp.seek(SeekFrom::Start(0))?;
    let mut buf = Vec::new();
    tmp.read_to_end(&mut buf)?;
    Ok((status.code().unwrap_or(1), buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_both_streams_and_exit_code() {
        let (sh, cmd) = if cfg!(windows) {
            ("cmd", "echo out& echo err 1>&2& exit /b 3")
        } else {
            ("sh", "echo out; echo err >&2; exit 3")
        };
        let (code, out) = run(sh, cmd).unwrap();
        let s = String::from_utf8_lossy(&out);
        assert_eq!(code, 3);
        assert!(s.contains("out") && s.contains("err"), "{s:?}");
    }
}
