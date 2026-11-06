//! The HTTP side of WebDAV: turning a bookmark into a configured client,
//! building URLs for paths, and reading the status codes back as the
//! errors the app acts on.

use std::io::Read;
use std::time::Duration;

use base64::Engine as _;
use reqwest::Method;
use reqwest::blocking::{Client, RequestBuilder, Response};
use tl_core::{Bookmark, VPath, VfsError, VfsResult};

/// How the server is told who we are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Auth {
    None,
    Basic(String, String),
    Bearer(String),
}

/// Everything a bookmark says about a WebDAV server.
#[derive(Clone, Debug)]
pub struct DavTarget {
    /// Base URL with no trailing slash; it may carry a path of its own, as
    /// Nextcloud's /remote.php/dav/files/<user> does.
    pub base: String,
    pub auth: Auth,
    pub insecure: bool,
    /// Where a pane opens, below the base.
    pub start: String,
}

impl DavTarget {
    pub fn from_bookmark(bookmark: &Bookmark) -> Result<Self, String> {
        let raw = bookmark.get("url");
        if raw.is_empty() {
            return Err("A server URL is required".into());
        }
        let text = if raw.contains("://") {
            raw.to_string()
        } else {
            format!("https://{raw}")
        };
        let parsed = url::Url::parse(&text).map_err(|e| format!("“{text}” is not a URL: {e}"))?;
        match parsed.scheme() {
            "http" | "https" => {}
            other => return Err(format!("“{other}” is not a WebDAV scheme; use http or https")),
        }
        if parsed.host_str().is_none() {
            return Err(format!("“{text}” has no host"));
        }

        // A user or password in the URL beats the separate fields, because
        // that is what someone pasting a URL expects.
        let user = match parsed.username() {
            "" => bookmark.get("user").to_string(),
            name => percent_decode(name),
        };
        let password = match parsed.password() {
            Some(p) => percent_decode(p),
            None => bookmark.get("password").to_string(),
        };
        let token = bookmark.get("token").to_string();
        if !token.is_empty() && !user.is_empty() {
            return Err("Use either a token or a user name, not both".into());
        }
        let auth = if !token.is_empty() {
            Auth::Bearer(token)
        } else if !user.is_empty() {
            Auth::Basic(user, password)
        } else {
            Auth::None
        };

        let mut base = String::new();
        base.push_str(parsed.scheme());
        base.push_str("://");
        base.push_str(parsed.host_str().unwrap_or_default());
        if let Some(port) = parsed.port() {
            base.push(':');
            base.push_str(&port.to_string());
        }
        base.push_str(parsed.path().trim_end_matches('/'));

        Ok(DavTarget {
            base,
            auth,
            insecure: bookmark.flag("insecure"),
            start: bookmark.get("path").trim().trim_matches('/').to_string(),
        })
    }

    /// Two bookmarks sharing this can COPY and MOVE between each other on
    /// the server, because one set of credentials reaches both sides.
    pub fn location_key(&self) -> String {
        let who = match &self.auth {
            Auth::Basic(user, _) => user.as_str(),
            Auth::Bearer(_) => "token",
            Auth::None => "anonymous",
        };
        format!("webdav://{who}@{}", self.base)
    }

    pub fn label(&self) -> String {
        let shown = self
            .base
            .trim_start_matches("https://")
            .trim_start_matches("http://");
        match &self.auth {
            Auth::Basic(user, _) if !user.is_empty() => format!("{user}@{shown}"),
            _ => shown.to_string(),
        }
    }
}

/// What goes up with a request.
pub enum DavBody<'a> {
    /// Nothing at all: MKCOL, COPY, MOVE, DELETE and GET.
    None,
    /// Bytes already in memory: a PROPFIND body, or a file small enough
    /// not to be worth streaming.
    Bytes(&'a [u8]),
}

impl<'a> From<&'a str> for DavBody<'a> {
    fn from(text: &'a str) -> Self {
        DavBody::Bytes(text.as_bytes())
    }
}

impl<'a> From<&'a [u8]> for DavBody<'a> {
    fn from(bytes: &'a [u8]) -> Self {
        DavBody::Bytes(bytes)
    }
}

/// rustls picks its cipher implementation from a process-wide default, and
/// ring is the one built here. Installing it twice is not a failure: the
/// S3 provider may have got there first, and either way the result is the
/// same provider.
fn install_crypto() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// A connected server.
pub struct Dav {
    client: Client,
    pub base: String,
    auth: Option<String>,
}

impl Dav {
    pub fn connect(target: &DavTarget) -> VfsResult<Self> {
        install_crypto();
        let client = Client::builder()
            // No deadline on a request as a whole: a transfer can run for
            // hours, and a server-side COPY of a large tree answers only
            // once it has finished. A peer that has gone away is caught by
            // the keepalive instead.
            .timeout(None)
            .connect_timeout(Duration::from_secs(30))
            .tcp_keepalive(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(5))
            .danger_accept_invalid_certs(target.insecure)
            .build()
            .map_err(|e| VfsError::Connection(format!("Cannot set up the HTTP client: {e}")))?;
        let auth = match &target.auth {
            Auth::None => None,
            Auth::Basic(user, password) => Some(format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
            )),
            Auth::Bearer(token) => Some(format!("Bearer {token}")),
        };
        Ok(Dav {
            client,
            base: target.base.clone(),
            auth,
        })
    }

    /// The URL for a path in the pane, with every segment encoded.
    pub fn url(&self, path: &VPath) -> String {
        let mut url = self.base.clone();
        for segment in path.components() {
            url.push('/');
            url.push_str(&encode_segment(segment));
        }
        url
    }

    /// Collections are addressed with a trailing slash, which some servers
    /// insist on and none mind.
    pub fn dir_url(&self, path: &VPath) -> String {
        let url = self.url(path);
        if url.ends_with('/') { url } else { format!("{url}/") }
    }

    fn build(&self, method: &str, url: &str, headers: &[(&str, &str)]) -> VfsResult<RequestBuilder> {
        // PROPFIND, MKCOL, COPY and MOVE are not in RFC 9110's list, which
        // is why the client underneath has to accept any method at all.
        let verb = Method::from_bytes(method.as_bytes())
            .map_err(|_| VfsError::other(format!("“{method}” is not an HTTP method")))?;
        let mut builder = self.client.request(verb, url);
        if let Some(auth) = &self.auth {
            builder = builder.header(reqwest::header::AUTHORIZATION, auth);
        }
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        Ok(builder)
    }

    pub fn send<'a>(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: impl Into<DavBody<'a>>,
    ) -> VfsResult<Response> {
        let mut builder = self.build(method, url, headers)?;
        if let DavBody::Bytes(bytes) = body.into() {
            builder = builder.body(bytes.to_vec());
        }
        // Statuses come back as they are rather than as errors, because
        // WebDAV says a great deal with 207, 404, 405 and 412.
        builder
            .send()
            .map_err(|err| transport_error(method, url, &err))
    }

    /// Sends and insists on a successful status.
    pub fn send_ok<'a>(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: impl Into<DavBody<'a>>,
        what: &str,
    ) -> VfsResult<Response> {
        let response = self.send(method, url, headers, body)?;
        let status = response.status().as_u16();
        if (200..300).contains(&status) {
            Ok(response)
        } else {
            Err(status_error(status, what))
        }
    }

    /// PUTs `len` bytes read from somewhere else, without holding the file
    /// in memory.
    ///
    /// The client wants a body it can own and move to a thread of its own,
    /// and a tar member is borrowed from the archive it came in, so the
    /// bytes go across a pipe. The length is known, so the request is a
    /// plain sized one rather than chunked, which some servers refuse.
    pub fn put_stream(
        &self,
        url: &str,
        reader: &mut dyn Read,
        len: u64,
        what: &str,
    ) -> VfsResult<()> {
        let (rx, mut tx) = std::io::pipe().map_err(|e| VfsError::other(format!("{what}: {e}")))?;
        let builder = self
            .build("PUT", url, &[])?
            .body(reqwest::blocking::Body::sized(rx, len));
        let sender = std::thread::Builder::new()
            .name("webdav-put".into())
            .spawn(move || builder.send())
            .map_err(|e| VfsError::other(format!("{what}: {e}")))?;

        let mut limited = reader.take(len);
        let copied = std::io::copy(&mut limited, &mut tx);
        drop(tx);

        let response = sender
            .join()
            .map_err(|_| VfsError::other(format!("{what}: the upload thread panicked")))?
            .map_err(|err| transport_error("PUT", url, &err))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            // A server that refused the file says more than the broken
            // pipe its refusal caused on this side.
            return Err(status_error(status, what));
        }
        copied.map_err(|e| VfsError::other(format!("{what}: {e}")))?;
        Ok(())
    }
}

fn transport_error(method: &str, url: &str, err: &reqwest::Error) -> VfsError {
    let mut text = err.to_string();
    // reqwest keeps the interesting part in the source chain.
    let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(err);
    while let Some(inner) = source {
        text.push_str(": ");
        text.push_str(&inner.to_string());
        source = inner.source();
    }
    let lower = text.to_ascii_lowercase();
    if lower.contains("certificate") || lower.contains("tls") || lower.contains("handshake") {
        return VfsError::Connection(format!(
            "{method} {url}: {text}. A self-signed certificate needs “Trust any certificate” \
             turned on for this bookmark."
        ));
    }
    VfsError::Connection(format!("{method} {url}: {text}"))
}

/// WebDAV answers with status codes, and the app acts on the difference
/// between "gone", "not allowed" and "already there".
pub fn status_error(status: u16, what: &str) -> VfsError {
    match status {
        401 => VfsError::Auth(format!("{what}: the server rejected the credentials (401)")),
        403 => VfsError::PermissionDenied(format!("{what}: forbidden (403)")),
        404 | 410 => VfsError::NotFound(format!("{what}: not found ({status})")),
        405 => VfsError::AlreadyExists(format!("{what}: already exists (405)")),
        409 => VfsError::NotFound(format!(
            "{what}: the parent folder does not exist (409)"
        )),
        412 => VfsError::AlreadyExists(format!("{what}: something is already there (412)")),
        423 => VfsError::PermissionDenied(format!("{what}: locked by someone else (423)")),
        507 => VfsError::other(format!("{what}: the server is out of space (507)")),
        other => VfsError::other(format!("{what}: the server answered {other}")),
    }
}

const UNRESERVED: &[u8] = b"-_.~";

/// Percent-encodes one path segment. Names are raw bytes, which is what
/// lets a server holding a name that is not UTF-8 still work.
pub fn encode_segment(segment: &[u8]) -> String {
    let mut out = String::with_capacity(segment.len());
    for &byte in segment {
        if byte.is_ascii_alphanumeric() || UNRESERVED.contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Turns the percent-encoding in an href back into bytes.
pub fn percent_decode_bytes(text: &str) -> Vec<u8> {
    let raw = text.as_bytes();
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' && i + 2 < raw.len() {
            let hex = std::str::from_utf8(&raw[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(raw[i]);
        i += 1;
    }
    out
}

fn percent_decode(text: &str) -> String {
    String::from_utf8_lossy(&percent_decode_bytes(text)).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bookmark(pairs: &[(&str, &str)]) -> Bookmark {
        let mut b = Bookmark::new("test", crate::PLUGIN_ID);
        for (key, value) in pairs {
            b.set(*key, *value);
        }
        b
    }

    #[test]
    fn urls_and_defaults() {
        let plain = DavTarget::from_bookmark(&bookmark(&[("url", "dav.example.com")])).unwrap();
        // A bare host is assumed to be https.
        assert_eq!(plain.base, "https://dav.example.com");
        assert_eq!(plain.auth, Auth::None);

        let nested = DavTarget::from_bookmark(&bookmark(&[(
            "url",
            "https://cloud.example.com/remote.php/dav/files/me/",
        )]))
        .unwrap();
        assert_eq!(nested.base, "https://cloud.example.com/remote.php/dav/files/me");

        let port = DavTarget::from_bookmark(&bookmark(&[("url", "http://127.0.0.1:8080")])).unwrap();
        assert_eq!(port.base, "http://127.0.0.1:8080");

        assert!(DavTarget::from_bookmark(&bookmark(&[])).is_err());
        assert!(DavTarget::from_bookmark(&bookmark(&[("url", "ftp://x.example.com")])).is_err());
    }

    #[test]
    fn credentials_come_from_the_url_or_the_fields() {
        let fields = DavTarget::from_bookmark(&bookmark(&[
            ("url", "https://dav.example.com"),
            ("user", "me"),
            ("password", "secret"),
        ]))
        .unwrap();
        assert_eq!(fields.auth, Auth::Basic("me".into(), "secret".into()));

        // Credentials inside the URL win, and arrive decoded.
        let inline = DavTarget::from_bookmark(&bookmark(&[(
            "url",
            "https://me%40work:p%40ss@dav.example.com",
        )]))
        .unwrap();
        assert_eq!(inline.auth, Auth::Basic("me@work".into(), "p@ss".into()));
        assert!(!inline.base.contains("me"), "credentials must not stay in the URL");

        let token = DavTarget::from_bookmark(&bookmark(&[
            ("url", "https://dav.example.com"),
            ("token", "abc123"),
        ]))
        .unwrap();
        assert_eq!(token.auth, Auth::Bearer("abc123".into()));

        assert!(
            DavTarget::from_bookmark(&bookmark(&[
                ("url", "https://dav.example.com"),
                ("user", "me"),
                ("token", "abc123"),
            ]))
            .is_err()
        );
    }

    #[test]
    fn location_key_decides_what_copies_on_the_server() {
        let key = |pairs: &[(&str, &str)]| {
            DavTarget::from_bookmark(&bookmark(pairs)).unwrap().location_key()
        };
        let base = [("url", "https://dav.example.com"), ("user", "me"), ("password", "s")];
        let mut other_path = base.to_vec();
        other_path.push(("path", "elsewhere"));
        // Where the pane opens does not change which server it is.
        assert_eq!(key(&base), key(&other_path));
        assert_ne!(
            key(&base),
            key(&[("url", "https://dav.example.com"), ("user", "you"), ("password", "s")])
        );
        assert_ne!(key(&base), key(&[("url", "https://other.example.com"), ("user", "me")]));
    }

    #[test]
    fn segments_survive_a_round_trip() {
        assert_eq!(encode_segment(b"plain.txt"), "plain.txt");
        assert_eq!(encode_segment(b"we ird&x"), "we%20ird%26x");
        assert_eq!(encode_segment("ünï".as_bytes()), "%C3%BCn%C3%AF");
        // A name that is not UTF-8 still makes a legal URL.
        assert_eq!(encode_segment(&[0xff, b'a']), "%FFa");
        for raw in [&b"we ird&x"[..], "ünï".as_bytes(), &[0xff, b'a'][..]] {
            assert_eq!(percent_decode_bytes(&encode_segment(raw)), raw);
        }
    }
}
