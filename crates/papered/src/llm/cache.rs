//! Simple fixed-capacity in-memory cache for LLM-based features.
//!
//! Used by the unified query enhancement layer to avoid repeated identical
//! calls to LLM providers.

use std::borrow::Borrow;
use std::collections::{HashMap, VecDeque};

/// A fixed-capacity FIFO cache.
///
/// When the cache reaches `capacity`, the oldest inserted key is evicted to
/// make room for the new entry. Lookups are O(1). This is intentionally
/// simple: it does not need external dependencies and is scoped to the
/// lifetime of the owning component (e.g. a `RagEngine` rebuild on config
/// reload naturally clears the cache).
pub struct BoundedCache<K, V> {
    capacity: usize,
    map: HashMap<K, V>,
    order: VecDeque<K>,
}

impl<K, V> BoundedCache<K, V>
where
    K: Eq + std::hash::Hash + Clone,
{
    /// Create a new cache with the given maximum number of entries.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            map: HashMap::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
        }
    }

    /// Look up a value by key.
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.map.get(key)
    }

    /// Insert a key/value pair. If the cache is full, evict the oldest entry.
    pub fn put(&mut self, key: K, value: V) {
        use std::collections::hash_map::Entry;
        if let Entry::Occupied(mut entry) = self.map.entry(key.clone()) {
            entry.insert(value);
            return;
        }

        if self.order.len() >= self.capacity
            && let Some(oldest) = self.order.pop_front()
        {
            self.map.remove(&oldest);
        }

        self.order.push_back(key.clone());
        self.map.insert(key, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_hit_and_miss() {
        let mut cache: BoundedCache<String, i32> = BoundedCache::new(4);
        assert!(cache.get("a").is_none());

        cache.put("a".to_string(), 1);
        assert_eq!(cache.get("a"), Some(&1));
        assert!(cache.get("b").is_none());

        cache.put("a".to_string(), 2);
        assert_eq!(cache.get("a"), Some(&2));
    }

    #[test]
    fn cache_evicts_oldest_when_full() {
        let mut cache: BoundedCache<String, i32> = BoundedCache::new(3);
        cache.put("a".to_string(), 1);
        cache.put("b".to_string(), 2);
        cache.put("c".to_string(), 3);

        // Fill the cache.
        assert_eq!(cache.get("a"), Some(&1));
        assert_eq!(cache.get("b"), Some(&2));
        assert_eq!(cache.get("c"), Some(&3));

        // Insert a fourth entry; the oldest (`a`) should be evicted.
        cache.put("d".to_string(), 4);
        assert!(cache.get("a").is_none());
        assert_eq!(cache.get("b"), Some(&2));
        assert_eq!(cache.get("c"), Some(&3));
        assert_eq!(cache.get("d"), Some(&4));
    }
}
