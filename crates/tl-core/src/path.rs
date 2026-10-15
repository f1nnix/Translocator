use std::fmt;

/// An absolute POSIX path stored as raw bytes.
///
/// Remote file names are not guaranteed to be UTF-8, so the path keeps the
/// exact bytes and only decodes (lossily) for display. The value is always
/// normalized: it starts with `/`, has no empty, `.` or `..` components and
/// no trailing slash (except for the root itself). `..` is resolved
/// lexically, which is what a user navigating "up" expects even through
/// symlinked directories.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VPath(Vec<u8>);

impl VPath {
    pub fn root() -> Self {
        VPath(b"/".to_vec())
    }

    /// Builds a normalized absolute path from raw bytes. Relative input is
    /// treated as relative to the root.
    pub fn new(raw: impl AsRef<[u8]>) -> Self {
        let raw = raw.as_ref();
        let mut parts: Vec<&[u8]> = Vec::new();
        for part in raw.split(|b| *b == b'/') {
            match part {
                b"" | b"." => {}
                b".." => {
                    parts.pop();
                }
                p => parts.push(p),
            }
        }
        let mut out = Vec::with_capacity(raw.len() + 1);
        if parts.is_empty() {
            out.push(b'/');
        }
        for p in parts {
            out.push(b'/');
            out.extend_from_slice(p);
        }
        VPath(out)
    }

    pub fn parse(s: &str) -> Self {
        Self::new(s.as_bytes())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn is_root(&self) -> bool {
        self.0 == b"/"
    }

    pub fn parent(&self) -> Option<VPath> {
        if self.is_root() {
            return None;
        }
        let idx = self.0.iter().rposition(|b| *b == b'/').unwrap_or(0);
        Some(if idx == 0 {
            VPath::root()
        } else {
            VPath(self.0[..idx].to_vec())
        })
    }

    /// The last component, or `None` for the root.
    pub fn file_name(&self) -> Option<&[u8]> {
        if self.is_root() {
            return None;
        }
        let idx = self.0.iter().rposition(|b| *b == b'/').unwrap_or(0);
        Some(&self.0[idx + 1..])
    }

    pub fn file_name_lossy(&self) -> String {
        match self.file_name() {
            Some(name) => String::from_utf8_lossy(name).into_owned(),
            None => "/".to_string(),
        }
    }

    /// Appends one verbatim name component. Names coming from directory
    /// listings never contain `/` and are never `.` or `..`.
    pub fn child(&self, name: &[u8]) -> VPath {
        debug_assert!(!name.contains(&b'/'), "child() takes a single component");
        let mut raw = self.0.clone();
        if !self.is_root() {
            raw.push(b'/');
        }
        raw.extend_from_slice(name);
        VPath(raw)
    }

    /// Resolves user input: absolute input replaces the path, relative input
    /// (which may contain `/` and `..`) is applied on top of it.
    pub fn join(&self, rel: impl AsRef<[u8]>) -> VPath {
        let rel = rel.as_ref();
        if rel.first() == Some(&b'/') {
            return VPath::new(rel);
        }
        let mut raw = self.0.clone();
        raw.push(b'/');
        raw.extend_from_slice(rel);
        VPath::new(raw)
    }

    /// Component-wise prefix test: `/a/b` starts with `/a` but not with `/a/bc`.
    pub fn starts_with(&self, prefix: &VPath) -> bool {
        if prefix.is_root() {
            return true;
        }
        self.0.starts_with(&prefix.0)
            && (self.0.len() == prefix.0.len() || self.0[prefix.0.len()] == b'/')
    }

    /// The part of `self` below `base` (`None` if `self` is not inside it).
    pub fn relative_to(&self, base: &VPath) -> Option<&[u8]> {
        if !self.starts_with(base) {
            return None;
        }
        if base.is_root() {
            return Some(&self.0[1..]);
        }
        let rest = &self.0[base.0.len()..];
        Some(rest.strip_prefix(b"/").unwrap_or(rest))
    }

    pub fn components(&self) -> impl Iterator<Item = &[u8]> {
        self.0.split(|b| *b == b'/').filter(|c| !c.is_empty())
    }

    pub fn display(&self) -> String {
        String::from_utf8_lossy(&self.0).into_owned()
    }

    #[cfg(unix)]
    pub fn to_local(&self) -> std::path::PathBuf {
        use std::os::unix::ffi::OsStrExt;
        std::path::PathBuf::from(std::ffi::OsStr::from_bytes(&self.0))
    }

    #[cfg(unix)]
    pub fn from_local(path: &std::path::Path) -> VPath {
        use std::os::unix::ffi::OsStrExt;
        VPath::new(path.as_os_str().as_bytes())
    }
}

impl fmt::Display for VPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&String::from_utf8_lossy(&self.0))
    }
}

impl fmt::Debug for VPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VPath({:?})", String::from_utf8_lossy(&self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes() {
        assert_eq!(VPath::parse("").display(), "/");
        assert_eq!(VPath::parse("//a//b/./c/").display(), "/a/b/c");
        assert_eq!(VPath::parse("/a/b/../c").display(), "/a/c");
        assert_eq!(VPath::parse("/../..").display(), "/");
    }

    #[test]
    fn parent_and_name() {
        let p = VPath::parse("/etc/nginx/nginx.conf");
        assert_eq!(p.file_name(), Some(&b"nginx.conf"[..]));
        assert_eq!(p.parent().unwrap().display(), "/etc/nginx");
        assert_eq!(VPath::parse("/etc").parent().unwrap(), VPath::root());
        assert!(VPath::root().parent().is_none());
    }

    #[test]
    fn join_and_child() {
        let p = VPath::parse("/home/me");
        assert_eq!(p.join("../you/x").display(), "/home/you/x");
        assert_eq!(p.join("/tmp").display(), "/tmp");
        assert_eq!(VPath::root().child(b"etc").display(), "/etc");
        assert_eq!(p.child(b"a b").display(), "/home/me/a b");
    }

    #[test]
    fn prefixes() {
        let a = VPath::parse("/a/b");
        assert!(a.starts_with(&VPath::parse("/a")));
        assert!(!VPath::parse("/a/bc").starts_with(&a));
        assert!(a.starts_with(&VPath::root()));
        assert_eq!(VPath::parse("/a/b/c").relative_to(&a), Some(&b"c"[..]));
        assert_eq!(a.relative_to(&a), Some(&b""[..]));
        assert_eq!(a.relative_to(&VPath::root()), Some(&b"a/b"[..]));
    }

    #[test]
    fn non_utf8_names_survive() {
        let p = VPath::root().child(&[0xff, 0xfe, b'x']);
        assert_eq!(p.file_name(), Some(&[0xff, 0xfe, b'x'][..]));
    }
}
