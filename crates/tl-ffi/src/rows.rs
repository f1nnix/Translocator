//! The rows of a file list, display-ready, fetched by range.
//!
//! A pane's snapshot carries only its row count, cursor and
//! `listing_version`; the list asks for the rows it is about to draw with
//! `Core::rows(side, start, count)` and keeps them until the version moves.
//! Sizes and dates are formatted here, for those rows alone.

use tl_app::pane::PARENT;
use tl_app::settings::DateFormat;
use tl_app::tl_model::util::human_size;
use tl_app::tl_model::{Entry, EntryKind};
use tl_app::view::{LocationKind, PaneView};

use crate::input::Dragged;
use crate::types::{path_bytes, usize32};

/// What a row's icon shows. The shell picks the symbol and its colour:
/// folders in the accent blue, executables green, the rest muted.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IconKind {
    /// The ".." row.
    Parent,
    Folder,
    FolderSymlink,
    FileSymlink,
    /// A device, socket, pipe or other special file.
    Special,
    Archive,
    Image,
    Audio,
    Video,
    Code,
    Config,
    Script,
    Text,
    Key,
    File,
}

/// One row of a file list.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Row {
    /// The row's position in the list.
    pub index: u32,
    /// The name as text (".." for the parent row).
    pub name: String,
    /// The extension, for files.
    pub ext: String,
    /// "UP", "DIR", a computed folder size, or the file's size.
    pub size: String,
    /// The modification time in the date format of the settings.
    pub date: String,
    /// `-rw-r--r--`.
    pub mode: String,
    /// "→ target" for a symbolic link.
    pub link: Option<String>,
    pub icon: IconKind,
    pub is_dir: bool,
    pub is_parent: bool,
    /// Drawn faded.
    pub hidden: bool,
    pub executable: bool,
    /// Drawn in the marked colour, semibold.
    pub marked: bool,
    /// The cursor was on this row in the snapshot the rows were fetched
    /// with. The list draws the cursor from `PaneSnapshot.cursor`, which
    /// moves without the rows being fetched again.
    pub cursor: bool,
}

/// The rows `start..start + count` of a pane (fewer at the end).
pub fn rows(pane: &PaneView, date_format: DateFormat, start: u32, count: u32) -> Vec<Row> {
    let order = &pane.order;
    let start = (start as usize).min(order.len());
    let end = start.saturating_add(count as usize).min(order.len());
    (start..end).map(|ix| row(pane, ix, date_format)).collect()
}

fn row(pane: &PaneView, ix: usize, date_format: DateFormat) -> Row {
    let index = usize32(ix);
    let cursor = ix == pane.cursor;
    let at = pane.order[ix];
    let entry = (at != PARENT).then(|| &pane.listing.entries[at as usize]);
    let Some(entry) = entry else {
        return Row {
            index,
            name: "..".into(),
            ext: String::new(),
            size: "UP".into(),
            date: String::new(),
            mode: String::new(),
            link: None,
            icon: IconKind::Parent,
            is_dir: true,
            is_parent: true,
            hidden: false,
            executable: false,
            marked: false,
            cursor,
        };
    };
    let is_dir = entry.is_dir();
    let ext = if is_dir {
        String::new()
    } else {
        entry
            .extension()
            .map(|e| String::from_utf8_lossy(e).into_owned())
            .unwrap_or_default()
    };
    let size = if is_dir {
        match pane.dir_sizes.get(&entry.name) {
            Some(size) => human_size(*size),
            None => "DIR".into(),
        }
    } else {
        human_size(entry.size)
    };
    Row {
        index,
        name: pane.listing.names[at as usize].clone(),
        ext,
        size,
        date: crate::format::date(entry.mtime, date_format),
        mode: entry.mode_string(),
        link: entry
            .link_target
            .as_ref()
            .map(|t| format!("→ {}", String::from_utf8_lossy(t))),
        icon: icon(entry),
        is_dir,
        is_parent: false,
        hidden: entry.is_hidden(),
        executable: entry.is_executable(),
        marked: pane.marked.contains(&entry.name),
        cursor,
    }
}

/// The icon for an entry, by type and extension.
pub fn icon(entry: &Entry) -> IconKind {
    if entry.is_dir() {
        return if entry.is_symlink() {
            IconKind::FolderSymlink
        } else {
            IconKind::Folder
        };
    }
    if entry.is_symlink() {
        return IconKind::FileSymlink;
    }
    if entry.kind != EntryKind::File {
        return IconKind::Special;
    }
    let ext = entry
        .extension()
        .map(|e| String::from_utf8_lossy(e).to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "jar" | "deb" | "rpm" | "dmg"
        | "iso" | "pkg" => IconKind::Archive,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "svg" | "heic" | "tif" | "tiff" | "psd" => {
            IconKind::Image
        }
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "aiff" => IconKind::Audio,
        "mp4" | "mov" | "mkv" | "avi" | "webm" | "m4v" => IconKind::Video,
        "rs" | "go" | "py" | "js" | "mjs" | "ts" | "tsx" | "jsx" | "c" | "h" | "cc" | "cpp" | "hpp" | "java"
        | "kt" | "swift" | "rb" | "php" | "lua" | "sql" | "html" | "css" | "scss" | "vue" | "svelte" | "pl"
        | "cs" | "scala" | "zig" | "ex" | "exs" => IconKind::Code,
        "json" | "toml" | "yaml" | "yml" | "xml" | "ini" | "conf" | "cfg" | "env" | "plist" | "lock" => {
            IconKind::Config
        }
        "sh" | "bash" | "zsh" | "fish" | "command" => IconKind::Script,
        "md" | "txt" | "log" | "rst" | "csv" | "tsv" | "pdf" | "doc" | "docx" | "rtf" => IconKind::Text,
        "key" | "pem" | "crt" | "cer" | "pub" | "p12" | "gpg" | "asc" => IconKind::Key,
        _ if entry.is_executable() => IconKind::Script,
        _ => IconKind::File,
    }
}

/// What dragging a row carries.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Hash)]
pub struct DragItems {
    /// For a drop on the other pane (`PaneInput::Drop`).
    pub dragged: Dragged,
    /// The files on this machine, for other applications; empty when the
    /// pane is not on the local disk.
    pub local_paths: Vec<String>,
    /// Which of `local_paths` are folders.
    pub local_dirs: Vec<bool>,
    /// "notes.txt" or "3 items", for the drag image.
    pub label: String,
}

/// A drag starting on `row`: the marked rows (in list order) if that row
/// is marked, else that row. `None` for the ".." row.
pub fn drag(pane: &PaneView, side: crate::types::Side, row: u32) -> Option<DragItems> {
    let at = *pane.order.get(row as usize)?;
    if at == PARENT {
        return None;
    }
    let entries = &pane.listing.entries;
    let entry = &entries[at as usize];
    let picked: Vec<&Entry> = if pane.marked.contains(&entry.name) {
        pane.order
            .iter()
            .filter(|ix| **ix != PARENT)
            .map(|ix| &entries[*ix as usize])
            .filter(|e| pane.marked.contains(&e.name))
            .collect()
    } else {
        vec![entry]
    };
    let names: Vec<Vec<u8>> = picked.iter().map(|e| e.name.clone()).collect();
    let local = pane.kind == LocationKind::Local;
    let (local_paths, local_dirs) = if local {
        picked
            .iter()
            .map(|e| (pane.path.child(&e.name).to_local().to_string_lossy().into_owned(), e.is_dir()))
            .unzip()
    } else {
        (Vec::new(), Vec::new())
    };
    Some(DragItems {
        label: tl_app::pane::describe_names(&names).trim_matches(['“', '”']).to_string(),
        dragged: Dragged {
            side,
            conn: pane.conn.0,
            dir: path_bytes(&pane.path),
            names,
            elevated: pane.elevated,
        },
        local_paths,
        local_dirs,
    })
}
