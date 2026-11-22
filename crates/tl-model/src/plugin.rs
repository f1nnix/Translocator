//! What the UI knows about a provider plugin: its name and the fields a
//! bookmark of its kind is edited with. The plugin itself (connecting,
//! validating, summarising) is `tl_core::plugin::ProviderPlugin`.

use serde::{Deserialize, Serialize};

use crate::bookmark::BookmarkConfig;

/// Widget used to edit a bookmark field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldKind {
    Text,
    Number,
    Toggle,
    /// A path on this machine (e.g. an SSH identity file).
    LocalPath,
    /// A password or key that must not be shown while typing.
    Secret,
    /// One of fixed `(value, label)` options.
    Choice(Vec<(String, String)>),
}

/// One bookmark setting. The bookmark editor renders these generically, so
/// new plugins need no UI code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldInfo {
    pub key: String,
    pub label: String,
    pub kind: FieldKind,
    pub placeholder: String,
    pub help: String,
    pub required: bool,
    pub default: String,
    /// Shown under "Advanced".
    pub advanced: bool,
}

/// A connection type compiled into the application.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInfo {
    /// Stable id stored in bookmarks ("ssh").
    pub id: String,
    /// Human name ("SSH (shell commands)").
    pub name: String,
    pub description: String,
    pub fields: Vec<FieldInfo>,
}

/// The checks every plugin gets for free: required fields are filled in and
/// numbers are numbers.
pub fn validate_fields(fields: &[FieldInfo], config: &BookmarkConfig) -> Result<(), String> {
    for field in fields {
        let value = config.get(&field.key).map(|v| v.trim()).unwrap_or("");
        if field.required && value.is_empty() {
            return Err(format!("{} is required", field.label));
        }
        if field.kind == FieldKind::Number && !value.is_empty() && value.parse::<u32>().is_err() {
            return Err(format!("{} must be a number", field.label));
        }
    }
    Ok(())
}
