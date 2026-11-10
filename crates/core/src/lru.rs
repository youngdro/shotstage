//! A small least-recently-used cache with a size budget, for the per-thread caches of built
//! geometry. Unlike "clear when full", a scene with more parts than the old entry limit keeps
//! hitting the cache (only what has not been drawn for a while is dropped), and a few huge
//! meshes cannot pile up past the memory budget.

use std::collections::HashMap;
use std::rc::Rc;

pub struct Lru<V> {
    map: HashMap<u64, (Rc<V>, usize, u64)>,
    /// Sum of the entries' sizes.
    used: usize,
    budget: usize,
    clock: u64,
}

impl<V> Lru<V> {
    /// `budget` is in the same unit `insert` is given sizes in (usually bytes).
    pub fn new(budget: usize) -> Self {
        Self { map: HashMap::new(), used: 0, budget, clock: 0 }
    }

    pub fn get(&mut self, key: u64) -> Option<Rc<V>> {
        self.clock += 1;
        let clock = self.clock;
        self.map.get_mut(&key).map(|e| {
            e.2 = clock;
            e.0.clone()
        })
    }

    pub fn insert(&mut self, key: u64, value: Rc<V>, size: usize) {
        self.clock += 1;
        if let Some((_, old, _)) = self.map.insert(key, (value, size, self.clock)) {
            self.used -= old;
        }
        self.used += size;
        if self.used > self.budget {
            self.evict();
        }
    }

    /// Drop the least recently used entries until a quarter of the budget is free (so this
    /// sort runs now and then, not on every insert). The newest entry always stays.
    fn evict(&mut self) {
        let target = self.budget / 4 * 3;
        let mut by_age: Vec<(u64, u64, usize)> = self.map.iter().map(|(k, (_, s, t))| (*t, *k, *s)).collect();
        by_age.sort_unstable();
        let newest = by_age.last().map(|e| e.1);
        for (_, k, s) in by_age {
            if self.used <= target {
                break;
            }
            if Some(k) == newest {
                continue;
            }
            self.map.remove(&k);
            self.used -= s;
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
    /// Sum of the entries' sizes.
    pub fn used(&self) -> usize {
        self.used
    }
    pub fn clear(&mut self) {
        self.map.clear();
        self.used = 0;
    }
}

/// Memory for one geometry cache: less in the browser, where the whole tab has a few GB.
pub const fn cache_budget(native_mb: usize) -> usize {
    if cfg!(target_arch = "wasm32") { native_mb / 2 * 1024 * 1024 } else { native_mb * 1024 * 1024 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_what_was_used_recently_within_budget() {
        let mut c = Lru::new(100);
        for k in 0..10 {
            c.insert(k, Rc::new(k), 10);
        }
        assert_eq!(c.len(), 10);
        assert!(c.get(0).is_some()); // 0 is now the most recently used
        c.insert(10, Rc::new(10), 10); // over budget: drop the oldest down to 75
        assert!(c.used() <= 75);
        assert!(c.get(0).is_some(), "recently used entries stay");
        assert!(c.get(10).is_some(), "the new entry stays");
        assert!(c.get(1).is_none(), "the oldest went");
    }

    #[test]
    fn a_huge_entry_still_fits_alone() {
        let mut c = Lru::new(10);
        c.insert(1, Rc::new(1), 5);
        c.insert(2, Rc::new(2), 50);
        assert!(c.get(2).is_some());
        assert!(c.get(1).is_none());
    }
}
