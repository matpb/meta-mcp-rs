//! Owner-only files and folders: modes 0700 and 0600 on Unix, an owner-only ACL on Windows.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// Creates `path` and its parents, then makes `path` private (resetting a folder that exists).
pub fn private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(path)?;
    set_private_dir(path)
}

/// Makes the existing folder `path` private.
pub fn set_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(windows)]
    {
        super::winsec::restrict_to_owner(path, true)
    }
}

/// Makes a file created with these options private from the start (Unix mode 0600).
pub fn private_open(opts: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600)
    }
    #[cfg(not(unix))]
    opts
}

/// Makes the open file at `path` private, also when it existed with looser access.
pub fn set_private_file(file: &File, path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = path;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(windows)]
    {
        let _ = file;
        super::winsec::restrict_to_owner(path, false)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
    }

    #[test]
    fn private_dir_is_0700_also_for_parents_and_when_it_existed_loose() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let deep = tmp.path().join("a/b");
        private_dir(&deep).expect("create");
        assert_eq!(mode(&deep), 0o700);
        assert_eq!(mode(&tmp.path().join("a")), 0o700);
        std::fs::set_permissions(&deep, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        private_dir(&deep).expect("again");
        assert_eq!(mode(&deep), 0o700);
    }

    #[test]
    fn set_private_file_gives_0600_and_private_open_creates_it_that_way() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("f");
        std::fs::write(&path, b"x").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let file = OpenOptions::new().append(true).open(&path).expect("open");
        set_private_file(&file, &path).expect("restrict");
        assert_eq!(mode(&path), 0o600);
        let fresh = tmp.path().join("g");
        let mut opts = OpenOptions::new();
        private_open(&mut opts)
            .write(true)
            .create_new(true)
            .open(&fresh)
            .expect("create");
        assert_eq!(mode(&fresh), 0o600);
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use crate::personal::winsec::{current_user_sid, dacl_sddl};

    #[test]
    fn private_dir_leaves_one_protected_ace_for_the_user() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let deep = tmp.path().join("a/b");
        private_dir(&deep).expect("create");
        let sid = current_user_sid().expect("sid");
        let sddl = dacl_sddl(&deep).expect("sddl");
        assert!(sddl.starts_with("D:P"), "{sddl}");
        assert_eq!(sddl.matches('(').count(), 1, "{sddl}");
        assert!(sddl.contains(&format!("(A;OICI;FA;;;{sid})")), "{sddl}");
        let child = deep.join("child");
        std::fs::write(&child, b"x").expect("child");
        assert!(dacl_sddl(&child).expect("child sddl").contains(&sid));
    }

    #[test]
    fn set_private_file_works_on_an_open_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("f");
        let mut opts = OpenOptions::new();
        let file = private_open(&mut opts)
            .write(true)
            .create_new(true)
            .open(&path)
            .expect("create");
        set_private_file(&file, &path).expect("restrict");
        let sid = current_user_sid().expect("sid");
        let sddl = dacl_sddl(&path).expect("sddl");
        assert!(sddl.starts_with("D:P"), "{sddl}");
        assert!(sddl.contains(&format!("(A;;FA;;;{sid})")), "{sddl}");
    }
}
