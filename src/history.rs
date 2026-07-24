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
    pub fn push(&mut self, value: String) {
        if value.is_empty() {
            return;
        }
        if let Some(pos) = self.items.iter().position(|v| v == &value) {
            // Already present — promote to most-recent.
            if pos == 0 {
                return;
            }
            self.items.remove(pos);
        }
        self.items.push_front(value);
        while self.items.len() > self.capacity {
            self.items.pop_back();
        }
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
