use std::time::Duration;

pub const HOOK_BUDGET: Duration = Duration::from_millis(25);
pub const MIN_PAYLOAD_TOKENS: u64 = 200;
pub const MIN_SAVING_RATIO: f64 = 0.15;
pub const RECALL_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
pub const RECALL_MAX_BYTES: u64 = 500 * 1024 * 1024;
pub const RECALL_ID_LEN: usize = 6;
pub const SAY_LESS_MAX_TOKENS: u64 = 300;

pub fn worth_it(before: u64, after: u64) -> bool {
    if before < MIN_PAYLOAD_TOKENS || after >= before {
        return false;
    }
    (before - after) as f64 / before as f64 >= MIN_SAVING_RATIO
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn skip_rules() {
        assert!(!worth_it(199, 10));
        assert!(!worth_it(1000, 900));
        assert!(worth_it(1000, 850));
        assert!(!worth_it(1000, 1200));
    }
}
