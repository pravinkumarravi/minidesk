//! WebSocket signaling client for miniDesk self-hosted internet connectivity.
//!
//! Exchanging SDP offers, answers, and ICE candidates with cryptographic peer identity verification.

use futures_util::{SinkExt, StreamExt};
use minidesk_domain::{DeviceId, SessionId};
use minidesk_security::DeviceIdentity;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;

#[derive(Error, Debug)]
pub enum SignalingError {
    #[error("WebSocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),

    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("Authentication failed: {0}")]
    Auth(String),

    #[error("Signaling channel closed")]
    Closed,
}

/// Messages routed through the self-hosted signaling server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SignalingMessage {
    /// Peer registration on the signaling server with signed identity.
    Register {
        device_id: DeviceId,
        public_key: [u8; 32],
        signature: Vec<u8>,
        timestamp_unix: i64,
    },
    /// Acknowledgment of registration.
    Registered {
        device_id: DeviceId,
        server_version: String,
    },
    /// Forward an SDP offer to a target device.
    Offer {
        sender_device_id: DeviceId,
        target_device_id: DeviceId,
        session_id: SessionId,
        sdp: String,
    },
    /// Forward an SDP answer back to the offering device.
    Answer {
        sender_device_id: DeviceId,
        target_device_id: DeviceId,
        session_id: SessionId,
        sdp: String,
    },
    /// Exchange an ICE candidate between peers.
    IceCandidate {
        sender_device_id: DeviceId,
        target_device_id: DeviceId,
        session_id: SessionId,
        candidate: String,
    },
    /// Signaling or routing error.
    Error {
        message: String,
    },
}

/// Client managing the live signaling connection to the server.
pub struct SignalingClient {
    tx: mpsc::UnboundedSender<SignalingMessage>,
    pub rx: mpsc::UnboundedReceiver<SignalingMessage>,
    pub local_device_id: DeviceId,
}

impl SignalingClient {
    /// Connects to the signaling server and registers the local device's identity.
    pub async fn connect(
        server_url: &str,
        identity: &DeviceIdentity,
    ) -> Result<Self, SignalingError> {
        let (ws_stream, _) = connect_async(server_url).await?;
        let (mut write, mut read) = ws_stream.split();

        let device_id = identity.device_id().clone();
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let challenge = format!("signaling-auth:{}:{}", device_id, now_unix);
        let sig = identity.sign(challenge.as_bytes()).to_vec();

        let register_msg = SignalingMessage::Register {
            device_id: device_id.clone(),
            public_key: identity.verifying_key().to_bytes(),
            signature: sig,
            timestamp_unix: now_unix,
        };

        let json = serde_json::to_string(&register_msg)?;
        write.send(WsMessage::Text(json)).await?;

        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<SignalingMessage>();
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel::<SignalingMessage>();

        // Background write loop
        tokio::spawn(async move {
            while let Some(msg) = outgoing_rx.recv().await {
                if let Ok(json) = serde_json::to_string(&msg) {
                    if write.send(WsMessage::Text(json)).await.is_err() {
                        break;
                    }
                }
            }
        });

        // Background read loop
        tokio::spawn(async move {
            while let Some(Ok(ws_msg)) = read.next().await {
                if let WsMessage::Text(text) = ws_msg {
                    if let Ok(msg) = serde_json::from_str::<SignalingMessage>(&text) {
                        if incoming_tx.send(msg).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        Ok(Self {
            tx: outgoing_tx,
            rx: incoming_rx,
            local_device_id: device_id,
        })
    }

    /// Send a signaling message to the server (e.g. Offer, Answer, IceCandidate).
    pub fn send(&self, msg: SignalingMessage) -> Result<(), SignalingError> {
        self.tx.send(msg).map_err(|_| SignalingError::Closed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signaling_message_serialization() {
        let session_id = SessionId::new();
        let dev1 = DeviceId::new("device_a_1234567890").unwrap();
        let dev2 = DeviceId::new("device_b_1234567890").unwrap();

        let offer = SignalingMessage::Offer {
            sender_device_id: dev1.clone(),
            target_device_id: dev2.clone(),
            session_id,
            sdp: "v=0\r\no=- 0 0 IN IP4 127.0.0.1".into(),
        };

        let json = serde_json::to_string(&offer).expect("Serialize failed");
        let deserialized: SignalingMessage = serde_json::from_str(&json).expect("Deserialize failed");
        assert_eq!(offer, deserialized);
    }
}
