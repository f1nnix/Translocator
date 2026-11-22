//! Version counters for the big parts of the view.
//!
//! The core hands out its listings, orders, marks and plans as shared
//! `Arc`s and replaces an `Arc` whenever what it holds changes, so the
//! addresses say whether anything changed. A [`Tracker`] turns a key made
//! of such addresses into a number that changes exactly when the key does;
//! the snapshot carries the number, and the shell fetches rows again only
//! when it moves.
//!
//! The key compared against is the previous snapshot's, whose `Arc`s the
//! core keeps cached until the new snapshot replaces it, so an address
//! cannot be freed and reused in between.

/// A counter that moves when its key changes.
#[derive(Debug, Default)]
pub struct Tracker<K> {
    key: Option<K>,
    version: u64,
}

impl<K: PartialEq> Tracker<K> {
    /// The version for `key`: the last one if the key is unchanged, the
    /// next one otherwise. Starts at 1.
    pub fn version(&mut self, key: K) -> u64 {
        if self.key.as_ref() != Some(&key) {
            self.key = Some(key);
            self.version += 1;
        }
        self.version
    }
}

/// The address of what an `Arc` points to, as part of a key.
pub fn addr<T: ?Sized>(arc: &std::sync::Arc<T>) -> usize {
    std::sync::Arc::as_ptr(arc) as *const () as usize
}
