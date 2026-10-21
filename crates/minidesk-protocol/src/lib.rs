//! Protocol messages, framing, and serialization for miniDesk.
//!
//! Provides strictly typed, versioned wire messages for control, media, input,
//! clipboard, and file transfer.

use bytes::{Buf, BufMut, BytesMut};
use minidesk_domain::{CapabilitySet, DisplayInfo, SessionId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Protocol version 1.
pub const PROTOCOL_VERSION: u32 = 1;

/// Maximum single packet size allowed over the control channel (64 KB).
pub const MAX_CONTROL_PACKET_SIZE: usize = 64 * 1024;

/// Maximum payload size allowed for clipboard or chunk buffers (16 MB).
pub const MAX_PAYLOAD_PACKET_SIZE: usize = 16 * 1024 * 1024;

#[derive(Error, Debug)]
pub enum ProtocolError {
    #[error("Framing error: packet size {0} exceeds limit {1}")]
    PacketTooLarge(usize, usize),

    #[error("Incomplete packet frame")]
    IncompleteFrame,

    #[error("Serialization / Deserialization error: {0}")]
    Codec(#[from] bincode::Error),

    #[error("Unsupported protocol version: {0}")]
    UnsupportedVersion(u32),

    #[error("Invalid message format: {0}")]
    InvalidFormat(String),
}

/// Top-level control and session messages exchanged between endpoints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Message {
    /// Client initiates connection and offers version.
    ClientHello {
        protocol_version: u32,
        client_name: String,
        session_id: SessionId,
    },

    /// Host responds with challenge token for mutual Ed25519 authentication.
    HostHello {
        protocol_version: u32,
        host_name: String,
        auth_challenge: [u8; 32],
    },

    /// Client responds with its public key and signature over the challenge.
    AuthenticateRequest {
        public_key: [u8; 32],
        signature: Vec<u8>,
        requested_capabilities: CapabilitySet,
    },

    /// Host responds with authentication outcome and approval requirement.
    AuthenticateResponse {
        authenticated: bool,
        requires_user_approval: bool,
        error_message: Option<String>,
    },

    /// Host sends granted capabilities once accepted (either interactively or unattended).
    SessionApprovalGrant {
        granted_capabilities: CapabilitySet,
        displays: Vec<DisplayInfo>,
        active_display_id: u32,
    },

    /// Host explicitly rejects the connection.
    SessionRejected {
        reason: String,
    },

    /// Real-time video frame streamed from host to viewer.
    VideoFrame {
        width: u32,
        height: u32,
        format: u8,
        is_keyframe: bool,
        timestamp_unix_ms: u64,
        payload: Vec<u8>,
    },

    /// Input event forwarded to the host (mouse / keyboard).
    Input(InputMessage),

    /// Clipboard content synchronization.
    Clipboard(ClipboardMessage),

    /// File transfer coordination.
    FileTransfer(FileTransferMessage),

    /// Remote audio packet stream.
    Audio(AudioPacket),

    /// Display change / monitor switch request.
    SwitchMonitor {
        display_id: u32,
    },

    /// Heartbeat keep-alive.
    Heartbeat {
        sequence: u64,
        timestamp_unix_ms: u64,
    },

    /// Clean session termination.
    CloseSession {
        reason: String,
    },
}

/// Remote input events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InputMessage {
    MouseMove {
        x: f32, // Normalized coordinates (0.0 to 1.0) or display pixels
        y: f32,
    },
    MouseDown {
        button: MouseButton,
    },
    MouseUp {
        button: MouseButton,
    },
    MouseScroll {
        delta_x: f32,
        delta_y: f32,
    },
    KeyDown {
        key_code: u32,
        modifiers: KeyModifiers,
    },
    KeyUp {
        key_code: u32,
        modifiers: KeyModifiers,
    },
    /// Emergency release of all pressed keys/buttons on blur or disconnect.
    ResetInputState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    Back,
    Forward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct KeyModifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool, // Windows key / Command key
}

/// Clipboard payload format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClipboardMessage {
    Text(String),
    ImagePng(Vec<u8>),
}

/// File transfer coordination messages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FileTransferMessage {
    /// Initiate a transfer request.
    Request {
        transfer_id: u64,
        filename: String,
        file_size_bytes: u64,
        sha256_hash: [u8; 32],
    },
    /// Host approval response.
    Response {
        transfer_id: u64,
        accepted: bool,
        rejection_reason: Option<String>,
    },
    /// Stream of data chunks.
    Chunk {
        transfer_id: u64,
        chunk_index: u64,
        data: Vec<u8>,
    },
    /// Acknowledge receipt of a chunk for flow control.
    Ack {
        transfer_id: u64,
        chunk_index: u64,
    },
    /// Cancel an active transfer.
    Cancel {
        transfer_id: u64,
        reason: String,
    },
    /// Signal completion and request integrity verification.
    Complete {
        transfer_id: u64,
    },
}

/// Remote audio packet payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioPacket {
    pub sample_rate: u32,
    pub channels: u16,
    pub timestamp_unix_ms: u64,
    pub data: Vec<u8>,
}

/// Wire framing helper: encodes a message with a 4-byte BE length prefix.
pub fn encode_message(msg: &Message) -> Result<Vec<u8>, ProtocolError> {
    let payload = bincode::serialize(msg)?;
    if payload.len() > MAX_PAYLOAD_PACKET_SIZE {
        return Err(ProtocolError::PacketTooLarge(
            payload.len(),
            MAX_PAYLOAD_PACKET_SIZE,
        ));
    }

    let mut buf = Vec::with_capacity(4 + payload.len());
    buf.put_u32(payload.len() as u32);
    buf.extend_from_slice(&payload);
    Ok(buf)
}

/// Wire framing helper: decodes a length-prefixed frame from a buffer.
pub fn decode_message(buf: &mut BytesMut) -> Result<Option<Message>, ProtocolError> {
    if buf.len() < 4 {
        return Ok(None);
    }

    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if len > MAX_PAYLOAD_PACKET_SIZE {
        return Err(ProtocolError::PacketTooLarge(len, MAX_PAYLOAD_PACKET_SIZE));
    }

    if buf.len() < 4 + len {
        return Ok(None); // Need more bytes
    }

    buf.advance(4);
    let payload = buf.split_to(len);
    let msg: Message = bincode::deserialize(&payload)?;
    Ok(Some(msg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_roundtrip() {
        let msg = Message::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "TestClient".into(),
            session_id: SessionId::new(),
        };

        let encoded = encode_message(&msg).expect("Encoding failed");
        let mut bytes_mut = BytesMut::from(&encoded[..]);

        let decoded = decode_message(&mut bytes_mut)
            .expect("Decoding failed")
            .expect("Expected complete message");

        assert_eq!(msg, decoded);
        assert!(bytes_mut.is_empty());
    }

    #[test]
    fn test_input_reset_message() {
        let msg = Message::Input(InputMessage::ResetInputState);
        let encoded = encode_message(&msg).unwrap();
        let mut bytes_mut = BytesMut::from(&encoded[..]);
        let decoded = decode_message(&mut bytes_mut).unwrap().unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_oversized_packet_rejection() {
        let mut fake_buf = BytesMut::new();
        // Claim length is 32 MB (exceeds MAX_PAYLOAD_PACKET_SIZE 16 MB)
        fake_buf.put_u32(32 * 1024 * 1024);
        fake_buf.extend_from_slice(&[0u8; 100]);

        let res = decode_message(&mut fake_buf);
        assert!(res.is_err());
        match res.unwrap_err() {
            ProtocolError::PacketTooLarge(len, max) => {
                assert_eq!(len, 32 * 1024 * 1024);
                assert_eq!(max, MAX_PAYLOAD_PACKET_SIZE);
            }
            other => panic!("Expected PacketTooLarge, got {:?}", other),
        }
    }

    #[test]
    fn test_corrupted_payload_handling() {
        let mut buf = BytesMut::new();
        buf.put_u32(8);
        buf.extend_from_slice(&[0xFF, 0xEE, 0xDD, 0xCC, 0xBB, 0xAA, 0x99, 0x88]);

        let res = decode_message(&mut buf);
        assert!(res.is_err());
        match res.unwrap_err() {
            ProtocolError::Codec(_) => {}
            other => panic!("Expected Codec error, got {:?}", other),
        }
    }

    #[test]
    fn test_protocol_pseudo_fuzzing_no_panics() {
        // Feed various random and adversarial byte sequences
        let seed_patterns: Vec<Vec<u8>> = vec![
            vec![],
            vec![0],
            vec![0, 0],
            vec![0, 0, 0],
            vec![0, 0, 0, 1],
            vec![0, 0, 0, 1, 0xFF],
            vec![0xFF; 16],
            vec![0xAA; 1024],
            vec![0x00; 256],
        ];

        for pattern in seed_patterns {
            let mut buf = BytesMut::from(&pattern[..]);
            // Must not panic under any permutation
            let _ = decode_message(&mut buf);
        }
    }
}
