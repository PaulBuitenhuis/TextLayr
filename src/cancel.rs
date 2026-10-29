//! A typed "the user asked to stop" error.
//!
//! Cancellation used to be detected by matching the substring `"cancelled"` in
//! an error message, which silently swallowed any unrelated failure that
//! happened to contain the word (a path, an ffmpeg log line). Workers that stop
//! because the user asked them to now return [`Cancelled`], and the UI asks
//! [`is_cancelled`] instead of looking at the text.

use std::fmt;

/// Returned by work that stopped because the user cancelled it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// Whether `err` is (or wraps) a [`Cancelled`].
///
/// Walks the whole `anyhow` chain, so a `Cancelled` that was contextualised on
/// its way up still counts.
pub fn is_cancelled(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|cause| cause.downcast_ref::<Cancelled>().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    #[test]
    fn a_cancelled_error_is_recognised_through_context() {
        let err = Err::<(), _>(Cancelled)
            .context("transcription failed")
            .unwrap_err();
        assert!(is_cancelled(&err));
    }

    #[test]
    fn a_message_that_merely_contains_the_word_is_not_a_cancel() {
        // The old substring check treated this as a user cancel and hid it.
        let err = anyhow::anyhow!("cannot write /tmp/cancelled-take.mp4");
        assert!(!is_cancelled(&err));
    }
}
