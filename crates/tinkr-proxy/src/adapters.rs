use serde_json::Value;
use tinkr_core::{
    compress,
    provider::Provider,
    recall::RecallStore,
    say_less::{self, Billing},
    tokenizer::TokenizerKind,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePolicy {
    ExplicitMarkers,
    PrefixLastTurnOnly,
}

#[derive(Debug, Clone)]
pub struct TransformResult {
    pub body: Vec<u8>,
    pub provider: Provider,
    pub before: u64,
    pub after: u64,
    pub modified: bool,
    pub say_less_applied: bool,
}

pub trait ProviderAdapter: Send + Sync {
    fn provider(&self) -> Provider;
    fn detect(&self, path: &str, body: &Value) -> bool;
    fn cache_policy(&self) -> CachePolicy;
    fn tokenizer(&self) -> TokenizerKind;
    fn billing(&self) -> Billing;
    fn transform(
        &self,
        body: &mut Value,
        recall: Option<&RecallStore>,
        unicode: bool,
    ) -> (u64, u64, bool, bool);
}

fn has_cache_control(v: &Value) -> bool {
    match v {
        Value::Object(map) => {
            map.contains_key("cache_control") || map.values().any(has_cache_control)
        }
        Value::Array(arr) => arr.iter().any(has_cache_control),
        _ => false,
    }
}

fn extract_text_from_content(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = v.as_array() {
        let mut last = None;
        for item in arr {
            if item["type"].as_str() == Some("text") || item["type"].as_str() == Some("input_text")
            {
                if let Some(t) = item["text"].as_str() {
                    last = Some(t.to_string());
                }
            }
        }
        return last;
    }
    None
}

pub struct AnthropicAdapter;

impl ProviderAdapter for AnthropicAdapter {
    fn provider(&self) -> Provider {
        Provider::Anthropic
    }
    fn detect(&self, path: &str, body: &Value) -> bool {
        path.contains("/v1/messages") || body.get("anthropic_version").is_some()
    }
    fn cache_policy(&self) -> CachePolicy {
        CachePolicy::ExplicitMarkers
    }
    fn tokenizer(&self) -> TokenizerKind {
        TokenizerKind::Calibrated(1.05)
    }
    fn billing(&self) -> Billing {
        Billing::PerToken
    }
    fn transform(
        &self,
        body: &mut Value,
        recall: Option<&RecallStore>,
        unicode: bool,
    ) -> (u64, u64, bool, bool) {
        let mut before_total = 0u64;
        let mut after_total = 0u64;
        let mut changed = false;

        let mut last_cached_msg = 0usize;
        let mut last_user_text: Option<String> = None;
        if let Some(msgs) = body.get("messages").and_then(Value::as_array) {
            for (i, m) in msgs.iter().enumerate() {
                if has_cache_control(m) {
                    last_cached_msg = i + 1;
                }
                if m["role"].as_str() == Some("user") {
                    if let Some(s) = extract_text_from_content(&m["content"]) {
                        last_user_text = Some(s);
                    }
                }
            }
            if last_cached_msg == 0 && msgs.len() > 2 {
                last_cached_msg = msgs.len() - 2;
            }
        }

        if let Some(msgs) = body.get_mut("messages").and_then(Value::as_array_mut) {
            for m in msgs.iter_mut().skip(last_cached_msg) {
                if let Some(blocks) = m.get_mut("content").and_then(Value::as_array_mut) {
                    for b in blocks.iter_mut() {
                        if b["type"].as_str() != Some("tool_result")
                            || b.get("cache_control").is_some()
                        {
                            continue;
                        }
                        if let Some(s) = b["content"].as_str() {
                            if let Some(c) =
                                compress::compress_tool_result(s, self.tokenizer(), recall, unicode)
                            {
                                before_total += c.before;
                                after_total += c.after;
                                b["content"] = Value::String(c.text);
                                changed = true;
                            }
                        } else if let Some(inner) =
                            b.get_mut("content").and_then(Value::as_array_mut)
                        {
                            for ib in inner.iter_mut() {
                                if ib["type"].as_str() == Some("text")
                                    && ib.get("cache_control").is_none()
                                {
                                    if let Some(s) = ib["text"].as_str() {
                                        if let Some(c) = compress::compress_tool_result(
                                            s,
                                            self.tokenizer(),
                                            recall,
                                            unicode,
                                        ) {
                                            before_total += c.before;
                                            after_total += c.after;
                                            ib["text"] = Value::String(c.text);
                                            changed = true;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let mut say_less_applied = false;
        if say_less::should_apply(self.billing())
            && !last_user_text
                .as_deref()
                .is_some_and(say_less::user_needs_full_explanation)
        {
            match body.get_mut("system") {
                Some(Value::String(s)) => {
                    let next =
                        say_less::append_to_system(s, self.billing(), last_user_text.as_deref());
                    if next != *s {
                        *s = next;
                        say_less_applied = true;
                        changed = true;
                    }
                }
                Some(Value::Array(arr)) => {
                    let already = arr.iter().any(|b| {
                        b["text"]
                            .as_str()
                            .is_some_and(|t| t.contains("Response style (Tinkr):"))
                    });
                    if !already {
                        arr.push(
                            serde_json::json!({ "type": "text", "text": say_less::TERSE_RULE }),
                        );
                        say_less_applied = true;
                        changed = true;
                    }
                }
                None => {
                    body["system"] = Value::String(say_less::TERSE_RULE.to_string());
                    say_less_applied = true;
                    changed = true;
                }
                _ => {}
            }
        }

        (before_total, after_total, changed, say_less_applied)
    }
}

pub struct OpenAiAdapter {
    pub provider_kind: Provider,
    pub billing_kind: Billing,
}

impl ProviderAdapter for OpenAiAdapter {
    fn provider(&self) -> Provider {
        self.provider_kind
    }
    fn detect(&self, path: &str, body: &Value) -> bool {
        path.contains("/chat/completions")
            || path.contains("/responses")
            || body.get("messages").is_some()
            || body.get("input").is_some()
    }
    fn cache_policy(&self) -> CachePolicy {
        CachePolicy::PrefixLastTurnOnly
    }
    fn tokenizer(&self) -> TokenizerKind {
        TokenizerKind::Exact
    }
    fn billing(&self) -> Billing {
        self.billing_kind
    }
    fn transform(
        &self,
        body: &mut Value,
        recall: Option<&RecallStore>,
        unicode: bool,
    ) -> (u64, u64, bool, bool) {
        let mut before_total = 0u64;
        let mut after_total = 0u64;
        let mut changed = false;
        let mut last_user_text: Option<String> = None;

        if let Some(msgs) = body.get_mut("messages").and_then(Value::as_array_mut) {
            for m in msgs.iter() {
                if m["role"].as_str() == Some("user") {
                    if let Some(s) = extract_text_from_content(&m["content"]) {
                        last_user_text = Some(s);
                    }
                }
            }
            let last_asst = msgs
                .iter()
                .rposition(|m| m["role"].as_str() == Some("assistant"))
                .unwrap_or(0);
            for m in msgs.iter_mut().skip(last_asst) {
                if m["role"].as_str() == Some("tool") {
                    if let Some(s) = m["content"].as_str() {
                        if let Some(c) =
                            compress::compress_tool_result(s, self.tokenizer(), recall, unicode)
                        {
                            before_total += c.before;
                            after_total += c.after;
                            m["content"] = Value::String(c.text);
                            changed = true;
                        }
                    }
                }
            }
        }

        if let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) {
            for item in input.iter() {
                if item["role"].as_str() == Some("user") {
                    if let Some(s) = extract_text_from_content(&item["content"]) {
                        last_user_text = Some(s);
                    }
                }
            }
            let start = input.len().saturating_sub(4);
            for item in input.iter_mut().skip(start) {
                if item["type"].as_str() == Some("function_call_output") {
                    if let Some(s) = item["output"].as_str() {
                        if let Some(c) =
                            compress::compress_tool_result(s, self.tokenizer(), recall, unicode)
                        {
                            before_total += c.before;
                            after_total += c.after;
                            item["output"] = Value::String(c.text);
                            changed = true;
                        }
                    }
                }
            }
        }

        let mut say_less_applied = false;
        if say_less::should_apply(self.billing_kind)
            && !last_user_text
                .as_deref()
                .is_some_and(say_less::user_needs_full_explanation)
        {
            if let Some(Value::String(instr)) = body.get_mut("instructions") {
                let next =
                    say_less::append_to_system(instr, self.billing_kind, last_user_text.as_deref());
                if next != *instr {
                    *instr = next;
                    say_less_applied = true;
                    changed = true;
                }
            } else if let Some(msgs) = body.get_mut("messages").and_then(Value::as_array_mut) {
                let already = msgs.iter().any(|m| {
                    matches!(m["role"].as_str(), Some("system" | "developer"))
                        && m["content"]
                            .as_str()
                            .is_some_and(|t| t.contains("Response style (Tinkr):"))
                });
                if !already {
                    if let Some(sys) = msgs
                        .iter_mut()
                        .rev()
                        .find(|m| matches!(m["role"].as_str(), Some("system" | "developer")))
                    {
                        if let Some(s) = sys["content"].as_str() {
                            let next = say_less::append_to_system(
                                s,
                                self.billing_kind,
                                last_user_text.as_deref(),
                            );
                            if next != s {
                                sys["content"] = Value::String(next);
                                say_less_applied = true;
                                changed = true;
                            }
                        }
                    }
                }
            }
        }

        (before_total, after_total, changed, say_less_applied)
    }
}

pub struct GeminiAdapter {
    pub vertex: bool,
}

impl ProviderAdapter for GeminiAdapter {
    fn provider(&self) -> Provider {
        if self.vertex {
            Provider::Vertex
        } else {
            Provider::Gemini
        }
    }
    fn detect(&self, path: &str, body: &Value) -> bool {
        path.contains(":generateContent")
            || path.contains(":streamGenerateContent")
            || body.get("contents").is_some()
    }
    fn cache_policy(&self) -> CachePolicy {
        CachePolicy::PrefixLastTurnOnly
    }
    fn tokenizer(&self) -> TokenizerKind {
        TokenizerKind::Calibrated(1.0)
    }
    fn billing(&self) -> Billing {
        Billing::PerToken
    }
    fn transform(
        &self,
        body: &mut Value,
        recall: Option<&RecallStore>,
        unicode: bool,
    ) -> (u64, u64, bool, bool) {
        let mut before_total = 0u64;
        let mut after_total = 0u64;
        let mut changed = false;
        let mut last_user_text: Option<String> = None;

        if let Some(contents) = body.get_mut("contents").and_then(Value::as_array_mut) {
            if let Some(last) = contents.last() {
                if let Some(parts) = last["parts"].as_array() {
                    for p in parts {
                        if let Some(t) = p["text"].as_str() {
                            last_user_text = Some(t.to_string());
                        }
                    }
                }
            }
            let start = contents.len().saturating_sub(2);
            for c in contents.iter_mut().skip(start) {
                if let Some(parts) = c.get_mut("parts").and_then(Value::as_array_mut) {
                    for p in parts.iter_mut() {
                        if let Some(resp) = p
                            .get_mut("functionResponse")
                            .and_then(|fr| fr.get_mut("response"))
                        {
                            if let Some(map) = resp.as_object_mut() {
                                for (_, val) in map.iter_mut() {
                                    if let Some(s) = val.as_str() {
                                        if let Some(res) = compress::compress_tool_result(
                                            s,
                                            self.tokenizer(),
                                            recall,
                                            unicode,
                                        ) {
                                            before_total += res.before;
                                            after_total += res.after;
                                            *val = Value::String(res.text);
                                            changed = true;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let mut say_less_applied = false;
        if say_less::should_apply(self.billing())
            && !last_user_text
                .as_deref()
                .is_some_and(say_less::user_needs_full_explanation)
        {
            if let Some(si) = body.get_mut("systemInstruction") {
                if let Some(parts) = si.get_mut("parts").and_then(Value::as_array_mut) {
                    let already = parts.iter().any(|p| {
                        p["text"]
                            .as_str()
                            .is_some_and(|t| t.contains("Response style (Tinkr):"))
                    });
                    if !already {
                        parts.push(serde_json::json!({ "text": say_less::TERSE_RULE }));
                        say_less_applied = true;
                        changed = true;
                    }
                }
            }
        }

        (before_total, after_total, changed, say_less_applied)
    }
}

pub fn transform_request(
    provider_hint: Provider,
    path: &str,
    raw_body: &[u8],
    recall: Option<&RecallStore>,
    unicode: bool,
) -> TransformResult {
    let passthrough = |p: Provider| TransformResult {
        body: raw_body.to_vec(),
        provider: p,
        before: 0,
        after: 0,
        modified: false,
        say_less_applied: false,
    };
    if provider_hint == Provider::Bedrock {
        return passthrough(Provider::Bedrock);
    }
    let Ok(mut val) = serde_json::from_slice::<Value>(raw_body) else {
        return passthrough(provider_hint);
    };
    let adapter: Box<dyn ProviderAdapter> = match provider_hint {
        Provider::Anthropic => Box::new(AnthropicAdapter),
        Provider::OpenAi => Box::new(OpenAiAdapter {
            provider_kind: Provider::OpenAi,
            billing_kind: Billing::PerToken,
        }),
        Provider::AzureOpenAi => Box::new(OpenAiAdapter {
            provider_kind: Provider::AzureOpenAi,
            billing_kind: Billing::PerToken,
        }),
        Provider::Copilot => Box::new(OpenAiAdapter {
            provider_kind: Provider::Copilot,
            billing_kind: Billing::PerRequest,
        }),
        Provider::OpenAiCompatible => Box::new(OpenAiAdapter {
            provider_kind: Provider::OpenAiCompatible,
            billing_kind: Billing::PerToken,
        }),
        Provider::Gemini => Box::new(GeminiAdapter { vertex: false }),
        Provider::Vertex => Box::new(GeminiAdapter { vertex: true }),
        _ => {
            if AnthropicAdapter.detect(path, &val) {
                Box::new(AnthropicAdapter)
            } else if (GeminiAdapter { vertex: false }).detect(path, &val) {
                Box::new(GeminiAdapter { vertex: false })
            } else if (OpenAiAdapter {
                provider_kind: Provider::OpenAi,
                billing_kind: Billing::PerToken,
            })
            .detect(path, &val)
            {
                Box::new(OpenAiAdapter {
                    provider_kind: Provider::OpenAi,
                    billing_kind: Billing::PerToken,
                })
            } else {
                return passthrough(Provider::Unknown);
            }
        }
    };
    let (before, after, changed, say_less_applied) = adapter.transform(&mut val, recall, unicode);
    if !changed {
        return passthrough(adapter.provider());
    }
    match serde_json::to_vec(&val) {
        Ok(bytes) => TransformResult {
            body: bytes,
            provider: adapter.provider(),
            before,
            after,
            modified: true,
            say_less_applied,
        },
        Err(_) => passthrough(adapter.provider()),
    }
}
