//! Turning a bookmark into an S3 client, and the blocking bridge.
//!
//! The AWS SDK is async; the [`Vfs`](tl_core::Vfs) API is blocking and runs
//! on worker threads. Each connection therefore owns a small Tokio runtime
//! and every operation goes through [`Blocking::block`].

use std::sync::Arc;
use std::time::Duration;

use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::error::SdkError;
use aws_smithy_http_client::{Builder as HttpBuilder, tls};
use tl_core::{Bookmark, VfsError, VfsResult};

/// How a connection proves who it is, and what makes two connections able
/// to copy between each other without the data leaving the service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Identity {
    Profile(String),
    AccessKey(String),
    /// From the environment or an instance role.
    Default,
}

impl Identity {
    fn as_key(&self) -> &str {
        match self {
            Identity::Profile(name) => name,
            Identity::AccessKey(id) => id,
            Identity::Default => "default",
        }
    }
}

/// Everything a bookmark says about where and how to connect.
#[derive(Clone, Debug)]
pub struct S3Target {
    pub endpoint: Option<String>,
    pub region: String,
    pub identity: Identity,
    pub path_style: bool,
    /// Bucket the pane opens in; empty means start at the bucket list.
    pub bucket: String,
    pub prefix: String,
    secret: Option<(String, String, Option<String>)>,
}

impl S3Target {
    pub fn from_bookmark(bookmark: &Bookmark) -> Result<Self, String> {
        let endpoint = Some(bookmark.get("endpoint").trim().to_string())
            .filter(|e| !e.is_empty())
            .map(|e| {
                if e.contains("://") {
                    e
                } else {
                    format!("https://{e}")
                }
            });
        if let Some(url) = &endpoint
            && url::Url::parse(url).is_err() {
                return Err(format!("Endpoint “{url}” is not a valid URL"));
            }
        let region = match bookmark.get("region") {
            "" => "us-east-1".to_string(),
            r => r.to_string(),
        };
        let (key, secret, token) = (
            bookmark.get("access_key").to_string(),
            bookmark.get("secret_key").to_string(),
            bookmark.get("session_token").to_string(),
        );
        if key.is_empty() != secret.is_empty() {
            return Err("Give both an access key and a secret key, or neither".into());
        }
        let profile = bookmark.get("profile").to_string();
        if !key.is_empty() && !profile.is_empty() {
            return Err("Use either a profile or an access key, not both".into());
        }
        let identity = if !key.is_empty() {
            Identity::AccessKey(key.clone())
        } else if !profile.is_empty() {
            Identity::Profile(profile)
        } else {
            Identity::Default
        };
        // A custom endpoint almost always means MinIO, R2 or similar, where
        // virtual-host addressing is not set up.
        let path_style = if bookmark.config.contains_key("path_style") {
            bookmark.flag("path_style")
        } else {
            endpoint.is_some()
        };
        let start = bookmark.get("path").trim().trim_start_matches('/').to_string();
        let (bucket, prefix) = match start.split_once('/') {
            Some((b, p)) => (b.to_string(), p.trim_end_matches('/').to_string()),
            None => (start, String::new()),
        };
        Ok(S3Target {
            endpoint,
            region,
            identity,
            path_style,
            bucket,
            prefix,
            secret: (!key.is_empty()).then(|| (key, secret, (!token.is_empty()).then_some(token))),
        })
    }

    /// Two connections with the same key can copy server-side, because one
    /// set of credentials can read the source and write the destination.
    pub fn location_key(&self) -> String {
        format!(
            "s3://{}@{}/{}",
            self.identity.as_key(),
            self.endpoint.as_deref().unwrap_or("amazonaws.com"),
            self.region
        )
    }

    pub fn label(&self) -> String {
        let host = match &self.endpoint {
            Some(url) => url::Url::parse(url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .unwrap_or_else(|| url.clone()),
            None => format!("s3.{}.amazonaws.com", self.region),
        };
        match &self.identity {
            Identity::Profile(name) => format!("{name}@{host}"),
            _ => host,
        }
    }
}

/// A connection: the SDK client plus the runtime its futures run on.
pub struct Blocking {
    runtime: Arc<tokio::runtime::Runtime>,
    pub client: aws_sdk_s3::Client,
}

impl Blocking {
    pub fn connect(target: &S3Target) -> VfsResult<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| VfsError::other(format!("cannot start the S3 runtime: {e}")))?;

        // rustls over ring: see the note in Cargo.toml.
        let http = HttpBuilder::new()
            .tls_provider(tls::Provider::Rustls(tls::rustls_provider::CryptoMode::Ring))
            .build_https();

        let target = target.clone();
        let client = runtime.block_on(async move {
            let mut loader = aws_config::defaults(BehaviorVersion::latest())
                .http_client(http)
                .region(Region::new(target.region.clone()));
            match (&target.identity, &target.secret) {
                (Identity::AccessKey(_), Some((key, secret, token))) => {
                    loader = loader.credentials_provider(Credentials::new(
                        key.clone(),
                        secret.clone(),
                        token.clone(),
                        None,
                        "translocator-bookmark",
                    ));
                }
                (Identity::Profile(name), _) => loader = loader.profile_name(name),
                _ => {}
            }
            let shared = loader.load().await;
            let mut builder = aws_sdk_s3::config::Builder::from(&shared)
                .force_path_style(target.path_style)
                // A dead endpoint should fail quickly, not hang a pane.
                .stalled_stream_protection(
                    aws_sdk_s3::config::StalledStreamProtectionConfig::enabled()
                        .grace_period(Duration::from_secs(30))
                        .build(),
                );
            if let Some(endpoint) = &target.endpoint {
                builder = builder.endpoint_url(endpoint);
            }
            aws_sdk_s3::Client::from_conf(builder.build())
        });
        Ok(Blocking {
            runtime: Arc::new(runtime),
            client,
        })
    }

    /// Runs a future to completion on this connection's runtime.
    pub fn block<F: std::future::Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }

    /// A handle for worker threads (tar streaming) that need the runtime.
    pub fn handle(&self) -> Worker {
        Worker {
            runtime: self.runtime.clone(),
            client: self.client.clone(),
        }
    }
}

/// The parts of a connection a helper thread needs.
#[derive(Clone)]
pub struct Worker {
    runtime: Arc<tokio::runtime::Runtime>,
    pub client: aws_sdk_s3::Client,
}

impl Worker {
    pub fn block<F: std::future::Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }
}

/// Turns an SDK error into something the app can act on, keeping the
/// service's own message where there is one.
pub fn map_err<E, R>(what: &str, err: SdkError<E, R>) -> VfsError
where
    E: std::error::Error + aws_sdk_s3::error::ProvideErrorMetadata,
{
    // Only a service error carries S3's own code and message; a transport
    // failure has to be described from the variant itself.
    let service = err.as_service_error();
    let code = service
        .and_then(|e| e.code())
        .unwrap_or_default()
        .to_string();
    let message = service
        .and_then(|e| e.message())
        .map(str::to_string)
        .unwrap_or_else(|| match &err {
            SdkError::ServiceError(_) => "the service rejected the request".into(),
            SdkError::TimeoutError(_) => "timed out".into(),
            SdkError::DispatchFailure(_) => "cannot reach the service".into(),
            other => other.to_string(),
        });
    let detail = if code.is_empty() {
        format!("{what}: {message}")
    } else {
        format!("{what}: {message} ({code})")
    };
    match code.as_str() {
        "NoSuchKey" | "NoSuchBucket" | "NotFound" | "404" => VfsError::NotFound(detail),
        "AccessDenied" | "InvalidAccessKeyId" | "SignatureDoesNotMatch" | "403" => {
            VfsError::PermissionDenied(detail)
        }
        "BucketAlreadyExists" | "BucketAlreadyOwnedByYou" => VfsError::AlreadyExists(detail),
        "ExpiredToken" | "InvalidToken" => VfsError::Auth(detail),
        _ => match &err {
            SdkError::DispatchFailure(_) | SdkError::TimeoutError(_) => {
                VfsError::Connection(detail)
            }
            _ => VfsError::other(detail),
        },
    }
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
    fn endpoints_and_defaults() {
        let plain = S3Target::from_bookmark(&bookmark(&[])).unwrap();
        assert_eq!(plain.endpoint, None);
        assert_eq!(plain.region, "us-east-1");
        assert_eq!(plain.identity, Identity::Default);
        // Amazon S3 uses virtual-host addressing.
        assert!(!plain.path_style);

        // A bare host gets a scheme, and a custom endpoint implies
        // path-style, which is what MinIO and friends need.
        let minio = S3Target::from_bookmark(&bookmark(&[("endpoint", "minio.example.com:9000")])).unwrap();
        assert_eq!(minio.endpoint.as_deref(), Some("https://minio.example.com:9000"));
        assert!(minio.path_style);

        // An explicit toggle still wins.
        let forced = S3Target::from_bookmark(&bookmark(&[
            ("endpoint", "http://localhost:9000"),
            ("path_style", "false"),
        ]))
        .unwrap();
        assert!(!forced.path_style);
    }

    #[test]
    fn start_path_splits_into_bucket_and_prefix() {
        let whole = S3Target::from_bookmark(&bookmark(&[("path", "/media/photos/2026")])).unwrap();
        assert_eq!(whole.bucket, "media");
        assert_eq!(whole.prefix, "photos/2026");

        let bucket_only = S3Target::from_bookmark(&bookmark(&[("path", "media")])).unwrap();
        assert_eq!(bucket_only.bucket, "media");
        assert_eq!(bucket_only.prefix, "");

        let none = S3Target::from_bookmark(&bookmark(&[])).unwrap();
        assert_eq!(none.bucket, "");
    }

    #[test]
    fn credentials_must_make_sense() {
        assert!(S3Target::from_bookmark(&bookmark(&[("access_key", "AKIA")])).is_err());
        assert!(S3Target::from_bookmark(&bookmark(&[("secret_key", "s")])).is_err());
        assert!(
            S3Target::from_bookmark(&bookmark(&[
                ("access_key", "AKIA"),
                ("secret_key", "s"),
                ("profile", "work"),
            ]))
            .is_err(),
            "a profile and a key are two different answers to the same question"
        );
        let keys = S3Target::from_bookmark(&bookmark(&[("access_key", "AKIA"), ("secret_key", "s")])).unwrap();
        assert_eq!(keys.identity, Identity::AccessKey("AKIA".into()));
        let profile = S3Target::from_bookmark(&bookmark(&[("profile", "work")])).unwrap();
        assert_eq!(profile.identity, Identity::Profile("work".into()));
    }

    #[test]
    fn location_key_decides_what_copies_server_side() {
        let key = |pairs: &[(&str, &str)]| S3Target::from_bookmark(&bookmark(pairs)).unwrap().location_key();
        let aws = key(&[("access_key", "AKIA"), ("secret_key", "s")]);
        // Two buckets reached with the same credentials are one location,
        // so the engine copies inside the service.
        assert_eq!(aws, key(&[("access_key", "AKIA"), ("secret_key", "s"), ("path", "other-bucket")]));
        // Different credentials, endpoint or region are not.
        assert_ne!(aws, key(&[("access_key", "AKIB"), ("secret_key", "s")]));
        assert_ne!(aws, key(&[("access_key", "AKIA"), ("secret_key", "s"), ("region", "eu-west-1")]));
        assert_ne!(aws, key(&[("access_key", "AKIA"), ("secret_key", "s"), ("endpoint", "minio.example.com")]));
    }
}
