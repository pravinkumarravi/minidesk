//! Bidirectional clipboard synchronization with capability gating and bounded memory.
//!
//! Enforces security bounds (maximum 1 MB text), echo-loop suppression via cryptographic
//! content hashing, and explicit capability authorization.

use arboard::Clipboard;
use minidesk_domain::CapabilitySet;
use minidesk_protocol::ClipboardMessage;
use sha2::{Digest, Sha256};
use std::sync::Mutex;
use thiserror::Error;

/// Maximum allowed clipboard text length in characters (1 MB of UTF-8).
pub const MAX_CLIPBOARD_TEXT_CHARS: usize = 1_000_000;

#[derive(Error, Debug)]
pub enum ClipboardSyncError {
    #[error("Clipboard access error: {0}")]
    ClipboardAccess(String),

    #[error("Clipboard sharing capability is not granted")]
    CapabilityDenied,

    #[error("Clipboard text length {0} exceeds maximum allowed {1}")]
    TextTooLarge(usize, usize),
}

/// Manages bidirectional clipboard exchange between local OS and remote peer.
pub struct ClipboardSynchronizer {
    clipboard: Option<Mutex<Clipboard>>,
    last_applied_hash: Option<[u8; 32]>,
    last_sent_hash: Option<[u8; 32]>,
}

impl ClipboardSynchronizer {
    pub fn new() -> Self {
        let clipboard = Clipboard::new().ok().map(Mutex::new);
        Self {
            clipboard,
            last_applied_hash: None,
            last_sent_hash: None,
        }
    }

    /// Checks local OS clipboard for user-initiated changes to sync to remote peer.
    /// Returns `Some(ClipboardMessage)` if a new local text change is detected.
    pub fn check_local_change(
        &mut self,
        capabilities: &CapabilitySet,
    ) -> Result<Option<ClipboardMessage>, ClipboardSyncError> {
        if !capabilities.contains(CapabilitySet::CLIPBOARD_READ) {
            return Ok(None);
        }

        let Some(ref mutex) = self.clipboard else {
            return Ok(None);
        };

        let mut clip = mutex.lock().unwrap();
        match clip.get_text() {
            Ok(text) => {
                if text.len() > MAX_CLIPBOARD_TEXT_CHARS {
                    return Ok(None); // Skip oversized payloads
                }

                let hash = compute_hash(&text);

                // Echo suppression: do not send if this was just applied from remote
                if Some(hash) == self.last_applied_hash {
                    return Ok(None);
                }

                // Do not re-send if already sent
                if Some(hash) == self.last_sent_hash {
                    return Ok(None);
                }

                self.last_sent_hash = Some(hash);
                Ok(Some(ClipboardMessage::Text(text)))
            }
            Err(_) => Ok(None),
        }
    }

    /// Applies an incoming remote clipboard message to the local OS clipboard.
    /// Enforces capability authorization and echo suppression.
    pub fn apply_remote(
        &mut self,
        capabilities: &CapabilitySet,
        msg: &ClipboardMessage,
    ) -> Result<bool, ClipboardSyncError> {
        if !capabilities.contains(CapabilitySet::CLIPBOARD_WRITE) {
            return Err(ClipboardSyncError::CapabilityDenied);
        }

        match msg {
            ClipboardMessage::Text(text) => {
                if text.len() > MAX_CLIPBOARD_TEXT_CHARS {
                    return Err(ClipboardSyncError::TextTooLarge(
                        text.len(),
                        MAX_CLIPBOARD_TEXT_CHARS,
                    ));
                }

                let hash = compute_hash(text);
                self.last_applied_hash = Some(hash);

                if let Some(ref mutex) = self.clipboard {
                    let mut clip = mutex.lock().unwrap();
                    let _ = clip.set_text(text.clone());
                }

                Ok(true)
            }
            ClipboardMessage::ImagePng(_) => {
                // Images reserved for future extension or platform-specific pasteboard
                Ok(false)
            }
        }
    }

    /// Explicitly resets hash states on session start/stop.
    pub fn reset(&mut self) {
        self.last_applied_hash = None;
        self.last_sent_hash = None;
    }
}

impl Default for ClipboardSynchronizer {
    fn default() -> Self {
        Self::new()
    }
}

fn compute_hash(text: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let result = hasher.finalize();
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&result);
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clipboard_capability_gating() {
        let mut sync = ClipboardSynchronizer::new();
        let disabled_caps = CapabilitySet::VIEW_ONLY;

        let msg = ClipboardMessage::Text("Secret Data".into());
        let res = sync.apply_remote(&disabled_caps, &msg);
        assert!(res.is_err());
        match res.unwrap_err() {
            ClipboardSyncError::CapabilityDenied => {}
            _ => panic!("Expected CapabilityDenied"),
        }
    }

    #[test]
    fn test_clipboard_payload_size_limit() {
        let mut sync = ClipboardSynchronizer::new();
        let enabled_caps = CapabilitySet::FULL_CONTROL;

        let oversized = "A".repeat(MAX_CLIPBOARD_TEXT_CHARS + 100);
        let msg = ClipboardMessage::Text(oversized);
        let res = sync.apply_remote(&enabled_caps, &msg);
        assert!(res.is_err());
        match res.unwrap_err() {
            ClipboardSyncError::TextTooLarge(..) => {}
            _ => panic!("Expected TextTooLarge"),
        }
    }

    #[test]
    fn test_echo_suppression_logic() {
        let mut sync = ClipboardSynchronizer::new();
        let enabled_caps = CapabilitySet::FULL_CONTROL;

        let text = "Hello remote desktop clipboard";
        let msg = ClipboardMessage::Text(text.into());
        let _ = sync.apply_remote(&enabled_caps, &msg);

        // After applying remote text, its hash must be recorded
        let expected_hash = compute_hash(text);
        assert_eq!(sync.last_applied_hash, Some(expected_hash));
    }
}
