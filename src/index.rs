//! The order reference index: from an order reference number to wherever the
//! caller keeps that order.
//!
//! Nasdaq hands out order reference numbers in rising order through the day,
//! and most orders die young, so at any moment the live references sit in a
//! few dense ranges. The index uses that. A reference is split into a page
//! number and an offset; a page is a flat array of 4,096 entries, allocated
//! when its first reference arrives and released when its last one leaves. A
//! lookup is two array reads with no hashing, and new orders land next to
//! each other in memory.
//!
//! Nothing about correctness depends on the references being dense or
//! rising. References at or above [`DIRECT_LIMIT`] go to an ordinary hash
//! map, so any `u64` works; sparse references only cost memory.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

const PAGE_BITS: u32 = 12;
const PAGE_SIZE: usize = 1 << PAGE_BITS;

/// References below this use the page table. The table's directory grows to
/// one pointer per 4,096 references, so this caps it at 32 MiB.
pub const DIRECT_LIMIT: u64 = 1 << 34;

/// Released pages kept for reuse instead of going back to the allocator.
const SPARE_PAGES: usize = 64;

/// Hasher for order reference numbers: one multiply and one fold.
#[derive(Default, Clone, Copy)]
pub struct RefHasher(u64);

impl Hasher for RefHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    #[inline]
    fn write_u64(&mut self, v: u64) {
        let x = v.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        self.0 = x ^ (x >> 32);
    }
}

/// A hash map keyed by order reference number.
pub type RefMap<V> = HashMap<u64, V, BuildHasherDefault<RefHasher>>;

/// What a replay needs from an order reference index. Two types provide
/// it: [`RefIndex`], the page table every published number uses, and
/// [`HashIndex`], the hash map the first version used, kept so the benchmark
/// can time one against the other in the same run.
pub trait OrderIndex<V: Copy>: Default {
    /// The value stored for `key`.
    fn get(&self, key: u64) -> Option<V>;
    /// Store `value` for `key`. Returns the value it replaced, if any.
    fn insert(&mut self, key: u64, value: V) -> Option<V>;
    /// Forget `key`. Returns the value it had, if any.
    fn remove(&mut self, key: u64) -> Option<V>;
    /// Number of references held.
    fn len(&self) -> usize;
    /// True when no reference is held.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// True when `key` is held.
    #[inline]
    fn contains(&self, key: u64) -> bool {
        self.get(key).is_some()
    }
    /// Hint that `key` is about to be looked up. Changes nothing.
    #[inline]
    fn prefetch(&self, _key: u64) {}
}

/// The first version's index: a hash map with a one-multiply hasher. It
/// cannot say where a key will land before hashing it into the table, so its
/// prefetch hint does nothing.
pub struct HashIndex<V>(RefMap<V>);

impl<V> Default for HashIndex<V> {
    fn default() -> Self {
        HashIndex(RefMap::default())
    }
}

impl<V: Copy> OrderIndex<V> for HashIndex<V> {
    #[inline]
    fn get(&self, key: u64) -> Option<V> {
        self.0.get(&key).copied()
    }

    #[inline]
    fn insert(&mut self, key: u64, value: V) -> Option<V> {
        self.0.insert(key, value)
    }

    #[inline]
    fn remove(&mut self, key: u64) -> Option<V> {
        self.0.remove(&key)
    }

    #[inline]
    fn len(&self) -> usize {
        self.0.len()
    }
}

impl<V: Copy> OrderIndex<V> for RefIndex<V> {
    #[inline]
    fn get(&self, key: u64) -> Option<V> {
        RefIndex::get(self, key)
    }

    #[inline]
    fn insert(&mut self, key: u64, value: V) -> Option<V> {
        RefIndex::insert(self, key, value)
    }

    #[inline]
    fn remove(&mut self, key: u64) -> Option<V> {
        RefIndex::remove(self, key)
    }

    #[inline]
    fn len(&self) -> usize {
        RefIndex::len(self)
    }

    #[inline]
    fn prefetch(&self, key: u64) {
        RefIndex::prefetch(self, key)
    }
}

struct Page<V> {
    live: u32,
    slots: [Option<V>; PAGE_SIZE],
}

impl<V: Copy> Page<V> {
    fn new() -> Box<Self> {
        Box::new(Page {
            live: 0,
            slots: [None; PAGE_SIZE],
        })
    }
}

/// A map from order reference number to a small copyable value.
pub struct RefIndex<V> {
    pages: Vec<Option<Box<Page<V>>>>,
    spare: Vec<Box<Page<V>>>,
    spill: RefMap<V>,
    len: usize,
}

impl<V: Copy> Default for RefIndex<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: Copy> RefIndex<V> {
    /// An empty index.
    pub fn new() -> Self {
        RefIndex {
            pages: Vec::new(),
            spare: Vec::new(),
            spill: RefMap::default(),
            len: 0,
        }
    }

    /// Number of references held.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when no reference is held.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Pages currently allocated, for tests and memory accounting.
    pub fn pages_in_use(&self) -> usize {
        self.pages.iter().filter(|p| p.is_some()).count()
    }

    /// The value stored for `key`.
    #[inline]
    pub fn get(&self, key: u64) -> Option<V> {
        if key < DIRECT_LIMIT {
            let page = self.pages.get((key >> PAGE_BITS) as usize)?.as_ref()?;
            page.slots[(key as usize) & (PAGE_SIZE - 1)]
        } else {
            self.spill.get(&key).copied()
        }
    }

    /// Hint that `key` is about to be looked up. Changes nothing.
    #[inline]
    pub fn prefetch(&self, key: u64) {
        if key < DIRECT_LIMIT {
            if let Some(Some(page)) = self.pages.get((key >> PAGE_BITS) as usize) {
                crate::mem::prefetch(&page.slots[(key as usize) & (PAGE_SIZE - 1)]);
            }
        }
    }

    /// True when `key` is held.
    #[inline]
    pub fn contains(&self, key: u64) -> bool {
        self.get(key).is_some()
    }

    /// Store `value` for `key`. Returns the value it replaced, if any.
    #[inline]
    pub fn insert(&mut self, key: u64, value: V) -> Option<V> {
        let old = if key < DIRECT_LIMIT {
            let page_no = (key >> PAGE_BITS) as usize;
            if page_no >= self.pages.len() {
                self.pages.resize_with(page_no + 1, || None);
            }
            let spare = &mut self.spare;
            let page =
                self.pages[page_no].get_or_insert_with(|| spare.pop().unwrap_or_else(Page::new));
            let old = page.slots[(key as usize) & (PAGE_SIZE - 1)].replace(value);
            if old.is_none() {
                page.live += 1;
            }
            old
        } else {
            self.spill.insert(key, value)
        };
        if old.is_none() {
            self.len += 1;
        }
        old
    }

    /// Forget `key`. Returns the value it had, if any.
    #[inline]
    pub fn remove(&mut self, key: u64) -> Option<V> {
        let old = if key < DIRECT_LIMIT {
            let entry = self.pages.get_mut((key >> PAGE_BITS) as usize)?;
            let page = entry.as_mut()?;
            let old = page.slots[(key as usize) & (PAGE_SIZE - 1)].take()?;
            page.live -= 1;
            if page.live == 0 {
                // An empty page holds only `None`, so it can be handed out
                // again as it is.
                if let Some(page) = entry.take() {
                    if self.spare.len() < SPARE_PAGES {
                        self.spare.push(page);
                    }
                }
            }
            Some(old)
        } else {
            self.spill.remove(&key)
        };
        if old.is_some() {
            self.len -= 1;
        }
        old
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_get_remove_on_both_paths() {
        let mut ix = RefIndex::new();
        let keys = [
            0u64,
            1,
            4095,
            4096,
            123_456_789,
            DIRECT_LIMIT - 1,
            DIRECT_LIMIT,
            u64::MAX,
        ];
        for (i, &k) in keys.iter().enumerate() {
            assert_eq!(ix.get(k), None);
            assert_eq!(ix.insert(k, i as u32), None);
            assert_eq!(ix.get(k), Some(i as u32));
        }
        assert_eq!(ix.len(), keys.len());
        // Overwriting returns the old value and does not change the count.
        assert_eq!(ix.insert(4096, 99), Some(3));
        assert_eq!(ix.len(), keys.len());
        assert_eq!(ix.remove(4096), Some(99));
        assert_eq!(ix.remove(4096), None);
        assert!(!ix.contains(4096));
        for &k in &keys {
            ix.remove(k);
        }
        assert!(ix.is_empty());
        assert_eq!(ix.pages_in_use(), 0);
    }

    #[test]
    fn empty_pages_are_released_and_reused() {
        let mut ix = RefIndex::new();
        for k in 0..10_000u64 {
            ix.insert(k, k);
        }
        assert_eq!(ix.pages_in_use(), 3);
        for k in 0..8_192u64 {
            assert_eq!(ix.remove(k), Some(k));
        }
        assert_eq!(ix.pages_in_use(), 1);
        // A reused page starts empty.
        ix.insert(1_000_000, 7);
        assert_eq!(ix.get(1_000_001), None);
        assert_eq!(ix.get(1_000_000), Some(7));
        assert_eq!(ix.len(), 10_000 - 8_192 + 1);
    }

    #[test]
    fn lookups_far_past_the_directory_are_misses() {
        let mut ix: RefIndex<u8> = RefIndex::new();
        ix.insert(5, 1);
        assert_eq!(ix.get(DIRECT_LIMIT - 1), None);
        assert_eq!(ix.remove(DIRECT_LIMIT - 1), None);
        assert_eq!(ix.len(), 1);
    }
}
