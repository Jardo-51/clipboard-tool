//! Clipboard history storage: a fixed-capacity, de-duplicated ring buffer.

use std::collections::VecDeque;
use std::sync::Arc;

/// Largest single entry the history will record, in bytes.
///
/// Nothing else bounds an entry: the watcher stores whatever `arboard` hands
/// back, so copying a large file's contents would put tens of megabytes into
/// memory, into every `snapshot()` and into `history.json`. Capacity alone
/// doesn't help — it counts items, not bytes.
///
/// Oversized entries are *skipped* rather than truncated. This is a paste
/// buffer: a silently shortened entry would be handed to the user later as if
/// it were what they copied, which is worse than not offering it at all. The
/// clipboard itself is untouched, so a plain Ctrl+V still pastes the original
/// in full.
///
/// 1 MiB is far above any text worth re-pasting from a menu, and bounds the
/// history at `history_size` × 1 MiB in the worst case.
const MAX_ITEM_BYTES: usize = 1024 * 1024;

/// Newest item is at the front (index 0).
///
/// Entries are `Arc<str>` rather than `String` because the UI snapshots the
/// whole list on every frame it renders, to avoid holding the lock across
/// rendering. Clipboard entries are unbounded in size — copying a log file or a
/// large JSON blob is routine — so deep-copying them at frame rate is not
/// affordable. Sharing makes a snapshot a handful of refcount bumps.
pub struct HistoryStore {
    items: VecDeque<Arc<str>>,
    capacity: usize,
}

impl HistoryStore {
    pub fn new(capacity: usize) -> Self {
        Self {
            items: VecDeque::with_capacity(capacity),
            capacity: capacity.max(1),
        }
    }

    /// Record a newly copied value. Ignores empties and entries larger than
    /// [`MAX_ITEM_BYTES`]; de-duplicates by moving an existing identical entry
    /// to the front instead of adding a second copy. Returns `true` if the
    /// stored contents/order changed.
    pub fn push(&mut self, value: String) -> bool {
        if value.is_empty() {
            return false;
        }
        if value.len() > MAX_ITEM_BYTES {
            // Say so rather than dropping it silently: the user would otherwise
            // just see their copy fail to appear in the popup.
            eprintln!(
                "history: skipping a {} byte clipboard entry (limit {} bytes); \
                 it is still on the clipboard, so Ctrl+V pastes it as usual",
                value.len(),
                MAX_ITEM_BYTES
            );
            return false;
        }
        if let Some(pos) = self.items.iter().position(|v| v.as_ref() == value.as_str()) {
            // Already present — promote to most-recent (no-op if already first).
            if pos == 0 {
                return false;
            }
            self.items.remove(pos);
        }
        self.items.push_front(Arc::from(value));
        while self.items.len() > self.capacity {
            self.items.pop_back();
        }
        true
    }

    #[allow(dead_code)] // test-only helper; the UI reads items through `iter`
    pub fn get(&self, index: usize) -> Option<&Arc<str>> {
        self.items.get(index)
    }

    #[allow(dead_code)] // test-only helper; the UI reads the length off its snapshot
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[allow(dead_code)] // test-only helper; the UI reads the length off its snapshot
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Newest-first handles to the items. Cloning what this yields is cheap.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<str>> {
        self.items.iter()
    }

    /// Newest-first copy of the items, for persistence. Unlike [`iter`], this
    /// does deep-copy — it runs at most once per persist interval.
    ///
    /// [`iter`]: Self::iter
    pub fn snapshot(&self) -> Vec<String> {
        self.items.iter().map(|s| s.to_string()).collect()
    }

    /// Replace the contents with a previously saved (newest-first) list,
    /// truncated to the current capacity.
    ///
    /// Entries over [`MAX_ITEM_BYTES`] are dropped here too, so a `history.json`
    /// written before the cap existed can't reintroduce them.
    pub fn restore(&mut self, items: Vec<String>) {
        self.items = items
            .into_iter()
            .filter(|s| s.len() <= MAX_ITEM_BYTES)
            .take(self.capacity)
            .map(Arc::from)
            .collect();
    }

    // Called by the tray's "Clear history", which only exists on Linux so far.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn clear(&mut self) {
        self.items.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_promotes_to_front() {
        let mut h = HistoryStore::new(5);
        h.push("a".into());
        h.push("b".into());
        h.push("a".into());
        assert_eq!(h.len(), 2);
        assert_eq!(h.get(0).unwrap().as_ref(), "a");
        assert_eq!(h.get(1).unwrap().as_ref(), "b");
    }

    #[test]
    fn respects_capacity() {
        let mut h = HistoryStore::new(2);
        h.push("a".into());
        h.push("b".into());
        h.push("c".into());
        assert_eq!(h.len(), 2);
        assert_eq!(h.get(0).unwrap().as_ref(), "c");
        assert_eq!(h.get(1).unwrap().as_ref(), "b");
        assert!(h.get(2).is_none());
    }

    #[test]
    fn snapshotting_shares_rather_than_copies() {
        // The UI clones this iterator's output on every rendered frame, so it
        // must hand out handles to the stored entries, not fresh copies.
        let mut h = HistoryStore::new(5);
        h.push("a".into());
        let cloned: Arc<str> = h.iter().next().unwrap().clone();
        assert!(Arc::ptr_eq(&cloned, h.get(0).unwrap()));
    }

    #[test]
    fn ignores_empty() {
        let mut h = HistoryStore::new(5);
        h.push(String::new());
        assert!(h.is_empty());
    }

    #[test]
    fn skips_oversized_entries() {
        let mut h = HistoryStore::new(5);
        assert!(h.push("a".repeat(MAX_ITEM_BYTES)), "the limit itself fits");
        assert!(
            !h.push("b".repeat(MAX_ITEM_BYTES + 1)),
            "one byte over must be rejected"
        );
        assert_eq!(h.len(), 1);
        assert_eq!(h.get(0).unwrap().len(), MAX_ITEM_BYTES);
    }

    #[test]
    fn an_oversized_entry_does_not_displace_the_history() {
        // Skipping must be inert: the earlier items keep their order, and the
        // watcher must not be told the store changed (which would mark it dirty
        // and trigger a pointless save).
        let mut h = HistoryStore::new(5);
        h.push("a".into());
        h.push("b".into());
        assert!(!h.push("x".repeat(MAX_ITEM_BYTES + 1)));
        assert_eq!(h.len(), 2);
        assert_eq!(h.get(0).unwrap().as_ref(), "b");
        assert_eq!(h.get(1).unwrap().as_ref(), "a");
    }

    #[test]
    fn restore_truncates_to_capacity() {
        // A saved history can be longer than the configured history_size (the
        // user shrank it between runs), and restore documents that it keeps the
        // newest items — which are at the front.
        let mut h = HistoryStore::new(3);
        h.restore(vec![
            "newest".to_string(),
            "second".to_string(),
            "third".to_string(),
            "dropped".to_string(),
            "also dropped".to_string(),
        ]);
        assert_eq!(h.len(), 3);
        assert_eq!(h.get(0).unwrap().as_ref(), "newest");
        assert_eq!(h.get(2).unwrap().as_ref(), "third");
    }

    #[test]
    fn restore_replaces_rather_than_appends() {
        let mut h = HistoryStore::new(5);
        h.push("stale".into());
        h.restore(vec!["from disk".to_string()]);
        assert_eq!(h.len(), 1);
        assert_eq!(h.get(0).unwrap().as_ref(), "from disk");
    }

    #[test]
    fn restore_drops_oversized_entries() {
        // A history.json written before the cap existed must not reintroduce
        // an entry `push` would now refuse.
        let mut h = HistoryStore::new(5);
        h.restore(vec![
            "small".to_string(),
            "x".repeat(MAX_ITEM_BYTES + 1),
            "also small".to_string(),
        ]);
        assert_eq!(h.len(), 2);
        assert_eq!(h.get(0).unwrap().as_ref(), "small");
        assert_eq!(h.get(1).unwrap().as_ref(), "also small");
    }
}
