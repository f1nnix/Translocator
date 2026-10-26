use std::path::PathBuf;
use std::sync::Arc;

use crate::askpass::AskpassHandle;
use crate::bookmark::{Bookmark, BookmarkConfig};
use crate::cancel::CancelToken;
use crate::error::VfsResult;
use crate::path::VPath;
use crate::prompt::Prompter;
use crate::vfs::Vfs;

/// Widget used to edit a bookmark field.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldKind {
    Text,
    Number,
    Toggle,
    /// A path on this machine (e.g. an SSH identity file).
    LocalPath,
    /// A password or key that must not be shown while typing.
    Secret,
    /// One of fixed `(value, label)` options.
    Choice(Vec<(&'static str, &'static str)>),
}

/// Declares one bookmark setting. The bookmark editor renders these
/// generically, so new plugins need no UI code.
#[derive(Clone, Debug)]
pub struct FieldSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
    pub placeholder: &'static str,
    pub help: &'static str,
    pub required: bool,
    pub default: &'static str,
    /// Shown under "Advanced".
    pub advanced: bool,
}

impl FieldSpec {
    pub fn text(key: &'static str, label: &'static str) -> Self {
        FieldSpec {
            key,
            label,
            kind: FieldKind::Text,
            placeholder: "",
            help: "",
            required: false,
            default: "",
            advanced: false,
        }
    }

    pub fn kind(mut self, kind: FieldKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn placeholder(mut self, placeholder: &'static str) -> Self {
        self.placeholder = placeholder;
        self
    }

    pub fn help(mut self, help: &'static str) -> Self {
        self.help = help;
        self
    }

    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    pub fn default_value(mut self, default: &'static str) -> Self {
        self.default = default;
        self
    }

    pub fn advanced(mut self) -> Self {
        self.advanced = true;
        self
    }
}

/// Services the host application offers to plugins.
#[derive(Clone)]
pub struct PluginEnv {
    pub prompter: Arc<dyn Prompter>,
    /// Lets child processes such as `ssh` route password prompts to the UI.
    pub askpass: Option<AskpassHandle>,
    /// Private directory for sockets and other runtime files.
    pub runtime_dir: PathBuf,
}

/// A connection type. Plugins are compiled into the app and registered at
/// startup; each turns bookmarks of its type into live [`Vfs`] instances.
pub trait ProviderPlugin: Send + Sync {
    /// Stable id stored in bookmarks ("ssh").
    fn id(&self) -> &'static str;

    /// Human name ("SSH (shell commands)").
    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    fn fields(&self) -> Vec<FieldSpec>;

    /// Checks a configuration before it is saved.
    fn validate(&self, config: &BookmarkConfig) -> Result<(), String> {
        for field in self.fields() {
            if field.required && config.get(field.key).is_none_or(|v| v.trim().is_empty()) {
                return Err(format!("{} is required", field.label));
            }
            if field.kind == FieldKind::Number
                && let Some(v) = config.get(field.key).map(|v| v.trim()).filter(|v| !v.is_empty())
                    && v.parse::<u32>().is_err() {
                        return Err(format!("{} must be a number", field.label));
                    }
        }
        Ok(())
    }

    /// One-line description of a configured bookmark ("deploy@web-1:2222").
    fn summary(&self, bookmark: &Bookmark) -> String;

    /// Opens a connection. Blocks; may prompt through `env.prompter`.
    fn connect(
        &self,
        bookmark: &Bookmark,
        env: &PluginEnv,
        cancel: &CancelToken,
    ) -> VfsResult<Arc<dyn Vfs>>;

    /// Where a fresh pane opens after connecting.
    fn initial_path(&self, bookmark: &Bookmark, vfs: &dyn Vfs) -> VPath {
        let start = bookmark.get("path");
        if start.is_empty() {
            vfs.home()
        } else if let Some(rest) = start.strip_prefix('~') {
            vfs.home().join(rest.trim_start_matches('/'))
        } else {
            vfs.home().join(start)
        }
    }
}

/// All plugins compiled into the application.
#[derive(Clone, Default)]
pub struct PluginRegistry {
    plugins: Vec<Arc<dyn ProviderPlugin>>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, plugin: Arc<dyn ProviderPlugin>) {
        self.plugins.retain(|p| p.id() != plugin.id());
        self.plugins.push(plugin);
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn ProviderPlugin>> {
        self.plugins.iter().find(|p| p.id() == id).cloned()
    }

    pub fn all(&self) -> &[Arc<dyn ProviderPlugin>] {
        &self.plugins
    }
}
