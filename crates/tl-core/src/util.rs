use crate::entry::split_extension;

/// `1536` → `1.5 KB`. Uses binary multiples with the familiar short units.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Mode applied to a directory by a recursive permission change: folders
/// get the search bit wherever the mode grants read (like `chmod -R a+rX`),
/// so `644` applied recursively leaves them traversable.
pub fn recursive_dir_mode(mode: u32) -> u32 {
    mode | ((mode & 0o444) >> 2)
}

/// `1234567` → `1 234 567`, for exact byte counts.
pub fn group_digits(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('\u{2009}');
        }
        out.push(ch);
    }
    out
}

/// Finds a free "keep both" name: `report.txt` → `report (2).txt`,
/// `report (2).txt` → `report (3).txt`, folders get the suffix at the end.
pub fn unique_name(name: &[u8], is_dir: bool, taken: impl Fn(&[u8]) -> bool) -> Vec<u8> {
    let (stem, ext) = if is_dir {
        (name, None)
    } else {
        split_extension(name)
    };
    let (base, start) = strip_counter(stem);
    for n in start.max(2).. {
        let mut candidate = base.to_vec();
        candidate.extend_from_slice(format!(" ({n})").as_bytes());
        if let Some(ext) = ext {
            candidate.push(b'.');
            candidate.extend_from_slice(ext);
        }
        if !taken(&candidate) {
            return candidate;
        }
    }
    unreachable!()
}

/// `name (7)` → (`name`, 8); anything else → (stem, 2).
fn strip_counter(stem: &[u8]) -> (&[u8], u64) {
    if stem.last() == Some(&b')')
        && let Some(open) = stem.iter().rposition(|b| *b == b'(')
            && open >= 1 && stem[open - 1] == b' ' {
                let digits = &stem[open + 1..stem.len() - 1];
                if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit)
                    && let Ok(n) = std::str::from_utf8(digits).unwrap().parse::<u64>() {
                        return (&stem[..open - 1], n + 1);
                    }
            }
    (stem, 2)
}

/// The key names are matched by on a file system that may ignore case.
pub fn name_key(name: &[u8], case_insensitive: bool) -> Vec<u8> {
    if !case_insensitive {
        return name.to_vec();
    }
    match std::str::from_utf8(name) {
        Ok(s) => s.to_lowercase().into_bytes(),
        Err(_) => name.to_ascii_lowercase(),
    }
}

/// Turns user search input into wildcard masks: text without `*` or `?`
/// matches anywhere in a name (`nginx` → `*nginx*`).
pub fn search_masks(input: &str) -> Vec<String> {
    input
        .split([';', ','])
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(|m| {
            if m.contains(['*', '?']) {
                m.to_string()
            } else {
                format!("*{m}*")
            }
        })
        .collect()
}

/// Matches `name` against a shell-style wildcard mask (`*.rs`, `img_??.png`).
/// Several masks can be separated by `;` or `,`. Case-insensitive.
pub fn wildcard_match(masks: &str, name: &str) -> bool {
    masks
        .split([';', ','])
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .any(|mask| glob(mask.as_bytes(), name.to_lowercase().as_bytes()))
}

fn glob(mask: &[u8], name: &[u8]) -> bool {
    let mask: Vec<u8> = mask.iter().map(u8::to_ascii_lowercase).collect();
    let (mut m, mut n) = (0, 0);
    let (mut star_m, mut star_n) = (usize::MAX, 0);
    while n < name.len() {
        if m < mask.len() && (mask[m] == b'?' || mask[m] == name[n]) {
            m += 1;
            n += 1;
        } else if m < mask.len() && mask[m] == b'*' {
            star_m = m;
            star_n = n;
            m += 1;
        } else if star_m != usize::MAX {
            m = star_m + 1;
            star_n += 1;
            n = star_n;
        } else {
            return false;
        }
    }
    while m < mask.len() && mask[m] == b'*' {
        m += 1;
    }
    m == mask.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(10 * 1024 * 1024), "10.0 MB");
        assert_eq!(human_size(500 * 1024 * 1024 * 1024), "500 GB");
        assert_eq!(group_digits(1234567), "1\u{2009}234\u{2009}567");
    }

    #[test]
    fn unique_names() {
        let taken = |n: &[u8]| n == b"a (2).txt";
        assert_eq!(unique_name(b"a.txt", false, taken), b"a (3).txt");
        assert_eq!(unique_name(b"a (4).txt", false, |_| false), b"a (5).txt");
        assert_eq!(unique_name(b"dir.d", true, |_| false), b"dir.d (2)");
        assert_eq!(unique_name(b".env", false, |_| false), b".env (2)");
    }

    #[test]
    fn wildcards() {
        assert!(wildcard_match("*.rs", "main.RS"));
        assert!(wildcard_match("*.txt; *.md", "readme.md"));
        assert!(wildcard_match("img_??.png", "img_01.png"));
        assert!(!wildcard_match("img_??.png", "img_1.png"));
        assert!(wildcard_match("*", ".hidden"));
        assert!(!wildcard_match("a*b", "acd"));
    }
}
