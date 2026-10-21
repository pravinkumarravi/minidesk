//! miniDesk background session agent daemon for unattended access and service management.
//!
//! Provides a headless background service capable of listening for incoming authenticated
//! remote sessions, enforcing pre-shared key authorization, and integrating with
//! system services / daemons without requiring interactive GUI presence.

use minidesk_app_core::{AppCore, CoreEvent};
use minidesk_config::AppConfig;
use minidesk_logging::init_logging;
use minidesk_security::DeviceIdentity;
use minidesk_storage::Storage;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::Level;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_logging(Level::INFO);
    tracing::info!("Starting miniDesk background agent daemon...");

    let mut config = AppConfig::default();

    // Check environment or command-line flags
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--config") {
        if let Some(path_str) = args.get(pos + 1) {
            let path = std::path::Path::new(path_str);
            if path.exists() {
                config = AppConfig::load_from_file(path)?;
                tracing::info!("Loaded configuration from {}", path.display());
            }
        }
    }

    if let Some(pos) = args.iter().position(|a| a == "--enroll-key") {
        if let Some(key_hex) = args.get(pos + 1) {
            if let Some(key_bytes) = decode_hex_32(key_hex) {
                config.security.enable_unattended_access = true;
                config.security.authorized_unattended_keys.push(key_bytes);
                tracing::info!("Enrolled authorized unattended access key: {}", key_hex);
            } else {
                tracing::error!("Invalid 64-hex-character enrollment key provided: {}", key_hex);
            }
        }
    }

    config.validate()?;

    if config.security.enable_unattended_access {
        tracing::info!(
            "Unattended access is ENABLED. Authorized keys: {}",
            config.security.authorized_unattended_keys.len()
        );
    } else {
        tracing::warn!(
            "Unattended access is DISABLED. Interactive consent will be required."
        );
    }

    // Initialize local storage
    let storage = Storage::open(&config.paths.database_path)?;
    let identity = Arc::new(DeviceIdentity::generate());
    tracing::info!("Agent Device ID: {}", identity.device_id());

    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<CoreEvent>();
    let core = Arc::new(AppCore::new(config, identity, storage, event_tx));

    // Start background host listener
    AppCore::start_host_service(Arc::clone(&core));
    tracing::info!("Agent host service active on background worker");

    // Event monitoring loop
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            match event {
                CoreEvent::StateChanged(state) => {
                    tracing::info!("Agent session state: {:?}", state);
                }
                CoreEvent::IncomingSessionRequest {
                    session_id,
                    peer_name,
                    ..
                } => {
                    tracing::info!(
                        session = %session_id,
                        peer = %peer_name,
                        "Incoming connection request received by agent"
                    );
                }
                CoreEvent::StatusMessage(msg) => {
                    tracing::info!("Agent status: {}", msg);
                }
                CoreEvent::ErrorMessage(err) => {
                    tracing::error!("Agent error: {}", err);
                }
                _ => {}
            }
        }
    });

    // Wait for termination signal (Ctrl+C / SIGTERM)
    tokio::signal::ctrl_c().await?;
    tracing::info!("Received shutdown signal. Stopping agent service...");
    core.terminate_active_session("Agent process shutdown").await?;

    Ok(())
}

fn decode_hex_32(s: &str) -> Option<[u8; 32]> {
    let clean = s.trim();
    if clean.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidesk_domain::DEFAULT_PORT;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_agent_unattended_config_validation() {
        let mut config = AppConfig::default();
        config.security.enable_unattended_access = true;
        // Without keys, validation fails
        assert!(config.validate().is_err());

        // Enroll valid key
        config.security.authorized_unattended_keys.push([42u8; 32]);
        assert!(config.validate().is_ok());
    }

    #[tokio::test]
    async fn test_agent_core_initialization() {
        let temp_dir = tempdir().unwrap();
        let db_path = temp_dir.path().join("agent_test.db");
        let storage = Storage::open(&db_path).unwrap();

        let mut config = AppConfig::default();
        config.network.listen_port = DEFAULT_PORT + 50;
        config.paths.database_path = db_path;

        let identity = Arc::new(DeviceIdentity::generate());
        let (tx, _rx) = mpsc::unbounded_channel();
        let core = Arc::new(AppCore::new(config, identity, storage, tx));

        assert!(core.active_session.read().await.is_none());
    }
}
