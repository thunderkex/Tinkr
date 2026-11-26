use super::{Native, Platform};
use crate::{io_err, Result};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

fn lock_dir() -> PathBuf {
    if let Some(h) = std::env::var_os("TINKR_HOME").filter(|s| !s.is_empty()) {
        return PathBuf::from(h).join("cache").join("locks");
    }
    super::paths(&Native)
        .map(|p| p.cache.join("locks"))
        .unwrap_or_else(|_| std::env::temp_dir().join("tinkr-locks"))
}

fn lock_path(path: &Path) -> PathBuf {
    let h = Sha256::digest(path.to_string_lossy().as_bytes());
    let name: String = h.iter().take(8).map(|b| format!("{b:02x}")).collect();
    lock_dir().join(format!("{name}.lock"))
}

pub fn with_lock<T>(path: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let lp = lock_path(path);
    if let Some(dir) = lp.parent() {
        fs::create_dir_all(dir).map_err(io_err(dir))?;
    }
    let lock: File = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lp)
        .map_err(io_err(&lp))?;
    lock.lock().map_err(io_err(&lp))?;
    let r = f();
    let _ = lock.unlock();
    r
}

pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    with_lock(path, || atomic_write_unlocked(path, data))
}

pub fn atomic_write_unlocked(path: &Path, data: &[u8]) -> Result<()> {
    let target: PathBuf = match fs::symlink_metadata(path) {
        Ok(md) if md.file_type().is_symlink() => {
            fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        }
        _ => path.to_path_buf(),
    };
    if let Some(dir) = target.parent() {
        fs::create_dir_all(dir).map_err(io_err(dir))?;
    }
    let dir = target.parent().unwrap_or(Path::new("."));
    let existing = fs::metadata(&target).ok();
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(io_err(dir))?;
    tmp.write_all(data).map_err(io_err(&target))?;
    tmp.as_file().sync_all().map_err(io_err(&target))?;
    match existing {
        Some(md) => {
            let _ = fs::set_permissions(tmp.path(), md.permissions());
        }
        None => {
            let _ = Native.set_private(tmp.path());
        }
    }
    tmp.persist(&target).map_err(|e| io_err(&target)(e.error))?;
    Ok(())
}

pub fn backup_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".tinkr.bak");
    PathBuf::from(s)
}

pub fn backup_once(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let bak = backup_path(path);
    match fs::symlink_metadata(&bak) {
        Ok(md) if md.file_type().is_symlink() => {
            return Err(io_err(&bak)(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "refusing to write backup through symlink",
            )));
        }
        Ok(_) => return Ok(Some(bak)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_err(&bak)(e)),
    }
    let data = fs::read(path).map_err(io_err(path))?;
    atomic_write(&bak, &data)?;
    Ok(Some(bak))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_replaces_and_leaves_only_the_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub dir").join("c\u{f6}nfig.json");
        atomic_write(&p, b"one").unwrap();
        atomic_write(&p, b"two\r\n").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two\r\n");
        let n = fs::read_dir(p.parent().unwrap()).unwrap().count();
        assert_eq!(n, 1, "no temp or lock residue next to the file");
    }

    #[test]
    fn backup_is_byte_exact_and_pristine() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.json");
        let orig = vec![0xef, 0xbb, 0xbf, b'{', 0xff, b'\r', b'\n'];
        fs::write(&p, &orig).unwrap();
        let bak = backup_once(&p).unwrap().unwrap();
        atomic_write(&p, b"changed").unwrap();
        backup_once(&p).unwrap();
        assert_eq!(fs::read(bak).unwrap(), orig);
        assert!(backup_once(&d.path().join("missing")).unwrap().is_none());
    }

    #[test]
    fn concurrent_writers_never_corrupt() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("shared.txt");
        let hs: Vec<_> = (0..8)
            .map(|i| {
                let p = p.clone();
                std::thread::spawn(move || {
                    for j in 0..20 {
                        let body = format!("{i}-{j}-").repeat(500);
                        atomic_write(&p, body.as_bytes()).unwrap();
                        let got = fs::read_to_string(&p).unwrap();
                        let first = got.split('-').take(2).collect::<Vec<_>>().join("-");
                        assert_eq!(got, format!("{first}-").repeat(500), "torn read");
                    }
                })
            })
            .collect();
        hs.into_iter().for_each(|h| h.join().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn modes_and_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("f");
        atomic_write(&p, b"x").unwrap();
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
        atomic_write(&p, b"y").unwrap();
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o644
        );
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&p, &link).unwrap();
        atomic_write(&link, b"z").unwrap();
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(&p).unwrap(), b"z");
    }
}
