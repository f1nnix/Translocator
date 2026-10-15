//! POSIX shell quoting. Scripts are built as bytes because paths may not be
//! valid UTF-8.

/// Quotes `s` for a POSIX shell: `it's` → `'it'\''s'`.
pub fn quote(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() + 2);
    quote_into(&mut out, s);
    out
}

pub fn quote_into(out: &mut Vec<u8>, s: &[u8]) {
    out.push(b'\'');
    for &b in s {
        if b == b'\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            out.push(b);
        }
    }
    out.push(b'\'');
}

pub fn quote_str(s: &str) -> String {
    String::from_utf8(quote(s.as_bytes())).expect("quoting preserves UTF-8")
}

/// A shell script under construction.
#[derive(Clone, Debug, Default)]
pub struct Script(Vec<u8>);

impl Script {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends literal script text.
    pub fn raw(&mut self, text: &str) -> &mut Self {
        self.0.extend_from_slice(text.as_bytes());
        self
    }

    /// Appends one quoted argument.
    pub fn arg(&mut self, value: impl AsRef<[u8]>) -> &mut Self {
        quote_into(&mut self.0, value.as_ref());
        self
    }

    /// Appends a space followed by one quoted argument.
    pub fn sp_arg(&mut self, value: impl AsRef<[u8]>) -> &mut Self {
        self.0.push(b' ');
        self.arg(value)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(quote_str("plain"), "'plain'");
        assert_eq!(quote_str(""), "''");
        assert_eq!(quote_str("it's"), "'it'\\''s'");
        assert_eq!(quote_str("$HOME `x` \"y\""), "'$HOME `x` \"y\"'");
    }

    #[test]
    fn quoting_roundtrips_through_sh() {
        let nasty = "a'b\"c$d`e\\f\ng h*";
        let mut script = Script::new();
        script.raw("printf %s ").arg(nasty);
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(String::from_utf8(script.into_bytes()).unwrap())
            .output()
            .unwrap();
        assert_eq!(String::from_utf8(out.stdout).unwrap(), nasty);
    }
}
