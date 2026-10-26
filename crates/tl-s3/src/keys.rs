//! Translating between the app's paths and S3's buckets and keys.
//!
//! A pane path is `/bucket/key/parts`. The root lists buckets. S3 has no
//! directories: a folder is a key prefix, and an empty one is kept alive by
//! a zero-byte object whose key ends in `/`.

use tl_core::{VPath, VfsError, VfsResult};

/// Bucket and key for a path; `None` at the root. The key is empty for a
/// bucket's own root.
pub fn split(path: &VPath) -> VfsResult<Option<(String, String)>> {
    let text = std::str::from_utf8(path.as_bytes()).map_err(|_| {
        VfsError::other(format!("{path} is not valid UTF-8, which S3 keys must be"))
    })?;
    let rest = text.trim_start_matches('/');
    if rest.is_empty() {
        return Ok(None);
    }
    Ok(Some(match rest.split_once('/') {
        Some((bucket, key)) => (bucket.to_string(), key.to_string()),
        None => (rest.to_string(), String::new()),
    }))
}

/// The prefix that lists a folder's contents: empty for a bucket root,
/// otherwise the key with a trailing slash.
pub fn dir_prefix(key: &str) -> String {
    if key.is_empty() {
        String::new()
    } else {
        format!("{}/", key.trim_end_matches('/'))
    }
}

/// The last segment of a key, which is what a pane shows as the name.
pub fn base_name(key: &str) -> &str {
    key.trim_end_matches('/').rsplit('/').next().unwrap_or(key)
}

/// Percent-encodes a bucket/key pair for `x-amz-copy-source`, which must be
/// URL-encoded while keeping the separators.
pub fn copy_source(bucket: &str, key: &str) -> String {
    const UNRESERVED: &[u8] = b"-_.~";
    let mut out = String::with_capacity(bucket.len() + key.len() + 8);
    out.push('/');
    out.push_str(bucket);
    out.push('/');
    for byte in key.bytes() {
        if byte.is_ascii_alphanumeric() || UNRESERVED.contains(&byte) || byte == b'/' {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Bucket names are far more restricted than keys, and a confusing S3 error
/// is worse than saying so here.
pub fn check_bucket_name(name: &str) -> VfsResult<()> {
    let ok = (3..=63).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.ends_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit());
    if ok {
        Ok(())
    } else {
        Err(VfsError::other(format!(
            "“{name}” is not a usable bucket name: 3 to 63 characters, lowercase letters, \
             digits, dots and hyphens, starting and ending with a letter or digit"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_map_to_buckets_and_keys() {
        assert_eq!(split(&VPath::root()).unwrap(), None);
        assert_eq!(
            split(&VPath::parse("/bucket")).unwrap(),
            Some(("bucket".into(), String::new()))
        );
        assert_eq!(
            split(&VPath::parse("/bucket/a/b.txt")).unwrap(),
            Some(("bucket".into(), "a/b.txt".into()))
        );
        assert!(split(&VPath::new([b'/', 0xff])).is_err());
    }

    #[test]
    fn prefixes_and_names() {
        assert_eq!(dir_prefix(""), "");
        assert_eq!(dir_prefix("a/b"), "a/b/");
        assert_eq!(dir_prefix("a/b/"), "a/b/");
        assert_eq!(base_name("a/b/c.txt"), "c.txt");
        assert_eq!(base_name("a/b/"), "b");
        assert_eq!(base_name("solo"), "solo");
    }

    #[test]
    fn copy_sources_are_encoded() {
        assert_eq!(copy_source("b", "a/c.txt"), "/b/a/c.txt");
        assert_eq!(copy_source("b", "we ird+x"), "/b/we%20ird%2Bx");
        assert_eq!(copy_source("b", "ünï"), "/b/%C3%BCn%C3%AF");
    }

    #[test]
    fn bucket_names() {
        assert!(check_bucket_name("my-bucket-1").is_ok());
        assert!(check_bucket_name("ab").is_err());
        assert!(check_bucket_name("My-Bucket").is_err());
        assert!(check_bucket_name("-leading").is_err());
    }
}
