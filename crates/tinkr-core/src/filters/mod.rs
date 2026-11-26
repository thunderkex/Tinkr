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
}

pub struct Ctx<'a> {
    pub recall: Option<&'a RecallStore>,
    pub unicode: bool,
    pub tokenizer: TokenizerKind,
}

fn passthrough(raw: &[u8], before: u64, kc: CountKind, kind: Kind, why: &'static str) -> Outcome {
    Outcome {
        output: raw.to_vec(),
        before,
        after: before,
        kind_count: kc,
        recall_id: None,
        decision: Decision::Passthrough(why),
        kind,
    }
}

pub fn process(cmd: &str, exit_code: i32, raw: &[u8], ctx: &Ctx) -> Outcome {
    let kind = classify::classify(cmd).unwrap_or(Kind::Generic);
    if raw.contains(&0) {
        let c = tokenizer::count("", ctx.tokenizer);
        return passthrough(raw, c.tokens, c.kind, kind, "binary");
    }
    if crate::overlap::has_foreign_marker(raw, &[]) {
        let c = tokenizer::count_bytes(raw, ctx.tokenizer);
        return passthrough(raw, c.tokens, c.kind, kind, "foreign-optimizer-marker");
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
                    };
                }
            }
        }
    }

    if before < defaults::MIN_PAYLOAD_TOKENS {
        return passthrough(raw, before, c.kind, kind, "small");
    }
    let mut saw_unverified = false;
    for level in [2u8, 1u8] {
        let out = kinds::apply(kind, &text, level);
        if out == text {
            continue;
        }
        if verify::verify(&text, &out, exit_code).is_err() {
            saw_unverified = true;
            continue;
        }
        let after = tokenizer::count(&out, ctx.tokenizer).tokens;
        if !defaults::worth_it(before, after + 14) {
            return passthrough(raw, before, c.kind, kind, "not-worth-it");
        }
        let (id, final_text, final_after) = match ctx.recall {
            Some(store) => match store.put(raw) {
                Ok(id) => {
                    let marker = recall::marker(&id, before, after, ctx.unicode);
                    let t = format!("{}\n{marker}\n", out.trim_end_matches('\n'));
                    let fa = tokenizer::count(&t, ctx.tokenizer).tokens;
                    (Some(id), t, fa)
                }
                Err(_) => return passthrough(raw, before, c.kind, kind, "recall-store-error"),
            },
            None => (None, format!("{}\n", out.trim_end_matches('\n')), after),
        };
        if !defaults::worth_it(before, final_after) {
            return passthrough(raw, before, c.kind, kind, "not-worth-it");
        }
        return Outcome {
            output: final_text.into_bytes(),
            before,
            after: final_after,
            kind_count: c.kind,
            recall_id: id,
            decision: Decision::Compressed { level },
            kind,
        };
    }
    passthrough(
        raw,
        before,
        c.kind,
        kind,
        if saw_unverified {
            "verify-failed"
        } else {
            "no-change"
        },
    )
}
