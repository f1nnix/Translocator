/// File type as reported by `lstat`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Fifo,
    Socket,
    CharDevice,
    BlockDevice,
    Other,
}

impl EntryKind {
    /// From the file-type bits of a `st_mode` value.
    pub fn from_mode(mode: u32) -> Self {
        match mode & 0o170000 {
            0o040000 => EntryKind::Dir,
            0o100000 => EntryKind::File,
            0o120000 => EntryKind::Symlink,
            0o010000 => EntryKind::Fifo,
            0o140000 => EntryKind::Socket,
            0o020000 => EntryKind::CharDevice,
            0o060000 => EntryKind::BlockDevice,
            _ => EntryKind::Other,
        }
    }

    /// From GNU find's `%y`/`%Y` letters. `None` means "does not exist"
    /// (dangling symlink), a loop or an error.
    pub fn from_find_letter(c: &[u8]) -> Option<Self> {
        Some(match c {
            b"f" => EntryKind::File,
            b"d" => EntryKind::Dir,
            b"l" => EntryKind::Symlink,
            b"p" => EntryKind::Fifo,
            b"s" => EntryKind::Socket,
            b"c" => EntryKind::CharDevice,
            b"b" => EntryKind::BlockDevice,
            b"N" | b"L" | b"?" => return None,
            _ => EntryKind::Other,
        })
    }

    /// The leading character of an `ls -l` style mode string.
    pub fn type_char(self) -> char {
        match self {
            EntryKind::File => '-',
            EntryKind::Dir => 'd',
            EntryKind::Symlink => 'l',
            EntryKind::Fifo => 'p',
            EntryKind::Socket => 's',
            EntryKind::CharDevice => 'c',
            EntryKind::BlockDevice => 'b',
            EntryKind::Other => '?',
        }
    }
}

/// One directory entry as shown in a pane.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Raw name bytes (not necessarily UTF-8).
    pub name: Vec<u8>,
    pub kind: EntryKind,
    /// For symlinks: the kind of the final target, `None` when dangling.
    pub target_kind: Option<EntryKind>,
    pub link_target: Option<Vec<u8>>,
    pub size: u64,
    /// Modification time, seconds since the Unix epoch.
    pub mtime: Option<i64>,
    /// Permission bits including setuid/setgid/sticky (`0o7777` mask).
    pub mode: u32,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub user: Option<String>,
    pub group: Option<String>,
    /// Hidden by a provider-specific rule beyond the dot prefix
    /// (e.g. the macOS `UF_HIDDEN` flag).
    pub hidden_flag: bool,
}

impl Entry {
    pub fn new(name: impl Into<Vec<u8>>, kind: EntryKind) -> Self {
        Entry {
            name: name.into(),
            kind,
            target_kind: None,
            link_target: None,
            size: 0,
            mtime: None,
            mode: 0,
            uid: None,
            gid: None,
            user: None,
            group: None,
            hidden_flag: false,
        }
    }

    pub fn name_lossy(&self) -> String {
        String::from_utf8_lossy(&self.name).into_owned()
    }

    /// True for directories and symlinks that resolve to directories.
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Dir
            || (self.kind == EntryKind::Symlink && self.target_kind == Some(EntryKind::Dir))
    }

    pub fn is_symlink(&self) -> bool {
        self.kind == EntryKind::Symlink
    }

    pub fn is_hidden(&self) -> bool {
        self.name.first() == Some(&b'.') || self.hidden_flag
    }

    pub fn is_executable(&self) -> bool {
        !self.is_dir() && self.mode & 0o111 != 0
    }

    /// Extension of a non-directory name, without the dot. A leading dot
    /// (`.bashrc`) does not start an extension.
    pub fn extension(&self) -> Option<&[u8]> {
        if self.is_dir() {
            return None;
        }
        split_extension(&self.name).1
    }

    /// `drwxr-xr-x` style string, including setuid/setgid/sticky letters.
    pub fn mode_string(&self) -> String {
        mode_string(self.kind, self.mode)
    }
}

/// Splits `name` into stem and extension (`archive.tar.gz` → `archive.tar`, `gz`).
pub fn split_extension(name: &[u8]) -> (&[u8], Option<&[u8]>) {
    match name.iter().rposition(|b| *b == b'.') {
        Some(0) | None => (name, None),
        Some(i) if i + 1 == name.len() => (name, None),
        Some(i) => (&name[..i], Some(&name[i + 1..])),
    }
}

pub fn mode_string(kind: EntryKind, mode: u32) -> String {
    let mut s = String::with_capacity(10);
    s.push(kind.type_char());
    let bit = |m: u32, c: char| if mode & m != 0 { c } else { '-' };
    let special = |x: u32, special_bit: u32, lower: char, upper: char| match (
        mode & x != 0,
        mode & special_bit != 0,
    ) {
        (true, true) => lower,
        (false, true) => upper,
        (true, false) => 'x',
        (false, false) => '-',
    };
    s.push(bit(0o400, 'r'));
    s.push(bit(0o200, 'w'));
    s.push(special(0o100, 0o4000, 's', 'S'));
    s.push(bit(0o040, 'r'));
    s.push(bit(0o020, 'w'));
    s.push(special(0o010, 0o2000, 's', 'S'));
    s.push(bit(0o004, 'r'));
    s.push(bit(0o002, 'w'));
    s.push(special(0o001, 0o1000, 't', 'T'));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions() {
        assert_eq!(split_extension(b"a.tar.gz"), (&b"a.tar"[..], Some(&b"gz"[..])));
        assert_eq!(split_extension(b".bashrc"), (&b".bashrc"[..], None));
        assert_eq!(split_extension(b"noext"), (&b"noext"[..], None));
        assert_eq!(split_extension(b"trailing."), (&b"trailing."[..], None));
    }

    #[test]
    fn modes() {
        assert_eq!(mode_string(EntryKind::Dir, 0o755), "drwxr-xr-x");
        assert_eq!(mode_string(EntryKind::File, 0o4755), "-rwsr-xr-x");
        assert_eq!(mode_string(EntryKind::Dir, 0o1777), "drwxrwxrwt");
        assert_eq!(mode_string(EntryKind::File, 0o2644), "-rw-r-Sr--");
    }
}
