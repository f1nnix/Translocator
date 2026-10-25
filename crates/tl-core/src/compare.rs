//! Compares two directory listings: which entries exist on one side only
//! and which differ in size or modification time ("compare directories").

use std::collections::HashMap;

use crate::entry::Entry;
use crate::util::name_key;

/// Modification times closer than this count as equal: FAT and exFAT
/// store them with two-second precision.
pub const MTIME_TOLERANCE: i64 = 2;

/// How an item relates to its namesake on the other side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Diff {
    /// Nothing with this name on the other side.
    Unique,
    /// Both sides have it; this one was modified later.
    Newer,
    /// Both sides have it; this one is older.
    Older,
    /// Same time but a different size, or a file facing a folder.
    Different,
    /// Same kind, size and time (or both are folders).
    Same,
}

/// Compares two files' metadata. `None` times compare by size only.
pub fn diff_files(a_size: u64, a_mtime: Option<i64>, b_size: u64, b_mtime: Option<i64>) -> Diff {
    if let (Some(a), Some(b)) = (a_mtime, b_mtime)
        && (a - b).abs() > MTIME_TOLERANCE
    {
        return if a > b { Diff::Newer } else { Diff::Older };
    }
    if a_size == b_size { Diff::Same } else { Diff::Different }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompareStats {
    pub only_left: usize,
    pub only_right: usize,
    pub newer_left: usize,
    pub newer_right: usize,
    /// Same time, different size, or file against folder: marked on both sides.
    pub different: usize,
    pub same: usize,
}

impl CompareStats {
    pub fn is_identical(&self) -> bool {
        self.only_left + self.only_right + self.newer_left + self.newer_right + self.different == 0
    }
}

/// Which names to mark on each side.
#[derive(Clone, Debug, Default)]
pub struct CompareResult {
    pub left: Vec<Vec<u8>>,
    pub right: Vec<Vec<u8>>,
    pub stats: CompareStats,
}

/// Compares two listings the classic way: entries that exist on one side
/// only are marked there; of two files that differ, the newer one is
/// marked (both when their times match but their sizes do not). Folders
/// present on both sides are not descended into.
pub fn compare_listings(left: &[Entry], right: &[Entry], case_insensitive: bool) -> CompareResult {
    let right_by_key: HashMap<Vec<u8>, &Entry> = right
        .iter()
        .map(|e| (name_key(&e.name, case_insensitive), e))
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut result = CompareResult::default();
    for l in left {
        let key = name_key(&l.name, case_insensitive);
        let Some(r) = right_by_key.get(&key) else {
            result.left.push(l.name.clone());
            result.stats.only_left += 1;
            continue;
        };
        seen.insert(key);
        let diff = match (l.is_dir(), r.is_dir()) {
            (true, true) => Diff::Same,
            (false, false) => diff_files(l.size, l.mtime, r.size, r.mtime),
            _ => Diff::Different,
        };
        match diff {
            Diff::Same | Diff::Unique => result.stats.same += 1,
            Diff::Newer => {
                result.left.push(l.name.clone());
                result.stats.newer_left += 1;
            }
            Diff::Older => {
                result.right.push(r.name.clone());
                result.stats.newer_right += 1;
            }
            Diff::Different => {
                result.left.push(l.name.clone());
                result.right.push(r.name.clone());
                result.stats.different += 1;
            }
        }
    }
    for r in right {
        if !seen.contains(&name_key(&r.name, case_insensitive)) {
            result.right.push(r.name.clone());
            result.stats.only_right += 1;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::EntryKind;

    fn file(name: &str, size: u64, mtime: i64) -> Entry {
        let mut e = Entry::new(name.as_bytes().to_vec(), EntryKind::File);
        e.size = size;
        e.mtime = Some(mtime);
        e
    }

    fn dir(name: &str) -> Entry {
        Entry::new(name.as_bytes().to_vec(), EntryKind::Dir)
    }

    fn names(list: &[Vec<u8>]) -> Vec<String> {
        let mut v: Vec<String> = list.iter().map(|n| String::from_utf8_lossy(n).into_owned()).collect();
        v.sort();
        v
    }

    #[test]
    fn marks_unique_and_newer() {
        let left = vec![
            file("same.txt", 10, 1000),
            file("newer-left.txt", 10, 2000),
            file("older-left.txt", 10, 1000),
            file("only-left.txt", 1, 1),
            file("size.txt", 10, 1000),
            file("fat.txt", 10, 1001),
            dir("both"),
            dir("only-left-dir"),
            file("kind", 1, 1),
        ];
        let right = vec![
            file("same.txt", 10, 1000),
            file("newer-left.txt", 10, 1000),
            file("older-left.txt", 99, 3000),
            file("size.txt", 11, 1000),
            file("fat.txt", 10, 1000),
            dir("both"),
            file("only-right.txt", 1, 1),
            dir("kind"),
        ];
        let r = compare_listings(&left, &right, false);
        assert_eq!(names(&r.left), ["kind", "newer-left.txt", "only-left-dir", "only-left.txt", "size.txt"]);
        assert_eq!(names(&r.right), ["kind", "older-left.txt", "only-right.txt", "size.txt"]);
        assert_eq!(
            r.stats,
            CompareStats { only_left: 2, only_right: 1, newer_left: 1, newer_right: 1, different: 2, same: 3 }
        );
        assert!(!r.stats.is_identical());
    }

    #[test]
    fn case_insensitive_matching() {
        let left = vec![file("README", 1, 1)];
        let right = vec![file("readme", 1, 1)];
        assert!(compare_listings(&left, &right, true).stats.is_identical());
        let r = compare_listings(&left, &right, false);
        assert_eq!((r.stats.only_left, r.stats.only_right), (1, 1));
    }

    #[test]
    fn missing_times_compare_sizes() {
        assert_eq!(diff_files(1, None, 1, Some(5)), Diff::Same);
        assert_eq!(diff_files(1, None, 2, Some(5)), Diff::Different);
        assert_eq!(diff_files(1, Some(10), 1, Some(12)), Diff::Same);
        assert_eq!(diff_files(1, Some(10), 1, Some(13)), Diff::Older);
    }
}
