/// Lets providers ask the user something while a blocking operation runs on
/// a worker thread (SSH passwords, host key confirmation, sudo passwords).
///
/// Implementations block the calling thread until the user answers, so they
/// must never be called from the UI thread.
pub trait Prompter: Send + Sync {
    /// Asks for a secret (password, passphrase, OTP). `None` = cancelled.
    fn secret(&self, title: &str, message: &str) -> Option<String>;

    /// Asks a yes/no question. `false` = declined or cancelled.
    fn confirm(&self, title: &str, message: &str) -> bool;
}

/// A prompter that declines everything; for tests and headless use.
pub struct DenyPrompter;

impl Prompter for DenyPrompter {
    fn secret(&self, _title: &str, _message: &str) -> Option<String> {
        None
    }

    fn confirm(&self, _title: &str, _message: &str) -> bool {
        false
    }
}
