//! Configuration management for miniDesk.
//!
//! Provides strictly validated configuration with secure defaults.

use minidesk_domain::{CapabilitySet, DEFAULT_PORT};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Configuration I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Configuration parse error: {0}")]
    Parse(#[from] serde_json::Error),

    #[error("Configuration validation error: {0}")]
    Validation(String),
}

/// Primary configuration for the miniDesk application.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppConfig {
    /// Device friendly display name.
    pub device_name: String,

    /// Local network listener settings.
    pub network: NetworkConfig,

    /// Security & access policy.
    pub security: SecurityConfig,

    /// Storage and media paths.
    pub paths: StoragePaths,

    /// Audio and video streaming parameters.
    pub media: MediaConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NetworkConfig {
    /// Port for incoming direct connections.
    pub listen_port: u16,

    /// Whether to allow incoming connections.
    pub enable_incoming_connections: bool,

    /// Signaling server URL (optional, for cross-network internet sessions).
    pub signaling_server_url: Option<String>,

    /// STUN server URIs.
    pub stun_servers: Vec<String>,

    /// TURN relay server configuration.
    pub turn_servers: Vec<TurnConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TurnConfig {
    pub url: String,
    pub username: String,
    pub credential: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SecurityConfig {
    /// Incoming connections always prompt for interactive approval unless unattended policy matches.
    pub require_interactive_approval: bool,

    /// Unattended access is explicitly disabled by default.
    pub enable_unattended_access: bool,

    /// Explicitly authorized peer Ed25519 public keys permitted for unattended login.
    pub authorized_unattended_keys: Vec<[u8; 32]>,

    /// Maximum capabilities allowed to be requested by remote peers.
    pub max_allowed_capabilities: CapabilitySet,

    /// Maximum idle timeout in seconds before closing inactive sessions.
    pub session_idle_timeout_secs: u64,

    /// Clipboard synchronization enabled.
    pub allow_clipboard_sync: bool,

    /// Maximum clipboard sync payload size in bytes (default 16 MB).
    pub max_clipboard_bytes: usize,

    /// File transfer enabled.
    pub allow_file_transfer: bool,

    /// Maximum file size permitted for transfers in bytes (default 10 GB).
    pub max_file_transfer_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoragePaths {
    /// Directory where received files are saved.
    pub downloads_dir: PathBuf,

    /// Database file path for SQLite.
    pub database_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MediaConfig {
    /// Target frame rate (e.g. 30 or 60 FPS).
    pub target_fps: u32,

    /// Quality preset (1 to 100).
    pub video_quality: u8,

    /// Enable remote audio streaming.
    pub enable_audio: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        let home_dir = dirs_fallback();
        let minidesk_dir = home_dir.join(".minidesk");

        Self {
            device_name: default_hostname(),
            network: NetworkConfig {
                listen_port: DEFAULT_PORT,
                enable_incoming_connections: true,
                signaling_server_url: None,
                stun_servers: vec![
                    "stun:stun.l.google.com:19302".into(),
                    "stun:stun.cloudflare.com:3478".into(),
                ],
                turn_servers: Vec::new(),
            },
            security: SecurityConfig {
                require_interactive_approval: true,
                enable_unattended_access: false,
                authorized_unattended_keys: Vec::new(),
                max_allowed_capabilities: CapabilitySet::FULL_CONTROL,
                session_idle_timeout_secs: 1800, // 30 minutes
                allow_clipboard_sync: true,
                max_clipboard_bytes: 16 * 1024 * 1024, // 16 MB
                allow_file_transfer: true,
                max_file_transfer_bytes: 10 * 1024 * 1024 * 1024, // 10 GB
            },
            paths: StoragePaths {
                downloads_dir: minidesk_dir.join("downloads"),
                database_path: minidesk_dir.join("minidesk.db"),
            },
            media: MediaConfig {
                target_fps: 60,
                video_quality: 85,
                enable_audio: true,
            },
        }
    }
}

impl AppConfig {
    /// Validates the configuration, rejecting dangerous or contradictory settings.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.network.listen_port == 0 {
            return Err(ConfigError::Validation("Listen port cannot be 0".into()));
        }

        if self.media.target_fps == 0 || self.media.target_fps > 144 {
            return Err(ConfigError::Validation(
                "Target FPS must be between 1 and 144".into(),
            ));
        }

        if self.media.video_quality == 0 || self.media.video_quality > 100 {
            return Err(ConfigError::Validation(
                "Video quality must be between 1 and 100".into(),
            ));
        }

        if self.security.max_clipboard_bytes == 0 || self.security.max_clipboard_bytes > 64 * 1024 * 1024 {
            return Err(ConfigError::Validation(
                "Max clipboard bytes must be between 1 and 64 MB".into(),
            ));
        }

        if self.security.enable_unattended_access && self.security.authorized_unattended_keys.is_empty() {
            return Err(ConfigError::Validation(
                "Unattended access enabled without any authorized cryptographic public keys configured".into(),
            ));
        }

        Ok(())
    }

    /// Load configuration from a JSON file with validation.
    pub fn load_from_file(path: &Path) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path)?;
        let config: Self = serde_json::from_str(&content)?;
        config.validate()?;
        Ok(config)
    }

    /// Save configuration to a JSON file.
    pub fn save_to_file(&self, path: &Path) -> Result<(), ConfigError> {
        self.validate()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }
}

fn default_hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "miniDesk-Node".into())
}

fn dirs_fallback() -> PathBuf {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_validation() {
        let config = AppConfig::default();
        assert!(config.validate().is_ok());
        assert!(config.security.require_interactive_approval);
        assert!(!config.security.enable_unattended_access);
    }

    #[test]
    fn test_invalid_config_rejection() {
        let mut config = AppConfig::default();
        config.network.listen_port = 0;
        assert!(config.validate().is_err());

        config.network.listen_port = DEFAULT_PORT;
        config.media.target_fps = 300;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_unattended_access_policy() {
        let mut config = AppConfig::default();
        config.security.enable_unattended_access = true;
        // Without keys, validation MUST fail
        assert!(config.validate().is_err());

        // With authorized key, validation succeeds
        config.security.authorized_unattended_keys.push([7u8; 32]);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_serialization_roundtrip() {
        let config = AppConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, deserialized);
    }
}
