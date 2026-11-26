use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    GitStatus,
    GitLog,
    GitDiff,
    GitOther,
    ListDir,
    Find,
    FileRead,
    Grep,
    Test,
    Build,
    Docker,
    Kube,
    Generic,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::GitStatus => "git-status",
            Kind::GitLog => "git-log",
            Kind::GitDiff => "git-diff",
            Kind::GitOther => "git",
            Kind::ListDir => "ls",
            Kind::Find => "find",
            Kind::FileRead => "read",
            Kind::Grep => "grep",
            Kind::Test => "test",
            Kind::Build => "build",
            Kind::Docker => "docker",
            Kind::Kube => "kubectl",
            Kind::Generic => "generic",
        }
    }
    pub fn read_only(self) -> bool {
        matches!(
            self,
            Kind::GitStatus
                | Kind::GitLog
                | Kind::GitDiff
                | Kind::ListDir
                | Kind::Find
                | Kind::FileRead
                | Kind::Grep
        )
    }
}

fn split_segments(cmd: &str) -> (Vec<Vec<String>>, bool) {
    let mut segs: Vec<Vec<String>> = vec![vec![]];
    let mut cur = String::new();
    let mut have = false;
    let mut shaped = false;
    let mut q: Option<char> = None;
    let chars: Vec<char> = cmd.chars().collect();
    let mut i = 0;
    let flush = |segs: &mut Vec<Vec<String>>, cur: &mut String, have: &mut bool| {
        if *have {
            if let Some(last) = segs.last_mut() {
                last.push(std::mem::take(cur));
            }
            *have = false;
        }
    };
    while i < chars.len() {
        let c = chars[i];
        match q {
            Some(qc) => {
                if c == qc {
                    q = None;
                } else if c == '\\' && qc == '"' && chars.get(i + 1) == Some(&'"') {
                    i += 1;
                    cur.push(chars[i]);
                } else if qc == '"' && (c == '`' || (c == '$' && chars.get(i + 1) == Some(&'('))) {
                    shaped = true;
                } else {
                    cur.push(c);
                }
            }
            None => match c {
                '\'' | '"' => {
                    q = Some(c);
                    have = true;
                }
                '\n' | '\r' | '`' | '(' | ')' => shaped = true,
                '$' if matches!(chars.get(i + 1), Some('(' | '{')) => shaped = true,
                c if c.is_whitespace() => flush(&mut segs, &mut cur, &mut have),
                '|' | '>' | '<' => {
                    if c == '|' && chars.get(i + 1) == Some(&'|') {
                        flush(&mut segs, &mut cur, &mut have);
                        segs.push(vec![]);
                        i += 1;
                    } else if c == '>' && cur == "2" && chars.get(i + 1) == Some(&'&') {
                        cur.clear();
                        have = false;
                        i += 2;
                    } else {
                        shaped = true;
                    }
                }
                '&' => {
                    if chars.get(i + 1) == Some(&'&') {
                        flush(&mut segs, &mut cur, &mut have);
                        segs.push(vec![]);
                        i += 1;
                    } else {
                        shaped = true;
                    }
                }
                ';' => {
                    flush(&mut segs, &mut cur, &mut have);
                    segs.push(vec![]);
                }
                _ => {
                    cur.push(c);
                    have = true;
                }
            },
        }
        i += 1;
    }
    flush(&mut segs, &mut cur, &mut have);
    if q.is_some() {
        shaped = true;
    }
    segs.retain(|s| !s.is_empty());
    (segs, shaped)
}

fn base(word: &str) -> String {
    let w = word
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(word)
        .to_lowercase();
    w.strip_suffix(".exe").map(str::to_string).unwrap_or(w)
}

pub fn match_catalog(cmd: &str) -> Option<&'static super::catalog::CatalogRule> {
    let (segs, shaped) = split_segments(cmd);
    if shaped || segs.is_empty() {
        return None;
    }
    let mut matched = None;
    for s in &segs {
        let b = base(&s[0]);
        if matches!(
            b.as_str(),
            "cd" | "pushd" | "popd" | "set-location" | "export" | "set"
        ) {
            continue;
        }
        matched = Some(super::catalog::match_words(s)?);
    }
    matched
}

pub fn is_segment_read_only(seg: &str) -> Option<bool> {
    match_catalog(seg).map(|r| r.raw.read_only)
}

pub fn profile_id_of(cmd: &str) -> &'static str {
    match_catalog(cmd)
        .map(|r| r.raw.id.as_str())
        .unwrap_or("generic")
}

pub fn classify(cmd: &str) -> Option<Kind> {
    match_catalog(cmd).map(|r| r.raw.kind)
}

pub fn first_word(cmd: &str) -> Option<String> {
    let (segs, _) = split_segments(cmd);
    segs.last().and_then(|s| s.first()).map(|w| base(w))
}

pub fn command_segments(cmd: &str) -> Option<Vec<String>> {
    let (segs, shaped) = split_segments(cmd);
    if shaped {
        return None;
    }
    Some(
        segs.into_iter()
            .filter(|s| {
                !matches!(
                    base(&s[0]).as_str(),
                    "cd" | "pushd" | "popd" | "set-location" | "export" | "set"
                )
            })
            .map(|s| s.join(" "))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_commands_across_shells() {
        assert_eq!(classify("git status"), Some(Kind::GitStatus));
        assert_eq!(classify("git -C /x/y log --oneline"), Some(Kind::GitLog));
        assert_eq!(classify("C:\\Program Files\\Git\\cmd\\git.exe diff"), None);
        assert_eq!(
            classify("\"C:\\Program Files\\Git\\cmd\\git.exe\" diff"),
            Some(Kind::GitDiff)
        );
        assert_eq!(classify("Get-ChildItem -Recurse"), Some(Kind::ListDir));
        assert_eq!(classify("dir /s"), Some(Kind::ListDir));
        assert_eq!(classify("type foo.txt"), Some(Kind::FileRead));
        assert_eq!(classify("Select-String -Path *.rs foo"), Some(Kind::Grep));
        assert_eq!(classify("cargo test --workspace"), Some(Kind::Test));
        assert_eq!(classify("cargo build --release 2>&1"), Some(Kind::Build));
        assert_eq!(classify("cd sub && npm run build"), Some(Kind::Build));
        assert_eq!(classify("python -m pytest -x"), Some(Kind::Test));
        assert_eq!(classify("kubectl -n prod get pods"), Some(Kind::Kube));
        assert_eq!(classify("docker ps -a"), Some(Kind::Docker));
        assert_eq!(classify("docker build -t x ."), Some(Kind::Build));
        assert_eq!(classify("FOO=1 cargo check"), Some(Kind::Build));
    }

    #[test]
    fn leaves_shaped_or_unknown_commands_alone() {
        assert_eq!(classify("git log | head -5"), None);
        assert_eq!(classify("cargo test > out.txt"), None);
        assert_eq!(classify("cargo build &"), None);
        assert_eq!(classify("echo hi"), None);
        assert_eq!(classify("git push --force"), Some(Kind::GitOther));
        assert_eq!(classify("grep 'a|b' file"), Some(Kind::Grep));
        assert_eq!(classify("cargo test\nrm -rf x"), None);
        assert_eq!(classify("git"), None);
    }

    #[test]
    fn read_only_flag() {
        assert!(Kind::GitStatus.read_only());
        assert!(!Kind::Test.read_only());
        assert!(!Kind::GitOther.read_only());
    }
}
