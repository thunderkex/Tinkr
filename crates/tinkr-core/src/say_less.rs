use crate::{
    provider::Provider,
    tokenizer::{self, TokenizerKind},
};

pub const SAY_LESS_CONCISE_RATIO: f64 = 0.65;
pub const SAY_LESS_MIN_OUTPUT_TOKENS: u64 = 12;

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
    } else if m.contains("claude") || m.contains("sonnet") || m.contains("opus") || m.contains("haiku") {
        Provider::Anthropic
    } else if m.contains("gpt") || m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") {
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
- Be concise and direct. Lead with the answer, code, or command.
- Skip filler, throat-clearing, and restating the user's question.
- Prefer short bullets, diffs, and exact file:line references over prose.
- Never omit errors, numbers, negations, caveats, or failure details.
- Safety exception: ALWAYS use full, clear sentences for security warnings, \
destructive or irreversible actions (delete, drop, force-push, reset, credentials), \
legal/privacy notices, or whenever the user seems confused or asks for a detailed explanation.";

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
    ]
    .iter()
    .any(|p| lower.contains(p))
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
    fn rule_is_within_token_budget() {
        let n = rule_token_count();
        assert!(
            n > 20 && n <= crate::defaults::SAY_LESS_MAX_TOKENS,
            "rule is {n} tokens"
        );
    }

    #[test]
    fn safety_exceptions_and_billing_skip() {
        assert!(TERSE_RULE.contains("security warnings"));
        assert!(TERSE_RULE.contains("destructive"));
        assert!(TERSE_RULE.contains("confused"));
        assert_eq!(append_to_system("sys", Billing::PerRequest, None), "sys");
        assert_eq!(
            append_to_system(
                "sys",
                Billing::PerToken,
                Some("I'm confused, explain in detail")
            ),
            "sys"
        );
        let applied = append_to_system("sys", Billing::PerToken, Some("fix bug"));
        assert!(applied.starts_with("sys\n\nResponse style (Tinkr):"));
        assert_eq!(
            append_to_system(&applied, Billing::PerToken, Some("fix bug")),
            applied
        );
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
