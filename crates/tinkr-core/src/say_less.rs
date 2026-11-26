use crate::{
    ledger::Ledger,
    provider::Provider,
    tokenizer::{self, TokenizerKind},
};

pub const SAY_LESS_CONCISE_RATIO: f64 = 0.65;
pub const SAY_LESS_MIN_OUTPUT_TOKENS: u64 = 12;
pub const DEFAULT_PAYBACK_TURNS: u64 = 5;

pub fn estimate_before_tokens(after_tokens: u64) -> Option<u64> {
    if after_tokens < SAY_LESS_MIN_OUTPUT_TOKENS {
        return None;
    }
    let before = ((after_tokens as f64) / SAY_LESS_CONCISE_RATIO).round() as u64;
    (before > after_tokens).then_some(before)
}

pub fn infer_provider_from_model(model: &str) -> Provider {
    let m = model.to_ascii_lowercase();
    if m.contains("gemini") {
        Provider::Gemini
    } else if m.contains("claude")
        || m.contains("sonnet")
        || m.contains("opus")
        || m.contains("haiku")
    {
        Provider::Anthropic
    } else if m.contains("gpt")
        || m.starts_with("o1")
        || m.starts_with("o3")
        || m.starts_with("o4")
    {
        Provider::OpenAi
    } else {
        Provider::Unknown
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Billing {
    PerToken,
    PerRequest,
}

pub const TERSE_RULE: &str = "\
Response style (Tinkr):
- Be terse for status updates and routine narration; lead with the answer, code, diff, or command.
- Skip filler, throat-clearing, and restating the user's question.
- Never alter, abbreviate, or truncate code blocks, commit messages, diffs, or tool arguments.
- Never omit errors, numbers, negations, caveats, or failure details.
- Safety & clarity exception: ALWAYS use full, clear sentences for security warnings, \
destructive or irreversible actions (delete/rm, drop, force-push, reset, credentials), \
legal/privacy notices, ambiguous requests (ask clarifying questions clearly), \
or whenever the user seems confused or asks for a detailed explanation.";

pub fn should_apply(billing: Billing) -> bool {
    billing == Billing::PerToken
}

pub fn user_needs_full_explanation(user_prompt: &str) -> bool {
    let lower = user_prompt.to_lowercase();
    [
        "confused",
        "don't understand",
        "do not understand",
        "explain in detail",
        "step by step",
        "step-by-step",
        "walk me through",
        "eli5",
        "security warning",
        "security audit",
        "threat model",
        "vulnerability",
        "force-push",
        "force push",
        "drop table",
        "drop database",
        "rm -rf",
        "reset --hard",
        "delete production",
        "not sure which",
        "pros and cons",
        "what are the tradeoffs",
        "help me decide",
        "ambiguous",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

pub fn is_overhead_repaid(turns: u64, net_saved: i64, payback_turns: u64) -> bool {
    if turns < payback_turns.max(1) {
        return true;
    }
    net_saved > 0
}

pub fn is_repaid_in_ledger(ledger: &Ledger, tool: &str, payback_turns: u64) -> bool {
    let Ok(s) = ledger.summary(None) else {
        return true;
    };
    if let Some(ls) = s.by_tool.get(tool) {
        if ls.events >= payback_turns.max(1) && ls.net_saved <= 0 {
            return false;
        }
    }
    if let Some(layer_ls) = s.by_layer.get("say_less") {
        if layer_ls.events >= payback_turns.max(1) && layer_ls.net_saved <= 0 {
            return false;
        }
    }
    true
}

/// Evaluate whether assistant prose in an ongoing multi-turn conversation is repaying `TERSE_RULE`.
/// If `>= payback_turns` assistant turns have occurred and cumulative estimated output savings
/// fail to cover a single unamortized rule injection (`rule_token_count()`), drop `TERSE_RULE`.
pub fn conversation_repays_overhead(
    assistant_prose_tokens: &[u64],
    payback_turns: usize,
) -> bool {
    let window = payback_turns.max(1);
    if assistant_prose_tokens.len() < window {
        return true;
    }
    let recent = &assistant_prose_tokens[assistant_prose_tokens.len() - window..];
    let saved: u64 = recent
        .iter()
        .filter_map(|&after| estimate_before_tokens(after).map(|b| b.saturating_sub(after)))
        .sum();
    saved > rule_token_count()
}

pub fn append_to_system(system: &str, billing: Billing, last_user_prompt: Option<&str>) -> String {
    if !should_apply(billing) {
        return system.to_string();
    }
    if last_user_prompt.is_some_and(user_needs_full_explanation) {
        return system.to_string();
    }
    if system.contains("Response style (Tinkr):") || crate::overlap::has_foreign_voice_rule(system)
    {
        return system.to_string();
    }
    if system.is_empty() {
        TERSE_RULE.to_string()
    } else {
        format!("{system}\n\n{TERSE_RULE}")
    }
}

pub fn rule_token_count() -> u64 {
    tokenizer::count(TERSE_RULE, TokenizerKind::Exact).tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_is_within_token_budget_and_preserves_code_commits_tool_args() {
        let n = rule_token_count();
        assert!(
            n > 20 && n <= crate::defaults::SAY_LESS_MAX_TOKENS,
            "rule is {n} tokens"
        );
        assert!(TERSE_RULE.contains("code blocks"));
        assert!(TERSE_RULE.contains("commit messages"));
        assert!(TERSE_RULE.contains("tool arguments"));
        assert!(TERSE_RULE.contains("ambiguous"));
    }

    #[test]
    fn safety_destructive_ambiguity_and_confused_exceptions_skip() {
        assert!(TERSE_RULE.contains("security warnings"));
        assert!(TERSE_RULE.contains("destructive"));
        assert!(TERSE_RULE.contains("confused"));
        assert_eq!(append_to_system("sys", Billing::PerRequest, None), "sys");
        for prompt in [
            "I'm confused, explain in detail",
            "Should I run git push --force-push or reset --hard?",
            "Run a security audit on auth.rs",
            "I'm not sure which design to pick, what are the tradeoffs?",
        ] {
            assert_eq!(
                append_to_system("sys", Billing::PerToken, Some(prompt)),
                "sys",
                "expected full-sentence bypass for: {prompt}"
            );
        }
        let applied = append_to_system("sys", Billing::PerToken, Some("fix bug"));
        assert!(applied.starts_with("sys\n\nResponse style (Tinkr):"));
        assert_eq!(
            append_to_system(&applied, Billing::PerToken, Some("fix bug")),
            applied
        );
    }

    #[test]
    fn drops_rule_when_overhead_not_repaid_within_n_turns() {
        assert!(is_overhead_repaid(3, -100, 5));
        assert!(!is_overhead_repaid(5, 0, 5));
        assert!(!is_overhead_repaid(6, -45, 5));
        assert!(is_overhead_repaid(5, 80, 5));

        // 5 tool-only assistant turns with 4 tokens of prose each -> 0 savings -> drops rule!
        assert!(!conversation_repays_overhead(&[4, 6, 0, 5, 8], 5));
        // 5 substantive assistant turns with 120 tokens each -> ~320 tokens saved > rule_token_count() -> keeps rule!
        assert!(conversation_repays_overhead(&[120, 110, 90, 130, 100], 5));
    }

    #[test]
    fn estimates_before_tokens_and_infers_provider() {
        assert_eq!(estimate_before_tokens(0), None);
        assert_eq!(estimate_before_tokens(11), None);
        let before = estimate_before_tokens(130).unwrap();
        assert_eq!(before, 200);
        assert_eq!(infer_provider_from_model("gemini-4-neon-high"), Provider::Gemini);
        assert_eq!(infer_provider_from_model("claude-sonnet-4-5"), Provider::Anthropic);
        assert_eq!(infer_provider_from_model("gpt-5"), Provider::OpenAi);
        assert_eq!(infer_provider_from_model("custom-local"), Provider::Unknown);
    }
}
