//! Jailed file directory: every read and write is confined to `META_MCP_FILES_DIR`.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static TMP_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Failures resolving a path inside the jail.
#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("path `{0}` resolves outside META_MCP_FILES_DIR")]
    Escape(String),
    #[error("no such file: {0}")]
    NotFound(String),
    #[error("not a regular file: {0}")]
    NotAFile(String),
    #[error("destination `{0}` already exists; pass overwrite=true to replace it")]
    Exists(String),
    #[error("invalid path: {0}")]
    Invalid(String),
    #[error("file is {size} bytes, above the limit of {cap} (META_MCP_MAX_FILE_BYTES)")]
    TooLarge { size: u64, cap: u64 },
    #[error("file tools are disabled: set META_MCP_FILES_DIR to a directory to enable them")]
    Disabled,
    #[error("io error on `{path}`: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}

fn io_err(path: &Path, source: std::io::Error) -> FileError {
    FileError::Io {
        path: path.display().to_string(),
        source,
    }
}

/// A canonicalized directory that scopes all file reads and writes.
#[derive(Clone, Debug)]
pub struct FileJail {
    root: PathBuf,
}

/// Resolves `.` and `..` without touching the filesystem.
fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

impl FileJail {
    /// The directory must exist; it is canonicalized once here.
    pub fn new(dir: &Path) -> Result<Self, FileError> {
        let root = std::fs::canonicalize(dir).map_err(|e| io_err(dir, e))?;
        if !root.is_dir() {
            return Err(FileError::Invalid(format!(
                "{} is not a directory",
                dir.display()
            )));
        }
        Ok(Self { root })
    }

    /// The canonical root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn join(&self, input: &str) -> Result<PathBuf, FileError> {
        if input.trim().is_empty() || input.contains('\0') {
            return Err(FileError::Invalid("path is empty or contains NUL".into()));
        }
        let p = Path::new(input);
        Ok(if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.join(p)
        })
    }

    /// Existing regular file that canonicalizes inside the root (symlinks followed, then checked).
    pub fn resolve_read(&self, input: &str) -> Result<PathBuf, FileError> {
        let joined = self.join(input)?;
        if !lexical_normalize(&joined).starts_with(&self.root) {
            return Err(FileError::Escape(input.to_string()));
        }
        let canon = std::fs::canonicalize(&joined).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                FileError::NotFound(input.to_string())
            } else {
                io_err(&joined, e)
            }
        })?;
        if !canon.starts_with(&self.root) {
            return Err(FileError::Escape(input.to_string()));
        }
        if !canon.is_file() {
            return Err(FileError::NotAFile(input.to_string()));
        }
        Ok(canon)
    }

    /// Destination inside the root that must not exist yet. Missing parent directories are
    /// created one level at a time, re-checking containment; dot-prefixed components are refused.
    pub fn resolve_write(&self, input: &str) -> Result<PathBuf, FileError> {
        self.resolve_dest(input, false)
    }

    /// Like `resolve_write`, but an existing regular file at the target is allowed.
    pub fn resolve_write_overwrite(&self, input: &str) -> Result<PathBuf, FileError> {
        self.resolve_dest(input, true)
    }

    fn resolve_dest(&self, input: &str, overwrite: bool) -> Result<PathBuf, FileError> {
        let lexical = lexical_normalize(&self.join(input)?);
        if !lexical.starts_with(&self.root) {
            return Err(FileError::Escape(input.to_string()));
        }
        let rel_all = lexical.strip_prefix(&self.root).unwrap_or(&lexical);
        if rel_all
            .components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        {
            return Err(FileError::Invalid(
                "destination components must not start with `.`".into(),
            ));
        }
        let name = lexical
            .file_name()
            .ok_or_else(|| FileError::Invalid("destination has no file name".into()))?
            .to_owned();
        let parent = lexical.parent().unwrap_or(&self.root);
        let rel = parent
            .strip_prefix(&self.root)
            .map_err(|_| FileError::Escape(input.to_string()))?;
        let mut dir = self.root.clone();
        for comp in rel.components() {
            let next = dir.join(comp.as_os_str());
            if std::fs::symlink_metadata(&next).is_err() {
                std::fs::create_dir(&next).map_err(|e| io_err(&next, e))?;
            }
            dir = std::fs::canonicalize(&next).map_err(|e| io_err(&next, e))?;
            if !dir.starts_with(&self.root) {
                return Err(FileError::Escape(input.to_string()));
            }
        }
        let target = dir.join(name);
        if let Ok(meta) = std::fs::symlink_metadata(&target) {
            if meta.file_type().is_symlink() {
                return Err(FileError::Escape(input.to_string()));
            }
            if meta.is_dir() {
                return Err(FileError::NotAFile(input.to_string()));
            }
            if !overwrite {
                return Err(FileError::Exists(input.to_string()));
            }
        }
        Ok(target)
    }

    /// Reads a jailed file, refusing anything above `max_bytes`. Blocking: use `spawn_blocking`.
    pub fn read_capped(&self, input: &str, max_bytes: u64) -> Result<Vec<u8>, FileError> {
        let path = self.resolve_read(input)?;
        let size = std::fs::metadata(&path)
            .map_err(|e| io_err(&path, e))?
            .len();
        if size > max_bytes {
            return Err(FileError::TooLarge {
                size,
                cap: max_bytes,
            });
        }
        let mut buf = Vec::new();
        std::fs::File::open(&path)
            .and_then(|f| f.take(max_bytes.saturating_add(1)).read_to_end(&mut buf))
            .map_err(|e| io_err(&path, e))?;
        if buf.len() as u64 > max_bytes {
            return Err(FileError::TooLarge {
                size: buf.len() as u64,
                cap: max_bytes,
            });
        }
        Ok(buf)
    }

    /// Writes `bytes` to a temp file and moves it into place; without `overwrite` an existing
    /// destination is never replaced. Blocking: use `spawn_blocking`.
    pub fn write_new(
        &self,
        input: &str,
        bytes: &[u8],
        overwrite: bool,
        max_bytes: u64,
    ) -> Result<PathBuf, FileError> {
        if bytes.len() as u64 > max_bytes {
            return Err(FileError::TooLarge {
                size: bytes.len() as u64,
                cap: max_bytes,
            });
        }
        let dest = if overwrite {
            self.resolve_write_overwrite(input)?
        } else {
            self.resolve_write(input)?
        };
        let tmp = temp_path(&dest);
        let result = write_temp(&tmp, bytes).and_then(|()| finalize(&tmp, &dest, overwrite, input));
        let _ = std::fs::remove_file(&tmp);
        result.map(|()| dest)
    }
}

fn temp_path(dest: &Path) -> PathBuf {
    let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    dest.with_file_name(format!(".{name}.{}.{n}.part", std::process::id()))
}

fn write_temp(tmp: &Path, bytes: &[u8]) -> Result<(), FileError> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)
        .map_err(|e| io_err(tmp, e))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| io_err(tmp, e))
}

/// Hard link first so an existing destination is never replaced, exclusive copy as fallback.
fn finalize(tmp: &Path, dest: &Path, overwrite: bool, input: &str) -> Result<(), FileError> {
    if overwrite {
        return std::fs::rename(tmp, dest).map_err(|e| io_err(dest, e));
    }
    match std::fs::hard_link(tmp, dest) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(FileError::Exists(input.to_string()))
        }
        Err(_) => copy_exclusive(tmp, dest, input),
    }
}

fn copy_exclusive(tmp: &Path, dest: &Path, input: &str) -> Result<(), FileError> {
    let mut out = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(FileError::Exists(input.to_string()));
        }
        Err(e) => return Err(io_err(dest, e)),
    };
    let copied = std::fs::File::open(tmp)
        .and_then(|mut src| std::io::copy(&mut src, &mut out))
        .and_then(|_| out.sync_all());
    if let Err(e) = copied {
        drop(out);
        let _ = std::fs::remove_file(dest);
        return Err(io_err(dest, e));
    }
    Ok(())
}

/// Reduces a remote file name to `[A-Za-z0-9._-]`, at most 100 chars, never dot-leading.
pub fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(100)
        .collect();
    let trimmed = cleaned.trim_start_matches('.');
    if trimmed.is_empty() {
        "file".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Filename sent to Meta: keeps Unicode, strips path separators and control characters.
pub fn clean_remote_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control() && *c != '/' && *c != '\\')
        .take(255)
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "file".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canon_tmp() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tmp");
        let base = std::fs::canonicalize(dir.path()).expect("canon");
        (dir, base)
    }

    #[test]
    fn files_dir_rejects_path_escape() {
        let (_guard, base) = canon_tmp();
        let root = base.join("jail");
        let outside = base.join("outside");
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::create_dir_all(&outside).expect("mkdir");
        std::fs::write(base.join("x"), b"secret").expect("write");
        std::fs::write(outside.join("f"), b"secret").expect("write");
        std::fs::write(root.join("ok.txt"), b"fine").expect("write");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.join("f"), root.join("link_file")).expect("ln");
            std::os::unix::fs::symlink(&outside, root.join("link_dir")).expect("ln");
        }
        let jail = FileJail::new(&root).expect("jail");

        assert!(jail.resolve_read("ok.txt").is_ok());
        assert!(jail.resolve_write("new/dir/file.bin").is_ok());

        let abs_outside = outside.join("f").display().to_string();
        let escape = |r: Result<PathBuf, FileError>| matches!(r, Err(FileError::Escape(_)));
        assert!(escape(jail.resolve_read("../x")));
        assert!(escape(jail.resolve_read(&abs_outside)));
        assert!(escape(jail.resolve_write("../x")));
        assert!(escape(jail.resolve_write(&abs_outside)));
        assert!(escape(jail.resolve_write("a/../../x")));

        #[cfg(unix)]
        {
            assert!(escape(jail.resolve_read("link_file")));
            assert!(escape(jail.resolve_read("link_dir/f")));
            assert!(escape(jail.resolve_write("link_file")));
            assert!(escape(jail.resolve_write("link_dir/new")));
        }
    }

    #[test]
    fn write_refuses_existing_and_dotfiles() {
        let (_guard, root) = canon_tmp();
        std::fs::write(root.join("have.txt"), b"x").expect("write");
        let jail = FileJail::new(&root).expect("jail");
        assert!(matches!(
            jail.resolve_write("have.txt"),
            Err(FileError::Exists(_))
        ));
        assert!(jail.resolve_write_overwrite("have.txt").is_ok());
        assert!(matches!(
            jail.resolve_write(".hidden"),
            Err(FileError::Invalid(_))
        ));
        assert!(matches!(
            jail.resolve_write("a/.b/c"),
            Err(FileError::Invalid(_))
        ));
    }

    #[test]
    fn write_new_is_atomic_and_respects_overwrite() {
        let (_guard, root) = canon_tmp();
        let jail = FileJail::new(&root).expect("jail");
        let p = jail
            .write_new("sub/a.bin", b"one", false, 100)
            .expect("write");
        assert_eq!(std::fs::read(&p).expect("read"), b"one");
        assert!(matches!(
            jail.write_new("sub/a.bin", b"two", false, 100),
            Err(FileError::Exists(_))
        ));
        assert_eq!(std::fs::read(&p).expect("read"), b"one");
        jail.write_new("sub/a.bin", b"two", true, 100)
            .expect("overwrite");
        assert_eq!(std::fs::read(&p).expect("read"), b"two");
        assert!(matches!(
            jail.write_new("big.bin", &[0u8; 11], false, 10),
            Err(FileError::TooLarge { size: 11, cap: 10 })
        ));
        assert!(!root.join("big.bin").exists());
        let names: Vec<String> = std::fs::read_dir(root.join("sub"))
            .expect("dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.bin".to_string()]);
    }

    #[test]
    fn read_capped_enforces_the_cap() {
        let (_guard, root) = canon_tmp();
        std::fs::write(root.join("f.txt"), b"hello").expect("write");
        let jail = FileJail::new(&root).expect("jail");
        assert_eq!(jail.read_capped("f.txt", 5).expect("read"), b"hello");
        assert!(matches!(
            jail.read_capped("f.txt", 4),
            Err(FileError::TooLarge { size: 5, cap: 4 })
        ));
        assert!(matches!(
            jail.read_capped("missing.txt", 4),
            Err(FileError::NotFound(_))
        ));
        assert!(matches!(
            jail.read_capped("../f.txt", 4),
            Err(FileError::Escape(_))
        ));
        assert!(matches!(
            jail.read_capped("/definitely/not/here", 4),
            Err(FileError::Escape(_))
        ));
    }

    #[test]
    fn remote_names_keep_unicode() {
        assert_eq!(
            clean_remote_filename(" rapport été.pdf "),
            "rapport été.pdf"
        );
        assert_eq!(clean_remote_filename("a/b\\c\0d.txt"), "abcd.txt");
        assert_eq!(clean_remote_filename("  "), "file");
    }

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(sanitize_filename("..."), "file");
        assert_eq!(sanitize_filename("report 1.pdf"), "report_1.pdf");
    }
}
