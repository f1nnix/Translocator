//! PROPFIND: the request bodies and the multistatus XML that comes back.
//!
//! Servers differ in which namespace prefix they use (`D:`, `d:`, `lp1:`),
//! so everything here matches on the local name and ignores the prefix.

use quick_xml::Reader;
use quick_xml::XmlVersion;
use quick_xml::events::Event;
use tl_core::{VfsError, VfsResult};

use crate::client::percent_decode_bytes;

/// Only the properties the panes actually show, so a listing stays one
/// small round trip even on a folder with thousands of entries.
pub const PROPS: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<propfind xmlns="DAV:"><prop>
<resourcetype/><getcontentlength/><getlastmodified/>
</prop></propfind>"#;

/// RFC 4331, which only some servers answer.
pub const QUOTA_PROPS: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<propfind xmlns="DAV:"><prop>
<quota-available-bytes/><quota-used-bytes/>
</prop></propfind>"#;

/// One `<response>` from a multistatus document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DavEntry {
    /// The decoded path from the href, as the server spelled it.
    pub path: Vec<u8>,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: Option<i64>,
    pub quota_available: Option<u64>,
    pub quota_used: Option<u64>,
}

/// Strips scheme and host from an href, leaving the decoded server path
/// without its trailing slash.
fn href_to_path(href: &str) -> Vec<u8> {
    let rest = match href.find("://") {
        Some(at) => match href[at + 3..].find('/') {
            Some(slash) => &href[at + 3 + slash..],
            None => "",
        },
        None => href,
    };
    let decoded = percent_decode_bytes(rest);
    let trimmed = decoded
        .iter()
        .rposition(|b| *b != b'/')
        .map(|end| &decoded[..=end])
        .unwrap_or(b"");
    trimmed.to_vec()
}

/// `Fri, 06 Nov 2026 16:40:00 GMT`, or an ISO 8601 time from the servers
/// that answer with one.
fn parse_time(text: &str) -> Option<i64> {
    let text = text.trim();
    chrono::DateTime::parse_from_rfc2822(text)
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(text))
        .ok()
        .map(|t| t.timestamp())
}

pub fn parse_multistatus(xml: &[u8]) -> VfsResult<Vec<DavEntry>> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut out: Vec<DavEntry> = Vec::new();
    let mut current: Option<DavEntry> = None;
    // Which property's text we are inside, and whether we are inside the
    // resourcetype element, where <collection/> may appear.
    let mut field: Option<Vec<u8>> = None;
    let mut in_resourcetype = false;

    loop {
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|e| VfsError::other(format!("The server sent XML we cannot read: {e}")))?;
        match event {
            Event::Start(start) => {
                let name = start.local_name().as_ref().to_vec();
                match name.as_slice() {
                    b"response" => current = Some(DavEntry::default()),
                    b"resourcetype" => in_resourcetype = true,
                    _ => field = Some(name),
                }
            }
            Event::Empty(empty) => {
                // <collection/> is the whole point of resourcetype.
                if in_resourcetype
                    && empty.local_name().as_ref() == b"collection"
                    && let Some(entry) = current.as_mut()
                {
                    entry.is_dir = true;
                }
            }
            Event::Text(text) => {
                let (Some(entry), Some(name)) = (current.as_mut(), field.as_ref()) else {
                    buf.clear();
                    continue;
                };
                let value = text
                    .xml_content(XmlVersion::default())
                    .map_err(|e| VfsError::other(format!("Bad XML text: {e}")))?;
                match name.as_slice() {
                    b"href" => entry.path = href_to_path(&value),
                    b"getcontentlength" => entry.size = value.trim().parse().unwrap_or(0),
                    b"getlastmodified" => entry.mtime = parse_time(&value),
                    b"quota-available-bytes" => entry.quota_available = value.trim().parse().ok(),
                    b"quota-used-bytes" => entry.quota_used = value.trim().parse().ok(),
                    _ => {}
                }
            }
            Event::End(end) => {
                let name = end.local_name().as_ref().to_vec();
                match name.as_slice() {
                    b"response" => {
                        if let Some(entry) = current.take() {
                            out.push(entry);
                        }
                    }
                    b"resourcetype" => in_resourcetype = false,
                    _ => {}
                }
                field = None;
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // What rclone's server answers, prefix and all.
    const RCLONE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<D:multistatus xmlns:D="DAV:">
<D:response><D:href>/dir/</D:href><D:propstat><D:prop>
<D:resourcetype><D:collection xmlns:D="DAV:"/></D:resourcetype>
<D:getlastmodified>Fri, 06 Nov 2026 16:40:00 GMT</D:getlastmodified>
</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
<D:response><D:href>/dir/we%20ird%20%26.txt</D:href><D:propstat><D:prop>
<D:resourcetype></D:resourcetype>
<D:getcontentlength>1234</D:getcontentlength>
<D:getlastmodified>Fri, 06 Nov 2026 16:41:02 GMT</D:getlastmodified>
</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
</D:multistatus>"#;

    #[test]
    fn reads_a_listing() {
        let entries = parse_multistatus(RCLONE.as_bytes()).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, b"/dir");
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].path, "/dir/we ird &.txt".as_bytes());
        assert!(!entries[1].is_dir);
        assert_eq!(entries[1].size, 1234);
        assert_eq!(entries[1].mtime, Some(1793983262));
    }

    #[test]
    fn copes_with_other_prefixes_and_absolute_hrefs() {
        // Apache answers with lp1:/lp2: prefixes and often a full URL.
        let apache = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:lp1="DAV:">
<d:response><d:href>https://host.example.com:8443/dav/sub/</d:href><d:propstat><d:prop>
<lp1:resourcetype><d:collection/></lp1:resourcetype>
<lp1:getlastmodified>Fri, 06 Nov 2026 16:40:00 GMT</lp1:getlastmodified>
</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#;
        let entries = parse_multistatus(apache.as_bytes()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, b"/dav/sub");
        assert!(entries[0].is_dir);
    }

    #[test]
    fn a_missing_property_is_not_a_failure() {
        // Half the properties come back 404 inside their own propstat.
        let partial = r#"<multistatus xmlns="DAV:">
<response><href>/x.bin</href>
<propstat><prop><resourcetype/></prop><status>HTTP/1.1 200 OK</status></propstat>
<propstat><prop><getcontentlength/><getlastmodified/></prop><status>HTTP/1.1 404 Not Found</status></propstat>
</response></multistatus>"#;
        let entries = parse_multistatus(partial.as_bytes()).unwrap();
        assert_eq!(entries[0].path, b"/x.bin");
        assert_eq!(entries[0].size, 0);
        assert_eq!(entries[0].mtime, None);
        assert!(!entries[0].is_dir);
    }

    #[test]
    fn reads_quota_when_the_server_reports_it() {
        let quota = r#"<multistatus xmlns="DAV:"><response><href>/</href><propstat><prop>
<quota-available-bytes>1048576</quota-available-bytes>
<quota-used-bytes>524288</quota-used-bytes>
</prop><status>HTTP/1.1 200 OK</status></propstat></response></multistatus>"#;
        let entries = parse_multistatus(quota.as_bytes()).unwrap();
        assert_eq!(entries[0].quota_available, Some(1048576));
        assert_eq!(entries[0].quota_used, Some(524288));
    }

    #[test]
    fn hrefs_lose_their_host_and_trailing_slash() {
        assert_eq!(href_to_path("/a/b/"), b"/a/b");
        assert_eq!(href_to_path("http://h/a%20b"), "/a b".as_bytes());
        assert_eq!(href_to_path("/"), b"");
    }
}
