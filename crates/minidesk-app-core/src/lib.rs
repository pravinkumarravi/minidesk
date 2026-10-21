//! Application core orchestrator for miniDesk.
//!
//! Manages connection lifecycles, state machine transitions, incoming session requests,
//! capability grants, host streaming, client remote viewing, and UI event synchronization.

use minidesk_clipboard::ClipboardSynchronizer;
use minidesk_codecs::{Lz4VideoEncoder, VideoEncoder};
use minidesk_config::AppConfig;
use minidesk_domain::{
    CapabilitySet, DeviceId, DeviceProfile, NetworkAddress, SessionId, SessionState,
};
use minidesk_file_transfer::{FileReceiver, FileSender};
use minidesk_platform_api::{InputInjector, ScreenCapturer};
use minidesk_protocol::{ClipboardMessage, FileTransferMessage, InputMessage, Message};
use minidesk_security::DeviceIdentity;
use minidesk_storage::{SessionHistoryRecord, Storage};
use minidesk_transport::{ClientSession, HostListener};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};

#[cfg(windows)]
use minidesk_platform_windows::{WindowsInputInjector, WindowsScreenCapturer};

#[derive(Error, Debug)]
pub enum AppCoreError {
    #[error("Session error: {0}")]
    Session(String),

    #[error("Storage error: {0}")]
    Storage(#[from] minidesk_storage::StorageError),

    #[error("Configuration error: {0}")]
    Config(#[from] minidesk_config::ConfigError),

    #[error("Transport error: {0}")]
    Transport(#[from] minidesk_transport::TransportError),
}

/// Commands sent from the UI to the App Core.
#[derive(Debug)]
pub enum UiCommand {
    Connect {
        destination: NetworkAddress,
        capabilities: CapabilitySet,
    },
    ApproveIncoming {
        session_id: SessionId,
        granted_capabilities: CapabilitySet,
    },
    RejectIncoming {
        session_id: SessionId,
        reason: String,
    },
    SendInput(InputMessage),
    SendClipboard(String),
    InitiateFileTransfer {
        path: std::path::PathBuf,
    },
    RespondFileTransfer {
        transfer_id: u64,
        accept: bool,
    },
    CancelFileTransfer {
        transfer_id: u64,
    },
    SwitchMonitor {
        display_id: u32,
    },
    DisconnectSession,
    SaveDevice(DeviceProfile),
    DeleteDevice(DeviceId),
    ClearHistory,
}

/// Events emitted from App Core back to the UI.
#[derive(Debug, Clone)]
pub enum CoreEvent {
    StateChanged(SessionState),
    IncomingSessionRequest {
        session_id: SessionId,
        peer_name: String,
        peer_device_id: Option<DeviceId>,
        peer_address: NetworkAddress,
        requested_capabilities: CapabilitySet,
    },
    FrameReceived {
        width: u32,
        height: u32,
        data: Vec<u8>,
    },
    ClipboardReceived(String),
    FileTransferRequest {
        transfer_id: u64,
        filename: String,
        file_size_bytes: u64,
    },
    FileTransferProgress {
        transfer_id: u64,
        progress: f32,
    },
    FileTransferCompleted {
        transfer_id: u64,
        path: std::path::PathBuf,
    },
    FileTransferFailed {
        transfer_id: u64,
        error: String,
    },
    DisplaysAvailable(Vec<minidesk_domain::DisplayInfo>),
    StatusMessage(String),
    ErrorMessage(String),
    SavedDevicesUpdated(Vec<DeviceProfile>),
    HistoryUpdated(Vec<SessionHistoryRecord>),
}

/// Active session tracking context.
pub struct ActiveSession {
    pub session_id: SessionId,
    pub peer_address: NetworkAddress,
    pub peer_name: String,
    pub peer_device_id: Option<DeviceId>,
    pub granted_capabilities: CapabilitySet,
    pub state: SessionState,
    pub start_time_unix: i64,
}

/// Main application service coordinating state and background tasks.
pub struct AppCore {
    pub config: RwLock<AppConfig>,
    pub identity: Arc<DeviceIdentity>,
    pub storage: Storage,
    pub active_session: RwLock<Option<ActiveSession>>,
    pub event_sender: mpsc::UnboundedSender<CoreEvent>,
    pub client_input_sender: Mutex<Option<mpsc::UnboundedSender<InputMessage>>>,
    pub client_msg_sender: Mutex<Option<mpsc::UnboundedSender<Message>>>,
    pub pending_approvals: Arc<Mutex<HashMap<SessionId, oneshot::Sender<bool>>>>,
}

impl AppCore {
    pub fn new(
        config: AppConfig,
        identity: Arc<DeviceIdentity>,
        storage: Storage,
        event_sender: mpsc::UnboundedSender<CoreEvent>,
    ) -> Self {
        Self {
            config: RwLock::new(config),
            identity,
            storage,
            active_session: RwLock::new(None),
            event_sender,
            client_input_sender: Mutex::new(None),
            client_msg_sender: Mutex::new(None),
            pending_approvals: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Starts the background host listener to accept incoming connections.
    pub fn start_host_service(core: Arc<Self>) {
        tokio::spawn(async move {
            let port = {
                let cfg = core.config.read().await;
                cfg.network.listen_port
            };

            let listen_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);
            tracing::info!("Binding host listener on {}", listen_addr);

            let listener = match HostListener::bind(
                listen_addr,
                Arc::clone(&core.identity),
                core.identity.device_id().to_string(),
            )
            .await
            {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!("Failed to bind host listener: {:?}", e);
                    let _ = core.event_sender.send(CoreEvent::ErrorMessage(format!(
                        "Failed to bind host listener on port {}: {:?}",
                        port, e
                    )));
                    return;
                }
            };

            loop {
                match listener.accept_and_authenticate().await {
                    Ok((mut framed, session_id, peer_device_id, requested_caps, peer_pubkey)) => {
                        let (is_unattended, authorized_keys) = {
                            let cfg = core.config.read().await;
                            (
                                cfg.security.enable_unattended_access,
                                cfg.security.authorized_unattended_keys.clone(),
                            )
                        };

                        let is_authorized_unattended =
                            is_unattended && authorized_keys.contains(&peer_pubkey);

                        let approved = if is_authorized_unattended {
                            tracing::info!(
                                session = %session_id,
                                peer = %peer_device_id,
                                "Incoming peer authorized via pre-shared unattended cryptographic key"
                            );
                            true
                        } else {
                            tracing::info!(
                                session = %session_id,
                                peer = %peer_device_id,
                                "Incoming peer authenticated, awaiting interactive approval"
                            );

                            let (tx, rx) = oneshot::channel::<bool>();
                            {
                                let mut approvals = core.pending_approvals.lock().await;
                                approvals.insert(session_id, tx);
                            }

                            let peer_address = "Direct TCP".parse::<NetworkAddress>().unwrap_or_else(|_| {
                                NetworkAddress::new(IpAddr::V4(Ipv4Addr::LOCALHOST), None)
                            });

                            let _ = core.event_sender.send(CoreEvent::IncomingSessionRequest {
                                session_id,
                                peer_name: format!("Peer {}", &peer_device_id.to_string()[..8]),
                                peer_device_id: Some(peer_device_id.clone()),
                                peer_address: peer_address.clone(),
                                requested_capabilities: requested_caps,
                            });

                            match rx.await {
                                Ok(appr) => appr,
                                Err(_) => false,
                            }
                        };

                        if !approved {
                            tracing::info!(session = %session_id, "Session rejected by host");
                            let _ = framed
                                .write_message(&Message::SessionRejected {
                                    reason: "Rejected by host policy or user".into(),
                                })
                                .await;
                            continue;
                        }

                        // Initialize capturer and enumerate displays
                        #[cfg(windows)]
                        let mut capturer = match WindowsScreenCapturer::new() {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::error!("Failed to create WindowsScreenCapturer: {:?}", e);
                                continue;
                            }
                        };

                        #[cfg(windows)]
                        let displays = capturer.enumerate_displays().unwrap_or_default();
                        #[cfg(not(windows))]
                        let displays = Vec::new();

                        // Send approval grant
                        if let Err(e) = framed
                            .write_message(&Message::SessionApprovalGrant {
                                granted_capabilities: requested_caps,
                                displays,
                                active_display_id: 0,
                            })
                            .await
                        {
                            tracing::error!("Failed to write approval grant: {:?}", e);
                            continue;
                        }

                        let _ = core.event_sender.send(CoreEvent::StateChanged(SessionState::Active));
                        let _ = core.event_sender.send(CoreEvent::StatusMessage("Incoming session established".into()));

                        // Host video capture & input streaming loop
                        #[cfg(windows)]
                        {
                            tokio::task::spawn_blocking(move || {
                                let mut injector = WindowsInputInjector::new();
                                let mut encoder = Lz4VideoEncoder::new();
                                let mut clipboard_sync = ClipboardSynchronizer::new();
                                let mut file_receiver: Option<FileReceiver> = None;

                                let rt = tokio::runtime::Builder::new_current_thread()
                                    .enable_all()
                                    .build()
                                    .unwrap();

                                rt.block_on(async move {
                                    let mut interval = tokio::time::interval(Duration::from_millis(33)); // ~30 FPS
                                    let mut clip_interval = tokio::time::interval(Duration::from_millis(500));
                                    loop {
                                        tokio::select! {
                                            _ = interval.tick() => {
                                                if let Ok(Some(frame)) = capturer.capture_frame() {
                                                    if let Ok(compressed) = encoder.encode(&frame) {
                                                        let msg = Message::VideoFrame {
                                                            width: compressed.width,
                                                            height: compressed.height,
                                                            format: 1, // Lz4
                                                            is_keyframe: compressed.is_keyframe,
                                                            timestamp_unix_ms: compressed.timestamp_unix_ms,
                                                            payload: compressed.payload,
                                                        };
                                                        if framed.write_message(&msg).await.is_err() {
                                                            break;
                                                        }
                                                    }
                                                }
                                            }
                                            _ = clip_interval.tick() => {
                                                if let Ok(Some(clip_msg)) = clipboard_sync.check_local_change(&requested_caps) {
                                                    let _ = framed.write_message(&Message::Clipboard(clip_msg)).await;
                                                }
                                            }
                                            incoming_res = framed.read_message() => {
                                                match incoming_res {
                                                    Ok(Some(Message::Input(input))) => {
                                                        match input {
                                                            InputMessage::MouseMove { x, y } => {
                                                                let _ = injector.move_cursor(x, y);
                                                            }
                                                            InputMessage::MouseDown { button } => {
                                                                let btn = match button {
                                                                    minidesk_protocol::MouseButton::Left => 0,
                                                                    minidesk_protocol::MouseButton::Middle => 1,
                                                                    minidesk_protocol::MouseButton::Right => 2,
                                                                    _ => 0,
                                                                };
                                                                let _ = injector.mouse_button(btn, true);
                                                            }
                                                            InputMessage::MouseUp { button } => {
                                                                let btn = match button {
                                                                    minidesk_protocol::MouseButton::Left => 0,
                                                                    minidesk_protocol::MouseButton::Middle => 1,
                                                                    minidesk_protocol::MouseButton::Right => 2,
                                                                    _ => 0,
                                                                };
                                                                let _ = injector.mouse_button(btn, false);
                                                            }
                                                            InputMessage::MouseScroll { delta_x, delta_y } => {
                                                                let _ = injector.mouse_scroll(delta_x, delta_y);
                                                            }
                                                            InputMessage::KeyDown { key_code, .. } => {
                                                                let _ = injector.key_event(key_code, true);
                                                            }
                                                            InputMessage::KeyUp { key_code, .. } => {
                                                                let _ = injector.key_event(key_code, false);
                                                            }
                                                            InputMessage::ResetInputState => {
                                                                let _ = injector.reset_input_state();
                                                            }
                                                        }
                                                    }
                                                    Ok(Some(Message::SwitchMonitor { display_id })) => {
                                                        let _ = capturer.set_active_display(display_id);
                                                    }
                                                    Ok(Some(Message::Clipboard(ref clip))) => {
                                                        let _ = clipboard_sync.apply_remote(&requested_caps, clip);
                                                    }
                                                    Ok(Some(Message::FileTransfer(ref ft))) => {
                                                        match ft {
                                                            FileTransferMessage::Request { transfer_id, filename, file_size_bytes, sha256_hash } => {
                                                                if requested_caps.contains(CapabilitySet::FILE_UPLOAD) {
                                                                    let download_dir = std::env::current_dir().unwrap_or_default().join("downloads");
                                                                    match FileReceiver::new(&download_dir, *transfer_id, filename, *file_size_bytes, *sha256_hash) {
                                                                        Ok(rx) => {
                                                                            file_receiver = Some(rx);
                                                                            let _ = framed.write_message(&Message::FileTransfer(FileTransferMessage::Response {
                                                                                transfer_id: *transfer_id,
                                                                                accepted: true,
                                                                                rejection_reason: None,
                                                                            })).await;
                                                                        }
                                                                        Err(e) => {
                                                                            let _ = framed.write_message(&Message::FileTransfer(FileTransferMessage::Response {
                                                                                transfer_id: *transfer_id,
                                                                                accepted: false,
                                                                                rejection_reason: Some(e.to_string()),
                                                                            })).await;
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                            FileTransferMessage::Chunk { transfer_id, chunk_index, data } => {
                                                                if let Some(ref mut rx) = file_receiver {
                                                                    if rx.transfer_id == *transfer_id {
                                                                        let _ = rx.write_chunk(*chunk_index, data);
                                                                        let _ = framed.write_message(&Message::FileTransfer(FileTransferMessage::Ack {
                                                                            transfer_id: *transfer_id,
                                                                            chunk_index: *chunk_index,
                                                                        })).await;
                                                                    }
                                                                }
                                                            }
                                                            FileTransferMessage::Complete { transfer_id } => {
                                                                if let Some(mut rx) = file_receiver.take() {
                                                                    if rx.transfer_id == *transfer_id {
                                                                        let _ = rx.finalize();
                                                                    }
                                                                }
                                                            }
                                                            FileTransferMessage::Cancel { transfer_id, .. } => {
                                                                if let Some(mut rx) = file_receiver.take() {
                                                                    if rx.transfer_id == *transfer_id {
                                                                        rx.abort();
                                                                    }
                                                                }
                                                            }
                                                            _ => {}
                                                        }
                                                    }
                                                    Ok(Some(Message::CloseSession { .. })) | Ok(None) | Err(_) => {
                                                        break;
                                                    }
                                                    _ => {}
                                                }
                                            }
                                        }
                                    }
                                    let _ = injector.reset_input_state();
                                });
                            });
                        }
                    }
                    Err(e) => {
                        tracing::error!("HostListener error: {:?}", e);
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                }
            }
        });
    }

    /// Handles a command originating from the user interface.
    pub async fn handle_command(&self, cmd: UiCommand) -> Result<(), AppCoreError> {
        match cmd {
            UiCommand::Connect { destination, capabilities } => {
                self.start_connection(destination, capabilities).await?;
            }
            UiCommand::ApproveIncoming { session_id, granted_capabilities: _ } => {
                let mut approvals = self.pending_approvals.lock().await;
                if let Some(tx) = approvals.remove(&session_id) {
                    let _ = tx.send(true);
                }
            }
            UiCommand::RejectIncoming { session_id, reason: _ } => {
                let mut approvals = self.pending_approvals.lock().await;
                if let Some(tx) = approvals.remove(&session_id) {
                    let _ = tx.send(false);
                }
            }
            UiCommand::SendInput(input) => {
                let sender = self.client_msg_sender.lock().await;
                if let Some(tx) = sender.as_ref() {
                    let _ = tx.send(Message::Input(input));
                }
            }
            UiCommand::SendClipboard(text) => {
                let sender = self.client_msg_sender.lock().await;
                if let Some(tx) = sender.as_ref() {
                    let _ = tx.send(Message::Clipboard(ClipboardMessage::Text(text)));
                }
            }
            UiCommand::SwitchMonitor { display_id } => {
                let sender = self.client_msg_sender.lock().await;
                if let Some(tx) = sender.as_ref() {
                    let _ = tx.send(Message::SwitchMonitor { display_id });
                }
            }
            UiCommand::InitiateFileTransfer { path } => {
                let sender = self.client_msg_sender.lock().await;
                if let Some(tx) = sender.as_ref() {
                    match FileSender::open(&path) {
                        Ok(mut file_sender) => {
                            let _ = tx.send(Message::FileTransfer(file_sender.request_message()));
                            let tx_clone = tx.clone();
                            let event_tx = self.event_sender.clone();
                            tokio::spawn(async move {
                                while let Ok(Some(chunk)) = file_sender.next_chunk() {
                                    if tx_clone.send(Message::FileTransfer(chunk)).is_err() {
                                        break;
                                    }
                                    tokio::time::sleep(Duration::from_millis(5)).await;
                                }
                                let _ = tx_clone.send(Message::FileTransfer(file_sender.complete_message()));
                                let _ = event_tx.send(CoreEvent::StatusMessage("File upload finished".into()));
                            });
                        }
                        Err(e) => {
                            let _ = self.event_sender.send(CoreEvent::ErrorMessage(format!("Failed to open file: {}", e)));
                        }
                    }
                }
            }
            UiCommand::RespondFileTransfer { transfer_id, accept } => {
                let sender = self.client_msg_sender.lock().await;
                if let Some(tx) = sender.as_ref() {
                    let _ = tx.send(Message::FileTransfer(FileTransferMessage::Response {
                        transfer_id,
                        accepted: accept,
                        rejection_reason: if accept { None } else { Some("User rejected transfer".into()) },
                    }));
                }
            }
            UiCommand::CancelFileTransfer { transfer_id } => {
                let sender = self.client_msg_sender.lock().await;
                if let Some(tx) = sender.as_ref() {
                    let _ = tx.send(Message::FileTransfer(FileTransferMessage::Cancel {
                        transfer_id,
                        reason: "Cancelled by user".into(),
                    }));
                }
            }
            UiCommand::DisconnectSession => {
                self.terminate_active_session("User requested disconnect").await?;
            }
            UiCommand::SaveDevice(profile) => {
                self.storage.save_device(&profile)?;
                self.refresh_saved_devices().await?;
            }
            UiCommand::DeleteDevice(device_id) => {
                self.storage.delete_device(&device_id)?;
                self.refresh_saved_devices().await?;
            }
            UiCommand::ClearHistory => {
                self.storage.clear_history()?;
                self.refresh_history().await?;
            }
        }
        Ok(())
    }

    /// Initiates an outgoing connection to a destination IP.
    pub async fn start_connection(
        &self,
        destination: NetworkAddress,
        capabilities: CapabilitySet,
    ) -> Result<(), AppCoreError> {
        let session_id = SessionId::new();
        tracing::info!(session = %session_id, dest = %destination, "Starting connection attempt");

        let session = ActiveSession {
            session_id,
            peer_address: destination.clone(),
            peer_name: destination.to_string(),
            peer_device_id: None,
            granted_capabilities: capabilities,
            state: SessionState::Resolving,
            start_time_unix: chrono_now(),
        };

        {
            let mut active = self.active_session.write().await;
            *active = Some(session);
        }

        let _ = self.event_sender.send(CoreEvent::StateChanged(SessionState::Resolving));
        let _ = self.event_sender.send(CoreEvent::StatusMessage(format!(
            "Connecting to {}",
            destination
        )));

        // Record start in history
        let _ = self.storage.record_session_start(
            &session_id,
            None,
            &destination.to_string(),
            &destination,
            chrono_now(),
            capabilities,
        );

        let event_tx = self.event_sender.clone();
        let identity = Arc::clone(&self.identity);
        let sock_addr = destination.to_socket_addr();

        let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<Message>();
        {
            let mut sender_guard = self.client_msg_sender.lock().await;
            *sender_guard = Some(msg_tx);
        }

        tokio::spawn(async move {
            let _ = event_tx.send(CoreEvent::StateChanged(SessionState::Authenticating));
            let mut client = match ClientSession::connect(
                sock_addr,
                "miniDesk Client",
                &identity,
                capabilities,
            )
            .await
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("Client connect failed: {:?}", e);
                    let _ = event_tx.send(CoreEvent::StateChanged(SessionState::Failed));
                    let _ = event_tx.send(CoreEvent::ErrorMessage(format!(
                        "Connection failed: {:?}",
                        e
                    )));
                    return;
                }
            };

            let _ = event_tx.send(CoreEvent::StateChanged(SessionState::Active));
            let _ = event_tx.send(CoreEvent::StatusMessage(format!(
                "Connected to {} (Capabilities: {:?})",
                sock_addr, client.granted_capabilities
            )));

            let mut clipboard_sync = ClipboardSynchronizer::new();
            let mut clip_interval = tokio::time::interval(Duration::from_millis(500));

            loop {
                tokio::select! {
                    msg_res = client.next_message() => {
                        match msg_res {
                            Ok(Some(Message::VideoFrame { width, height, format, is_keyframe, timestamp_unix_ms, payload })) => {
                                if let Ok(frame) = client.decode_video_frame(width, height, format, is_keyframe, timestamp_unix_ms, payload) {
                                    let _ = event_tx.send(CoreEvent::FrameReceived {
                                        width: frame.width,
                                        height: frame.height,
                                        data: frame.data,
                                    });
                                }
                            }
                            Ok(Some(Message::SessionApprovalGrant { displays, .. })) => {
                                let _ = event_tx.send(CoreEvent::DisplaysAvailable(displays));
                            }
                            Ok(Some(Message::Clipboard(ClipboardMessage::Text(text)))) => {
                                let _ = clipboard_sync.apply_remote(&client.granted_capabilities, &ClipboardMessage::Text(text.clone()));
                                let _ = event_tx.send(CoreEvent::ClipboardReceived(text));
                            }
                            Ok(Some(Message::FileTransfer(FileTransferMessage::Request { transfer_id, filename, file_size_bytes, .. }))) => {
                                let _ = event_tx.send(CoreEvent::FileTransferRequest {
                                    transfer_id,
                                    filename,
                                    file_size_bytes,
                                });
                            }
                            Ok(Some(Message::CloseSession { reason })) => {
                                let _ = event_tx.send(CoreEvent::StatusMessage(format!("Host closed session: {}", reason)));
                                break;
                            }
                            Ok(Some(_)) => {}
                            Ok(None) | Err(_) => {
                                break;
                            }
                        }
                    }
                    _ = clip_interval.tick() => {
                        if let Ok(Some(clip_msg)) = clipboard_sync.check_local_change(&client.granted_capabilities) {
                            let _ = client.send_message(&Message::Clipboard(clip_msg)).await;
                        }
                    }
                    Some(msg) = msg_rx.recv() => {
                        let _ = client.send_message(&msg).await;
                    }
                }
            }

            let _ = event_tx.send(CoreEvent::StateChanged(SessionState::Ended));
            let _ = event_tx.send(CoreEvent::StatusMessage("Session disconnected".into()));
        });

        Ok(())
    }

    /// Terminate currently active session with cleanup.
    pub async fn terminate_active_session(&self, reason: &str) -> Result<(), AppCoreError> {
        let mut active = self.active_session.write().await;
        if let Some(session) = active.take() {
            tracing::info!(session = %session.session_id, reason = reason, "Terminating active session");
            let _ = self.storage.record_session_end(&session.session_id, chrono_now(), reason);
            let _ = self.event_sender.send(CoreEvent::StateChanged(SessionState::Ended));
            let _ = self.event_sender.send(CoreEvent::StatusMessage("Session disconnected".into()));
        }
        {
            let mut sender = self.client_msg_sender.lock().await;
            *sender = None;
        }
        self.refresh_history().await?;
        Ok(())
    }

    /// Broadcast updated saved devices list to UI.
    pub async fn refresh_saved_devices(&self) -> Result<(), AppCoreError> {
        let devices = self.storage.get_saved_devices()?;
        let _ = self.event_sender.send(CoreEvent::SavedDevicesUpdated(devices));
        Ok(())
    }

    /// Broadcast updated history list to UI.
    pub async fn refresh_history(&self) -> Result<(), AppCoreError> {
        let history = self.storage.get_session_history(50)?;
        let _ = self.event_sender.send(CoreEvent::HistoryUpdated(history));
        Ok(())
    }
}

fn chrono_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_app_core_lifecycle() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let config = AppConfig::default();
        let identity = Arc::new(DeviceIdentity::generate());
        let storage = Storage::open_in_memory().unwrap();

        let core = AppCore::new(config, identity, storage, tx);

        let dest = "192.168.1.50:47823".parse::<NetworkAddress>().unwrap();
        core.start_connection(dest, CapabilitySet::FULL_CONTROL)
            .await
            .unwrap();

        // Check state changed event
        let evt = rx.recv().await.unwrap();
        match evt {
            CoreEvent::StateChanged(st) => assert_eq!(st, SessionState::Resolving),
            _ => panic!("Expected StateChanged"),
        }

        core.terminate_active_session("Test Disconnect")
            .await
            .unwrap();
    }
}
