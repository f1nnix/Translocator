use std::sync::Arc;

use parking_lot::Mutex;

pub use tl_model::progress::ProgressState;

/// Shared progress of a running job. Workers update it; the UI polls
/// snapshots on a timer, so updates are cheap and never block on the UI.
#[derive(Clone, Default)]
pub struct Progress(Arc<Mutex<ProgressState>>);

impl Progress {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_phase(&self, phase: impl Into<String>) {
        self.0.lock().phase = phase.into();
    }

    pub fn set_current(&self, current: impl Into<String>) {
        self.0.lock().current = current.into();
    }

    pub fn set_totals(&self, bytes: u64, items: u64) {
        let mut s = self.0.lock();
        s.bytes_total = bytes;
        s.items_total = items;
        s.indeterminate = false;
    }

    pub fn set_indeterminate(&self, indeterminate: bool) {
        self.0.lock().indeterminate = indeterminate;
    }

    pub fn add_bytes(&self, n: u64) {
        self.0.lock().bytes_done += n;
    }

    pub fn add_items(&self, n: u64) {
        self.0.lock().items_done += n;
    }

    pub fn set_done(&self, bytes: u64, items: u64) {
        let mut s = self.0.lock();
        s.bytes_done = bytes;
        s.items_done = items;
    }

    pub fn snapshot(&self) -> ProgressState {
        self.0.lock().clone()
    }
}
