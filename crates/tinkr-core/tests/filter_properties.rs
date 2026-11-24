use tinkr_core::{
    compress,
    edit::{ini, jsonc, tomled, xml, yaml},
    filters::{self, classify, Ctx, Decision},
    recall::RecallStore,
    tokenizer::TokenizerKind,
};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| (self.next() & 0xff) as u8).collect()
    }
    fn ascii_or_utf8(&mut self, len: usize) -> String {
        const ALPHABET: &[char] = &[
            'a',
            'b',
            '0',
            '9',
            ' ',
            '\n',
            '\r',
            '\t',
            ':',
            '/',
            '\\',
            '"',
            '\'',
            '{',
            '}',
            '[',
            ']',
            '<',
            '>',
            '=',
            ',',
            ';',
            '|',
            '&',
            '$',
            '`',
            '#',
            '-',
            '_',
            '.',
            '\u{00fc}',
            '\u{2603}',
            '\u{1f680}',
        ];
        (0..len)
            .map(|_| ALPHABET[(self.next() as usize) % ALPHABET.len()])
            .collect()
    }
}

#[test]
fn pipeline_never_panics_on_arbitrary_bytes_and_preserves_invariants() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecallStore::open(dir.path()).unwrap();
    let ctx = Ctx {
        recall: Some(&store),
        unicode: false,
        tokenizer: TokenizerKind::Exact,
    };
    let cmds = [
        "git status",
        "git log",
        "git diff",
        "ls -la",
        "Get-ChildItem",
        "dir /s",
        "cat foo.txt",
        "grep foo bar",
        "Select-String foo *.rs",
        "cargo test",
        "cargo build",
        "npm test",
        "pytest",
        "go test ./...",
        "dotnet test",
        "docker ps",
        "kubectl get pods",
        "unknown-cmd",
    ];
    let mut rng = Rng(0xDEAD_BEEF_CAFE_BABE);
    for case in 0..200 {
        let len = (rng.next() as usize) % 4096;
        let raw = if case % 3 == 0 {
            rng.bytes(len)
        } else {
            rng.ascii_or_utf8(len).into_bytes()
        };
        let cmd = cmds[case % cmds.len()];
        let exit_code = if case % 4 == 0 { 1 } else { 0 };
        let out = filters::process(cmd, exit_code, &raw, &ctx);
        match out.decision {
            Decision::Passthrough(_) => {
                assert_eq!(out.output, raw, "passthrough must be byte-identical");
            }
            Decision::Compressed { .. } => {
                assert!(
                    out.after < out.before,
                    "compressed output must be strictly smaller in tokens"
                );
                let id = out.recall_id.as_deref().expect("recall id required");
                assert_eq!(
                    store.get(id).unwrap(),
                    raw,
                    "compress -> recall must equal exact original bytes"
                );
            }
        }
    }
}

#[test]
fn edge_cases_empty_binary_nul_huge_line_non_utf8_crlf() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecallStore::open(dir.path()).unwrap();
    let ctx = Ctx {
        recall: Some(&store),
        unicode: true,
        tokenizer: TokenizerKind::Exact,
    };

    let empty = filters::process("cargo test", 0, b"", &ctx);
    assert!(matches!(empty.decision, Decision::Passthrough("small")));
    assert!(empty.output.is_empty());

    let mut bin = vec![b'a'; 4000];
    bin[100] = 0;
    bin[200] = 0xff;
    let out_bin = filters::process("cat image.bin", 0, &bin, &ctx);
    assert!(matches!(out_bin.decision, Decision::Passthrough("binary")));
    assert_eq!(out_bin.output, bin);

    let bad_git = format!(
        "commit \u{1f680}\u{1f680}\u{1f680}abcdef\nAuthor: Dev\nDate: Today\n\n    subject\n{}",
        "    body line\n".repeat(120)
    );
    let _ = filters::process("git log", 0, bad_git.as_bytes(), &ctx);

    let huge_line = "x ".repeat(100_000);
    let _ = filters::process("cat huge.txt", 0, huge_line.as_bytes(), &ctx);

    let deep = format!("{}{}{}", "[".repeat(200), "1", "]".repeat(200));
    let _ = compress::compress_tool_result(&deep, TokenizerKind::Exact, None, false);

    let mut mixed = b"line1\r\nline2\n\xff\xfeerror: keep_me\r\n".to_vec();
    for i in 0..100 {
        mixed.extend_from_slice(format!("ok line {i}\r\n").as_bytes());
    }
    let out_mixed = filters::process("cargo test", 1, &mixed, &ctx);
    let s = String::from_utf8_lossy(&out_mixed.output);
    assert!(s.contains("keep_me"));
}

#[test]
fn parsers_and_rewriter_never_panic_on_fuzzed_inputs() {
    let mut rng = Rng(0x1234_5678_9ABC_DEF0);
    for _ in 0..300 {
        let len = (rng.next() as usize) % 512;
        let s = rng.ascii_or_utf8(len);
        let _ = jsonc::parse(&s);
        let _ = jsonc::get_path(&s, &["a", "b"]);
        let _ = jsonc::set_path(&s, &["a", "b"], &serde_json::json!(1));
        let _ = jsonc::set_path(&s, &[], &serde_json::json!(1));
        let _ = ini::set_key(&s, "sec", "k", "v");
        let _ = xml::set_jetbrains_option(&s, "Comp", "opt", "val");
        let _ = tomled::set_path(&s, &["a", "b"], "v");
        let _ = yaml::set_key(&s, "k", "v");
        let _ = classify::classify(&s);
        let _ = classify::first_word(&s);
        let _ = classify::command_segments(&s);
        let _ = compress::detect_kind(&s);
        let _ = compress::compress_tool_result(&s, TokenizerKind::Exact, None, false);
    }
}

#[test]
fn golden_fixtures_for_all_supported_tools_and_shells() {
    let ctx = Ctx {
        recall: None,
        unicode: false,
        tokenizer: TokenizerKind::Exact,
    };

    let mut pytest_out = String::from(
        "platform linux -- Python 3.12\nrootdir: /repo\ncollecting ... collected 80 items\n",
    );
    for i in 0..79 {
        pytest_out.push_str(&format!("tests/test_mod.py::test_case_{i} PASSED\n"));
    }
    pytest_out.push_str("tests/test_mod.py::test_auth FAILED\nAssertionError: token not valid (expected 401, got 200)\n1 failed, 79 passed in 1.2s\n");
    let res = filters::process("pytest -v", 1, pytest_out.as_bytes(), &ctx);
    assert!(matches!(res.decision, Decision::Compressed { .. }));
    let s = String::from_utf8_lossy(&res.output);
    assert!(s.contains("test_auth FAILED") && s.contains("expected 401, got 200"));
    assert!(!s.contains("test_case_10 PASSED"));

    let mut go_out = String::new();
    for i in 0..80 {
        go_out.push_str(&format!(
            "=== RUN   TestFunc{i}\n--- PASS: TestFunc{i} (0.00s)\n"
        ));
    }
    go_out.push_str("=== RUN   TestBroken\n    main_test.go:42: expected 7, got 9\n--- FAIL: TestBroken (0.01s)\nFAIL\tgithub.com/x/y\t0.1s\n");
    let res = filters::process("go test -v ./...", 1, go_out.as_bytes(), &ctx);
    assert!(matches!(res.decision, Decision::Compressed { .. }));
    let s = String::from_utf8_lossy(&res.output);
    assert!(s.contains("main_test.go:42") && s.contains("FAIL"));

    let mut dn_out = String::from(
        "Determining projects to restore...\n  Restored /app/app.csproj (in 120 ms).\n",
    );
    for i in 0..80 {
        dn_out.push_str(&format!("  Passed App.Tests.Case{i} [1 ms]\n"));
    }
    dn_out.push_str("  Failed App.Tests.Critical [5 ms]\n  Error Message: Assert.Equal() Failure\nFailed!  - Failed: 1, Passed: 80\n");
    let res = filters::process("dotnet test", 1, dn_out.as_bytes(), &ctx);
    assert!(matches!(res.decision, Decision::Compressed { .. }));
    let s = String::from_utf8_lossy(&res.output);
    assert!(s.contains("App.Tests.Critical") && s.contains("Assert.Equal() Failure"));

    let mut npm_out = String::new();
    for i in 0..80 {
        npm_out.push_str(&format!(
            "npm http fetch GET 200 https://registry.npmjs.org/pkg-{i} 12ms\n"
        ));
    }
    npm_out.push_str("added 80 packages in 2s\n");
    let res = filters::process("npm install", 0, npm_out.as_bytes(), &ctx);
    assert!(matches!(res.decision, Decision::Compressed { .. }));
    assert!(String::from_utf8_lossy(&res.output).contains("added 80 packages"));

    let mut kube =
        String::from("NAME                                    READY   STATUS    RESTARTS   AGE\n");
    for i in 0..250 {
        if i == 125 {
            kube.push_str(
                "payments-worker-7f8b9c-needle           0/1     Error     9          2m\n",
            );
        } else {
            kube.push_str(&format!(
                "api-server-deployment-7f8b9c-{i:03}        1/1     Running   0          12d\n"
            ));
        }
    }
    let res = filters::process("kubectl get pods", 0, kube.as_bytes(), &ctx);
    assert!(
        matches!(res.decision, Decision::Compressed { .. }),
        "kubectl get pods not compressed: {:?}",
        res.decision
    );
    assert!(String::from_utf8_lossy(&res.output).contains("payments-worker-7f8b9c-needle"));
}
