use regex::Regex;
use std::sync::LazyLock;

static ANSI: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-Z\\-_]")
        .unwrap()
});

pub fn normalize(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    let s = s.trim_start_matches('\u{feff}');
    let s = ANSI.replace_all(s, "");
    let s = s.replace("\r\n", "\n");
    if !s.contains('\r') {
        return s;
    }
    s.split('\n')
        .map(|l| l.rsplit('\r').find(|seg| !seg.is_empty()).unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ansi_crlf_progress_and_bad_utf8() {
        assert_eq!(normalize(b"\x1b[31mred\x1b[0m\r\nok\r\n"), "red\nok\n");
        assert_eq!(normalize(b"10%\r50%\r100%\ndone"), "100%\ndone");
        assert!(normalize(&[b'a', 0xff, b'b']).contains('a'));
        assert_eq!(normalize("\u{feff}x".as_bytes()), "x");
    }
}
