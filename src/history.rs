//! Clipboard history storage: a fixed-capacity, de-duplicated ring buffer with
//! a pinned block of favorites at the front.

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

/// One history entry: the copied text plus whether the user has starred it.
///
/// The text is an `Arc<str>` rather than a `String` because the UI snapshots the
/// whole list on every frame it renders, to avoid holding the lock across
/// rendering. Clipboard entries are unbounded in size — copying a log file or a
/// large JSON blob is routine — so deep-copying them at frame rate is not
/// affordable. Sharing makes a snapshot a handful of refcount bumps.
#[derive(Clone)]
pub struct Entry {
    pub text: Arc<str>,
    pub favorite: bool,
}

impl Entry {
    pub fn new(text: impl Into<Arc<str>>, favorite: bool) -> Self {
        Self {
            text: text.into(),
            favorite,
        }
    }
}

/// Favorites first, then the rest; within each block the newest item is at the
/// front. So index 0 is the newest favorite, or the newest item outright when
/// nothing is starred.
///
/// Every method that inserts or moves an entry re-establishes that split, and
/// the rest of the program relies on it: the popup renders the list in order,
/// and [`favorite_count`] locates the boundary by scanning the leading run of
/// favorites rather than filtering the whole list.
///
/// `capacity` bounds the *non-favorite* items only. Favorites are exempt so
/// that starring an item is a promise it will still be there later — otherwise a
/// busy hour of copying would silently evict the very entries the user marked as
/// worth keeping, which is the one thing the star is for. That leaves the
/// footprint bounded by (`capacity` + favorites) × [`MAX_ITEM_BYTES`], and
/// favorites only grow by an explicit click each.
///
/// The bound on the non-favorite block is enforced by [`push`], not on every
/// mutation: [`toggle_favorite`] deliberately lets the block sit over `capacity`
/// rather than evict, so the store can hold a few more entries than `capacity`
/// between one copy and the next. The footprint bound above is unaffected, since
/// the overshoot is only ever entries that were already being held as favorites.
///
/// [`favorite_count`]: Self::favorite_count
/// [`push`]: Self::push
/// [`toggle_favorite`]: Self::toggle_favorite
pub struct HistoryStore {
    items: VecDeque<Entry>,
    capacity: usize,
}

impl HistoryStore {
    pub fn new(capacity: usize) -> Self {
        Self {
            items: VecDeque::with_capacity(capacity),
            capacity: capacity.max(1),
        }
    }

    /// Index of the first non-favorite — i.e. where a non-favorite entry belongs
    /// if it is the most recent one. Relies on favorites forming a prefix.
    fn favorite_count(&self) -> usize {
        self.items.iter().take_while(|e| e.favorite).count()
    }

    /// Position of the entry whose contents equal `value`.
    ///
    /// Contents identify an entry throughout this module (see [`remove`]); at
    /// most one can match. The popup uses this to keep its highlight on the same
    /// entry after an operation has reordered the list.
    ///
    /// [`remove`]: Self::remove
    pub fn position(&self, value: &str) -> Option<usize> {
        self.items.iter().position(|e| e.text.as_ref() == value)
    }

    /// Drop the oldest non-favorite entries until the non-favorite block fits in
    /// `capacity`. Favorites sit ahead of every non-favorite, so the back of the
    /// deque is always the oldest non-favorite whenever there is one too many.
    fn trim_to_capacity(&mut self) {
        while self.items.len() - self.favorite_count() > self.capacity {
            self.items.pop_back();
        }
    }

    /// Record a newly copied value. Ignores empties and entries larger than
    /// [`MAX_ITEM_BYTES`]; de-duplicates by moving an existing identical entry
    /// to the front of its own block instead of adding a second copy — a
    /// re-copied favorite stays a favorite, and stays above the unstarred items.
    /// Returns `true` if the stored contents/order changed.
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
        if let Some(pos) = self.position(&value) {
            // Already present — promote to most-recent within its own block.
            let entry = self.items.remove(pos).expect("position is in bounds");
            let target = if entry.favorite {
                0
            } else {
                self.favorite_count()
            };
            self.items.insert(target, entry);
            return pos != target;
        }
        self.items
            .insert(self.favorite_count(), Entry::new(value, false));
        self.trim_to_capacity();
        true
    }

    /// Drop the entry whose contents equal `value`, returning `true` if one was
    /// removed.
    ///
    /// Matching on contents rather than on an index is deliberate. The popup
    /// acts on a snapshot it rendered, and the watcher thread prepends on every
    /// clipboard change, so an index from that snapshot can address a different
    /// entry by the time the click is handled — deleting the neighbour of the
    /// row the user aimed at. Contents are unique here (`push` de-duplicates),
    /// so at most one entry can match.
    pub fn remove(&mut self, value: &str) -> bool {
        let Some(pos) = self.position(value) else {
            return false;
        };
        self.items.remove(pos);
        true
    }

    /// Star or unstar the entry whose contents equal `value`, moving it into its
    /// new block: a starred entry goes to the top of the list, an unstarred one
    /// to the top of the unstarred block. Returns `true` if an entry matched.
    ///
    /// Landing at the top of the block, rather than back where the entry sat
    /// before, is the only ordering available: entries carry no timestamp, so
    /// there is nothing to restore an unstarred item to its "real" age with. It
    /// is also the more useful of the two — the user just reached for that row,
    /// so it is the one they are most likely to want next.
    ///
    /// Unstarring can push the unstarred block past `capacity`, and nothing is
    /// evicted here to bring it back down — the overshoot is left for the next
    /// [`push`] to trim. Trimming on the spot would make a star/unstar round trip
    /// destroy a *third* entry the user never touched: starring frees a slot in
    /// the unstarred block, an ordinary copy fills it, and unstarring would then
    /// evict whatever had aged to the back. Nothing in the popup suggests that a
    /// toggle of a star deletes a row further down, so it must not.
    ///
    /// Letting the block run over is safe: the entries are still contiguous and
    /// still behind the favorites, so every other method's invariant holds, and
    /// `push` trims with a `while` loop that clears an overshoot of any size.
    ///
    /// [`push`]: Self::push
    pub fn toggle_favorite(&mut self, value: &str) -> bool {
        let Some(pos) = self.position(value) else {
            return false;
        };
        let mut entry = self.items.remove(pos).expect("position is in bounds");
        entry.favorite = !entry.favorite;
        let target = if entry.favorite {
            0
        } else {
            self.favorite_count()
        };
        self.items.insert(target, entry);
        true
    }

    #[allow(dead_code)] // test-only helper; the UI reads items through `iter`
    pub fn get(&self, index: usize) -> Option<&Entry> {
        self.items.get(index)
    }

    /// The UI otherwise reads the length off its snapshot; this is for the one
    /// caller that has to ask the store itself, because it is deciding whether
    /// an index taken from an older snapshot is still in range.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[allow(dead_code)] // test-only helper; the UI reads the length off its snapshot
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The items in display order (favorites first, newest first within each).
    /// Cloning what this yields is cheap.
    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.items.iter()
    }

    /// A copy of the items in the same order, for persistence. Like [`iter`],
    /// this shares the entry texts rather than copying them.
    ///
    /// [`iter`]: Self::iter
    pub fn snapshot(&self) -> Vec<Entry> {
        self.items.iter().cloned().collect()
    }

    /// Replace the contents with a previously saved list, re-establishing the
    /// favorites-first order and truncating the unstarred block to the current
    /// capacity.
    ///
    /// The order is rebuilt rather than trusted because a `history.json` may
    /// have been hand-edited, or written by a build whose capacity or ordering
    /// rules differed; every other method assumes the invariant holds from the
    /// first frame on. Favorites are kept in full, matching the exemption
    /// [`HistoryStore`] documents.
    ///
    /// Entries over [`MAX_ITEM_BYTES`] are dropped here too, so a `history.json`
    /// written before the cap existed can't reintroduce them.
    ///
    /// Duplicates are dropped for the same reason, keeping the first
    /// occurrence: [`remove`] identifies an entry by its contents and documents
    /// that at most one can match, and a file that was hand-written rather than
    /// produced by [`snapshot`] is under no obligation to be unique. With
    /// duplicates loaded, deleting the second of two identical rows would take
    /// out the first and leave the row the user clicked sitting there.
    ///
    /// [`remove`]: Self::remove
    /// [`snapshot`]: Self::snapshot
    pub fn restore(&mut self, items: Vec<Entry>) {
        let mut seen = std::collections::HashSet::new();
        let (favorites, rest): (Vec<Entry>, Vec<Entry>) = items
            .into_iter()
            .filter(|e| e.text.len() <= MAX_ITEM_BYTES)
            .filter(|e| seen.insert(e.text.clone()))
            .partition(|e| e.favorite);
        self.items = favorites
            .into_iter()
            .chain(rest.into_iter().take(self.capacity))
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

    /// The stored texts in order — what a favorites assertion is usually about.
    fn texts(h: &HistoryStore) -> Vec<&str> {
        h.iter().map(|e| e.text.as_ref()).collect()
    }

    /// The stored favorite flags in order, to pair with [`texts`].
    fn flags(h: &HistoryStore) -> Vec<bool> {
        h.iter().map(|e| e.favorite).collect()
    }

    /// A saved list with nothing starred — what a pre-favorites `history.json`
    /// deserializes to.
    fn plain(items: &[&str]) -> Vec<Entry> {
        items.iter().map(|s| Entry::new(*s, false)).collect()
    }

    #[test]
    fn dedup_promotes_to_front() {
        let mut h = HistoryStore::new(5);
        h.push("a".into());
        h.push("b".into());
        h.push("a".into());
        assert_eq!(h.len(), 2);
        assert_eq!(h.get(0).unwrap().text.as_ref(), "a");
        assert_eq!(h.get(1).unwrap().text.as_ref(), "b");
    }

    #[test]
    fn respects_capacity() {
        let mut h = HistoryStore::new(2);
        h.push("a".into());
        h.push("b".into());
        h.push("c".into());
        assert_eq!(h.len(), 2);
        assert_eq!(h.get(0).unwrap().text.as_ref(), "c");
        assert_eq!(h.get(1).unwrap().text.as_ref(), "b");
        assert!(h.get(2).is_none());
    }

    #[test]
    fn snapshotting_shares_rather_than_copies() {
        // The UI clones this iterator's output on every rendered frame, so it
        // must hand out handles to the stored entries, not fresh copies.
        let mut h = HistoryStore::new(5);
        h.push("a".into());
        let cloned: Arc<str> = h.iter().next().unwrap().text.clone();
        assert!(Arc::ptr_eq(&cloned, &h.get(0).unwrap().text));
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
        assert_eq!(h.get(0).unwrap().text.len(), MAX_ITEM_BYTES);
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
        assert_eq!(h.get(0).unwrap().text.as_ref(), "b");
        assert_eq!(h.get(1).unwrap().text.as_ref(), "a");
    }

    #[test]
    fn remove_drops_only_the_named_entry() {
        let mut h = HistoryStore::new(5);
        h.push("a".into());
        h.push("b".into());
        h.push("c".into());
        assert!(h.remove("b"));
        assert_eq!(h.len(), 2);
        assert_eq!(h.get(0).unwrap().text.as_ref(), "c");
        assert_eq!(h.get(1).unwrap().text.as_ref(), "a");
    }

    #[test]
    fn remove_reports_a_miss() {
        // The popup can ask for an entry the watcher or a "Clear" already took
        // out; that must be a no-op rather than disturbing the rest.
        let mut h = HistoryStore::new(5);
        h.push("a".into());
        assert!(!h.remove("gone"));
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn removed_entries_can_be_recorded_again() {
        let mut h = HistoryStore::new(5);
        h.push("a".into());
        assert!(h.remove("a"));
        assert!(h.is_empty());
        assert!(h.push("a".into()));
        assert_eq!(h.get(0).unwrap().text.as_ref(), "a");
    }

    #[test]
    fn restore_truncates_to_capacity() {
        // A saved history can be longer than the configured history_size (the
        // user shrank it between runs), and restore documents that it keeps the
        // newest items — which are at the front.
        let mut h = HistoryStore::new(3);
        h.restore(plain(&[
            "newest",
            "second",
            "third",
            "dropped",
            "also dropped",
        ]));
        assert_eq!(texts(&h), ["newest", "second", "third"]);
    }

    #[test]
    fn restore_replaces_rather_than_appends() {
        let mut h = HistoryStore::new(5);
        h.push("stale".into());
        h.restore(plain(&["from disk"]));
        assert_eq!(texts(&h), ["from disk"]);
    }

    #[test]
    fn restore_drops_duplicates() {
        // `remove` matches on contents and relies on them being unique. A
        // hand-written history.json isn't obliged to be, so the de-duplication
        // has to happen on the way in — keeping the newest occurrence.
        let mut h = HistoryStore::new(5);
        h.restore(plain(&["dup", "unique", "dup"]));
        assert_eq!(texts(&h), ["dup", "unique"]);
        assert!(h.remove("dup"));
        assert!(!h.remove("dup"), "no second copy may be left behind");
    }

    #[test]
    fn restore_drops_oversized_entries() {
        // A history.json written before the cap existed must not reintroduce
        // an entry `push` would now refuse.
        let mut h = HistoryStore::new(5);
        h.restore(vec![
            Entry::new("small", false),
            Entry::new("x".repeat(MAX_ITEM_BYTES + 1), false),
            Entry::new("also small", false),
        ]);
        assert_eq!(texts(&h), ["small", "also small"]);
    }

    // --- Favorites ---------------------------------------------------------

    #[test]
    fn favoriting_pins_an_item_to_the_top() {
        let mut h = HistoryStore::new(5);
        h.push("a".into());
        h.push("b".into());
        h.push("c".into());
        assert!(h.toggle_favorite("a"));
        assert_eq!(texts(&h), ["a", "c", "b"]);
        assert_eq!(flags(&h), [true, false, false]);
    }

    #[test]
    fn new_copies_go_below_the_favorites() {
        // The whole point of the star: a fresh copy is newest, but it must not
        // displace the items the user pinned.
        let mut h = HistoryStore::new(5);
        h.push("pinned".into());
        assert!(h.toggle_favorite("pinned"));
        h.push("fresh".into());
        assert_eq!(texts(&h), ["pinned", "fresh"]);
    }

    #[test]
    fn favorites_keep_their_own_recency_order() {
        let mut h = HistoryStore::new(5);
        h.push("a".into());
        h.push("b".into());
        h.toggle_favorite("a");
        h.toggle_favorite("b");
        // "b" was starred last, so it goes above "a" — same newest-first rule as
        // the unstarred block, applied within the favorites.
        assert_eq!(texts(&h), ["b", "a"]);
    }

    #[test]
    fn unfavoriting_drops_the_item_below_the_remaining_favorites() {
        let mut h = HistoryStore::new(5);
        h.push("old".into());
        h.push("keep".into());
        h.push("demote".into());
        h.toggle_favorite("keep");
        h.toggle_favorite("demote");
        assert_eq!(texts(&h), ["demote", "keep", "old"]);

        assert!(h.toggle_favorite("demote"));
        assert_eq!(texts(&h), ["keep", "demote", "old"]);
        assert_eq!(flags(&h), [true, false, false]);
    }

    #[test]
    fn toggle_reports_a_miss() {
        // Same race as `remove`: the popup can star a row the watcher or a
        // "Clear" already took out.
        let mut h = HistoryStore::new(5);
        h.push("a".into());
        assert!(!h.toggle_favorite("gone"));
        assert_eq!(texts(&h), ["a"]);
    }

    #[test]
    fn recopying_a_favorite_keeps_it_starred() {
        // The watcher pushes whatever is copied; re-copying a pinned entry must
        // not quietly unpin it or drop it below the other favorites.
        let mut h = HistoryStore::new(5);
        h.push("pinned".into());
        h.push("other".into());
        h.toggle_favorite("pinned");
        h.toggle_favorite("other");
        assert_eq!(texts(&h), ["other", "pinned"]);

        assert!(h.push("pinned".into()));
        assert_eq!(texts(&h), ["pinned", "other"]);
        assert_eq!(flags(&h), [true, true]);
    }

    #[test]
    fn recopying_the_newest_unstarred_item_is_a_no_op() {
        // It is already at the top of its block, so nothing changed and the
        // watcher must not be told to save.
        let mut h = HistoryStore::new(5);
        h.push("pinned".into());
        h.toggle_favorite("pinned");
        h.push("newest".into());
        h.push("older".into());
        assert!(h.push("newest".into()));
        assert!(!h.push("newest".into()));
        assert_eq!(texts(&h), ["pinned", "newest", "older"]);
    }

    #[test]
    fn favorites_do_not_count_against_capacity() {
        // Starring an item is a promise it stays; a full history of ordinary
        // copies must not evict it.
        let mut h = HistoryStore::new(2);
        h.push("pinned".into());
        h.toggle_favorite("pinned");
        h.push("a".into());
        h.push("b".into());
        h.push("c".into());
        assert_eq!(texts(&h), ["pinned", "c", "b"]);
    }

    #[test]
    fn a_star_unstar_round_trip_keeps_every_other_entry() {
        // Starring frees a slot in the unstarred block, which the next ordinary
        // copy fills; the block is therefore over capacity the moment the entry
        // rejoins it. Evicting there would delete "oldest", which the user never
        // touched — they only clicked the same star twice.
        let mut h = HistoryStore::new(2);
        h.push("demote".into());
        h.toggle_favorite("demote");
        h.push("oldest".into());
        h.push("newer".into());
        assert_eq!(texts(&h), ["demote", "newer", "oldest"]);

        h.toggle_favorite("demote");
        assert_eq!(texts(&h), ["demote", "newer", "oldest"]);
        assert_eq!(flags(&h), [false, false, false]);
    }

    #[test]
    fn the_next_copy_trims_an_over_capacity_unstarred_block() {
        // The overshoot an unstar leaves is transient, and `push` clears it in
        // one go however far over the block has run.
        let mut h = HistoryStore::new(2);
        h.push("a".into());
        h.push("b".into());
        h.toggle_favorite("a");
        h.toggle_favorite("b");
        h.push("c".into());
        h.toggle_favorite("a");
        h.toggle_favorite("b");
        assert_eq!(texts(&h), ["b", "a", "c"], "two unstars, no eviction yet");

        h.push("d".into());
        assert_eq!(texts(&h), ["d", "b"]);
    }

    #[test]
    fn favorites_survive_a_restore_and_come_back_first() {
        let mut h = HistoryStore::new(5);
        h.restore(vec![
            Entry::new("plain", false),
            Entry::new("starred", true),
            Entry::new("also plain", false),
        ]);
        assert_eq!(texts(&h), ["starred", "plain", "also plain"]);
        assert_eq!(flags(&h), [true, false, false]);
    }

    #[test]
    fn restore_keeps_every_favorite_but_caps_the_rest() {
        // Favorites are exempt from `capacity`, so a saved history with more of
        // them than the configured size still comes back whole.
        let mut h = HistoryStore::new(1);
        h.restore(vec![
            Entry::new("fav 1", true),
            Entry::new("plain 1", false),
            Entry::new("fav 2", true),
            Entry::new("plain 2", false),
        ]);
        assert_eq!(texts(&h), ["fav 1", "fav 2", "plain 1"]);
    }

    #[test]
    fn position_finds_an_entry_by_contents() {
        // The popup uses this to keep its highlight on the same entry after a
        // toggle has reordered the list.
        let mut h = HistoryStore::new(5);
        h.push("a".into());
        h.push("b".into());
        h.toggle_favorite("a");
        assert_eq!(h.position("a"), Some(0));
        assert_eq!(h.position("b"), Some(1));
        assert_eq!(h.position("gone"), None);
    }
}
