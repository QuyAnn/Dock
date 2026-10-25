//! Byte-bounded LRU storage for rebuildable rendering assets.
use std::collections::HashMap;
use std::hash::Hash;

struct Entry<V> { value: V, bytes: usize, used: u64 }

pub struct ByteCache<K, V> {
    entries: HashMap<K, Entry<V>>,
    bytes: usize,
    budget: usize,
    clock: u64,
}

impl<K: Eq + Hash + Clone, V> ByteCache<K, V> {
    pub fn new(budget: usize) -> Self {
        Self { entries: HashMap::new(), bytes: 0, budget, clock: 0 }
    }

    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        let entry = self.entries.get_mut(key)?;
        self.clock += 1;
        entry.used = self.clock;
        Some(&mut entry.value)
    }

    pub fn insert(&mut self, key: K, value: V, bytes: usize) {
        if let Some(old) = self.entries.remove(&key) { self.bytes -= old.bytes; }
        // Oversized images are drawn by the caller, but never retained in the cache.
        if bytes > self.budget { return; }
        while self.bytes > self.budget - bytes {
            let Some(oldest) = self.entries.iter().min_by_key(|(_, e)| e.used).map(|(k, _)| k.clone()) else { break };
            if let Some(old) = self.entries.remove(&oldest) { self.bytes -= old.bytes; }
        }
        self.clock += 1;
        self.bytes += bytes;
        self.entries.insert(key, Entry { value, bytes, used: self.clock });
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&K, &V) -> bool) {
        self.entries.retain(|k, e| keep(k, &e.value));
        self.bytes = self.entries.values().map(|e| e.bytes).sum();
    }

    pub fn clear(&mut self) {
        self.entries = HashMap::new();
        self.bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn evicts_by_bytes_and_recent_use_including_same_frame() {
        let mut c = ByteCache::new(10);
        c.insert(1, "a", 4); c.insert(2, "b", 4);
        assert_eq!(c.get_mut(&1), Some(&mut "a"));
        c.insert(3, "c", 5);
        assert!(c.get_mut(&2).is_none());
        assert!(c.get_mut(&1).is_some());
        assert_eq!(c.bytes, 9);
        c.insert(1, "replacement", 6);
        assert!(c.get_mut(&3).is_none());
        assert_eq!(c.bytes, 6);
        c.insert(4, "oversized", 11);
        assert!(c.get_mut(&4).is_none());
        c.retain(|_, _| false);
        assert_eq!(c.bytes, 0);
        c.insert(5, "new", 10); c.clear();
        assert_eq!(c.bytes, 0);
        assert_eq!(c.entries.capacity(), 0);
    }
}
