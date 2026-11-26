use serde_json::json;
use tinkr_core::{
    filters::{self, catalog, classify, Ctx, Decision},
    recall::RecallStore,
    tokenizer::TokenizerKind,
};

#[test]
fn all_declarative_toml_rules_compile_and_cover_rtk_parity_matrix() {
    let rules = catalog::rules();
    assert!(rules.len() >= 20, "expected full declarative catalog");
    for r in rules {
        let _ = r.drop_set();
        let _ = r.keep_set();
    }

    assert_eq!(classify::profile_id_of("npx jest"), "jest-vitest");
    assert_eq!(classify::profile_id_of("pnpm vitest run"), "jest-vitest");
    assert_eq!(classify::profile_id_of("npx tsc --noEmit"), "tsc");
    assert_eq!(classify::profile_id_of("npx eslint src/"), "eslint");
    assert_eq!(classify::profile_id_of("bundle exec rspec"), "rspec");
    assert_eq!(classify::profile_id_of("gh pr checks 42"), "gh");
    assert_eq!(
        classify::profile_id_of("aws --region us-east-1 ec2 describe-instances"),
        "aws"
    );
    assert_eq!(classify::profile_id_of("pip install -r requirements.txt"), "pip");
    assert_eq!(classify::profile_id_of("Get-ChildItem -Recurse"), "ls");
    assert_eq!(classify::profile_id_of("Select-String -Path src/*.rs -Pattern foo"), "grep");
    assert_eq!(classify::profile_id_of("Get-Content app.log"), "read");

    assert_eq!(classify::is_segment_read_only("gh pr list"), Some(true));
    assert_eq!(classify::is_segment_read_only("aws s3 ls s3://my-bucket"), Some(true));
    assert_eq!(classify::is_segment_read_only("npx tsc --noEmit"), Some(true));
    assert_eq!(classify::is_segment_read_only("npx eslint src/"), Some(true));
    assert_eq!(classify::is_segment_read_only("cargo test"), Some(false));
}

#[test]
fn golden_fixtures_across_all_17_catalog_tools_preserve_needles() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecallStore::open(dir.path()).unwrap();
    let ctx = Ctx {
        recall: Some(&store),
        unicode: false,
        tokenizer: TokenizerKind::Exact,
    };

    let mut git_status = String::from("On branch main\nChanges not staged for commit:\n  (use \"git add <file>...\" to update what will be committed)\n  (use \"git restore <file>...\" to discard changes in working directory)\n");
    for i in 0..55 {
        git_status.push_str(&format!("\tmodified:   crates/mod_{i:02}/src/lib.rs\n"));
    }
    git_status.push_str("\tmodified:   crates/auth/src/needle_git.rs\n");

    let mut cargo_out = String::new();
    for i in 0..70 {
        cargo_out.push_str(&format!("   Compiling dep_{i:02} v0.1.{i}\n"));
    }
    cargo_out.push_str("error[E0425]: cannot find value `needle_cargo` in this scope\n  --> src/main.rs:14:9\n");

    let mut pytest_out = String::from("============================= test session starts ==============================\nplatform linux -- Python 3.12.1\nrootdir: /app\ncollected 75 items\n");
    for i in 0..74 {
        pytest_out.push_str(&format!("tests/test_api.py::test_ok_{i:02} PASSED\n"));
    }
    pytest_out.push_str("tests/test_api.py::test_needle_pytest FAILED\nE       AssertionError: expected 201 == 500\n========================= 1 failed, 74 passed in 0.88s =========================\n");

    let mut vitest_out = String::from(" RUN  v2.1.0 /app\n");
    for i in 0..75 {
        vitest_out.push_str(&format!(" ✓ src/unit/u_{i:02}.spec.ts (3 tests) 5ms\n"));
    }
    vitest_out.push_str(" FAIL  src/unit/auth.spec.ts\nAssertionError: needle_vitest_token_expired at src/unit/auth.spec.ts:31:12\n");

    let mut tsc_out = String::new();
    for _ in 0..45 {
        tsc_out.push_str("> @acme/pkg@1.0.0 typecheck /workspace/packages/pkg\n> tsc --noEmit\n");
    }
    tsc_out.push_str("src/billing/tax.ts(88,15): error TS2322: Type 'null' is not assignable to type 'NeedleTscRate'.\n");

    let mut eslint_out = String::new();
    for i in 0..65 {
        eslint_out.push_str(&format!(
            "  eslint:cli-engine Processing /workspace/packages/web/src/components/view_{i:02}.tsx +2ms\n"
        ));
    }
    eslint_out.push_str("/workspace/src/security.ts\n  42:7  error  Unexpected eval() usage: needle_eslint_eval  no-eval\n✖ 1 problem (1 error, 0 warnings)\n");

    let mut docker_out = String::new();
    for i in 1..=65 {
        docker_out.push_str(&format!("#{i} CACHED [stage-1 {i}/65] RUN echo layer_{i}_cached_build_step\n"));
    }
    docker_out.push_str("ERROR: failed to solve: process \"/bin/sh -c make needle_docker_target\" did not complete successfully: exit code: 2\n");

    let mut kube_out = String::new();
    for i in 0..90 {
        kube_out.push_str(&format!("2026-11-25T10:00:{:02}Z INFO kubelet pod_ready pod=web-{i:02}\n", i % 60));
    }
    kube_out.push_str("2026-11-25T10:02:00Z ERROR kubelet BackOff restarting failed container needle_kubectl_pod\n");

    let mut gh_out = String::new();
    for i in 0..65 {
        gh_out.push_str(&format!("ci-linux-shard-{i:02}\tpass\t3m12s\thttps://github.com/org/repo/actions/runs/10{i}\n"));
    }
    gh_out.push_str("ci-windows-needle-gh\tfail\t1m44s\thttps://github.com/org/repo/actions/runs/9999\n");

    let aws_arr: Vec<_> = (0..70)
        .map(|i| {
            if i == 35 {
                json!({"Bucket": "prod-needle-aws-compromised", "PublicAccess": "ENABLED"})
            } else {
                json!({"Bucket": format!("internal-logs-archive-{i:03}"), "PublicAccess": "BLOCKED", "Region": "us-west-2"})
            }
        })
        .collect();
    let aws_out = serde_json::to_string_pretty(&aws_arr).unwrap();

    let mut pnpm_out = String::new();
    for i in 0..75 {
        pnpm_out.push_str(&format!("Downloading registry.npmjs.org/@types/pkg-{i:02}/1.0.0.tgz\n"));
    }
    pnpm_out.push_str("ERR_PNPM_PEER_DEP_ISSUES  Unmet peer needle_pnpm_react@19.0.0\n");

    let mut pip_out = String::new();
    for i in 0..65 {
        pip_out.push_str(&format!("Requirement already satisfied: pkg-{i:02}>=1.0 in /venv/lib/python3.12/site-packages (1.2.{i})\n"));
    }
    pip_out.push_str("ERROR: Cannot install needle-pip-pkg==2.0.0 because these package versions have conflicting dependencies.\n");

    let mut go_out = String::new();
    for i in 0..70 {
        go_out.push_str(&format!("=== RUN   TestHandler{i:02}\n--- PASS: TestHandler{i:02} (0.01s)\n"));
    }
    go_out.push_str("=== RUN   TestNeedleGoRace\n    race_test.go:77: race detected on map write\n--- FAIL: TestNeedleGoRace (0.04s)\nFAIL\tacme/pkg/sync\t0.81s\n");

    let mut rspec_out = String::from("Randomized with seed 12345\n");
    for i in 0..65 {
        rspec_out.push_str(&format!("  is expected to validate field_{i:02} cleanly\n"));
    }
    rspec_out.push_str("Failures:\n  1) Auth needle_rspec_jwt rejects forged signature\n     # ./spec/auth/jwt_spec.rb:52:in `block'\n66 examples, 1 failure\n");

    let mut gci_out = String::new();
    for i in 0..60 {
        gci_out.push_str(&format!(
            "\n    Directory: C:\\proj\\node_modules\\dep_{i:02}\n\nMode                 LastWriteTime         Length Name\n----                 -------------         ------ ----\n-a----        11/25/2026   1:00 PM           2048 index.js\n"
        ));
    }
    gci_out.push_str("\n    Directory: C:\\proj\\src\n\nMode                 LastWriteTime         Length Name\n----                 -------------         ------ ----\n-a----        11/25/2026   1:05 PM           4096 needle_gci_app.ts\n");

    let mut sls_out = String::new();
    for i in 0..55 {
        sls_out.push_str(&format!(
            "C:\\proj\\crates\\very_verbose_submodule_path\\handler.rs:{}:    let ok_{i} = validate_input(req);\n",
            i * 2 + 1
        ));
    }
    sls_out.push_str("C:\\proj\\crates\\very_verbose_submodule_path\\handler.rs:199:    return Err(AuthErr::NeedleSelectString);\n");

    let mut gc_out = String::new();
    for i in 0..95 {
        gc_out.push_str(&format!("2026-11-25T14:00:{:02}Z DEBUG background_sync tick={i} healthy=true\n", i % 60));
    }
    gc_out.push_str("2026-11-25T14:02:00Z ERROR disk_monitor needle_get_content_full_disk=/dev/sda1\n");

    let matrix: Vec<(&str, i32, &str, &str)> = vec![
        ("git status", 0, &git_status, "needle_git.rs"),
        ("cargo check", 101, &cargo_out, "needle_cargo"),
        ("pytest -v", 1, &pytest_out, "test_needle_pytest"),
        ("npx vitest run", 1, &vitest_out, "needle_vitest_token_expired"),
        ("npx tsc --noEmit", 2, &tsc_out, "NeedleTscRate"),
        ("npx eslint src/", 1, &eslint_out, "needle_eslint_eval"),
        ("docker build .", 1, &docker_out, "needle_docker_target"),
        ("kubectl -n prod logs deploy/web", 0, &kube_out, "needle_kubectl_pod"),
        ("gh pr checks 99", 1, &gh_out, "ci-windows-needle-gh"),
        ("aws s3api list-buckets", 0, &aws_out, "prod-needle-aws-compromised"),
        ("pnpm install", 1, &pnpm_out, "needle_pnpm_react@19.0.0"),
        ("pip install -r requirements.txt", 1, &pip_out, "needle-pip-pkg==2.0.0"),
        ("go test -v ./...", 1, &go_out, "race_test.go:77"),
        ("bundle exec rspec", 1, &rspec_out, "needle_rspec_jwt"),
        ("Get-ChildItem -Recurse", 0, &gci_out, "needle_gci_app.ts"),
        ("Select-String -Path C:\\proj\\*.rs -Pattern validate", 0, &sls_out, "NeedleSelectString"),
        ("Get-Content service_debug.log", 0, &gc_out, "needle_get_content_full_disk"),
    ];

    for (cmd, exit_code, raw, needle) in matrix {
        let out = filters::process(cmd, exit_code, raw.as_bytes(), &ctx);
        assert!(
            matches!(out.decision, Decision::Compressed { .. }),
            "expected `{cmd}` to compress, got {:?} (before={} tok)",
            out.decision,
            out.before
        );
        let text = String::from_utf8_lossy(&out.output);
        assert!(
            text.contains(needle),
            "needle `{needle}` missing from `{cmd}` compressed output:\n{text}"
        );
        assert!(out.after < out.before, "`{cmd}` did not shrink tokens");
    }
}
