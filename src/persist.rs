//! History persistence: the item list is stored as a small JSON file in the
//! platform data dir (`~/.local/share/clipboard-tool/history.json` on Linux).
//! JSON keeps the footprint minimal — a capped list of strings doesn't warrant
//! an embedded database.
//!
//! Clipboard history is sensitive — passwords, tokens and recovery codes all
//! pass through it — so on Unix the file is created `0600` and the containing
//! directory is tightened to `0700`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::history::{Entry, EntryKind};

/// One entry as written to `history.json`.
///
/// Borrowed on the way out so saving doesn't copy the entry texts, which are
/// what the [`Entry`]'s `Arc<str>` exists to avoid.
///
/// `kind` is omitted for ordinary text, which is nearly every entry. That keeps
/// a text-only history byte-identical to what earlier versions wrote, so the
/// field only ever appears on the rows that actually need it.
#[derive(Serialize)]
struct StoredItem<'a> {
    text: &'a str,
    favorite: bool,
    #[serde(skip_serializing_if = "StoredKind::is_text")]
    kind: StoredKind,
}

/// [`EntryKind`] as it appears in `history.json`.
///
/// A separate type rather than `#[derive(Deserialize)]` on `EntryKind` itself:
/// the file format is this module's business, and spelling it out here keeps the
/// history model free of `serde` attributes.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum StoredKind {
    Text,
    Paths,
}

impl StoredKind {
    fn is_text(&self) -> bool {
        matches!(self, Self::Text)
    }
}

/// One entry as read back. Files written before favorites existed hold a bare
/// string per entry, and a user who downgrades then upgrades again leaves one
/// behind, so both shapes stay readable — a plain string simply isn't a
/// favorite. `serde`'s untagged enum tries the variants in order, and a JSON
/// string can only match `Text`.
///
/// `kind` is defaulted for the same reason `favorite` is: it postdates both
/// earlier shapes, and a file without it is a history of ordinary text. Nothing
/// widens it to accept unrecognised spellings — a downgrade drops the field
/// rather than mangling it, so the only way to produce one is a hand edit, which
/// [`load`] already moves aside and reports.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawItem {
    Text(String),
    Full {
        text: String,
        #[serde(default)]
        favorite: bool,
        #[serde(default)]
        kind: Option<StoredKind>,
    },
}

impl From<StoredKind> for EntryKind {
    fn from(kind: StoredKind) -> Self {
        match kind {
            StoredKind::Text => EntryKind::Text,
            StoredKind::Paths => EntryKind::Paths,
        }
    }
}

impl From<EntryKind> for StoredKind {
    fn from(kind: EntryKind) -> Self {
        match kind {
            EntryKind::Text => StoredKind::Text,
            EntryKind::Paths => StoredKind::Paths,
        }
    }
}

impl From<RawItem> for Entry {
    fn from(raw: RawItem) -> Self {
        match raw {
            RawItem::Text(text) => Entry::new(text, false),
            RawItem::Full {
                text,
                favorite,
                kind,
            } => Entry::with_kind(
                text,
                favorite,
                kind.map(EntryKind::from).unwrap_or_default(),
            ),
        }
    }
}

/// Distinguishes concurrent temp files within one process; the pid separates
/// them across processes.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

pub fn history_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("clipboard-tool").join("history.json"))
}

/// Load a previously saved history, in the order it was written. A missing file
/// yields an empty list; an unparseable one yields an empty list too, rather
/// than failing the startup that reads it — but it is moved aside first and the
/// reason is printed.
///
/// Returning empty is not the inert act it looks like. The first copy after
/// startup marks the store dirty, and the next flush renames a fresh
/// `history.json` over the old one, so a file this function merely ignored is
/// gone for good within seconds. That is a lot to do silently to the only copy
/// of something the user never asked to delete, and it doesn't take a mangled
/// file to reach: `serde`'s untagged enum fails the *whole* array on one bad
/// element, so a single hand-edited entry takes out every other one with it.
///
/// Keeping the bytes under a sibling name costs a rename and leaves the user
/// something to salvage; saying so on stderr follows the precedent [`push`] sets
/// when it skips an oversized entry.
///
/// [`push`]: crate::history::HistoryStore::push
pub fn load(path: &Path) -> Vec<Entry> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    match serde_json::from_str::<Vec<RawItem>>(&contents) {
        Ok(items) => items.into_iter().map(Entry::from).collect(),
        Err(e) => {
            let aside = path.with_extension("json.corrupt");
            match std::fs::rename(path, &aside) {
                Ok(()) => eprintln!(
                    "history: {} could not be read ({e}); starting with an empty history. \
                     The unreadable file has been kept as {}.",
                    path.display(),
                    aside.display()
                ),
                Err(rename_error) => eprintln!(
                    "history: {} could not be read ({e}), and could not be moved aside \
                     ({rename_error}); starting with an empty history. Copy the file \
                     elsewhere now if you want to keep it — the next save overwrites it.",
                    path.display()
                ),
            }
            Vec::new()
        }
    }
}

/// Save the history, in the order given, via a temp-file + rename, so an
/// interrupted write can't corrupt the existing file.
///
/// The temp name is unique per call: `save` has several callers that can run
/// concurrently (the periodic flush thread and the tray's Quit handler), and a
/// shared temp path would let one writer truncate the file another is about to
/// rename into place. Both the temp file and the parent directory are fsynced,
/// so the guarantee holds for power loss and not just for a process crash.
///
/// Concurrent saves are still last-rename-wins; each writes a complete, valid
/// file, so the worst case is a slightly stale history rather than a corrupt one.
pub fn save(path: &Path, items: &[Entry]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        restrict_dir(parent);
    }
    let stored: Vec<StoredItem> = items
        .iter()
        .map(|e| StoredItem {
            text: &e.text,
            favorite: e.favorite,
            kind: e.kind.into(),
        })
        .collect();
    let json = serde_json::to_string(&stored)?;
    let tmp = path.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));

    if let Err(e) = write_private(&tmp, json.as_bytes()).and_then(|()| std::fs::rename(&tmp, path))
    {
        // Unique names don't self-clean the way a fixed one did.
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    // Durably record the new directory entry, so a crash after this returns
    // can't leave the rename unapplied.
    sync_dir(path.parent());
    Ok(())
}

/// Best-effort fsync of a directory, which is what makes a rename durable on
/// Unix. Not meaningful on Windows, where directories can't be opened this way.
fn sync_dir(dir: Option<&Path>) {
    #[cfg(unix)]
    if let Some(dir) = dir {
        if let Ok(handle) = std::fs::File::open(dir) {
            let _ = handle.sync_all();
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
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
        // Get the contents on disk before the rename publishes the name.
        f.sync_all()
    }
    #[cfg(not(unix))]
    {
        use std::io::Write;

        let mut f = std::fs::File::create(path)?;
        f.write_all(bytes)?;
        f.sync_all()
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

    /// `(text, favorite)` pairs, since [`Entry`] is compared field by field in
    /// these tests rather than carrying a `PartialEq` the app has no use for.
    fn pairs(items: &[Entry]) -> Vec<(&str, bool)> {
        items
            .iter()
            .map(|e| (e.text.as_ref(), e.favorite))
            .collect()
    }

    #[test]
    fn roundtrip_preserves_order_and_favorites() {
        let dir = std::env::temp_dir().join(format!("clipboard-tool-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");

        let items = vec![
            Entry::new("starred", true),
            Entry::new("middle", false),
            Entry::new("oldest", false),
        ];
        save(&path, &items).unwrap();
        assert_eq!(pairs(&load(&path)), pairs(&items));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `(text, kind)` pairs, for the tests that are about the kind rather than
    /// the star.
    fn kinds(items: &[Entry]) -> Vec<(&str, EntryKind)> {
        items.iter().map(|e| (e.text.as_ref(), e.kind)).collect()
    }

    #[test]
    fn roundtrip_preserves_the_entry_kind() {
        let dir = std::env::temp_dir().join(format!("clipboard-tool-kind-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");

        let items = vec![
            Entry::with_kind("/home/me/notes", true, EntryKind::Paths),
            Entry::new("typed", false),
        ];
        save(&path, &items).unwrap();
        assert_eq!(
            kinds(&load(&path)),
            [
                ("/home/me/notes", EntryKind::Paths),
                ("typed", EntryKind::Text)
            ]
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ordinary_text_is_written_without_a_kind() {
        // The field is only meaningful on the rows that aren't plain text, and
        // leaving it off keeps a text-only history byte-identical to what
        // earlier versions wrote — which is also what an older binary reading
        // this file expects to find.
        let dir =
            std::env::temp_dir().join(format!("clipboard-tool-nokind-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");

        save(&path, &[Entry::new("typed", false)]).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"[{"text":"typed","favorite":false}]"#
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_pre_paths_history_loads_as_text() {
        // Entries written before the kind existed carry no `kind` field, and
        // everything in such a file was copied as text.
        let dir =
            std::env::temp_dir().join(format!("clipboard-tool-oldkind-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");

        std::fs::write(&path, r#"[{"text":"typed","favorite":true}]"#).unwrap();
        assert_eq!(kinds(&load(&path)), [("typed", EntryKind::Text)]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_pre_favorites_history_still_loads() {
        // Upgrading must not throw the user's history away: before favorites,
        // the file was a plain array of strings.
        let dir =
            std::env::temp_dir().join(format!("clipboard-tool-legacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");

        std::fs::write(&path, r#"["newest","oldest"]"#).unwrap();
        assert_eq!(pairs(&load(&path)), [("newest", false), ("oldest", false)]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_corrupt_history_loads_empty_and_is_kept() {
        // Neither shape: the file is ignored rather than failing the startup
        // that reads it — but the next save would otherwise overwrite it, so the
        // bytes have to survive somewhere the user can get at them.
        let dir =
            std::env::temp_dir().join(format!("clipboard-tool-corrupt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");

        std::fs::write(&path, "{ not json at all").unwrap();
        assert!(load(&path).is_empty());
        assert!(!path.exists(), "the unreadable file must be out of the way");
        assert_eq!(
            std::fs::read_to_string(dir.join("history.json.corrupt")).unwrap(),
            "{ not json at all"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn one_bad_entry_does_not_take_the_file_down_silently() {
        // `untagged` fails the whole array on a single malformed element, so a
        // hand edit is enough to lose everything. It still loads empty, but the
        // other entries have to be recoverable from the file left behind.
        let dir =
            std::env::temp_dir().join(format!("clipboard-tool-partial-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");

        let written = r#"[{"text":"keep me","favorite":true},{"text":5}]"#;
        std::fs::write(&path, written).unwrap();
        assert!(load(&path).is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.join("history.json.corrupt")).unwrap(),
            written
        );

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

        save(&path, &[Entry::new("secret", false)]).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "history.json must not be group/world readable");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn concurrent_saves_never_corrupt() {
        let dir =
            std::env::temp_dir().join(format!("clipboard-tool-concurrent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");

        // A shared temp path let one writer truncate the file another was about
        // to rename into place, leaving the destination empty or partial.
        std::thread::scope(|s| {
            for n in 0..8 {
                let path = path.clone();
                s.spawn(move || {
                    let items: Vec<Entry> = (0..50)
                        .map(|i| Entry::new(format!("thread {n} item {i}"), false))
                        .collect();
                    for _ in 0..20 {
                        save(&path, &items).unwrap();
                        // Every observed state must be a complete history.
                        assert_eq!(load(&path).len(), items.len());
                    }
                });
            }
        });

        // Unique temp names must not accumulate.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_loads_empty() {
        let path = std::env::temp_dir().join("clipboard-tool-does-not-exist-xyz.json");
        assert!(load(&path).is_empty());
    }
}
