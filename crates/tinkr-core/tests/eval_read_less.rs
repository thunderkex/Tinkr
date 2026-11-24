use tinkr_core::{
    filters::{self, Ctx, Decision},
    recall::RecallStore,
    tokenizer::TokenizerKind,
};

fn build_fixtures() -> Vec<(&'static str, &'static str, i32, String, &'static str)> {
    let mut cargo_test = String::from("   Compiling tinkr v0.1.0\nrunning 121 tests\n");
    for i in 0..120 {
        cargo_test.push_str(&format!("test module_a::case_{i:03} ... ok\n"));
        if i == 60 {
            cargo_test.push_str("test module_a::needle_case ... FAILED\n");
        }
    }
    cargo_test.push_str("\nfailures:\n\n---- module_a::needle_case stdout ----\nthread 'module_a::needle_case' panicked at src/needle.rs:88:13:\nassertion failed: expected 42, got 0\n\ntest result: FAILED. 120 passed; 1 failed; 0 ignored\n");

    let mut cargo_build = String::new();
    for i in 0..80 {
        cargo_build.push_str(&format!(
            "   Compiling dep_crate_{i:03} v1.2.{i} (https://crates.io)\n"
        ));
        if i == 40 {
            cargo_build.push_str("error[E0308]: mismatched types\n  --> src/auth.rs:42:17\n   |\n42 |     let x: u32 = \"bad\";\n   |                  ^^^^^ expected `u32`, found `&str`\n");
        }
    }
    cargo_build.push_str("error: could not compile `app` (lib) due to 1 previous error\n");

    let mut git_log = String::new();
    for i in 0..35 {
        git_log.push_str(&format!(
            "commit {i:040x}\nAuthor: Dev {i} <dev{i}@example.com>\nDate:   Mon Nov 23 10:{:02}:00 2026 +0700\n\n    Fix issue #{i} in subsystem\n\n    Detailed multi-line commit body explaining background context\n    and rationale for change number {i} across several sentences.\n\n",
            i % 60
        ));
    }

    let mut ls_l = String::from("total 240\n");
    for i in 0..60 {
        ls_l.push_str(&format!(
            "-rw-r--r--  1 user staff  {:>7} Nov 23 10:00 file_{i:03}.rs\n",
            (i + 1) * 1024
        ));
    }

    let mut grep_out = String::new();
    for i in 0..40 {
        grep_out.push_str(&format!(
            "crates/tinkr-core/src/very_long_module_name.rs:{}:    let val_{i} = compute({i});\n",
            i * 3 + 1
        ));
    }
    grep_out.push_str(
        "crates/tinkr-core/src/very_long_module_name.rs:199:    // error: critical_grep_needle\n",
    );
    for i in 0..40 {
        grep_out.push_str(&format!(
            "crates/tinkr-core/src/another_long_module_name.rs:{}:    let other_{i} = run({i});\n",
            i * 3 + 1
        ));
    }

    let mut git_diff = String::from("diff --git a/src/lib.rs b/src/lib.rs\nindex 1234567..89abcde 100644\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,60 +1,60 @@\n");
    for i in 0..25 {
        git_diff.push_str(&format!(
            "     let unchanged_before_{i} = compute_something({i});\n"
        ));
    }
    git_diff.push_str("-    let old_bug = true;\n+    let old_bug = false; // needle_diff_fix\n");
    for i in 0..25 {
        git_diff.push_str(&format!(
            "     let unchanged_after_{i} = finalize_something({i});\n"
        ));
    }

    vec![
        (
            "cargo test",
            "cargo test --workspace",
            1,
            cargo_test,
            "src/needle.rs:88:13",
        ),
        (
            "cargo build",
            "cargo build --release",
            1,
            cargo_build,
            "src/auth.rs:42:17",
        ),
        ("git log", "git log -n 35", 0, git_log, "Fix issue #17"),
        ("ls -l", "ls -la", 0, ls_l, "file_030.rs"),
        (
            "grep",
            "rg compute crates/",
            0,
            grep_out,
            "critical_grep_needle",
        ),
        ("git diff", "git diff", 0, git_diff, "needle_diff_fix"),
    ]
}

#[test]
fn median_reduction_exceeds_60_percent_and_all_needles_survive() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecallStore::open(dir.path()).unwrap();
    let ctx = Ctx {
        recall: Some(&store),
        unicode: false,
        tokenizer: TokenizerKind::Exact,
    };

    let mut reductions = Vec::new();
    for (name, cmd, code, raw, needle) in build_fixtures() {
        let out = filters::process(cmd, code, raw.as_bytes(), &ctx);
        assert!(
            matches!(out.decision, Decision::Compressed { .. }),
            "{name} was not compressed: {:?}",
            out.decision
        );
        let text = String::from_utf8_lossy(&out.output);
        assert!(
            text.contains(needle),
            "needle '{needle}' missing from {name} output:\n{text}"
        );
        let id = out.recall_id.as_deref().expect("recall id present");
        assert_eq!(
            store.get(id).unwrap(),
            raw.as_bytes(),
            "recall mismatch for {name}"
        );
        let crlf = raw.replace('\n', "\r\n");
        let out_crlf = filters::process(cmd, code, crlf.as_bytes(), &ctx);
        let strip_marker = |b: &[u8]| {
            let s = String::from_utf8_lossy(b);
            s.lines()
                .filter(|l| !l.starts_with("[tinkr:"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(
            strip_marker(&out.output),
            strip_marker(&out_crlf.output),
            "CRLF changed compressed body for {name}"
        );

        let pct = (out.before - out.after) as f64 * 100.0 / out.before as f64;
        reductions.push((name, out.before, out.after, pct));
    }

    reductions.sort_by(|a, b| a.3.partial_cmp(&b.3).unwrap());
    let median = reductions[reductions.len() / 2].3;
    assert!(
        median >= 60.0,
        "median reduction {median:.1}% < 60%: {reductions:?}"
    );
}
