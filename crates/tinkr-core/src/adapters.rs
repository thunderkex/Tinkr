use serde_json::Value;
use std::collections::HashMap;
use crate::{
    compress,
    provider::Provider,
    read_dedup::{ReadCache, SnapshotOrigin},
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

#[derive(Debug, Clone)]
enum TrackedToolCall {
    Read {
        path: String,
        range: Option<(usize, usize)>,
    },
    Write {
        path: String,
        content: String,
    },
}

fn parse_tool_call_meta(name: &str, input: &Value) -> Option<TrackedToolCall> {
    let lower = name.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "read" | "read_file" | "view_file" | "tinkr_read" | "get_file_contents"
    ) {
        let path = input["file_path"]
            .as_str()
            .or_else(|| input["path"].as_str())
            .or_else(|| input["Path"].as_str())?
            .to_string();
        let start = input["start_line"]
            .as_u64()
            .or_else(|| input["StartLine"].as_u64())
            .or_else(|| input["offset"].as_u64());
        let end = input["end_line"]
            .as_u64()
            .or_else(|| input["EndLine"].as_u64())
            .or_else(|| {
                input["limit"]
                    .as_u64()
                    .map(|lim| start.unwrap_or(1) + lim.saturating_sub(1))
            });
        let range = match (start, end) {
            (Some(s), Some(e)) if s >= 1 && e >= s => Some((s as usize, e as usize)),
            _ => None,
        };
        return Some(TrackedToolCall::Read { path, range });
    }
    if matches!(lower.as_str(), "write" | "write_file" | "create_file") {
        let path = input["file_path"]
            .as_str()
            .or_else(|| input["path"].as_str())
            .or_else(|| input["Path"].as_str())?
            .to_string();
        let content = input["content"]
            .as_str()
            .or_else(|| input["Content"].as_str())?
            .to_string();
        return Some(TrackedToolCall::Write { path, content });
    }
    None
}

fn extract_tool_result_str(b: &Value) -> Option<&str> {
    if let Some(s) = b["content"].as_str() {
        return Some(s);
    }
    if let Some(arr) = b["content"].as_array() {
        if arr.len() == 1 && arr[0]["type"].as_str() == Some("text") {
            return arr[0]["text"].as_str();
        }
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
        let mut assistant_prose_tokens: Vec<u64> = Vec::new();
        let mut tool_uses: HashMap<String, TrackedToolCall> = HashMap::new();
        let mut read_cache = ReadCache::default();

        if let Some(msgs) = body.get("messages").and_then(Value::as_array) {
            for (i, m) in msgs.iter().enumerate() {
                if has_cache_control(m) {
                    last_cached_msg = i + 1;
                }
                if m["role"].as_str() == Some("user") {
                    if let Some(s) = extract_text_from_content(&m["content"]) {
                        last_user_text = Some(s);
                    }
                } else if m["role"].as_str() == Some("assistant") {
                    let prose = extract_text_from_content(&m["content"]).unwrap_or_default();
                    assistant_prose_tokens
                        .push(crate::tokenizer::count(&prose, TokenizerKind::Exact).tokens);
                }
                if let Some(blocks) = m["content"].as_array() {
                    for b in blocks {
                        if b["type"].as_str() == Some("tool_use") {
                            if let (Some(id), Some(name)) = (b["id"].as_str(), b["name"].as_str()) {
                                if let Some(meta) = parse_tool_call_meta(name, &b["input"]) {
                                    tool_uses.insert(id.to_string(), meta);
                                }
                            }
                        }
                    }
                }
            }
            if last_cached_msg == 0 && msgs.len() > 2 {
                last_cached_msg = msgs.len() - 2;
            }
            for m in msgs.iter().take(last_cached_msg) {
                if let Some(blocks) = m["content"].as_array() {
                    for b in blocks {
                        if b["type"].as_str() == Some("tool_use") {
                            if let Some(id) = b["id"].as_str() {
                                if let Some(TrackedToolCall::Write { path, content }) =
                                    tool_uses.get(id)
                                {
                                    read_cache.record_baseline(
                                        path,
                                        content,
                                        None,
                                        SnapshotOrigin::AgentEdit,
                                    );
                                }
                            }
                        } else if b["type"].as_str() == Some("tool_result")
                            && b["is_error"].as_bool() != Some(true)
                        {
                            if let (Some(id), Some(txt)) =
                                (b["tool_use_id"].as_str(), extract_tool_result_str(b))
                            {
                                if let Some(TrackedToolCall::Read { path, range }) =
                                    tool_uses.get(id)
                                {
                                    read_cache.record_baseline(
                                        path,
                                        txt,
                                        *range,
                                        SnapshotOrigin::Read,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        if let Some(msgs) = body.get_mut("messages").and_then(Value::as_array_mut) {
            for m in msgs.iter_mut().skip(last_cached_msg) {
                if let Some(blocks) = m.get_mut("content").and_then(Value::as_array_mut) {
                    for b in blocks.iter_mut() {
                        if b["type"].as_str() == Some("tool_use") {
                            if let Some(id) = b["id"].as_str() {
                                if let Some(TrackedToolCall::Write { path, content }) =
                                    tool_uses.get(id)
                                {
                                    read_cache.record_baseline(
                                        path,
                                        content,
                                        None,
                                        SnapshotOrigin::AgentEdit,
                                    );
                                }
                            }
                            continue;
                        }
                        if b["type"].as_str() != Some("tool_result")
                            || b.get("cache_control").is_some()
                        {
                            continue;
                        }
                        let read_meta = b["tool_use_id"]
                            .as_str()
                            .filter(|_| b["is_error"].as_bool() != Some(true))
                            .and_then(|id| match tool_uses.get(id) {
                                Some(TrackedToolCall::Read { path, range }) => {
                                    Some((path.clone(), *range))
                                }
                                _ => None,
                            });

                        if let Some(s) = b["content"].as_str().map(str::to_string) {
                            if let Some((ref path, range)) = read_meta {
                                if let Some(hit) = read_cache.observe_read(
                                    path,
                                    s.as_bytes(),
                                    range,
                                    self.tokenizer(),
                                    recall,
                                    unicode,
                                ) {
                                    before_total += hit.before;
                                    after_total += hit.after;
                                    b["content"] = Value::String(hit.text);
                                    changed = true;
                                    continue;
                                }
                            }
                            if let Some(c) =
                                compress::compress_tool_result(&s, self.tokenizer(), recall, unicode)
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
                                    if let Some(s) = ib["text"].as_str().map(str::to_string) {
                                        if let Some((ref path, range)) = read_meta {
                                            if let Some(hit) = read_cache.observe_read(
                                                path,
                                                s.as_bytes(),
                                                range,
                                                self.tokenizer(),
                                                recall,
                                                unicode,
                                            ) {
                                                before_total += hit.before;
                                                after_total += hit.after;
                                                ib["text"] = Value::String(hit.text);
                                                changed = true;
                                                continue;
                                            }
                                        }
                                        if let Some(c) = compress::compress_tool_result(
                                            &s,
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
            && say_less::conversation_repays_overhead(
                &assistant_prose_tokens,
                say_less::DEFAULT_PAYBACK_TURNS as usize,
            )
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
                        b["text"].as_str().is_some_and(|t| {
                            t.contains("Response style (Tinkr):")
                                || crate::overlap::has_foreign_voice_rule(t)
                        })
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
        let mut assistant_prose_tokens: Vec<u64> = Vec::new();

        if let Some(msgs) = body.get_mut("messages").and_then(Value::as_array_mut) {
            let mut tool_calls: HashMap<String, TrackedToolCall> = HashMap::new();
            for m in msgs.iter() {
                if m["role"].as_str() == Some("user") {
                    if let Some(s) = extract_text_from_content(&m["content"]) {
                        last_user_text = Some(s);
                    }
                } else if m["role"].as_str() == Some("assistant") {
                    let prose = extract_text_from_content(&m["content"]).unwrap_or_default();
                    assistant_prose_tokens
                        .push(crate::tokenizer::count(&prose, TokenizerKind::Exact).tokens);
                }
                if let Some(tcs) = m["tool_calls"].as_array() {
                    for tc in tcs {
                        let id = tc["id"].as_str();
                        let name = tc["function"]["name"].as_str();
                        let args_val = tc["function"]["arguments"]
                            .as_str()
                            .and_then(|s| serde_json::from_str::<Value>(s).ok());
                        if let (Some(id), Some(name), Some(av)) = (id, name, args_val) {
                            if let Some(meta) = parse_tool_call_meta(name, &av) {
                                tool_calls.insert(id.to_string(), meta);
                            }
                        }
                    }
                }
            }
            let last_asst = msgs
                .iter()
                .rposition(|m| m["role"].as_str() == Some("assistant"))
                .unwrap_or(0);
            let mut read_cache = ReadCache::default();
            for m in msgs.iter().take(last_asst) {
                if let Some(tcs) = m["tool_calls"].as_array() {
                    for tc in tcs {
                        if let Some(id) = tc["id"].as_str() {
                            if let Some(TrackedToolCall::Write { path, content }) =
                                tool_calls.get(id)
                            {
                                read_cache.record_baseline(
                                    path,
                                    content,
                                    None,
                                    SnapshotOrigin::AgentEdit,
                                );
                            }
                        }
                    }
                }
                if m["role"].as_str() == Some("tool") {
                    if let (Some(id), Some(s)) = (m["tool_call_id"].as_str(), m["content"].as_str())
                    {
                        if let Some(TrackedToolCall::Read { path, range }) = tool_calls.get(id) {
                            read_cache.record_baseline(path, s, *range, SnapshotOrigin::Read);
                        }
                    }
                }
            }
            for m in msgs.iter_mut().skip(last_asst) {
                if let Some(tcs) = m["tool_calls"].as_array() {
                    for tc in tcs {
                        if let Some(id) = tc["id"].as_str() {
                            if let Some(TrackedToolCall::Write { path, content }) =
                                tool_calls.get(id)
                            {
                                read_cache.record_baseline(
                                    path,
                                    content,
                                    None,
                                    SnapshotOrigin::AgentEdit,
                                );
                            }
                        }
                    }
                }
                if m["role"].as_str() == Some("tool") {
                    let read_meta = m["tool_call_id"].as_str().and_then(|id| {
                        match tool_calls.get(id) {
                            Some(TrackedToolCall::Read { path, range }) => {
                                Some((path.clone(), *range))
                            }
                            _ => None,
                        }
                    });
                    if let Some(s) = m["content"].as_str().map(str::to_string) {
                        if let Some((ref path, range)) = read_meta {
                            if let Some(hit) = read_cache.observe_read(
                                path,
                                s.as_bytes(),
                                range,
                                self.tokenizer(),
                                recall,
                                unicode,
                            ) {
                                before_total += hit.before;
                                after_total += hit.after;
                                m["content"] = Value::String(hit.text);
                                changed = true;
                                continue;
                            }
                        }
                        if let Some(c) =
                            compress::compress_tool_result(&s, self.tokenizer(), recall, unicode)
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
            && say_less::conversation_repays_overhead(
                &assistant_prose_tokens,
                say_less::DEFAULT_PAYBACK_TURNS as usize,
            )
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
        let mut assistant_prose_tokens: Vec<u64> = Vec::new();

        if let Some(contents) = body.get_mut("contents").and_then(Value::as_array_mut) {
            for item in contents.iter() {
                if item["role"].as_str() == Some("model") {
                    let mut text_buf = String::new();
                    if let Some(parts) = item["parts"].as_array() {
                        for p in parts {
                            if let Some(t) = p["text"].as_str() {
                                text_buf.push_str(t);
                            }
                        }
                    }
                    assistant_prose_tokens
                        .push(crate::tokenizer::count(&text_buf, TokenizerKind::Exact).tokens);
                }
            }
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
            && say_less::conversation_repays_overhead(
                &assistant_prose_tokens,
                say_less::DEFAULT_PAYBACK_TURNS as usize,
            )
            && !last_user_text
                .as_deref()
                .is_some_and(say_less::user_needs_full_explanation)
        {
            if let Some(si) = body.get_mut("systemInstruction") {
                if let Some(parts) = si.get_mut("parts").and_then(Value::as_array_mut) {
                    let already = parts.iter().any(|p| {
                        p["text"].as_str().is_some_and(|t| {
                            t.contains("Response style (Tinkr):")
                                || crate::overlap::has_foreign_voice_rule(t)
                        })
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
