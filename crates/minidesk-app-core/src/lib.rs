//! Application core orchestrator for miniDesk.
//!
//! Manages connection lifecycles, state machine transitions, incoming session requests,
//! capability grants, and UI event synchronization.

use minidesk_config::AppConfig;
use minidesk_domain::{
    CapabilitySet, DeviceId, DeviceProfile, NetworkAddress, SessionId, SessionState,
};
use minidesk_security::DeviceIdentity;
use minidesk_storage::{SessionHistoryRecord, Storage};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{mpsc, RwLock};

#[derive(Error, Debug)]
pub enum AppCoreError {
    #[error("Session error: {0}")]
    Session(String),

    #[error("Storage error: {0}")]
    Storage(#[from] minidesk_storage::StorageError),

    #[error("Configuration error: {0}")]
    Config(#[from] minidesk_config::ConfigError),
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
        }
    }

    /// Handles a command originating from the user interface.
    pub async fn handle_command(&self, cmd: UiCommand) -> Result<(), AppCoreError> {
        match cmd {
            UiCommand::Connect { destination, capabilities } => {
                self.start_connection(destination, capabilities).await?;
            }
            UiCommand::ApproveIncoming { session_id, granted_capabilities } => {
                self.approve_session(session_id, granted_capabilities).await?;
            }
            UiCommand::RejectIncoming { session_id, reason } => {
                self.reject_session(session_id, reason).await?;
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

        Ok(())
    }

    /// Approve an incoming connection request.
    pub async fn approve_session(
        &self,
        session_id: SessionId,
        granted_caps: CapabilitySet,
    ) -> Result<(), AppCoreError> {
        let mut active = self.active_session.write().await;
        if let Some(session) = active.as_mut() {
            if session.session_id == session_id {
                session.state = SessionState::Active;
                session.granted_capabilities = granted_caps;
                let _ = self.event_sender.send(CoreEvent::StateChanged(SessionState::Active));
                let _ = self.event_sender.send(CoreEvent::StatusMessage("Session established".into()));
            }
        }
        Ok(())
    }

    /// Reject an incoming connection request.
    pub async fn reject_session(
        &self,
        session_id: SessionId,
        reason: String,
    ) -> Result<(), AppCoreError> {
        let mut active = self.active_session.write().await;
        if let Some(session) = active.as_mut() {
            if session.session_id == session_id {
                session.state = SessionState::Rejected;
                let _ = self.event_sender.send(CoreEvent::StateChanged(SessionState::Rejected));
                let _ = self.event_sender.send(CoreEvent::ErrorMessage(format!(
                    "Connection rejected: {}",
                    reason
                )));
                let _ = self.storage.record_session_end(&session_id, chrono_now(), &format!("Rejected: {}", reason));
            }
        }
        *active = None;
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
