#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

fn classify_words(words: &[String]) -> Option<Kind> {
    let mut i = 0;
    while i < words.len()
        && (words[i].contains('=') && !words[i].starts_with('-')
            || matches!(
                base(&words[i]).as_str(),
                "env" | "sudo" | "time" | "command" | "call"
            ))
    {
        i += 1;
    }
    let first = base(words.get(i)?);
    let args: Vec<&str> = words[i + 1..].iter().map(String::as_str).collect();
    let sub = |skip_flags_with_arg: &[&str]| -> Option<String> {
        let mut j = 0;
        while j < args.len() {
            let a = args[j];
            if skip_flags_with_arg.contains(&a) {
                j += 2;
            } else if a.starts_with('-') || a.starts_with('+') {
                j += 1;
            } else {
                return Some(a.to_lowercase());
            }
        }
        None
    };
    Some(match first.as_str() {
        "git" => match sub(&["-C", "-c", "--git-dir", "--work-tree"])?.as_str() {
            "status" => Kind::GitStatus,
            "log" => Kind::GitLog,
            "diff" | "show" => Kind::GitDiff,
            "branch" | "remote" | "tag" | "stash" | "fetch" | "pull" | "push" | "clone" | "add"
            | "commit" | "checkout" | "switch" | "merge" | "rebase" | "reflog" | "blame" => {
                Kind::GitOther
            }
            _ => return None,
        },
        "ls" | "dir" | "gci" | "get-childitem" | "tree" | "eza" | "exa" | "lsd" => Kind::ListDir,
        "find" | "where" | "fd" | "locate" => Kind::Find,
        "cat" | "type" | "get-content" | "gc" | "bat" | "head" | "tail" | "more" => Kind::FileRead,
        "grep" | "egrep" | "fgrep" | "rg" | "ag" | "ack" | "select-string" | "sls" | "findstr" => {
            Kind::Grep
        }
        "cargo" => match sub(&["--manifest-path", "-p", "--package", "--target"])?.as_str() {
            "test" | "nextest" | "bench" => Kind::Test,
            "build" | "check" | "clippy" | "doc" | "install" | "fetch" | "update" | "b" | "c"
            | "t" => {
                if matches!(sub(&[]).as_deref(), Some("t")) {
                    Kind::Test
                } else {
                    Kind::Build
                }
            }
            _ => return None,
        },
        "pytest" | "py.test" | "jest" | "vitest" | "mocha" | "ctest" | "rspec" | "phpunit"
        | "nosetests" => Kind::Test,
        "python" | "python3" | "py" => {
            if args
                .windows(2)
                .any(|w| w[0] == "-m" && matches!(w[1], "pytest" | "unittest"))
            {
                Kind::Test
            } else if args.windows(2).any(|w| w[0] == "-m" && w[1] == "pip") {
                Kind::Build
            } else {
                return None;
            }
        }
        "go" => match sub(&[])?.as_str() {
            "test" => Kind::Test,
            "build" | "install" | "get" | "vet" | "mod" => Kind::Build,
            _ => return None,
        },
        "npm" | "yarn" | "pnpm" | "bun" => match sub(&[])?.as_str() {
            "test" | "t" => Kind::Test,
            "install" | "i" | "ci" | "add" | "build" | "update" | "upgrade" | "audit" => {
                Kind::Build
            }
            "run" => {
                let target = args
                    .iter()
                    .skip_while(|a| **a != "run")
                    .nth(1)
                    .copied()
                    .unwrap_or("");
                if target.starts_with("test") {
                    Kind::Test
                } else if target.starts_with("build") {
                    Kind::Build
                } else {
                    return None;
                }
            }
            _ => return None,
        },
        "dotnet" => match sub(&[])?.as_str() {
            "test" => Kind::Test,
            "build" | "restore" | "publish" | "pack" => Kind::Build,
            _ => return None,
        },
        "mvn" | "mvnw" | "gradle" | "gradlew" => {
            if args.contains(&"test") {
                Kind::Test
            } else {
                Kind::Build
            }
        }
        "pip" | "pip3" | "tsc" | "make" | "cmake" | "msbuild" | "ninja" | "gcc" | "g++"
        | "clang" | "rustc" | "javac" | "poetry" | "uv" => Kind::Build,
        "docker" | "podman" | "docker-compose" => match sub(&[])?.as_str() {
            "build" | "buildx" | "pull" | "push" | "compose"
                if args.iter().any(|a| matches!(*a, "build" | "pull" | "push"))
                    || sub(&[]).as_deref() == Some("build") =>
            {
                Kind::Build
            }
            "ps" | "images" | "logs" | "ls" | "inspect" | "stats" | "compose" | "container"
            | "image" | "volume" | "network" => Kind::Docker,
            _ => return None,
        },
        "kubectl" | "k" | "oc" | "helm" => match sub(&["-n", "--namespace", "--context"])?.as_str()
        {
            "get" | "describe" | "logs" | "top" | "list" | "status" | "events" => Kind::Kube,
            _ => return None,
        },
        _ => return None,
    })
}

pub fn classify(cmd: &str) -> Option<Kind> {
    let (segs, shaped) = split_segments(cmd);
    if shaped || segs.is_empty() {
        return None;
    }
    let mut kind = None;
    for s in &segs {
        let b = base(&s[0]);
        if matches!(
            b.as_str(),
            "cd" | "pushd" | "popd" | "set-location" | "export" | "set"
        ) {
            continue;
        }
        kind = Some(classify_words(s)?);
    }
    kind
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
