pub fn windows_extended(path: &str) -> String {
    const LIMIT: usize = 240;
    if path.starts_with(r"\\?\") || path.starts_with(r"\\.\") || path.len() < LIMIT {
        return path.to_string();
    }
    let p = path.replace('/', r"\");
    if let Some(rest) = p.strip_prefix(r"\\") {
        return format!(r"\\?\UNC\{rest}");
    }
    let b = p.as_bytes();
    if b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
        return format!(r"\\?\{p}");
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_and_relative_unchanged() {
        assert_eq!(windows_extended(r"C:\a\b"), r"C:\a\b");
        let long_rel = "a\\".repeat(200);
        assert_eq!(windows_extended(&long_rel), long_rel);
    }

    #[test]
    fn long_drive_and_unc() {
        let tail = "d\\".repeat(150);
        assert_eq!(
            windows_extended(&format!(r"C:\{tail}")),
            format!(r"\\?\C:\{tail}")
        );
        assert_eq!(
            windows_extended(&format!(r"\\srv\share\{tail}")),
            format!(r"\\?\UNC\srv\share\{tail}")
        );
        let already = format!(r"\\?\C:\{tail}");
        assert_eq!(windows_extended(&already), already);
        assert_eq!(
            windows_extended(&format!("C:/{}", "d/".repeat(150))),
            format!(r"\\?\C:\{}", "d\\".repeat(150))
        );
    }
}
