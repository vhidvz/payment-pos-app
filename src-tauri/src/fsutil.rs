//! Durable file writes, shared by every file this app persists and by the installer.
//!
//! A reader always sees the old file or the new one, never a mix; the new one survives a
//! power cut (file and directory are both fsynced); and a temp file never exists with a
//! wider mode than the file it replaces — it can hold a password hash or webhook secrets.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The mode a temp file is created with: the original's when there is one.
pub fn creation_mode(like: Option<&std::fs::Metadata>, default_mode: u32) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Some(m) = like {
            return m.mode() & 0o7777;
        }
    }
    #[cfg(not(unix))]
    let _ = like;
    default_mode
}

fn parent_of(path: &Path) -> &Path {
    path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."))
}

/// Unique per call, not per process: two threads replacing the same file at once (two
/// settings updates, say) must not collide on one temp name.
fn tmp_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(format!(".tmp.{}.{}", std::process::id(), uuid::Uuid::new_v4().simple()));
    PathBuf::from(s)
}

/// Write a temp file beside `path` with the final mode from the start, fill it with
/// `fill`, fsync it, rename it over `path`, fsync the directory.
fn replace_with(
    path: &Path,
    mode: u32,
    like: Option<&std::fs::Metadata>,
    fill: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let dir = parent_of(path);
    std::fs::create_dir_all(dir)?;
    let tmp = tmp_path(path);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(mode);
    }
    let mut file = opts.open(&tmp)?;
    let result = (|| {
        fill(&mut file)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            // open() applied the umask, which can only have narrowed the mode; set it exactly.
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
            if let Some(m) = like {
                // Only root can give a file away; as anyone else it is already ours.
                let _ = std::os::unix::fs::fchown(&file, Some(m.uid()), Some(m.gid()));
            }
        }
        #[cfg(not(unix))]
        let _ = (like, mode);
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        fsync_dir(dir)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

pub fn atomic_write(
    path: &Path,
    bytes: &[u8],
    like: Option<&std::fs::Metadata>,
    default_mode: u32,
) -> std::io::Result<()> {
    let mode = creation_mode(like, default_mode);
    replace_with(path, mode, like, |f| f.write_all(bytes))
}

/// Stream `from` into `to` (atomically, with `mode`), hashing on the way. A 100 MB
/// AppImage is never held in memory.
pub fn atomic_copy(from: &Path, to: &Path, mode: u32) -> std::io::Result<String> {
    let mut src = std::fs::File::open(from)?;
    let mut hasher = Sha256::new();
    replace_with(to, mode, None, |f| {
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = src.read(&mut buf)?;
            if n == 0 {
                return Ok(());
            }
            hasher.update(&buf[..n]);
            f.write_all(&buf[..n])?;
        }
    })?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Copy `src` to `dest` once: an existing `dest` is the older backup and wins.
pub fn backup_once(src: &Path, dest: &Path) -> std::io::Result<bool> {
    if dest.exists() {
        return Ok(false);
    }
    let meta = std::fs::metadata(src)?;
    let bytes = std::fs::read(src)?;
    atomic_write(dest, &bytes, Some(&meta), 0o600)?;
    Ok(true)
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Make a rename durable: on Unix the directory entry lives in the directory's own data.
pub fn fsync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ledger-fsutil-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[cfg(unix)]
    fn mode(p: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn writes_and_replaces_without_leaving_temp_files() {
        let d = tmp();
        let p = d.join("settings.json");
        atomic_write(&p, b"one", None, 0o644).unwrap();
        atomic_write(&p, b"two", None, 0o644).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        let names: Vec<_> = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("settings.json")], "{names:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_new_file_gets_exactly_the_default_mode() {
        let d = tmp();
        let p = d.join("admin.json");
        atomic_write(&p, b"{}", None, 0o600).unwrap();
        assert_eq!(mode(&p), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn a_replaced_file_keeps_the_original_mode() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp();
        let p = d.join("admin.json");
        std::fs::write(&p, b"old").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o640)).unwrap();
        let meta = std::fs::metadata(&p).unwrap();
        atomic_write(&p, b"new", Some(&meta), 0o644).unwrap();
        assert_eq!(mode(&p), 0o640);
    }

    /// The temp file is opened with the final mode — it never exists wider than intended.
    #[cfg(unix)]
    #[test]
    fn the_creation_mode_is_the_originals_not_the_default() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp();
        let p = d.join("x");
        std::fs::write(&p, b"").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        let meta = std::fs::metadata(&p).unwrap();
        assert_eq!(creation_mode(Some(&meta), 0o644), 0o600);
        assert_eq!(creation_mode(None, 0o644), 0o644);
    }

    #[cfg(unix)]
    #[test]
    fn backup_once_copies_mode_and_never_overwrites() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp();
        let src = d.join("admin.json");
        let bak = d.join("admin.json.v0.bak");
        std::fs::write(&src, b"first").unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(backup_once(&src, &bak).unwrap());
        std::fs::write(&src, b"second").unwrap();
        assert!(!backup_once(&src, &bak).unwrap(), "an existing backup is the older one and wins");
        assert_eq!(std::fs::read(&bak).unwrap(), b"first");
        assert_eq!(mode(&bak), 0o640);
    }

    #[test]
    fn concurrent_writers_of_one_file_never_collide() {
        let d = tmp();
        let p = d.join("settings.json");
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let p = p.clone();
                std::thread::spawn(move || {
                    for _ in 0..20 {
                        atomic_write(&p, format!("{i}").as_bytes(), None, 0o644).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let left: Vec<_> = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left.len(), 1, "no temp file survives: {left:?}");
    }

    #[test]
    fn sha256_matches_the_known_vector() {
        let d = tmp();
        let p = d.join("abc");
        std::fs::write(&p, b"abc").unwrap();
        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(sha256_file(&p).unwrap(), expected);
        assert_eq!(sha256_bytes(b"abc"), expected);
    }

    #[test]
    fn atomic_copy_returns_the_digest_of_what_it_wrote() {
        let d = tmp();
        let from = d.join("from");
        let to = d.join("sub/to");
        std::fs::write(&from, b"abc").unwrap();
        let sum = atomic_copy(&from, &to, 0o755).unwrap();
        assert_eq!(sum, sha256_bytes(b"abc"));
        assert_eq!(std::fs::read(&to).unwrap(), b"abc");
        #[cfg(unix)]
        assert_eq!(mode(&to), 0o755);
    }
}
