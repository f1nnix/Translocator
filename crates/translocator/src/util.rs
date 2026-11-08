//! Small helpers shared by the UI modules.

use std::cmp::Ordering;
use std::future::Future;

use chrono::{Datelike, Local, TimeZone};
use gpui_kit::assets::IconName;
use tl_core::{Entry, EntryKind, VfsError, VfsResult};

use crate::settings::DateFormat;

/// Runs blocking provider work on a fresh thread and resolves with its
/// result. File operations block on disks and networks; this keeps them
/// off both the UI thread and gpui's shared executor.
pub fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> VfsResult<T> + Send + 'static,
) -> impl Future<Output = VfsResult<T>> {
    let (tx, rx) = futures::channel::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("tl-worker".into())
        .spawn(move || {
            let _ = tx.send(work());
        });
    async move {
        if let Err(err) = spawned {
            return Err(VfsError::other(format!("cannot start worker thread: {err}")));
        }
        rx.await
            .unwrap_or_else(|_| Err(VfsError::other("worker thread crashed")))
    }
}

/// A date in the file list, written the way the settings ask for.
pub fn format_date(mtime: Option<i64>, format: DateFormat) -> String {
    if format == DateFormat::Relative {
        return format_date_long(mtime);
    }
    let Some(secs) = mtime else {
        return String::new();
    };
    let pattern = match format {
        DateFormat::DayFirst => "%d.%m.%Y %H:%M",
        DateFormat::MonthFirst => "%m/%d/%Y %-I:%M %p",
        _ => "%Y-%m-%d %H:%M",
    };
    match Local.timestamp_opt(secs, 0).single() {
        Some(dt) => dt.format(pattern).to_string(),
        None => String::new(),
    }
}

/// Date for the status line: `today 14:32`, `Oct 3 09:12`, `2024-03-01`.
pub fn format_date_long(mtime: Option<i64>) -> String {
    let Some(secs) = mtime else {
        return String::new();
    };
    let Some(dt) = Local.timestamp_opt(secs, 0).single() else {
        return String::new();
    };
    let now = Local::now();
    if dt.date_naive() == now.date_naive() {
        dt.format("today %H:%M").to_string()
    } else if dt.year() == now.year() {
        dt.format("%b %-d %H:%M").to_string()
    } else {
        dt.format("%Y-%m-%d %H:%M").to_string()
    }
}

/// Case-insensitive natural order: `file2` before `file10`. Case only
/// breaks ties between otherwise equal names.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    natural_cmp_ignore_case(a, b).then_with(|| a.cmp(b))
}

fn natural_cmp_ignore_case(a: &str, b: &str) -> Ordering {
    let mut a = a.chars().peekable();
    let mut b = b.chars().peekable();
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = a.peek().copied().filter(char::is_ascii_digit) {
                    na.push(c);
                    a.next();
                }
                let mut nb = String::new();
                while let Some(c) = b.peek().copied().filter(char::is_ascii_digit) {
                    nb.push(c);
                    b.next();
                }
                let ta = na.trim_start_matches('0');
                let tb = nb.trim_start_matches('0');
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                a.next();
                b.next();
            }
        }
    }
}

/// What the system file manager is called, for menu labels.
pub const REVEAL_LABEL: &str = if cfg!(target_os = "macos") {
    "Reveal in Finder"
} else {
    "Show in File Manager"
};

/// Icon for an entry, chosen from the Lucide set by type and extension.
pub fn entry_icon(entry: &Entry) -> IconName {
    if entry.is_dir() {
        return if entry.is_symlink() {
            IconName::FolderSymlink
        } else {
            IconName::Folder
        };
    }
    if entry.is_symlink() {
        return IconName::FileSymlink;
    }
    if entry.kind != EntryKind::File {
        return IconName::FileCog;
    }
    let ext = entry
        .extension()
        .map(|e| String::from_utf8_lossy(e).to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "jar" | "deb"
        | "rpm" | "dmg" | "iso" | "pkg" => IconName::FileArchive,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "svg" | "heic" | "tif"
        | "tiff" | "psd" => IconName::FileImage,
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "aiff" => IconName::FileMusic,
        "mp4" | "mov" | "mkv" | "avi" | "webm" | "m4v" => IconName::FileVideoCamera,
        "rs" | "go" | "py" | "js" | "mjs" | "ts" | "tsx" | "jsx" | "c" | "h" | "cc" | "cpp"
        | "hpp" | "java" | "kt" | "swift" | "rb" | "php" | "lua" | "sql" | "html" | "css"
        | "scss" | "vue" | "svelte" | "pl" | "cs" | "scala" | "zig" | "ex" | "exs" => {
            IconName::FileCode
        }
        "json" | "toml" | "yaml" | "yml" | "xml" | "ini" | "conf" | "cfg" | "env" | "plist"
        | "lock" => IconName::FileCog,
        "sh" | "bash" | "zsh" | "fish" | "command" => IconName::FileTerminal,
        "md" | "txt" | "log" | "rst" | "csv" | "tsv" | "pdf" | "doc" | "docx" | "rtf" => {
            IconName::FileText
        }
        "key" | "pem" | "crt" | "cer" | "pub" | "p12" | "gpg" | "asc" => IconName::FileKey,
        _ if entry.is_executable() => IconName::FileTerminal,
        _ => IconName::File,
    }
}

/// Editor language for a file name.
pub fn language_for(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    match (lower.as_str(), ext) {
        ("makefile" | "gnumakefile", _) => "make",
        ("dockerfile", _) => "bash",
        (_, "rs") => "rust",
        (_, "toml") => "toml",
        (_, "json" | "jsonc") => "json",
        (_, "md" | "markdown") => "markdown",
        (_, "py") => "python",
        (_, "js" | "mjs" | "cjs" | "jsx") => "javascript",
        (_, "ts" | "mts" | "cts") => "typescript",
        (_, "tsx") => "tsx",
        (_, "yaml" | "yml") => "yaml",
        (_, "go") => "go",
        (_, "c" | "h") => "c",
        (_, "cpp" | "cc" | "cxx" | "hpp" | "hh") => "cpp",
        (_, "html" | "htm") => "html",
        (_, "css") => "css",
        (_, "sql") => "sql",
        (_, "diff" | "patch") => "diff",
        (_, "sh" | "bash" | "zsh" | "command" | "bashrc" | "zshrc" | "profile") => "bash",
        (_, "mk") => "make",
        _ if lower.starts_with(".bash") || lower.starts_with(".zsh") || lower == ".profile" => "bash",
        _ => "text",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut names = vec!["file10", "File2", "file1", "a", "B", "file02"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, ["a", "B", "file1", "File2", "file02", "file10"]);
        assert_eq!(natural_cmp("abc", "ABC"), "abc".cmp("ABC"));
        assert_eq!(natural_cmp("x9", "x10"), Ordering::Less);
    }

    #[test]
    fn dates_follow_the_setting() {
        // 2026-10-15 14:32:00 UTC, read back in the local zone, so the
        // test says what the formats look like without naming an hour.
        let when = Some(1_792_247_520);
        let iso = format_date(when, DateFormat::Iso);
        let day_first = format_date(when, DateFormat::DayFirst);
        let month_first = format_date(when, DateFormat::MonthFirst);
        assert_eq!(iso.len(), 16, "{iso}");
        assert_eq!(&iso[..7], "2026-10");
        assert_eq!(&day_first[2..3], ".");
        assert_eq!(&day_first[5..10], ".2026");
        assert!(month_first.ends_with("AM") || month_first.ends_with("PM"), "{month_first}");
        assert_eq!(format_date(when, DateFormat::Relative), format_date_long(when));
        for format in DateFormat::ALL {
            assert_eq!(format_date(None, format), "");
        }
    }

    #[test]
    fn languages() {
        assert_eq!(language_for("main.rs"), "rust");
        assert_eq!(language_for("Makefile"), "make");
        assert_eq!(language_for("nginx.conf"), "text");
        assert_eq!(language_for(".bashrc"), "bash");
    }
}
