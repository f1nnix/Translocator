//! S3 provider: Amazon S3 and anything that speaks its API (MinIO,
//! Cloudflare R2, Backblaze B2, DigitalOcean Spaces).
//!
//! A connection is an account rather than a single bucket, so the root of a
//! pane is the bucket list and a copy between two buckets is a server-side
//! `CopyObject` — the same rule the SSH plugin follows when both panes sit
//! on one server.

mod client;
mod keys;
mod tarstream;
mod vfs;

use std::sync::Arc;

use tl_core::plugin::FieldKind;
use tl_core::{
    Bookmark, BookmarkConfig, CancelToken, FieldSpec, PluginEnv, ProviderPlugin, VPath, Vfs,
    VfsError, VfsResult,
};

pub use client::S3Target;
pub use vfs::S3Vfs;

pub const PLUGIN_ID: &str = "s3";

pub struct S3Plugin;

impl ProviderPlugin for S3Plugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn name(&self) -> &'static str {
        "S3 (object storage)"
    }

    fn description(&self) -> &'static str {
        "Amazon S3 and compatible services such as MinIO, Cloudflare R2, Backblaze B2 and \
         DigitalOcean Spaces. The pane opens on the bucket list, and copying between two \
         buckets of one account happens inside the service, without the data passing through \
         this machine. Credentials come from a profile in ~/.aws/credentials, from the \
         AWS_* environment variables, or from the keys below."
    }

    fn fields(&self) -> Vec<FieldSpec> {
        vec![
            FieldSpec::text("endpoint", "Endpoint")
                .placeholder("blank for Amazon S3, else https://minio.example.com")
                .help("Any S3-compatible service. https:// is assumed when left off."),
            FieldSpec::text("region", "Region")
                .placeholder("us-east-1")
                .default_value("us-east-1"),
            FieldSpec::text("path", "Start folder")
                .placeholder("bucket or bucket/prefix (blank lists the buckets)"),
            FieldSpec::text("profile", "AWS profile")
                .placeholder("from ~/.aws/credentials")
                .help("Leave the keys below empty to use a profile, SSO or the environment."),
            FieldSpec::text("access_key", "Access key ID").placeholder("AKIA…"),
            FieldSpec::text("secret_key", "Secret access key").kind(FieldKind::Secret),
            FieldSpec::text("session_token", "Session token")
                .kind(FieldKind::Secret)
                .advanced(),
            FieldSpec::text("path_style", "Path-style addressing")
                .kind(FieldKind::Toggle)
                .help("On by default for custom endpoints, off for Amazon S3.")
                .advanced(),
        ]
    }

    fn validate(&self, config: &BookmarkConfig) -> Result<(), String> {
        let mut bookmark = Bookmark::new("", PLUGIN_ID);
        bookmark.config = config.clone();
        S3Target::from_bookmark(&bookmark).map(drop)
    }

    fn summary(&self, bookmark: &Bookmark) -> String {
        match S3Target::from_bookmark(bookmark) {
            Ok(target) => {
                let mut text = target.label();
                if !target.bucket.is_empty() {
                    text.push('/');
                    text.push_str(&target.bucket);
                    if !target.prefix.is_empty() {
                        text.push('/');
                        text.push_str(&target.prefix);
                    }
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

    /// [`S3Vfs::home`] already places the pane inside the configured bucket
    /// and prefix, so the usual "join the start path" step would apply it
    /// twice.
    fn initial_path(&self, _bookmark: &Bookmark, vfs: &dyn Vfs) -> VPath {
        vfs.home()
    }
}

pub fn connect(
    bookmark: &Bookmark,
    _env: &PluginEnv,
    cancel: &CancelToken,
) -> VfsResult<Arc<S3Vfs>> {
    let target = S3Target::from_bookmark(bookmark).map_err(VfsError::other)?;
    cancel.check()?;
    let conn = client::Blocking::connect(&target)?;
    let vfs = S3Vfs {
        id: format!("s3:{}", bookmark.id),
        key: target.location_key(),
        label: target.label(),
        target,
        conn,
    };
    // Fail now, with the service's own message, rather than when the first
    // pane tries to draw itself.
    vfs.check_reachable()?;
    Ok(Arc::new(vfs))
}
