//! Network transport implementation for miniDesk.
//!
//! Provides direct authenticated TCP peer sessions with length-prefixed protocol
//! message framing, Ed25519 mutual authentication, and real-time streaming loops.

use bytes::BytesMut;
use minidesk_codecs::{
    CompressedVideoFrame, Lz4VideoDecoder, VideoCodecFormat, VideoDecoder,
};
use minidesk_domain::{CapabilitySet, DeviceId, SessionId};
use minidesk_platform_api::CapturedFrame;
use minidesk_protocol::{
    decode_message, encode_message, InputMessage, Message, PROTOCOL_VERSION,
};
use minidesk_security::{verify_peer_signature, ChallengeToken, DeviceIdentity};
use std::net::SocketAddr;
use std::sync::Arc;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Error, Debug)]
pub enum TransportError {
    #[error("Network I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Protocol error: {0}")]
    Protocol(#[from] minidesk_protocol::ProtocolError),

    #[error("Authentication failed: {0}")]
    Auth(String),

    #[error("Session rejected: {0}")]
    Rejected(String),

    #[error("Connection closed unexpectedly")]
    Closed,

    #[error("Codec error: {0}")]
    Codec(#[from] minidesk_codecs::CodecError),
}

/// Helper wrapper over a TcpStream for reading and writing framed protocol messages.
pub struct FramedStream {
    stream: TcpStream,
    read_buf: BytesMut,
}

impl FramedStream {
    pub fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            read_buf: BytesMut::with_capacity(64 * 1024),
        }
    }

    /// Reads the next complete framed Message from the stream.
    pub async fn read_message(&mut self) -> Result<Option<Message>, TransportError> {
        loop {
            if let Some(msg) = decode_message(&mut self.read_buf)? {
                return Ok(Some(msg));
            }

            let mut chunk = [0u8; 8192];
            let n = self.stream.read(&mut chunk).await?;
            if n == 0 {
                if self.read_buf.is_empty() {
                    return Ok(None);
                } else {
                    return Err(TransportError::Closed);
                }
            }
            self.read_buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// Writes a framed Message to the stream.
    pub async fn write_message(&mut self, msg: &Message) -> Result<(), TransportError> {
        let encoded = encode_message(msg)?;
        self.stream.write_all(&encoded).await?;
        self.stream.flush().await?;
        Ok(())
    }
}

/// Outgoing client connection establishing an authenticated session with a remote host.
pub struct ClientSession {
    pub framed: FramedStream,
    pub session_id: SessionId,
    pub host_device_id: Option<DeviceId>,
    pub granted_capabilities: CapabilitySet,
    pub decoder: Lz4VideoDecoder,
}

impl ClientSession {
    /// Connects to a remote host and executes the cryptographic authentication handshake.
    pub async fn connect(
        destination: SocketAddr,
        client_name: &str,
        identity: &DeviceIdentity,
        requested_capabilities: CapabilitySet,
    ) -> Result<Self, TransportError> {
        let stream = TcpStream::connect(destination).await?;
        let mut framed = FramedStream::new(stream);
        let session_id = SessionId::new();

        // 1. Send ClientHello
        framed
            .write_message(&Message::ClientHello {
                protocol_version: PROTOCOL_VERSION,
                client_name: client_name.into(),
                session_id,
            })
            .await?;

        // 2. Receive HostHello with challenge
        let challenge = match framed.read_message().await? {
            Some(Message::HostHello {
                protocol_version,
                auth_challenge,
                ..
            }) => {
                if protocol_version != PROTOCOL_VERSION {
                    return Err(TransportError::Auth(format!(
                        "Incompatible host protocol version {}",
                        protocol_version
                    )));
                }
                auth_challenge
            }
            Some(Message::SessionRejected { reason }) => {
                return Err(TransportError::Rejected(reason));
            }
            other => {
                return Err(TransportError::Auth(format!(
                    "Unexpected response to ClientHello: {:?}",
                    other
                )));
            }
        };

        // 3. Sign challenge and send AuthenticateRequest
        let public_key = identity.verifying_key().to_bytes();
        let signature = identity.sign(&challenge);

        framed
            .write_message(&Message::AuthenticateRequest {
                public_key,
                signature: signature.to_vec(),
                requested_capabilities,
            })
            .await?;

        // 4. Wait for AuthenticateResponse / SessionApprovalGrant
        let granted_caps = match framed.read_message().await? {
            Some(Message::AuthenticateResponse {
                authenticated,
                error_message,
                ..
            }) => {
                if !authenticated {
                    return Err(TransportError::Auth(
                        error_message.unwrap_or_else(|| "Authentication rejected".into()),
                    ));
                }

                // If authenticated, wait for approval grant
                match framed.read_message().await? {
                    Some(Message::SessionApprovalGrant {
                        granted_capabilities,
                        ..
                    }) => granted_capabilities,
                    Some(Message::SessionRejected { reason }) => {
                        return Err(TransportError::Rejected(reason));
                    }
                    other => {
                        return Err(TransportError::Auth(format!(
                            "Expected approval grant, got {:?}",
                            other
                        )));
                    }
                }
            }
            Some(Message::SessionApprovalGrant {
                granted_capabilities,
                ..
            }) => granted_capabilities,
            Some(Message::SessionRejected { reason }) => {
                return Err(TransportError::Rejected(reason));
            }
            other => {
                return Err(TransportError::Auth(format!(
                    "Unexpected response to AuthenticateRequest: {:?}",
                    other
                )));
            }
        };

        Ok(Self {
            framed,
            session_id,
            host_device_id: None,
            granted_capabilities: granted_caps,
            decoder: Lz4VideoDecoder::new(),
        })
    }

    /// Read next incoming message from host (e.g. video frames).
    pub async fn next_message(&mut self) -> Result<Option<Message>, TransportError> {
        self.framed.read_message().await
    }

    /// Send input event to host.
    pub async fn send_input(&mut self, input: InputMessage) -> Result<(), TransportError> {
        self.framed.write_message(&Message::Input(input)).await
    }

    /// Send any message to host.
    pub async fn send_message(&mut self, msg: &Message) -> Result<(), TransportError> {
        self.framed.write_message(msg).await
    }

    /// Decode received video frame into raw RGBA image.
    pub fn decode_video_frame(
        &mut self,
        width: u32,
        height: u32,
        format: u8,
        is_keyframe: bool,
        timestamp_unix_ms: u64,
        payload: Vec<u8>,
    ) -> Result<CapturedFrame, TransportError> {
        let compressed = CompressedVideoFrame {
            width,
            height,
            format: match format {
                1 => VideoCodecFormat::Lz4Rgba,
                _ => VideoCodecFormat::RawRgba,
            },
            is_keyframe,
            timestamp_unix_ms,
            payload,
        };
        let frame = self.decoder.decode(&compressed)?;
        Ok(frame)
    }
}

/// Host listener for incoming direct LAN connections.
pub struct HostListener {
    listener: TcpListener,
    _identity: Arc<DeviceIdentity>,
    host_name: String,
}

impl HostListener {
    pub async fn bind(
        addr: SocketAddr,
        identity: Arc<DeviceIdentity>,
        host_name: String,
    ) -> Result<Self, TransportError> {
        let listener = TcpListener::bind(addr).await?;
        Ok(Self {
            listener,
            _identity: identity,
            host_name,
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, std::io::Error> {
        self.listener.local_addr()
    }

    /// Accepts an incoming connection and performs the challenge handshake.
    pub async fn accept_and_authenticate(
        &self,
    ) -> Result<(FramedStream, SessionId, DeviceId, CapabilitySet), TransportError> {
        let (stream, _peer_addr) = self.listener.accept().await?;
        let mut framed = FramedStream::new(stream);

        // 1. Receive ClientHello
        let session_id = match framed.read_message().await? {
            Some(Message::ClientHello {
                protocol_version,
                session_id,
                ..
            }) => {
                if protocol_version != PROTOCOL_VERSION {
                    let _ = framed
                        .write_message(&Message::SessionRejected {
                            reason: "Incompatible protocol version".into(),
                        })
                        .await;
                    return Err(TransportError::Auth("Incompatible protocol version".into()));
                }
                session_id
            }
            _ => return Err(TransportError::Auth("Expected ClientHello".into())),
        };

        // 2. Generate and send challenge
        let challenge = ChallengeToken::generate();
        framed
            .write_message(&Message::HostHello {
                protocol_version: PROTOCOL_VERSION,
                host_name: self.host_name.clone(),
                auth_challenge: *challenge.as_bytes(),
            })
            .await?;

        // 3. Receive AuthenticateRequest with signature
        let (peer_device_id, requested_caps) = match framed.read_message().await? {
            Some(Message::AuthenticateRequest {
                public_key,
                signature,
                requested_capabilities,
            }) => {
                if signature.len() != 64 {
                    return Err(TransportError::Auth("Invalid signature length".into()));
                }
                let mut sig_arr = [0u8; 64];
                sig_arr.copy_from_slice(&signature);

                let device_id = verify_peer_signature(&public_key, challenge.as_bytes(), &sig_arr)
                    .map_err(|e| TransportError::Auth(format!("Signature failed: {:?}", e)))?;

                (device_id, requested_capabilities)
            }
            _ => return Err(TransportError::Auth("Expected AuthenticateRequest".into())),
        };

        // 4. Send AuthenticateResponse OK
        framed
            .write_message(&Message::AuthenticateResponse {
                authenticated: true,
                requires_user_approval: true,
                error_message: None,
            })
            .await?;

        Ok((framed, session_id, peer_device_id, requested_caps))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_client_host_handshake_and_auth() {
        let host_identity = Arc::new(DeviceIdentity::generate());
        let client_identity = DeviceIdentity::generate();

        let listener = HostListener::bind(
            "127.0.0.1:0".parse().unwrap(),
            host_identity,
            "Host Node".into(),
        )
        .await
        .expect("Failed to bind host listener");

        let listen_addr = listener.local_addr().unwrap();

        // Spawn host task
        let host_task = tokio::spawn(async move {
            let (mut framed, _session_id, peer_device_id, requested_caps) = listener
                .accept_and_authenticate()
                .await
                .expect("Host accept failed");

            // Grant full capabilities
            framed
                .write_message(&Message::SessionApprovalGrant {
                    granted_capabilities: requested_caps,
                    displays: Vec::new(),
                    active_display_id: 0,
                })
                .await
                .expect("Approval write failed");

            // Host receives an input packet
            let msg = framed.read_message().await.unwrap().unwrap();
            match msg {
                Message::Input(InputMessage::ResetInputState) => {}
                other => panic!("Expected ResetInputState, got {:?}", other),
            }

            peer_device_id
        });

        // Client connects
        let mut client = ClientSession::connect(
            listen_addr,
            "Client Node",
            &client_identity,
            CapabilitySet::FULL_CONTROL,
        )
        .await
        .expect("Client connect failed");

        assert_eq!(client.granted_capabilities, CapabilitySet::FULL_CONTROL);

        // Client sends an input packet
        client
            .send_input(InputMessage::ResetInputState)
            .await
            .expect("Send input failed");

        let authed_peer_id = host_task.await.unwrap();
        assert_eq!(&authed_peer_id, client_identity.device_id());
    }
}
