//! Durable file writes: write to a sibling temp file, fsync, then rename over
//! the target so a reader never sees a half-written file. Shared by the event
//! log store and the hook installer; neither owns the primitive.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;

/// Atomically replace `path` with `contents`. When `existed` is true the
/// target's current permissions are preserved; otherwise the file is created
/// `0o600`. The temp file is removed on any failure.
pub fn write(path: &Path, contents: &[u8], existed: bool) -> Result<()> {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    let permissions = if existed {
        Some(fs::metadata(path)?.permissions())
    } else {
        None
    };
    let mut temp_name = OsString::from(path.as_os_str());
    temp_name.push(format!(
        ".gw-tmp-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let temp_path = PathBuf::from(temp_name);
    let result = (|| -> Result<()> {
        let mut temp = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp_path)?;
        temp.write_all(contents)?;
        temp.sync_all()?;
        if let Some(permissions) = permissions {
            fs::set_permissions(&temp_path, permissions)?;
        }
        fs::rename(&temp_path, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_then_overwrites_preserving_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("file");

        write(&path, b"first", false).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"first");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        write(&path, b"second", true).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[test]
    fn leaves_no_temp_files_behind() {
        let temp = tempfile::tempdir().unwrap();
        write(&temp.path().join("file"), b"x", false).unwrap();
        let entries: Vec<_> = fs::read_dir(temp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, ["file"]);
    }
}
