//! Core domain types for the miniDesk application.
//!
//! This crate contains pure business models and has no dependencies on UI,
//! concrete storage implementations, or platform-specific APIs.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use thiserror::Error;

/// The standard default port for miniDesk direct connections.
pub const DEFAULT_PORT: u16 = 47823;

/// Unique identifier for an active or historical session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId([u8; 16]);

impl SessionId {
    /// Generates a new random cryptographically strong SessionId.
    pub fn new() -> Self {
        use rand::RngCore;
        let mut bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut bytes);
        Self(bytes)
    }

    /// Creates a SessionId from raw 16 bytes.
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Returns the internal bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.0 {
            write!(f, "{:02x}", b)?;
        }
        Ok(())
    }
}

impl FromStr for SessionId {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let clean = s.trim().replace('-', "");
        if clean.len() != 32 {
            return Err(DomainError::InvalidFormat("Session ID must be 32 hex chars".into()));
        }
        let mut bytes = [0u8; 16];
        for i in 0..16 {
            bytes[i] = u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16)
                .map_err(|_| DomainError::InvalidFormat("Invalid hex in Session ID".into()))?;
        }
        Ok(Self(bytes))
    }
}

/// Unique cryptographic fingerprint identifying a machine.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeviceId(String);

impl DeviceId {
    /// Creates a new DeviceId from a validated hex string (typically SHA-256 of public key).
    pub fn new(id: impl Into<String>) -> Result<Self, DomainError> {
        let s = id.into().trim().to_lowercase();
        if s.is_empty() || s.len() > 128 {
            return Err(DomainError::InvalidFormat("Invalid DeviceId length".into()));
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Validated network destination (IPv4 or IPv6 with optional port).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NetworkAddress {
    pub ip: IpAddr,
    pub port: u16,
}

impl NetworkAddress {
    pub fn new(ip: IpAddr, port: Option<u16>) -> Self {
        Self {
            ip,
            port: port.unwrap_or(DEFAULT_PORT),
        }
    }

    pub fn to_socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.ip, self.port)
    }
}

impl fmt::Display for NetworkAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.ip {
            IpAddr::V4(v4) => write!(f, "{}:{}", v4, self.port),
            IpAddr::V6(v6) => write!(f, "[{}]:{}", v6, self.port),
        }
    }
}

impl FromStr for NetworkAddress {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let input = s.trim();
        if input.is_empty() {
            return Err(DomainError::InvalidFormat("Empty address".into()));
        }

        // Try direct SocketAddr parse first (e.g. "192.168.1.5:47823" or "[2001:db8::1]:47823")
        if let Ok(sa) = input.parse::<SocketAddr>() {
            return Ok(Self {
                ip: sa.ip(),
                port: sa.port(),
            });
        }

        // Try direct IpAddr parse without port (e.g. "192.168.1.5" or "2001:db8::1")
        if let Ok(ip) = input.parse::<IpAddr>() {
            return Ok(Self {
                ip,
                port: DEFAULT_PORT,
            });
        }

        // Handle IPv6 bracket notation without port (e.g. "[2001:db8::1]")
        if input.starts_with('[') && input.ends_with(']') {
            let inner = &input[1..input.len() - 1];
            if let Ok(ip) = inner.parse::<IpAddr>() {
                return Ok(Self {
                    ip,
                    port: DEFAULT_PORT,
                });
            }
        }

        // Handle IPv4 with colon or custom host:port split
        if let Some((host_part, port_part)) = input.rsplit_once(':') {
            let clean_host = host_part.trim_start_matches('[').trim_end_matches(']');
            let ip = clean_host.parse::<IpAddr>().map_err(|_| {
                DomainError::InvalidFormat(format!("Invalid IP address '{}'", clean_host))
            })?;
            let port = port_part.parse::<u16>().map_err(|_| {
                DomainError::InvalidFormat(format!("Invalid port number '{}'", port_part))
            })?;
            return Ok(Self { ip, port });
        }

        Err(DomainError::InvalidFormat(format!("Cannot parse '{}' as IPv4 or IPv6 address", input)))
    }
}

/// Granular session capability flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CapabilitySet(u32);

impl CapabilitySet {
    pub const NONE: Self = Self(0);
    pub const SCREEN_VIEW: Self = Self(1 << 0);
    pub const MOUSE_CONTROL: Self = Self(1 << 1);
    pub const KEYBOARD_CONTROL: Self = Self(1 << 2);
    pub const CLIPBOARD_READ: Self = Self(1 << 3);  // Remote can read local clipboard
    pub const CLIPBOARD_WRITE: Self = Self(1 << 4); // Remote can write local clipboard
    pub const FILE_DOWNLOAD: Self = Self(1 << 5);   // Remote can download files from host
    pub const FILE_UPLOAD: Self = Self(1 << 6);     // Remote can upload files to host
    pub const AUDIO_LISTEN: Self = Self(1 << 7);    // Remote can hear host audio
    pub const MONITOR_SWITCH: Self = Self(1 << 8);  // Remote can switch displays

    pub const FULL_CONTROL: Self = Self(
        Self::SCREEN_VIEW.0
            | Self::MOUSE_CONTROL.0
            | Self::KEYBOARD_CONTROL.0
            | Self::CLIPBOARD_READ.0
            | Self::CLIPBOARD_WRITE.0
            | Self::FILE_DOWNLOAD.0
            | Self::FILE_UPLOAD.0
            | Self::AUDIO_LISTEN.0
            | Self::MONITOR_SWITCH.0,
    );

    pub const VIEW_ONLY: Self = Self(Self::SCREEN_VIEW.0 | Self::AUDIO_LISTEN.0);

    pub const fn empty() -> Self {
        Self::NONE
    }

    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    pub const fn bits(&self) -> u32 {
        self.0
    }

    pub const fn contains(&self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }
}

impl Default for CapabilitySet {
    fn default() -> Self {
        Self::VIEW_ONLY
    }
}

/// Explicit lifecycle states for a remote desktop connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    Idle,
    Resolving,
    Authenticating,
    AwaitingApproval,
    Negotiating,
    Connecting,
    Active,
    Reconnecting,
    Rejected,
    Failed,
    Ended,
}

impl SessionState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Rejected | Self::Failed | Self::Ended)
    }

    pub fn is_active(&self) -> bool {
        matches!(self, Self::Active | Self::Reconnecting)
    }
}

impl fmt::Display for SessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle => write!(f, "Idle"),
            Self::Resolving => write!(f, "Resolving Destination"),
            Self::Authenticating => write!(f, "Authenticating Peer"),
            Self::AwaitingApproval => write!(f, "Awaiting Remote Approval"),
            Self::Negotiating => write!(f, "Negotiating Media & Transport"),
            Self::Connecting => write!(f, "Connecting Channels"),
            Self::Active => write!(f, "Session Active"),
            Self::Reconnecting => write!(f, "Reconnecting"),
            Self::Rejected => write!(f, "Connection Rejected"),
            Self::Failed => write!(f, "Connection Failed"),
            Self::Ended => write!(f, "Session Ended"),
        }
    }
}

/// Remote display information.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub id: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f32,
    pub is_primary: bool,
}

/// Profile for a saved / trusted machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceProfile {
    pub device_id: DeviceId,
    pub friendly_name: String,
    pub last_known_address: NetworkAddress,
    pub is_trusted: bool,
    pub preferred_capabilities: CapabilitySet,
    pub created_at_unix: i64,
    pub last_connected_unix: Option<i64>,
    pub notes: String,
}

/// Domain errors encountered during miniDesk operations.
#[derive(Error, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DomainError {
    #[error("Invalid format: {0}")]
    InvalidFormat(String),

    #[error("Authorization denied: {0}")]
    Unauthorized(String),

    #[error("Invalid state transition from {from} to {to}")]
    InvalidStateTransition {
        from: String,
        to: String,
    },

    #[error("Capability not granted: {0}")]
    MissingCapability(String),

    #[error("Connection timeout")]
    Timeout,

    #[error("Internal error: {0}")]
    Internal(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_id_roundtrip() {
        let id = SessionId::new();
        let s = id.to_string();
        let parsed: SessionId = s.parse().expect("Failed to parse SessionId");
        assert_eq!(id, parsed);
    }

    #[test]
    fn test_network_address_parsing() {
        // IPv4 with default port
        let addr1: NetworkAddress = "192.168.1.100".parse().unwrap();
        assert_eq!(addr1.ip, "192.168.1.100".parse::<IpAddr>().unwrap());
        assert_eq!(addr1.port, DEFAULT_PORT);

        // IPv4 with custom port
        let addr2: NetworkAddress = "10.0.0.1:9090".parse().unwrap();
        assert_eq!(addr2.ip, "10.0.0.1".parse::<IpAddr>().unwrap());
        assert_eq!(addr2.port, 9090);

        // IPv6 bracketed with port
        let addr3: NetworkAddress = "[2001:db8::1]:5000".parse().unwrap();
        assert_eq!(addr3.ip, "2001:db8::1".parse::<IpAddr>().unwrap());
        assert_eq!(addr3.port, 5000);

        // IPv6 plain
        let addr4: NetworkAddress = "::1".parse().unwrap();
        assert_eq!(addr4.port, DEFAULT_PORT);
    }

    #[test]
    fn test_capabilities() {
        let mut caps = CapabilitySet::empty();
        assert!(!caps.contains(CapabilitySet::SCREEN_VIEW));
        caps.insert(CapabilitySet::SCREEN_VIEW);
        assert!(caps.contains(CapabilitySet::SCREEN_VIEW));
        assert!(!caps.contains(CapabilitySet::MOUSE_CONTROL));

        let full = CapabilitySet::FULL_CONTROL;
        assert!(full.contains(CapabilitySet::KEYBOARD_CONTROL));
        assert!(full.contains(CapabilitySet::FILE_UPLOAD));
    }
}
