//! Per-platform session state kept on disk: `<home>/personal/<platform>/state.json` (0600) and
//! the sidecar's `data/` folder (0700). The content is opaque and never logged.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{PersonalPlatform, personal_dir};

const STATE_FILE: &str = "state.json";
const TMP_FILE: &str = "state.json.tmp";
const CORRUPT_FILE: &str = "state.json.corrupt";

#[derive(Debug, Clone)]
pub struct StateStore {
    personal: PathBuf,
    dir: PathBuf,
}

fn make_private_dir(path: &Path) -> io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

impl StateStore {
    pub fn new(home: &Path, platform: PersonalPlatform) -> Self {
        let personal = personal_dir(home);
        let dir = personal.join(platform.as_str());
        Self { personal, dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn state_path(&self) -> PathBuf {
        self.dir.join(STATE_FILE)
    }

    /// The sidecar's private folder, `<dir>/data`.
    pub fn data_dir(&self) -> PathBuf {
        self.dir.join("data")
    }

    /// Creates the folders with mode 0700 and resets the mode of ones that already exist.
    pub fn ensure_dirs(&self) -> io::Result<()> {
        make_private_dir(&self.personal)?;
        make_private_dir(&self.dir)?;
        make_private_dir(&self.data_dir())
    }

    pub fn exists(&self) -> bool {
        self.state_path().is_file()
    }

    /// The stored state; a damaged file is moved to `state.json.corrupt` and counts as absent.
    pub fn load(&self) -> Option<Value> {
        let path = self.state_path();
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
            Err(e) => {
                tracing::warn!(error = %e, "cannot read the state file, treating it as absent");
                return None;
            }
        };
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Null) => None,
            Ok(v) => Some(v),
            Err(_) => {
                tracing::warn!("the state file is damaged, moving it aside");
                let _ = std::fs::rename(&path, self.dir.join(CORRUPT_FILE));
                None
            }
        }
    }

    /// Writes the state atomically: private temp file, fsync, rename, fsync of the folder.
    pub fn save(&self, state: &Value) -> io::Result<()> {
        self.ensure_dirs()?;
        let bytes = serde_json::to_vec(state).map_err(io::Error::other)?;
        let tmp = self.dir.join(TMP_FILE);
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, self.state_path())?;
        sync_dir(&self.dir)
    }

    /// Empties the sidecar's `data/` folder (databases, thread cache, locks, outbox) and keeps the folder.
    pub fn wipe_data(&self) -> io::Result<()> {
        let entries = match std::fs::read_dir(self.data_dir()) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let mut first_error = None;
        for entry in entries {
            let result = entry.and_then(|e| {
                if e.file_type()?.is_dir() {
                    std::fs::remove_dir_all(e.path())
                } else {
                    std::fs::remove_file(e.path())
                }
            });
            if let Err(e) = result {
                first_error.get_or_insert(e);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Removes the state file and its leftovers (temp and damaged copies); missing ones are fine.
    pub fn delete(&self) -> io::Result<()> {
        for name in [STATE_FILE, TMP_FILE, CORRUPT_FILE] {
            match std::fs::remove_file(self.dir.join(name)) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        match sync_dir(&self.dir) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
    }

    fn store() -> (tempfile::TempDir, StateStore) {
        let home = tempfile::tempdir().expect("tempdir");
        let store = StateStore::new(home.path(), PersonalPlatform::WhatsApp);
        (home, store)
    }

    #[test]
    fn save_creates_private_files_and_dirs() {
        let (home, store) = store();
        assert!(!store.exists());
        store.save(&json!({"session": "s"})).expect("save");
        assert_eq!(mode(&store.state_path()), 0o600);
        assert_eq!(mode(&personal_dir(home.path())), 0o700);
        assert_eq!(mode(store.dir()), 0o700);
        assert_eq!(mode(&store.data_dir()), 0o700);
        assert_eq!(store.load(), Some(json!({"session": "s"})));
        assert!(!store.dir().join(TMP_FILE).exists());
    }

    #[test]
    fn existing_dirs_get_their_mode_reset() {
        let (_home, store) = store();
        store.ensure_dirs().expect("dirs");
        std::fs::set_permissions(store.dir(), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        std::fs::set_permissions(store.data_dir(), std::fs::Permissions::from_mode(0o777))
            .expect("chmod");
        store.ensure_dirs().expect("dirs again");
        assert_eq!(mode(store.dir()), 0o700);
        assert_eq!(mode(&store.data_dir()), 0o700);
    }

    #[test]
    fn save_replaces_atomically_and_tightens_a_loose_tmp_file() {
        let (_home, store) = store();
        store.save(&json!({"v": 1})).expect("first");
        let tmp = store.dir().join(TMP_FILE);
        std::fs::write(&tmp, b"stale").expect("stale tmp");
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        store.save(&json!({"v": 2})).expect("second");
        assert_eq!(store.load(), Some(json!({"v": 2})));
        assert_eq!(mode(&store.state_path()), 0o600);
        assert!(!tmp.exists(), "the temp file is renamed away");
    }

    #[test]
    fn a_corrupt_file_is_moved_aside_and_counts_as_absent() {
        let (_home, store) = store();
        store.ensure_dirs().expect("dirs");
        std::fs::write(store.state_path(), b"{not json").expect("write");
        assert_eq!(store.load(), None);
        assert!(!store.exists());
        assert!(store.dir().join(CORRUPT_FILE).exists());
    }

    #[test]
    fn delete_also_removes_the_damaged_copy() {
        let (_home, store) = store();
        store.ensure_dirs().expect("dirs");
        std::fs::write(store.state_path(), b"{broken").expect("write");
        assert_eq!(store.load(), None);
        assert!(store.dir().join(CORRUPT_FILE).exists());
        store.delete().expect("delete");
        assert!(!store.dir().join(CORRUPT_FILE).exists());
    }

    #[test]
    fn wipe_data_empties_the_folder_but_keeps_it() {
        let (_home, store) = store();
        store.wipe_data().expect("nothing to wipe");
        store.ensure_dirs().expect("dirs");
        let data = store.data_dir();
        std::fs::write(data.join("session.db"), b"keys").expect("db");
        std::fs::create_dir_all(data.join("outbox/sub")).expect("outbox");
        std::fs::write(data.join("outbox/sub/f.bin"), b"x").expect("file");
        std::os::unix::fs::symlink("/etc/passwd", data.join("link")).expect("link");
        store.save(&json!({"v": 1})).expect("save");
        store.wipe_data().expect("wipe");
        assert!(data.is_dir());
        assert_eq!(std::fs::read_dir(&data).expect("dir").count(), 0);
        assert!(store.exists(), "state.json is not part of the data folder");
        assert!(Path::new("/etc/passwd").exists());
    }

    #[test]
    fn delete_removes_the_file_and_tolerates_absence() {
        let (_home, store) = store();
        store.delete().expect("nothing to delete");
        store.save(&json!({"v": 1})).expect("save");
        store.delete().expect("delete");
        assert!(!store.exists());
        assert_eq!(store.load(), None);
    }
}
