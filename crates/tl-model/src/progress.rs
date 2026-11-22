//! A snapshot of a running job's progress, as the UI shows it. The live,
//! shared counter the workers update is `tl_core::Progress`.

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq)]
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

