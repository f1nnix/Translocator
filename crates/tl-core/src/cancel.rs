use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use parking_lot::Mutex;

use crate::error::{VfsError, VfsResult};

type Hook = Box<dyn FnOnce() + Send>;

/// Cooperative cancellation shared between a job and the UI.
///
/// Besides the flag that loops poll, hooks can be registered to interrupt
/// blocking work (typically: kill a child process) the moment the user hits
/// Cancel.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<Inner>);

#[derive(Default)]
struct Inner {
    cancelled: AtomicBool,
    next_id: AtomicU64,
    hooks: Mutex<Vec<(u64, Hook)>>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        if self.0.cancelled.swap(true, Ordering::SeqCst) {
            return;
        }
        let hooks = std::mem::take(&mut *self.0.hooks.lock());
        for (_, hook) in hooks {
            hook();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    pub fn check(&self) -> VfsResult<()> {
        if self.is_cancelled() {
            Err(VfsError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Runs `hook` once if the token is cancelled while the returned guard is
    /// alive; runs it immediately if the token is already cancelled.
    #[must_use = "the hook is unregistered when the guard is dropped"]
    pub fn on_cancel(&self, hook: impl FnOnce() + Send + 'static) -> CancelGuard {
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut hooks = self.0.hooks.lock();
            if !self.is_cancelled() {
                hooks.push((id, Box::new(hook)));
                return CancelGuard {
                    token: self.clone(),
                    id,
                };
            }
        }
        hook();
        CancelGuard {
            token: self.clone(),
            id,
        }
    }
}

pub struct CancelGuard {
    token: CancelToken,
    id: u64,
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        self.token.0.hooks.lock().retain(|(id, _)| *id != self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn hooks_fire_once_and_unregister() {
        let token = CancelToken::new();
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        let guard = token.on_cancel(move || {
            h.fetch_add(1, Ordering::SeqCst);
        });
        let h2 = hits.clone();
        drop(token.on_cancel(move || {
            h2.fetch_add(10, Ordering::SeqCst);
        }));
        token.cancel();
        token.cancel();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        drop(guard);
        let h3 = hits.clone();
        let _late = token.on_cancel(move || {
            h3.fetch_add(100, Ordering::SeqCst);
        });
        assert_eq!(hits.load(Ordering::SeqCst), 101);
    }
}
