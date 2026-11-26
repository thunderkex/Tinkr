pub mod catalog;
pub mod classify;
pub mod kinds;
pub mod normalize;
pub mod verify;

use crate::{
    defaults,
    ledger::CountKind,
    recall::{self, RecallStore},
    tokenizer::{self, TokenizerKind},
};
pub use classify::Kind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Passthrough(&'static str),
    Compressed { level: u8 },
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub output: Vec<u8>,
    pub before: u64,
    pub after: u64,
    pub kind_count: CountKind,
    pub recall_id: Option<String>,
    pub decision: Decision,
    pub kind: Kind,
    pub profile_id: &'static str,
}

pub struct Ctx<'a> {
    pub recall: Option<&'a RecallStore>,
    pub unicode: bool,
    pub tokenizer: TokenizerKind,
}

fn passthrough(
    raw: &[u8],
    before: u64,
    kc: CountKind,
    kind: Kind,
    profile_id: &'static str,
    why: &'static str,
) -> Outcome {
    Outcome {
        output: raw.to_vec(),
        before,
        after: before,
        kind_count: kc,
        recall_id: None,
        decision: Decision::Passthrough(why),
        kind,
        profile_id,
    }
}

pub fn process(cmd: &str, exit_code: i32, raw: &[u8], ctx: &Ctx) -> Outcome {
    process_tuned(cmd, exit_code, raw, ctx, 2)
}

pub fn process_tuned(
    cmd: &str,
    exit_code: i32,
    raw: &[u8],
    ctx: &Ctx,
    target_level: u8,
) -> Outcome {
    let matched_rule = classify::match_catalog(cmd);
    let kind = matched_rule.map(|r| r.raw.kind).unwrap_or(Kind::Generic);
    let profile_id = matched_rule
        .map(|r| r.raw.id.as_str())
        .unwrap_or("generic");
    if target_level == 0 {
        let c = tokenizer::count_bytes(raw, ctx.tokenizer);
        return passthrough(raw, c.tokens, c.kind, kind, profile_id, "self-tuned-disabled");
    }
    if raw.contains(&0) {
        let c = tokenizer::count("", ctx.tokenizer);
        return passthrough(raw, c.tokens, c.kind, kind, profile_id, "binary");
    }
    if crate::overlap::has_foreign_marker(raw, &[]) {
        let c = tokenizer::count_bytes(raw, ctx.tokenizer);
        return passthrough(raw, c.tokens, c.kind, kind, profile_id, "foreign-optimizer-marker");
    }
    let text = normalize::normalize(raw);
    let c = tokenizer::count(&text, ctx.tokenizer);
    let before = c.tokens;

    if kind == Kind::FileRead && exit_code == 0 {
        if let Some(store) = ctx.recall {
            if let Some(file_path) = crate::read_dedup::extract_shell_read_path(cmd) {
                let cache_path = store.dir().join("read-cache.json");
                let mut cache = crate::read_dedup::ReadCache::load(&cache_path);
                let hit = cache.observe_read(
                    &file_path,
                    raw,
                    None,
                    ctx.tokenizer,
                    Some(store),
                    ctx.unicode,
                );
                cache.save(&cache_path);
                if let Some(h) = hit {
                    let mut out_bytes = h.text.into_bytes();
                    if !out_bytes.ends_with(b"\n") {
                        out_bytes.push(b'\n');
                    }
                    return Outcome {
                        output: out_bytes,
                        before: h.before,
                        after: h.after,
                        kind_count: c.kind,
                        recall_id: h.recall_id,
                        decision: Decision::Compressed { level: 2 },
                        kind,
                        profile_id,
                    };
                }
            }
        }
    }

    if before < defaults::MIN_PAYLOAD_TOKENS {
        return passthrough(raw, before, c.kind, kind, profile_id, "small");
    }
    let levels: &[u8] = match target_level {
        3.. => &[3, 2, 1],
        2 => &[2, 1],
        _ => &[1],
    };
    let mut saw_unverified = false;
    for &level in levels {
        let out = match matched_rule {
            Some(rule) => kinds::apply_catalog(rule, &text, level),
            None => kinds::apply(kind, &text, level),
        };
        if out == text {
            continue;
        }
        let verified = if out.trim_start().starts_with('{') || out.trim_start().starts_with('[') {
            verify::critical_lines(&text)
                .into_iter()
                .all(|cl| out.contains(cl.trim_matches([',', ' '])))
                || verify::verify(&text, &out, exit_code).is_ok()
        } else {
            verify::verify(&text, &out, exit_code).is_ok()
        };
        if !verified {
            saw_unverified = true;
            continue;
        }
        let after = tokenizer::count(&out, ctx.tokenizer).tokens;
        if !defaults::worth_it(before, after + 14) {
            return passthrough(raw, before, c.kind, kind, profile_id, "not-worth-it");
        }
        let (id, final_text, final_after) = match ctx.recall {
            Some(store) => match store.put(raw) {
                Ok(id) => {
                    let marker = recall::marker(&id, before, after, ctx.unicode);
                    let t = format!("{}\n{marker}\n", out.trim_end_matches('\n'));
                    let fa = tokenizer::count(&t, ctx.tokenizer).tokens;
                    (Some(id), t, fa)
                }
                Err(_) => {
                    return passthrough(raw, before, c.kind, kind, profile_id, "recall-store-error")
                }
            },
            None => (None, format!("{}\n", out.trim_end_matches('\n')), after),
        };
        if !defaults::worth_it(before, final_after) {
            return passthrough(raw, before, c.kind, kind, profile_id, "not-worth-it");
        }
        return Outcome {
            output: final_text.into_bytes(),
            before,
            after: final_after,
            kind_count: c.kind,
            recall_id: id,
            decision: Decision::Compressed { level },
            kind,
            profile_id,
        };
    }
    passthrough(
        raw,
        before,
        c.kind,
        kind,
        profile_id,
        if saw_unverified {
            "verify-failed"
        } else {
            "no-change"
        },
    )
}
