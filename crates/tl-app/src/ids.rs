//! Identifiers the core hands out. All of them come from counters in the
//! model, so a run is deterministic and a test can predict them.

use serde::{Deserialize, Serialize};

macro_rules! id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub struct $name(pub u64);
    };
}

id!(
    /// A live file system the shell holds: the local disk, a server, an
    /// archive opened as a folder.
    ConnId
);
id!(
    /// One tab of a pane, stable while tabs are opened and closed around it.
    TabId
);
id!(
    /// A background job.
    JobId
);
id!(
    /// A modal dialog.
    DialogId
);
id!(
    /// A question a provider asked through the shell.
    PromptId
);
id!(
    /// A viewer or editor window.
    EditorId
);

/// The local file system is always there, under this id.
pub const LOCAL: ConnId = ConnId(0);

/// Hands out ids in order, and the strings new bookmarks and places are
/// named by (unique for this run: the shell seeds them).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Ids {
    next: u64,
    seed: u64,
}

impl Ids {
    pub fn seeded(seed: u64) -> Self {
        Ids { next: 1, seed }
    }

    pub fn reseed(&mut self, seed: u64) {
        self.seed = seed;
    }

    fn bump(&mut self) -> u64 {
        self.next = self.next.max(1);
        let id = self.next;
        self.next += 1;
        id
    }

    pub fn conn(&mut self) -> ConnId {
        ConnId(self.bump())
    }

    pub fn tab(&mut self) -> TabId {
        TabId(self.bump())
    }

    pub fn job(&mut self) -> JobId {
        JobId(self.bump())
    }

    pub fn dialog(&mut self) -> DialogId {
        DialogId(self.bump())
    }

    pub fn editor(&mut self) -> EditorId {
        EditorId(self.bump())
    }

    /// A UUID-shaped string for a new bookmark or place, from the seed and
    /// a counter run through SplitMix64.
    pub fn uuid(&mut self) -> String {
        let n = self.bump();
        let mix = |mut z: u64| {
            z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        };
        let a = mix(self.seed ^ n);
        let b = mix(a ^ n.rotate_left(17));
        format!(
            "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
            a >> 32,
            (a >> 16) & 0xffff,
            a & 0x0fff,
            0x8000 | (b >> 48) & 0x3fff,
            b & 0xffff_ffff_ffff
        )
    }
}
