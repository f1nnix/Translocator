use std::sync::Arc;

use parking_lot::Mutex;

/// Shared progress of a running job. Workers update it; the UI polls
/// snapshots on a timer, so updates are cheap and never block on the UI.
#[derive(Clone, Default)]
pub struct Progress(Arc<Mutex<ProgressState>>);

#[derive(Clone, Debug, Default)]
pub struct ProgressState {
    /// What the job is doing right now ("Scanning", "Copying", ...).
    pub phase: String,
    /// The item currently being processed.
    pub current: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub items_done: u64,
    pub items_total: u64,
    /// No meaningful totals (e.g. a server-side `cp` we cannot observe).
    pub indeterminate: bool,
}

impl ProgressState {
    /// Completion in `0.0..=1.0`, preferring bytes over item counts.
    pub fn fraction(&self) -> Option<f32> {
        if self.indeterminate {
            return None;
        }
        if self.bytes_total > 0 {
            Some((self.bytes_done as f64 / self.bytes_total as f64).min(1.0) as f32)
        } else if self.items_total > 0 {
            Some((self.items_done as f64 / self.items_total as f64).min(1.0) as f32)
        } else {
            None
        }
    }
}

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
