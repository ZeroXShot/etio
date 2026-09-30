//! A bounded, thread-safe string interner.
//!
//! Telemetry repeats the same small set of strings (service names, operation
//! names, metric names) millions of times. Interning them into 32-bit symbols
//! makes aggregation keys cheap to hash and compare.
//!
//! The interner is *bounded*: untrusted input (for example spans whose names
//! embed request IDs) must not be able to grow memory without limit. Once the
//! capacity is reached, unseen strings resolve to [`Sym::OVERFLOW`] and the
//! event is counted, so operators can see that data was folded.

use std::hash::BuildHasher;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use foldhash::fast::FixedState;
use hashbrown::HashTable;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// An interned string. Only meaningful together with the [`Interner`] that produced it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Sym(pub u32);

impl Sym {
    /// The empty string. Always present, always symbol 0.
    pub const EMPTY: Self = Self(0);
    /// Stands in for every string that did not fit in the interner.
    pub const OVERFLOW: Self = Self(1);

    /// The raw index of the symbol.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Text used for [`Sym::OVERFLOW`] when a symbol is resolved.
pub const OVERFLOW_TEXT: &str = "<overflow>";

const SHARDS: usize = 16;

struct Shard {
    table: HashTable<(u64, Sym)>,
}

/// A bounded concurrent string interner.
pub struct Interner {
    hasher: FixedState,
    shards: [RwLock<Shard>; SHARDS],
    strings: RwLock<Vec<Arc<str>>>,
    capacity: usize,
    overflowed: AtomicU64,
}

impl std::fmt::Debug for Interner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Interner")
            .field("len", &self.len())
            .field("capacity", &self.capacity)
            .field("overflowed", &self.overflowed())
            .finish()
    }
}

impl Default for Interner {
    fn default() -> Self {
        Self::with_capacity(1 << 20)
    }
}

impl Interner {
    /// Creates an interner that holds at most `capacity` distinct strings
    /// (including the two reserved symbols).
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let this = Self {
            hasher: FixedState::with_seed(0x6574_696f_5f73_796d),
            shards: std::array::from_fn(|_| RwLock::new(Shard { table: HashTable::new() })),
            strings: RwLock::new(Vec::new()),
            capacity: capacity.max(2),
            overflowed: AtomicU64::new(0),
        };
        let empty = this.insert_new("", this.hash(""));
        let overflow = this.insert_new(OVERFLOW_TEXT, this.hash(OVERFLOW_TEXT));
        debug_assert_eq!(empty, Some(Sym::EMPTY));
        debug_assert_eq!(overflow, Some(Sym::OVERFLOW));
        this
    }

    fn hash(&self, s: &str) -> u64 {
        self.hasher.hash_one(s)
    }

    fn shard(hash: u64) -> usize {
        // The low bits pick the bucket inside the table; use the high bits for the shard.
        (hash >> 60) as usize % SHARDS
    }

    /// Looks up a string without inserting it.
    #[must_use]
    pub fn get(&self, s: &str) -> Option<Sym> {
        let hash = self.hash(s);
        let shard = self.shards[Self::shard(hash)].read();
        let strings = self.strings.read();
        shard.table.find(hash, |&(h, sym)| h == hash && &*strings[sym.index()] == s).map(|&(_, sym)| sym)
    }

    /// Interns a string, returning its symbol, or [`Sym::OVERFLOW`] if the
    /// interner is full.
    pub fn intern(&self, s: &str) -> Sym {
        if let Some(sym) = self.get(s) {
            return sym;
        }
        let hash = self.hash(s);
        if let Some(sym) = self.insert_new(s, hash) {
            sym
        } else {
            self.overflowed.fetch_add(1, Ordering::Relaxed);
            Sym::OVERFLOW
        }
    }

    /// Inserts `s` under the shard write lock, re-checking for a concurrent insert.
    fn insert_new(&self, s: &str, hash: u64) -> Option<Sym> {
        let mut shard = self.shards[Self::shard(hash)].write();
        let mut strings = self.strings.write();
        if let Some(&(_, sym)) = shard.table.find(hash, |&(h, sym)| h == hash && &*strings[sym.index()] == s) {
            return Some(sym);
        }
        if strings.len() >= self.capacity {
            return None;
        }
        let sym = Sym(u32::try_from(strings.len()).ok()?);
        strings.push(Arc::from(s));
        shard.table.insert_unique(hash, (hash, sym), |&(h, _)| h);
        Some(sym)
    }

    /// Resolves a symbol to its text. Unknown symbols resolve to the overflow text.
    #[must_use]
    pub fn resolve(&self, sym: Sym) -> Arc<str> {
        let strings = self.strings.read();
        strings.get(sym.index()).cloned().unwrap_or_else(|| Arc::from(OVERFLOW_TEXT))
    }

    /// Number of interned strings, including the reserved ones.
    #[must_use]
    pub fn len(&self) -> usize {
        self.strings.read().len()
    }

    /// Always false: the reserved symbols are always present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Maximum number of strings this interner will hold.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// How many interning requests were folded into [`Sym::OVERFLOW`].
    #[must_use]
    pub fn overflowed(&self) -> u64 {
        self.overflowed.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn reserved_symbols_are_stable() {
        let i = Interner::with_capacity(8);
        assert_eq!(i.intern(""), Sym::EMPTY);
        assert_eq!(i.intern(OVERFLOW_TEXT), Sym::OVERFLOW);
        assert_eq!(&*i.resolve(Sym::EMPTY), "");
    }

    #[test]
    fn interning_is_idempotent() {
        let i = Interner::default();
        let a = i.intern("checkout");
        let b = i.intern("cart");
        assert_ne!(a, b);
        assert_eq!(i.intern("checkout"), a);
        assert_eq!(&*i.resolve(b), "cart");
        assert_eq!(i.get("cart"), Some(b));
        assert_eq!(i.get("missing"), None);
    }

    #[test]
    fn capacity_is_enforced() {
        let i = Interner::with_capacity(4);
        let a = i.intern("a");
        let b = i.intern("b");
        assert_ne!(a, Sym::OVERFLOW);
        assert_ne!(b, Sym::OVERFLOW);
        assert_eq!(i.intern("c"), Sym::OVERFLOW);
        assert_eq!(i.intern("d"), Sym::OVERFLOW);
        assert_eq!(i.overflowed(), 2);
        // Existing strings still resolve after the interner is full.
        assert_eq!(i.intern("a"), a);
        assert_eq!(i.len(), 4);
    }

    #[test]
    fn concurrent_interning_agrees() {
        let i = Arc::new(Interner::default());
        let handles: Vec<_> = (0..4)
            .map(|t| {
                let i = Arc::clone(&i);
                thread::spawn(move || {
                    (0..2000).map(|n| (n, i.intern(&format!("svc-{}", (n * 7 + t) % 500)))).collect::<Vec<_>>()
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        for per_thread in &results {
            for &(_, sym) in per_thread {
                let text = i.resolve(sym);
                assert_eq!(i.intern(&text), sym);
            }
        }
        assert_eq!(i.len(), 502);
    }
}
