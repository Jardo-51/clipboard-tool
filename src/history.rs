//! Clipboard history storage: a fixed-capacity, de-duplicated ring buffer.

use std::collections::VecDeque;

/// Newest item is at the front (index 0).
pub struct HistoryStore {
    items: VecDeque<String>,
    capacity: usize,
}

impl HistoryStore {
    pub fn new(capacity: usize) -> Self {
        Self {
            items: VecDeque::with_capacity(capacity),
            capacity: capacity.max(1),
        }
    }

    /// Record a newly copied value. Ignores empties; de-duplicates by moving an
    /// existing identical entry to the front instead of adding a second copy.
    /// Returns `true` if the stored contents/order changed.
    pub fn push(&mut self, value: String) -> bool {
        if value.is_empty() {
            return false;
        }
        if let Some(pos) = self.items.iter().position(|v| v == &value) {
            // Already present — promote to most-recent (no-op if already first).
            if pos == 0 {
                return false;
            }
            self.items.remove(pos);
        }
        self.items.push_front(value);
        while self.items.len() > self.capacity {
            self.items.pop_back();
        }
        true
    }

    pub fn get(&self, index: usize) -> Option<&String> {
        self.items.get(index)
    }

    #[allow(dead_code)] // used in tests; wired into the UI/tray in later phases
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[allow(dead_code)] // used in tests; wired into the UI/tray in later phases
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &String> {
        self.items.iter()
    }

    /// Newest-first copy of the items, for persistence.
    pub fn snapshot(&self) -> Vec<String> {
        self.items.iter().cloned().collect()
    }

    /// Replace the contents with a previously saved (newest-first) list,
    /// truncated to the current capacity.
    pub fn restore(&mut self, items: Vec<String>) {
        self.items = items.into_iter().take(self.capacity).collect();
    }

    #[allow(dead_code)] // used by the tray "clear history" action in Phase 6
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
        assert_eq!(h.get(0).unwrap(), "a");
        assert_eq!(h.get(1).unwrap(), "b");
    }

    #[test]
    fn respects_capacity() {
        let mut h = HistoryStore::new(2);
        h.push("a".into());
        h.push("b".into());
        h.push("c".into());
        assert_eq!(h.len(), 2);
        assert_eq!(h.get(0).unwrap(), "c");
        assert_eq!(h.get(1).unwrap(), "b");
        assert!(h.get(2).is_none());
    }

    #[test]
    fn ignores_empty() {
        let mut h = HistoryStore::new(5);
        h.push(String::new());
        assert!(h.is_empty());
    }
}
