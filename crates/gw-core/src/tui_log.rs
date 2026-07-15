use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use chrono::{DateTime, Utc};

pub fn error(message: &str) {
    let Ok(root) = crate::store::default_state_dir() else {
        return;
    };
    let _ = append(&root, Utc::now(), message);
}

fn append(root: &Path, timestamp: DateTime<Utc>, message: &str) -> io::Result<()> {
    fs::create_dir_all(root)?;
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(root.join("tui.log"))?;
    let message = message.replace(['\r', '\n'], " ");
    let line = format!("{} {message}\n", timestamp.to_rfc3339());
    log.write_all(line.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_one_line_per_error() {
        let temp = tempfile::tempdir().unwrap();
        let timestamp = "2026-07-15T12:34:56Z".parse().unwrap();

        append(temp.path(), timestamp, "first error\nwith detail").unwrap();
        append(temp.path(), timestamp, "second error").unwrap();

        assert_eq!(
            fs::read_to_string(temp.path().join("tui.log")).unwrap(),
            "2026-07-15T12:34:56+00:00 first error with detail\n\
             2026-07-15T12:34:56+00:00 second error\n"
        );
    }
}
