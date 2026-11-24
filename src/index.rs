//! The order reference index: from an order reference number to wherever the
//! caller keeps that order.
//!
//! Three types do the job, behind [`OrderIndex`].
//!
//! - [`OpenIndex`] is the one every published number uses: an open-addressing
//!   hash table with linear probing. Its entries sit in one flat array, so the
//!   slot a key will land in is known from the key alone and can be
//!   prefetched before the lookup.
//! - [`HashIndex`] is the standard library's hash map, which the first
//!   version used. The benchmark still times it.
//! - [`PageIndex`] is a page table indexed directly by the reference number.
//!   It was tried because Nasdaq hands out reference numbers in rising order,
//!   and it lost: long-lived orders are scattered through the number range
//!   and each one pins a 64 KiB page, so on the sample day the table grows to
//!   several gigabytes to hold under two million orders. It is kept so the
//!   benchmark can show that.

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

/// What a replay needs from an order reference index.
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
    /// Bytes of memory the index holds right now, to the nearest entry.
    fn bytes(&self) -> usize;
}

/// The first version's index: the standard hash map with a one-multiply
/// hasher. It
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

    /// An estimate: the map's capacity times one key, one value and one
    /// control byte. The standard library does not report its real size.
    fn bytes(&self) -> usize {
        self.0.capacity() * (std::mem::size_of::<(u64, V)>() + 1)
    }
}

impl<V: Copy> OrderIndex<V> for PageIndex<V> {
    #[inline]
    fn get(&self, key: u64) -> Option<V> {
        PageIndex::get(self, key)
    }

    #[inline]
    fn insert(&mut self, key: u64, value: V) -> Option<V> {
        PageIndex::insert(self, key, value)
    }

    #[inline]
    fn remove(&mut self, key: u64) -> Option<V> {
        PageIndex::remove(self, key)
    }

    #[inline]
    fn len(&self) -> usize {
        PageIndex::len(self)
    }

    #[inline]
    fn prefetch(&self, key: u64) {
        PageIndex::prefetch(self, key)
    }

    fn bytes(&self) -> usize {
        self.pages_in_use() * std::mem::size_of::<Page<V>>()
            + self.pages.len() * std::mem::size_of::<Option<Box<Page<V>>>>()
            + self.spill.capacity() * (std::mem::size_of::<(u64, V)>() + 1)
    }
}

#[derive(Clone, Copy)]
struct Slot<V> {
    key: u64,
    /// `None` marks an empty slot.
    value: Option<V>,
}

/// An open-addressing hash table from order reference number to a small
/// copyable value: one flat array, linear probing, and deletion by shifting
/// later entries back so that no tombstones are left behind.
///
/// The table doubles when it is half full and never shrinks.
pub struct OpenIndex<V> {
    slots: Vec<Slot<V>>,
    /// `64 - log2(slots.len())`: how far to shift a hash to get a slot.
    shift: u32,
    len: usize,
}

impl<V: Copy> Default for OpenIndex<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: Copy> OpenIndex<V> {
    const MIN_BITS: u32 = 4;

    /// An empty index.
    pub fn new() -> Self {
        OpenIndex {
            slots: vec![
                Slot {
                    key: 0,
                    value: None
                };
                1 << Self::MIN_BITS
            ],
            shift: 64 - Self::MIN_BITS,
            len: 0,
        }
    }

    /// The slot a key is looked for first.
    #[inline]
    fn home(&self, key: u64) -> usize {
        (key.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> self.shift) as usize
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

    /// Slots allocated, for tests and memory accounting.
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// Hint that `key` is about to be looked up. Changes nothing.
    #[inline]
    pub fn prefetch(&self, key: u64) {
        crate::mem::prefetch(&self.slots[self.home(key)]);
    }

    /// The value stored for `key`.
    #[inline]
    pub fn get(&self, key: u64) -> Option<V> {
        let mask = self.slots.len() - 1;
        let mut i = self.home(key);
        loop {
            let slot = &self.slots[i];
            match slot.value {
                None => return None,
                Some(v) if slot.key == key => return Some(v),
                Some(_) => i = (i + 1) & mask,
            }
        }
    }

    /// True when `key` is held.
    #[inline]
    pub fn contains(&self, key: u64) -> bool {
        self.get(key).is_some()
    }

    #[cold]
    fn grow(&mut self) {
        let bits = 64 - self.shift + 1;
        let old = std::mem::replace(
            &mut self.slots,
            vec![
                Slot {
                    key: 0,
                    value: None
                };
                1usize << bits
            ],
        );
        self.shift = 64 - bits;
        let mask = self.slots.len() - 1;
        for slot in old {
            if slot.value.is_some() {
                let mut i = self.home(slot.key);
                while self.slots[i].value.is_some() {
                    i = (i + 1) & mask;
                }
                self.slots[i] = slot;
            }
        }
    }

    /// Store `value` for `key`. Returns the value it replaced, if any.
    #[inline]
    pub fn insert(&mut self, key: u64, value: V) -> Option<V> {
        if (self.len + 1) * 2 > self.slots.len() {
            self.grow();
        }
        let mask = self.slots.len() - 1;
        let mut i = self.home(key);
        loop {
            let slot = &mut self.slots[i];
            match slot.value {
                None => {
                    *slot = Slot {
                        key,
                        value: Some(value),
                    };
                    self.len += 1;
                    return None;
                }
                Some(old) if slot.key == key => {
                    slot.value = Some(value);
                    return Some(old);
                }
                Some(_) => i = (i + 1) & mask,
            }
        }
    }

    /// Forget `key`. Returns the value it had, if any.
    #[inline]
    pub fn remove(&mut self, key: u64) -> Option<V> {
        let mask = self.slots.len() - 1;
        let mut i = self.home(key);
        let old = loop {
            let slot = &self.slots[i];
            match slot.value {
                None => return None,
                Some(v) if slot.key == key => break v,
                Some(_) => i = (i + 1) & mask,
            }
        };
        // Close the gap: walk the run of entries after the hole and move
        // back each one that the hole stands between it and its home slot.
        let mut hole = i;
        let mut j = i;
        loop {
            j = (j + 1) & mask;
            let slot = self.slots[j];
            if slot.value.is_none() {
                break;
            }
            let from_home = j.wrapping_sub(self.home(slot.key)) & mask;
            let from_hole = j.wrapping_sub(hole) & mask;
            if from_home >= from_hole {
                self.slots[hole] = slot;
                hole = j;
            }
        }
        self.slots[hole].value = None;
        self.len -= 1;
        Some(old)
    }
}

impl<V: Copy> OrderIndex<V> for OpenIndex<V> {
    #[inline]
    fn get(&self, key: u64) -> Option<V> {
        OpenIndex::get(self, key)
    }

    #[inline]
    fn insert(&mut self, key: u64, value: V) -> Option<V> {
        OpenIndex::insert(self, key, value)
    }

    #[inline]
    fn remove(&mut self, key: u64) -> Option<V> {
        OpenIndex::remove(self, key)
    }

    #[inline]
    fn len(&self) -> usize {
        OpenIndex::len(self)
    }

    #[inline]
    fn prefetch(&self, key: u64) {
        OpenIndex::prefetch(self, key)
    }

    fn bytes(&self) -> usize {
        self.slots.len() * std::mem::size_of::<Slot<V>>()
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

/// A page table from order reference number to a small copyable value.
///
/// A reference is split into a page number and an offset; a page is a flat
/// array of 4,096 entries, allocated when its first reference arrives and
/// released when its last one leaves. References at or above
/// [`DIRECT_LIMIT`] go to a hash map, so any `u64` works.
pub struct PageIndex<V> {
    pages: Vec<Option<Box<Page<V>>>>,
    spare: Vec<Box<Page<V>>>,
    spill: RefMap<V>,
    len: usize,
}

impl<V: Copy> Default for PageIndex<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: Copy> PageIndex<V> {
    /// An empty index.
    pub fn new() -> Self {
        PageIndex {
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
        let mut ix = PageIndex::new();
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
    fn open_index_inserts_grows_and_closes_gaps() {
        let mut ix = OpenIndex::new();
        // Enough keys to grow several times, in an order that collides.
        for k in 0..5_000u64 {
            assert_eq!(ix.insert(k * 7, k), None);
        }
        assert_eq!(ix.len(), 5_000);
        assert!(ix.capacity() >= 10_000);
        assert_eq!(
            OrderIndex::bytes(&ix),
            ix.capacity() * std::mem::size_of::<Slot<u64>>()
        );
        assert_eq!(ix.insert(14, 99), Some(2));
        // Remove every other key; the rest must still be found.
        for k in (0..5_000u64).step_by(2) {
            assert!(ix.remove(k * 7).is_some());
            assert_eq!(ix.remove(k * 7), None);
        }
        for k in 0..5_000u64 {
            let want = if k % 2 == 1 { Some(k) } else { None };
            assert_eq!(ix.get(k * 7), want, "key {}", k * 7);
        }
        ix.prefetch(u64::MAX);
        assert_eq!(ix.insert(u64::MAX, 1), None);
        assert_eq!(ix.insert(0, 2), None);
        assert_eq!(ix.remove(u64::MAX), Some(1));
        assert_eq!(ix.get(0), Some(2));
        assert_eq!(ix.len(), 2_501);
    }

    #[test]
    fn empty_pages_are_released_and_reused() {
        let mut ix = PageIndex::new();
        for k in 0..10_000u64 {
            ix.insert(k, k);
        }
        assert_eq!(ix.pages_in_use(), 3);
        assert!(OrderIndex::bytes(&ix) >= 3 * 4096 * 16);
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
        let mut ix: PageIndex<u8> = PageIndex::new();
        ix.insert(5, 1);
        assert_eq!(ix.get(DIRECT_LIMIT - 1), None);
        assert_eq!(ix.remove(DIRECT_LIMIT - 1), None);
        assert_eq!(ix.len(), 1);
    }
}
