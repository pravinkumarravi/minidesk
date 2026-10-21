//! miniDesk self-hosted control plane, device registry, and signaling server.

use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use minidesk_domain::DeviceId;
use minidesk_logging::init_logging;
use minidesk_security::verify_peer_signature;
use minidesk_signaling_client::SignalingMessage;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::Level;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrolledDevice {
    pub device_id: DeviceId,
    pub friendly_name: String,
    pub is_revoked: bool,
    pub enrolled_at_unix: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollRequest {
    pub device_id: DeviceId,
    pub friendly_name: String,
    pub public_key: [u8; 32],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnCredentialsResponse {
    pub urls: Vec<String>,
    pub username: String,
    pub credential: String,
    pub ttl_secs: u64,
}

pub struct ServerState {
    pub devices: RwLock<HashMap<DeviceId, EnrolledDevice>>,
    pub connected_peers: RwLock<HashMap<DeviceId, mpsc::UnboundedSender<SignalingMessage>>>,
}

#[tokio::main]
async fn main() {
    init_logging(Level::INFO);
    tracing::info!("Starting miniDesk signaling & control plane server...");

    let state = Arc::new(ServerState {
        devices: RwLock::new(HashMap::new()),
        connected_peers: RwLock::new(HashMap::new()),
    });

    let app = create_app(state);
    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));
    tracing::info!("miniDesk server listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

pub fn create_app(state: Arc<ServerState>) -> Router {
    Router::new()
        .route("/health", get(health_check))
        .route("/api/v1/auth/enroll", post(enroll_device))
        .route("/api/v1/devices", get(list_devices))
        .route("/api/v1/devices/:id/revoke", post(revoke_device))
        .route("/api/v1/turn/credentials", get(get_turn_credentials))
        .route("/ws/signaling", get(signaling_ws_handler))
        .with_state(state)
}

async fn health_check() -> &'static str {
    "OK"
}

async fn enroll_device(
    State(state): State<Arc<ServerState>>,
    Json(payload): Json<EnrollRequest>,
) -> impl IntoResponse {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let device = EnrolledDevice {
        device_id: payload.device_id.clone(),
        friendly_name: payload.friendly_name,
        is_revoked: false,
        enrolled_at_unix: now,
    };

    let mut devices = state.devices.write().await;
    devices.insert(payload.device_id, device.clone());
    Json(device)
}

async fn list_devices(State(state): State<Arc<ServerState>>) -> impl IntoResponse {
    let devices = state.devices.read().await;
    let list: Vec<EnrolledDevice> = devices.values().cloned().collect();
    Json(list)
}

async fn revoke_device(
    State(state): State<Arc<ServerState>>,
    Path(device_id_str): Path<String>,
) -> impl IntoResponse {
    let dev_id = match DeviceId::new(device_id_str) {
        Ok(id) => id,
        Err(_) => return Json(serde_json::json!({ "success": false, "error": "Invalid DeviceId" })),
    };

    let mut devices = state.devices.write().await;
    if let Some(dev) = devices.get_mut(&dev_id) {
        dev.is_revoked = true;
        // Disconnect active socket if connected
        let mut peers = state.connected_peers.write().await;
        peers.remove(&dev_id);
        return Json(serde_json::json!({ "success": true }));
    }

    Json(serde_json::json!({ "success": false, "error": "Device not found" }))
}

async fn get_turn_credentials() -> Json<TurnCredentialsResponse> {
    Json(TurnCredentialsResponse {
        urls: vec![
            "stun:stun.l.google.com:19302".into(),
            "turn:turn.minidesk.local:3478".into(),
        ],
        username: "minidesk_ephemeral_user".into(),
        credential: "ephemeral_relay_token_12345".into(),
        ttl_secs: 86400,
    })
}

async fn signaling_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ServerState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_signaling_socket(socket, state))
}

async fn handle_signaling_socket(socket: WebSocket, state: Arc<ServerState>) {
    let (mut sender, mut receiver) = socket.split();
    let mut authed_device_id: Option<DeviceId> = None;
    let (client_tx, mut client_rx) = mpsc::unbounded_channel::<SignalingMessage>();

    // Forward outbound messages from channel to WebSocket
    let forward_task = tokio::spawn(async move {
        while let Some(msg) = client_rx.recv().await {
            if let Ok(json) = serde_json::to_string(&msg) {
                if sender.send(WsMessage::Text(json)).await.is_err() {
                    break;
                }
            }
        }
    });

    while let Some(Ok(msg)) = receiver.next().await {
        if let WsMessage::Text(text) = msg {
            if let Ok(sig_msg) = serde_json::from_str::<SignalingMessage>(&text) {
                match sig_msg {
                    SignalingMessage::Register {
                        device_id,
                        public_key,
                        signature,
                        timestamp_unix,
                    } => {
                        let challenge = format!("signaling-auth:{}:{}", device_id, timestamp_unix);
                        let mut sig_arr = [0u8; 64];
                        if signature.len() == 64 {
                            sig_arr.copy_from_slice(&signature);
                            if let Ok(verified_id) = verify_peer_signature(&public_key, challenge.as_bytes(), &sig_arr) {
                                if verified_id == device_id {
                                    authed_device_id = Some(device_id.clone());
                                    let mut peers = state.connected_peers.write().await;
                                    peers.insert(device_id.clone(), client_tx.clone());

                                    let _ = client_tx.send(SignalingMessage::Registered {
                                        device_id,
                                        server_version: "0.1.0".into(),
                                    });
                                    continue;
                                }
                            }
                        }
                        let _ = client_tx.send(SignalingMessage::Error {
                            message: "Authentication signature verification failed".into(),
                        });
                    }
                    SignalingMessage::Offer {
                        sender_device_id,
                        target_device_id,
                        session_id,
                        sdp,
                    } => {
                        if authed_device_id.as_ref() == Some(&sender_device_id) {
                            let peers = state.connected_peers.read().await;
                            if let Some(target_tx) = peers.get(&target_device_id) {
                                let _ = target_tx.send(SignalingMessage::Offer {
                                    sender_device_id,
                                    target_device_id,
                                    session_id,
                                    sdp,
                                });
                            }
                        }
                    }
                    SignalingMessage::Answer {
                        sender_device_id,
                        target_device_id,
                        session_id,
                        sdp,
                    } => {
                        if authed_device_id.as_ref() == Some(&sender_device_id) {
                            let peers = state.connected_peers.read().await;
                            if let Some(target_tx) = peers.get(&target_device_id) {
                                let _ = target_tx.send(SignalingMessage::Answer {
                                    sender_device_id,
                                    target_device_id,
                                    session_id,
                                    sdp,
                                });
                            }
                        }
                    }
                    SignalingMessage::IceCandidate {
                        sender_device_id,
                        target_device_id,
                        session_id,
                        candidate,
                    } => {
                        if authed_device_id.as_ref() == Some(&sender_device_id) {
                            let peers = state.connected_peers.read().await;
                            if let Some(target_tx) = peers.get(&target_device_id) {
                                let _ = target_tx.send(SignalingMessage::IceCandidate {
                                    sender_device_id,
                                    target_device_id,
                                    session_id,
                                    candidate,
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    forward_task.abort();
    if let Some(dev_id) = authed_device_id {
        let mut peers = state.connected_peers.write().await;
        peers.remove(&dev_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidesk_security::DeviceIdentity;
    use minidesk_signaling_client::SignalingClient;

    #[tokio::test]
    async fn test_server_device_enrollment_and_revocation() {
        let state = Arc::new(ServerState {
            devices: RwLock::new(HashMap::new()),
            connected_peers: RwLock::new(HashMap::new()),
        });

        let identity = DeviceIdentity::generate();
        let dev_id = identity.device_id().clone();

        // 1. Enroll
        let req = EnrollRequest {
            device_id: dev_id.clone(),
            friendly_name: "Test Lab Node".into(),
            public_key: identity.verifying_key().to_bytes(),
        };
        let _ = enroll_device(State(Arc::clone(&state)), Json(req)).await;

        // 2. Verify in devices list
        let devices_guard = state.devices.read().await;
        assert!(devices_guard.contains_key(&dev_id));
        assert_eq!(devices_guard.get(&dev_id).unwrap().friendly_name, "Test Lab Node");
        assert!(!devices_guard.get(&dev_id).unwrap().is_revoked);
        drop(devices_guard);

        // 3. Revoke
        let _ = revoke_device(State(Arc::clone(&state)), Path(dev_id.to_string())).await;
        let devices_guard = state.devices.read().await;
        assert!(devices_guard.get(&dev_id).unwrap().is_revoked);
    }

    #[tokio::test]
    async fn test_turn_credentials() {
        let resp = get_turn_credentials().await;
        let Json(creds) = resp;
        assert!(!creds.urls.is_empty());
        assert!(!creds.username.is_empty());
        assert!(!creds.credential.is_empty());
    }

    #[tokio::test]
    async fn test_signaling_router_alice_to_bob() {
        let state = Arc::new(ServerState {
            devices: RwLock::new(HashMap::new()),
            connected_peers: RwLock::new(HashMap::new()),
        });

        let app = create_app(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let ws_url = format!("ws://127.0.0.1:{}/ws/signaling", port);
        let alice_identity = DeviceIdentity::generate();
        let bob_identity = DeviceIdentity::generate();

        let mut alice_client = SignalingClient::connect(&ws_url, &alice_identity)
            .await
            .expect("Alice connect failed");

        let mut bob_client = SignalingClient::connect(&ws_url, &bob_identity)
            .await
            .expect("Bob connect failed");

        // Wait for Registered acks
        let alice_ack = alice_client.rx.recv().await.unwrap();
        match alice_ack {
            SignalingMessage::Registered { device_id, .. } => assert_eq!(device_id, *alice_identity.device_id()),
            other => panic!("Expected Registered, got {:?}", other),
        }

        let bob_ack = bob_client.rx.recv().await.unwrap();
        match bob_ack {
            SignalingMessage::Registered { device_id, .. } => assert_eq!(device_id, *bob_identity.device_id()),
            other => panic!("Expected Registered, got {:?}", other),
        }

        // Alice sends offer to Bob
        let session_id = minidesk_domain::SessionId::new();
        alice_client
            .send(SignalingMessage::Offer {
                sender_device_id: alice_identity.device_id().clone(),
                target_device_id: bob_identity.device_id().clone(),
                session_id,
                sdp: "v=0\r\no=alice_offer".into(),
            })
            .unwrap();

        // Bob receives offer
        let bob_received = bob_client.rx.recv().await.unwrap();
        match bob_received {
            SignalingMessage::Offer { sender_device_id, session_id: s_id, sdp, .. } => {
                assert_eq!(sender_device_id, *alice_identity.device_id());
                assert_eq!(s_id, session_id);
                assert_eq!(sdp, "v=0\r\no=alice_offer");
            }
            other => panic!("Expected Offer, got {:?}", other),
        }

        // Bob responds with Answer
        bob_client
            .send(SignalingMessage::Answer {
                sender_device_id: bob_identity.device_id().clone(),
                target_device_id: alice_identity.device_id().clone(),
                session_id,
                sdp: "v=0\r\no=bob_answer".into(),
            })
            .unwrap();

        // Alice receives Answer
        let alice_received = alice_client.rx.recv().await.unwrap();
        match alice_received {
            SignalingMessage::Answer { sender_device_id, sdp, .. } => {
                assert_eq!(sender_device_id, *bob_identity.device_id());
                assert_eq!(sdp, "v=0\r\no=bob_answer");
            }
            other => panic!("Expected Answer, got {:?}", other),
        }
    }
}
