//! WebDAV provider: Nextcloud, ownCloud, Apache `mod_dav`, rclone's
//! server, and anything else that speaks RFC 4918.
//!
//! Of the remote providers this is the closest to a real file system: it
//! has collections, and COPY and MOVE run on the server, so a transfer
//! inside one server never passes through this machine — the same promise
//! the SSH plugin makes with `cp` and `mv`.

mod client;
mod propfind;
mod tarstream;
mod vfs;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tl_core::plugin::FieldKind;
use tl_core::{
    Bookmark, BookmarkConfig, CancelToken, FieldSpec, OpCtx, PluginEnv, ProviderPlugin, VPath,
    Vfs, VfsError, VfsResult,
};

pub use client::DavTarget;
pub use vfs::DavVfs;

pub const PLUGIN_ID: &str = "webdav";

pub struct WebDavPlugin;

impl ProviderPlugin for WebDavPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn name(&self) -> &'static str {
        "WebDAV"
    }

    fn description(&self) -> &'static str {
        "Nextcloud, ownCloud, Apache mod_dav and anything else that speaks WebDAV. Copying \
         between two folders on one server happens there, without the data passing through \
         this machine. Nextcloud's address looks like \
         https://cloud.example.com/remote.php/dav/files/<user>."
    }

    fn fields(&self) -> Vec<FieldSpec> {
        vec![
            FieldSpec::text("url", "Server URL")
                .placeholder("https://cloud.example.com/remote.php/dav/files/me")
                .help("https:// is assumed when left off. A user and password in the URL are used.")
                .required(),
            FieldSpec::text("user", "User"),
            FieldSpec::text("password", "Password").kind(FieldKind::Secret),
            FieldSpec::text("path", "Start folder").placeholder("blank for the server's root"),
            FieldSpec::text("token", "Bearer token")
                .kind(FieldKind::Secret)
                .help("For servers that take an OAuth token instead of a password.")
                .advanced(),
            FieldSpec::text("insecure", "Trust any certificate")
                .kind(FieldKind::Toggle)
                .help("Only for a server whose certificate you know, such as one you run yourself.")
                .advanced(),
        ]
    }

    fn validate(&self, config: &BookmarkConfig) -> Result<(), String> {
        let mut bookmark = Bookmark::new("", PLUGIN_ID);
        bookmark.config = config.clone();
        DavTarget::from_bookmark(&bookmark).map(drop)
    }

    fn summary(&self, bookmark: &Bookmark) -> String {
        match DavTarget::from_bookmark(bookmark) {
            Ok(target) => {
                let mut text = target.label();
                if !target.start.is_empty() {
                    text.push('/');
                    text.push_str(&target.start);
                }
                text
            }
            Err(err) => err,
        }
    }

    fn connect(
        &self,
        bookmark: &Bookmark,
        env: &PluginEnv,
        cancel: &CancelToken,
    ) -> VfsResult<Arc<dyn Vfs>> {
        Ok(connect(bookmark, env, cancel)?)
    }

    /// [`DavVfs::home`] already places the pane at the start folder, so the
    /// usual "join the start path" step would apply it twice.
    fn initial_path(&self, _bookmark: &Bookmark, vfs: &dyn Vfs) -> VPath {
        vfs.home()
    }
}

pub fn connect(
    bookmark: &Bookmark,
    _env: &PluginEnv,
    cancel: &CancelToken,
) -> VfsResult<Arc<DavVfs>> {
    let target = DavTarget::from_bookmark(bookmark).map_err(VfsError::other)?;
    cancel.check()?;
    let dav = client::Dav::connect(&target)?;
    // Every href the server sends starts with the base URL's own path.
    let base_path = url::Url::parse(&target.base)
        .ok()
        .map(|u| client::percent_decode_bytes(u.path().trim_end_matches('/')))
        .unwrap_or_default();
    let vfs = DavVfs {
        id: format!("webdav:{}", bookmark.id),
        key: target.location_key(),
        label: target.label(),
        base_path,
        dav,
        target,
        deep_walk: AtomicBool::new(true),
    };
    // Fail now, with the server's own answer, rather than when the first
    // pane tries to draw itself.
    let home = vfs.home();
    if vfs.stat(&home, &OpCtx::new())?.is_none() {
        return Err(VfsError::NotFound(format!(
            "{home} does not exist on {}",
            vfs.label()
        )));
    }
    Ok(Arc::new(vfs))
}
