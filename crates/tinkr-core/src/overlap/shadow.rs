use crate::{
    filters::{self, verify, Ctx},
    platform::fsx,
    tokenizer::{self, TokenizerKind},
    Result,
};
use serde::{Deserialize, Serialize};
use std::{path::Path, time::Instant};

pub const MIN_SHADOW_SAMPLES: usize = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowSample {
    pub ts: i64,
    pub tool_id: String,
    pub cmd_kind: String,
    pub raw_tokens: u64,
    pub their_tokens: u64,
    pub tinkr_tokens: u64,
    pub count_kind: String,
    pub tinkr_latency_us: u64,
    pub tinkr_dropped_that_theirs_kept: Vec<String>,
    pub theirs_dropped_that_tinkr_kept: Vec<String>,
}

pub fn shadow_evaluate<'a>(
    tool_id: &str,
    cmd: &str,
    exit_code: i32,
    raw_bytes: &[u8],
    their_output: &'a [u8],
    ts: i64,
) -> (&'a [u8], ShadowSample) {
    let raw_text = filters::normalize::normalize(raw_bytes);
    let their_text = filters::normalize::normalize(their_output);

    let t0 = Instant::now();
    let ctx = Ctx {
        recall: None,
        unicode: false,
        tokenizer: TokenizerKind::Exact,
    };
    let tinkr_out = filters::process(cmd, exit_code, raw_bytes, &ctx);
    let latency_us = t0.elapsed().as_micros() as u64;
    let tinkr_text = String::from_utf8_lossy(&tinkr_out.output);

    let raw_tokens = tokenizer::count(&raw_text, TokenizerKind::Exact).tokens;
    let their_tokens = tokenizer::count(&their_text, TokenizerKind::Exact).tokens;
    let tinkr_tokens = tokenizer::count(&tinkr_text, TokenizerKind::Exact).tokens;

    let mut tinkr_dropped = Vec::new();
    let mut theirs_dropped = Vec::new();
    for crit in verify::critical_lines(&raw_text) {
        let in_tinkr = tinkr_text.contains(crit);
        let in_theirs = their_text.contains(crit);
        if !in_tinkr && in_theirs {
            tinkr_dropped.push(crit.to_string());
        } else if !in_theirs && in_tinkr {
            theirs_dropped.push(crit.to_string());
        }
    }

    let sample = ShadowSample {
        ts,
        tool_id: tool_id.to_string(),
        cmd_kind: tinkr_out.kind.name().to_string(),
        raw_tokens,
        their_tokens,
        tinkr_tokens,
        count_kind: "measured".into(),
        tinkr_latency_us: latency_us,
        tinkr_dropped_that_theirs_kept: tinkr_dropped,
        theirs_dropped_that_tinkr_kept: theirs_dropped,
    };
    (their_output, sample)
}

pub fn append_sample(path: &Path, sample: &ShadowSample) -> Result<()> {
    let mut existing = std::fs::read(path).unwrap_or_default();
    let mut line = serde_json::to_vec(sample)?;
    line.push(b'\n');
    existing.extend_from_slice(&line);
    fsx::atomic_write(path, &existing)
}

pub fn load_samples(path: &Path) -> Vec<ShadowSample> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| serde_json::from_str::<ShadowSample>(l).ok())
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowSummary {
    pub tool_id: String,
    pub samples: usize,
    pub enough_data: bool,
    pub raw_tokens: u64,
    pub their_tokens: u64,
    pub tinkr_tokens: u64,
    pub their_saved: u64,
    pub tinkr_saved: u64,
    pub tinkr_dropped_count: usize,
    pub theirs_dropped_count: usize,
    pub median_tinkr_latency_us: u64,
}

pub fn summarize(tool_id: &str, samples: &[ShadowSample]) -> ShadowSummary {
    let mut raw = 0u64;
    let mut theirs = 0u64;
    let mut tinkr = 0u64;
    let mut tinkr_dropped = 0usize;
    let mut theirs_dropped = 0usize;
    let mut lats: Vec<u64> = Vec::with_capacity(samples.len());

    for s in samples {
        raw += s.raw_tokens;
        theirs += s.their_tokens;
        tinkr += s.tinkr_tokens;
        tinkr_dropped += s.tinkr_dropped_that_theirs_kept.len();
        theirs_dropped += s.theirs_dropped_that_tinkr_kept.len();
        lats.push(s.tinkr_latency_us);
    }
    lats.sort_unstable();
    let median_lat = if lats.is_empty() {
        0
    } else {
        lats[lats.len() / 2]
    };

    ShadowSummary {
        tool_id: tool_id.to_string(),
        samples: samples.len(),
        enough_data: samples.len() >= MIN_SHADOW_SAMPLES,
        raw_tokens: raw,
        their_tokens: theirs,
        tinkr_tokens: tinkr,
        their_saved: raw.saturating_sub(theirs),
        tinkr_saved: raw.saturating_sub(tinkr),
        tinkr_dropped_count: tinkr_dropped,
        theirs_dropped_count: theirs_dropped,
        median_tinkr_latency_us: median_lat,
    }
}

pub fn format_shadow_report(s: &ShadowSummary) -> String {
    if !s.enough_data {
        return format!(
            "Shadow comparison for {}: too little data ({}/{} samples measured so far).\n\
             Keep running normally; Tinkr has not altered any output.\n",
            s.tool_id, s.samples, MIN_SHADOW_SAMPLES
        );
    }
    let their_pct = if s.raw_tokens == 0 {
        0.0
    } else {
        s.their_saved as f64 * 100.0 / s.raw_tokens as f64
    };
    let tinkr_pct = if s.raw_tokens == 0 {
        0.0
    } else {
        s.tinkr_saved as f64 * 100.0 / s.raw_tokens as f64
    };
    format!(
        "Shadow comparison for {} ({} samples, measured on your machine):\n\
         \x20 Raw input tokens:       {}\n\
         \x20 {} output:              {} tokens (saved {}, {:.1}%)\n\
         \x20 Tinkr shadow output:    {} tokens (saved {}, {:.1}%)\n\
         \x20 Critical lines dropped: Tinkr={} (that {} kept), {}={} (that Tinkr kept)\n\
         \x20 Median Tinkr latency:   {:.2} ms\n\
         Run `tinkr overlap choose {} keep|tinkr|both` when ready.\n",
        s.tool_id,
        s.samples,
        s.raw_tokens,
        s.tool_id,
        s.their_tokens,
        s.their_saved,
        their_pct,
        s.tinkr_tokens,
        s.tinkr_saved,
        tinkr_pct,
        s.tinkr_dropped_count,
        s.tool_id,
        s.tool_id,
        s.theirs_dropped_count,
        s.median_tinkr_latency_us as f64 / 1000.0,
        s.tool_id
    )
}
