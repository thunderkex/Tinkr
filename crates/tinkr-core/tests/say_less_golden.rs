use serde_json::{json, Value};
use tinkr_core::{
    adapters,
    ledger::{CountKind, Event, Layer, Ledger},
    provider::Provider,
    say_less::{self, Billing},
};

#[test]
fn golden_say_less_exceptions_code_invariants_and_payback_drop() {
    // 1. Budget & code/commit/tool-arg preservation clauses
    let toks = say_less::rule_token_count();
    assert!(toks <= 300, "rule exceeded 300-token budget: {toks}");
    assert!(say_less::TERSE_RULE.contains("Never alter, abbreviate, or truncate code blocks, commit messages, diffs, or tool arguments."));

    // 2. Full-sentence bypasses for security, destructive actions, ambiguity, and confused user
    for p in [
        "Perform a security audit of our JWT verification",
        "Is it safe to git push --force-push to main?",
        "This spec is ambiguous — help me decide between option A and B",
        "I don't understand how borrow checking failed here",
    ] {
        assert!(
            say_less::user_needs_full_explanation(p),
            "should trigger full-sentence mode: {p}"
        );
        assert_eq!(say_less::append_to_system("SYS", Billing::PerToken, Some(p)), "SYS");
    }

    // 3. Routine narration prompt applies TERSE_RULE cleanly without touching code/tool_use blocks in messages!
    let code_snippet = "fn main() {\n    println!(\"exact code untouched\");\n}\n";
    let req_ok = json!({
        "model": "claude-sonnet-4-5",
        "system": "System base.",
        "messages": [
            { "role": "user", "content": format!("check this:\n```rust\n{code_snippet}```") },
            {
                "role": "assistant",
                "content": [
                    { "type": "text", "text": "Here is a detailed status update across multiple sentences explaining what I checked so far in the repository and what I am running next." },
                    { "type": "tool_use", "id": "t1", "name": "Bash", "input": { "command": "git commit -m \"feat: exact commit message\"" } }
                ]
            },
            { "role": "user", "content": "next step" }
        ]
    });
    let tr_ok = adapters::transform_request(
        Provider::Anthropic,
        "/v1/messages",
        &serde_json::to_vec(&req_ok).unwrap(),
        None,
        false,
    );
    assert!(tr_ok.say_less_applied);
    let out_ok: Value = serde_json::from_slice(&tr_ok.body).unwrap();
    assert!(out_ok["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains(code_snippet));
    assert_eq!(
        out_ok["messages"][1]["content"][1]["input"]["command"].as_str(),
        Some("git commit -m \"feat: exact commit message\"")
    );

    // 4. Ledger payback gate: 5 unrepaid turns -> `is_repaid_in_ledger` returns false!
    let ledger = Ledger::open_in_memory().unwrap();
    for i in 0..5 {
        ledger
            .record(&Event {
                ts: 100 + i,
                layer: Layer::SayLess,
                provider: Provider::Anthropic,
                tool: "anthropic".into(),
                session: None,
                before: 10,
                after: 10,
                kind: CountKind::Estimated,
                recall_id: None,
            })
            .unwrap();
    }
    assert!(!say_less::is_repaid_in_ledger(&ledger, "anthropic", 5));
}
