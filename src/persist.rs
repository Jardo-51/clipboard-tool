//! History persistence: the item list is stored as a small JSON file in the
//! platform data dir (`~/.local/share/clipboard-tool/history.json` on Linux).
//! JSON keeps the footprint minimal — a capped list of strings doesn't warrant
//! an embedded database.
//!
//! Clipboard history is sensitive — passwords, tokens and recovery codes all
//! pass through it — so on Unix the file is created `0600` and the containing
//! directory is tightened to `0700`.

use std::path::{Path, PathBuf};

pub fn history_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("clipboard-tool").join("history.json"))
}

/// Load a previously saved history (newest-first). Missing/corrupt files yield
/// an empty list rather than failing.
pub fn load(path: &Path) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Save the history (newest-first) via a temp-file + rename so a crash mid-write
/// can't corrupt the existing file.
pub fn save(path: &Path, items: &[String]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        restrict_dir(parent);
    }
    let json = serde_json::to_string(items)?;
    let tmp = path.with_extension("json.tmp");
    write_private(&tmp, json.as_bytes())?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Write `bytes` to `path`, creating the file owner-only (`0600`) on Unix.
///
/// The mode is applied at `open` time rather than with a follow-up
/// `set_permissions`, so the file is never even briefly world-readable. Because
/// `save` publishes this file by renaming it over the destination, the
/// destination inherits `0600` too — including for users upgrading from a
/// version that wrote a `0644` history.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(bytes)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
    }
}

/// Best-effort tightening of the data directory to `0700` on Unix. Failure is
/// ignored: the directory may be pre-existing and owned differently, and the
/// file mode above is what actually protects the contents.
fn restrict_dir(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_preserves_order() {
        let dir = std::env::temp_dir().join(format!("clipboard-tool-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");

        let items = vec!["newest".to_string(), "middle".to_string(), "oldest".to_string()];
        save(&path, &items).unwrap();
        assert_eq!(load(&path), items);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn saved_history_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("clipboard-tool-mode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");

        // A pre-existing world-readable file must not stay that way.
        std::fs::write(&path, "[]").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        save(&path, &["secret".to_string()]).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "history.json must not be group/world readable");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_loads_empty() {
        let path = std::env::temp_dir().join("clipboard-tool-does-not-exist-xyz.json");
        assert!(load(&path).is_empty());
    }
}
