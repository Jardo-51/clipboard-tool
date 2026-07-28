//! User configuration loaded from `config.toml` in the platform config dir
//! (`~/.config/clipboard-tool/config.toml` on Linux). A commented default file
//! is written on first run.

use std::path::{Path, PathBuf};

use global_hotkey::hotkey::HotKey;
use serde::{Deserialize, Serialize};

const DEFAULT_HOTKEY: &str = "ctrl+shift+KeyV";

const FILE_HEADER: &str = "\
# clipboard-tool configuration
#
# hotkey:       global shortcut to open the history popup. Format is
#               \"<modifiers>+<Code>\", modifiers are ctrl/shift/alt/super and the
#               key is a physical code such as KeyV, KeyA, Digit1, etc.
# history_size: maximum number of items to remember.
# persist:      keep history across restarts (stored as JSON in the data dir).

";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub hotkey: String,
    pub history_size: usize,
    pub persist: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: DEFAULT_HOTKEY.to_string(),
            history_size: 100,
            persist: true,
        }
    }
}

impl Config {
    pub fn config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("clipboard-tool").join("config.toml"))
    }

    /// Load config, creating a default file if none exists. Malformed files
    /// fall back to defaults (with a warning) rather than aborting startup.
    pub fn load() -> Self {
        let Some(path) = Self::config_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(contents) => match toml::from_str(&contents) {
                Ok(cfg) => cfg,
                Err(e) => {
                    eprintln!(
                        "config: {} is invalid ({e}); using defaults",
                        path.display()
                    );
                    Self::default()
                }
            },
            // First run: no config yet, so seed one with the defaults.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Self::default();
                cfg.write_default(&path);
                cfg
            }
            // Anything else — a root-owned file, a hardened ~/.config, an
            // SELinux denial — is a real error. Treating it as "not there yet"
            // would silently ignore the user's actual config and then try to
            // overwrite it.
            Err(e) => {
                eprintln!("config: cannot read {} ({e}); using defaults", path.display());
                Self::default()
            }
        }
    }

    fn write_default(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!(
                    "config: cannot create {} ({e}); continuing with defaults \
                     and no config file",
                    parent.display()
                );
                return;
            }
        }
        match toml::to_string(self) {
            Ok(body) => {
                if let Err(e) = std::fs::write(path, format!("{FILE_HEADER}{body}")) {
                    eprintln!(
                        "config: cannot write {} ({e}); continuing with defaults",
                        path.display()
                    );
                }
            }
            Err(e) => eprintln!("config: cannot serialize the default config ({e})"),
        }
    }

    /// Parse the configured hotkey, falling back to the default on error.
    pub fn parse_hotkey(&self) -> HotKey {
        self.hotkey.parse::<HotKey>().unwrap_or_else(|e| {
            eprintln!(
                "config: invalid hotkey '{}' ({e}); using {DEFAULT_HOTKEY}",
                self.hotkey
            );
            DEFAULT_HOTKEY
                .parse()
                .expect("default hotkey must always parse")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_hotkey_parses() {
        // Guards against a typo in DEFAULT_HOTKEY that would panic at runtime.
        assert!(Config::default().hotkey.parse::<HotKey>().is_ok());
    }

    #[test]
    fn partial_toml_fills_defaults() {
        // Only history_size is set; the rest must come from Default.
        let cfg: Config = toml::from_str("history_size = 5").unwrap();
        assert_eq!(cfg.history_size, 5);
        assert_eq!(cfg.hotkey, DEFAULT_HOTKEY);
        assert!(cfg.persist);
    }

    #[test]
    fn invalid_hotkey_falls_back() {
        let cfg = Config {
            hotkey: "not a hotkey".into(),
            ..Config::default()
        };
        // Should not panic; returns the default hotkey.
        let _ = cfg.parse_hotkey();
    }
}
