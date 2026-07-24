//! History persistence: the item list is stored as a small JSON file in the
//! platform data dir (`~/.local/share/clipboard-tool/history.json` on Linux).
//! JSON keeps the footprint minimal — a capped list of strings doesn't warrant
//! an embedded database.

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
    }
    let json = serde_json::to_string(items)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
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

    #[test]
    fn missing_file_loads_empty() {
        let path = std::env::temp_dir().join("clipboard-tool-does-not-exist-xyz.json");
        assert!(load(&path).is_empty());
    }
}
