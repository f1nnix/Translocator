//! What can be said about an archive without opening it: its format by
//! name, an empty one to add files to, and how member paths are spelled.
//! Browsing archives as folders is `tl_core::archive`.

use crate::entry::EntryKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ArchiveFormat {
    Tar,
    TarGz,
    Zip,
}

impl ArchiveFormat {
    /// The format of a file, by its name.
    pub fn detect(name: &[u8]) -> Option<Self> {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(b".tar.gz") || lower.ends_with(b".tgz") {
            Some(ArchiveFormat::TarGz)
        } else if lower.ends_with(b".tar") {
            Some(ArchiveFormat::Tar)
        } else if lower.ends_with(b".zip") {
            Some(ArchiveFormat::Zip)
        } else {
            None
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ArchiveFormat::Tar => "tar",
            ArchiveFormat::TarGz => "tar.gz",
            ArchiveFormat::Zip => "zip",
        }
    }

    /// A valid archive with nothing in it, to add files to.
    pub fn empty_archive(self) -> Vec<u8> {
        // Two zero blocks end a tar archive.
        let tar = vec![0u8; 1024];
        match self {
            ArchiveFormat::Tar => tar,
            ArchiveFormat::TarGz => {
                // gzip with one stored (uncompressed) deflate block.
                let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3, 1];
                let len = tar.len() as u16;
                out.extend_from_slice(&len.to_le_bytes());
                out.extend_from_slice(&(!len).to_le_bytes());
                out.extend_from_slice(&tar);
                out.extend_from_slice(&crc32(&tar).to_le_bytes());
                out.extend_from_slice(&(tar.len() as u32).to_le_bytes());
                out
            }
            // Just the end-of-central-directory record.
            ArchiveFormat::Zip => {
                let mut out = vec![0x50, 0x4b, 0x05, 0x06];
                out.resize(22, 0);
                out
            }
        }
    }
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// One member of an archive, as listed.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ArchiveMember {
    /// The path as stored (`./www/`, `www/index.html`).
    pub raw: Vec<u8>,
    pub kind: EntryKind,
    pub size: u64,
    pub mtime: Option<i64>,
    /// Permission bits, `0` when the archive does not record them.
    pub mode: u32,
    pub link_target: Option<Vec<u8>>,
}

impl ArchiveMember {
    pub fn new(raw: impl Into<Vec<u8>>, kind: EntryKind) -> Self {
        ArchiveMember {
            raw: raw.into(),
            kind,
            size: 0,
            mtime: None,
            mode: 0,
            link_target: None,
        }
    }
}

/// A stored member path as a clean relative path: `./a//b/` → `a/b`.
/// `None` for the archive root itself and for paths that would escape it.
pub fn normalize_member(raw: &[u8]) -> Option<Vec<u8>> {
    let mut parts: Vec<&[u8]> = Vec::new();
    for part in raw.split(|b| *b == b'/') {
        match part {
            b"" | b"." => {}
            b".." => return None,
            p => parts.push(p),
        }
    }
    (!parts.is_empty()).then(|| parts.join(&b'/'))
}

/// The part of `path` below `dir` (both normalized; `dir` empty is the root).
pub fn member_below<'a>(path: &'a [u8], dir: &[u8]) -> Option<&'a [u8]> {
    if dir.is_empty() {
        return Some(path);
    }
    let rest = path.strip_prefix(dir)?;
    if rest.is_empty() {
        Some(rest)
    } else {
        rest.strip_prefix(b"/")
    }
}

/// `dir` + `/` + `name`, for normalized member paths.
pub fn member_join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    if dir.is_empty() {
        return name.to_vec();
    }
    let mut path = dir.to_vec();
    path.push(b'/');
    path.extend_from_slice(name);
    path
}

