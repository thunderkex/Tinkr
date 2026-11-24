use crate::ledger::CountKind;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tiktoken_rs::CoreBPE;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TokenizerKind {
    Exact,
    Calibrated(f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Count {
    pub tokens: u64,
    pub kind: CountKind,
}

fn bpe() -> Option<&'static CoreBPE> {
    static BPE: OnceLock<Option<CoreBPE>> = OnceLock::new();
    BPE.get_or_init(|| tiktoken_rs::o200k_base().ok()).as_ref()
}

const MAX_EXACT_BYTES: usize = 256 * 1024;
const BPE_CHUNK_BYTES: usize = 512;

fn base_count(text: &str) -> (u64, bool) {
    if text.len() > MAX_EXACT_BYTES {
        return ((text.len() as u64).div_ceil(4), false);
    }
    match bpe() {
        Some(b) => {
            if text.len() <= BPE_CHUNK_BYTES {
                (b.encode_ordinary(text).len() as u64, true)
            } else {
                let mut total = 0u64;
                let mut i = 0;
                while i < text.len() {
                    let mut end = (i + BPE_CHUNK_BYTES).min(text.len());
                    while end < text.len() && !text.is_char_boundary(end) {
                        end += 1;
                    }
                    total += b.encode_ordinary(&text[i..end]).len() as u64;
                    i = end;
                }
                (total, true)
            }
        }
        None => ((text.len() as u64).div_ceil(4), false),
    }
}

pub fn count(text: &str, kind: TokenizerKind) -> Count {
    let (base, real) = base_count(text);
    match kind {
        TokenizerKind::Exact if real => Count {
            tokens: base,
            kind: CountKind::Measured,
        },
        TokenizerKind::Exact => Count {
            tokens: base,
            kind: CountKind::Estimated,
        },
        TokenizerKind::Calibrated(r) => Count {
            tokens: (base as f64 * r).round() as u64,
            kind: CountKind::Estimated,
        },
    }
}

pub fn count_bytes(bytes: &[u8], kind: TokenizerKind) -> Count {
    count(&String::from_utf8_lossy(bytes), kind)
}

pub fn from_usage(tokens: u64) -> Count {
    Count {
        tokens,
        kind: CountKind::Measured,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Calibrator {
    ratio: f64,
    samples: u32,
}

impl Default for Calibrator {
    fn default() -> Self {
        Self {
            ratio: 1.0,
            samples: 0,
        }
    }
}

impl Calibrator {
    pub fn observe(&mut self, ours: u64, reported: u64) {
        if ours < 50 || reported == 0 {
            return;
        }
        let r = (reported as f64 / ours as f64).clamp(0.5, 2.0);
        self.ratio = if self.samples == 0 {
            r
        } else {
            0.9 * self.ratio + 0.1 * r
        };
        self.samples = self.samples.saturating_add(1);
    }
    pub fn ratio(&self) -> f64 {
        self.ratio
    }
    pub fn samples(&self) -> u32 {
        self.samples
    }
    pub fn kind(&self) -> TokenizerKind {
        TokenizerKind::Calibrated(self.ratio)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counts_and_labels() {
        let c = count("hello world", TokenizerKind::Exact);
        assert!(c.tokens >= 2 && c.tokens <= 3);
        assert_eq!(c.kind, CountKind::Measured);
        assert_eq!(
            count("hello world", TokenizerKind::Calibrated(1.0)).kind,
            CountKind::Estimated
        );
    }
    #[test]
    fn calibration_scales() {
        let text = "word ".repeat(200);
        let a = count(&text, TokenizerKind::Exact).tokens;
        let b = count(&text, TokenizerKind::Calibrated(1.5)).tokens;
        assert_eq!(b, (a as f64 * 1.5).round() as u64);
    }
    #[test]
    fn calibrator_converges_and_clamps() {
        let mut c = Calibrator::default();
        for _ in 0..50 {
            c.observe(1000, 1200);
        }
        assert!((c.ratio() - 1.2).abs() < 1e-6);
        c.observe(1000, 100_000);
        assert!(c.ratio() <= 2.0);
        let mut d = Calibrator::default();
        d.observe(10, 500);
        assert_eq!(d.samples(), 0);
    }
    #[test]
    fn empty_and_invalid_utf8() {
        assert_eq!(count("", TokenizerKind::Exact).tokens, 0);
        assert!(count_bytes(&[0xff, 0xfe, b'a', 0x80], TokenizerKind::Exact).tokens > 0);
    }
    #[test]
    fn crlf_does_not_panic() {
        assert!(count("a\r\nb\r\n", TokenizerKind::Exact).tokens > 0);
    }
}
