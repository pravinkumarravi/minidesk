//! SQLite database storage implementation for miniDesk.
//!
//! Stores saved devices, trusted endpoints, application settings, and session audit history.

use minidesk_domain::{CapabilitySet, DeviceId, DeviceProfile, NetworkAddress, SessionId};
use rusqlite::{params, Connection};
use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum StorageError {
    #[error("Database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("Domain error: {0}")]
    Domain(#[from] minidesk_domain::DomainError),
}

/// Metadata record for a past remote session.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionHistoryRecord {
    pub session_id: SessionId,
    pub remote_device_id: Option<DeviceId>,
    pub remote_name: String,
    pub remote_address: NetworkAddress,
    pub start_time_unix: i64,
    pub end_time_unix: Option<i64>,
    pub outcome: String,
    pub capabilities_granted: CapabilitySet,
}

#[derive(Clone)]
pub struct Storage {
    conn: Arc<Mutex<Connection>>,
}

impl Storage {
    /// Opens or creates an SQLite database at the specified path and runs migrations.
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
            })?;
        }

        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;",
        )?;

        let storage = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        storage.migrate()?;
        Ok(storage)
    }

    /// Opens an in-memory SQLite database for unit tests.
    pub fn open_in_memory() -> Result<Self, StorageError> {
        let conn = Connection::open_in_memory()?;
        let storage = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        storage.migrate()?;
        Ok(storage)
    }

    fn migrate(&self) -> Result<(), StorageError> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS saved_devices (
                device_id TEXT PRIMARY KEY,
                friendly_name TEXT NOT NULL,
                last_known_address TEXT NOT NULL,
                is_trusted INTEGER NOT NULL DEFAULT 0,
                preferred_capabilities INTEGER NOT NULL,
                created_at_unix INTEGER NOT NULL,
                last_connected_unix INTEGER,
                notes TEXT NOT NULL DEFAULT ''
            );

            CREATE TABLE IF NOT EXISTS session_history (
                session_id TEXT PRIMARY KEY,
                remote_device_id TEXT,
                remote_name TEXT NOT NULL,
                remote_address TEXT NOT NULL,
                start_time_unix INTEGER NOT NULL,
                end_time_unix INTEGER,
                outcome TEXT NOT NULL,
                capabilities_granted INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_history_start ON session_history(start_time_unix DESC);",
        )?;
        Ok(())
    }

    /// Inserts or updates a saved device profile.
    pub fn save_device(&self, profile: &DeviceProfile) -> Result<(), StorageError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO saved_devices (
                device_id, friendly_name, last_known_address, is_trusted,
                preferred_capabilities, created_at_unix, last_connected_unix, notes
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(device_id) DO UPDATE SET
                friendly_name = excluded.friendly_name,
                last_known_address = excluded.last_known_address,
                is_trusted = excluded.is_trusted,
                preferred_capabilities = excluded.preferred_capabilities,
                last_connected_unix = excluded.last_connected_unix,
                notes = excluded.notes;",
            params![
                profile.device_id.as_str(),
                profile.friendly_name,
                profile.last_known_address.to_string(),
                if profile.is_trusted { 1 } else { 0 },
                profile.preferred_capabilities.bits(),
                profile.created_at_unix,
                profile.last_connected_unix,
                profile.notes,
            ],
        )?;
        Ok(())
    }

    /// Retrieves all saved device profiles.
    pub fn get_saved_devices(&self) -> Result<Vec<DeviceProfile>, StorageError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT device_id, friendly_name, last_known_address, is_trusted,
                    preferred_capabilities, created_at_unix, last_connected_unix, notes
             FROM saved_devices ORDER BY last_connected_unix DESC, friendly_name ASC",
        )?;

        let rows = stmt.query_map([], |row| {
            let device_id_str: String = row.get(0)?;
            let friendly_name: String = row.get(1)?;
            let addr_str: String = row.get(2)?;
            let is_trusted_int: i32 = row.get(3)?;
            let caps_bits: u32 = row.get(4)?;
            let created_at: i64 = row.get(5)?;
            let last_connected: Option<i64> = row.get(6)?;
            let notes: String = row.get(7)?;

            Ok((
                device_id_str,
                friendly_name,
                addr_str,
                is_trusted_int,
                caps_bits,
                created_at,
                last_connected,
                notes,
            ))
        })?;

        let mut devices = Vec::new();
        for r in rows {
            let (id_str, name, addr_str, trusted, caps, created, last_conn, notes) = r?;
            let device_id = DeviceId::new(id_str)?;
            let last_known_address = NetworkAddress::from_str(&addr_str)?;
            devices.push(DeviceProfile {
                device_id,
                friendly_name: name,
                last_known_address,
                is_trusted: trusted != 0,
                preferred_capabilities: CapabilitySet::from_bits(caps),
                created_at_unix: created,
                last_connected_unix: last_conn,
                notes,
            });
        }

        Ok(devices)
    }

    /// Deletes a device from saved computers.
    pub fn delete_device(&self, device_id: &DeviceId) -> Result<bool, StorageError> {
        let conn = self.conn.lock().unwrap();
        let rows_affected = conn.execute(
            "DELETE FROM saved_devices WHERE device_id = ?1",
            params![device_id.as_str()],
        )?;
        Ok(rows_affected > 0)
    }

    /// Records the start of a session in history.
    pub fn record_session_start(
        &self,
        session_id: &SessionId,
        remote_device_id: Option<&DeviceId>,
        remote_name: &str,
        address: &NetworkAddress,
        start_time_unix: i64,
        caps: CapabilitySet,
    ) -> Result<(), StorageError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO session_history (
                session_id, remote_device_id, remote_name, remote_address,
                start_time_unix, outcome, capabilities_granted
            ) VALUES (?1, ?2, ?3, ?4, ?5, 'Active', ?6)",
            params![
                session_id.to_string(),
                remote_device_id.map(|d| d.as_str()),
                remote_name,
                address.to_string(),
                start_time_unix,
                caps.bits(),
            ],
        )?;
        Ok(())
    }

    /// Updates session history upon completion.
    pub fn record_session_end(
        &self,
        session_id: &SessionId,
        end_time_unix: i64,
        outcome: &str,
    ) -> Result<(), StorageError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE session_history SET end_time_unix = ?1, outcome = ?2 WHERE session_id = ?3",
            params![end_time_unix, outcome, session_id.to_string()],
        )?;
        Ok(())
    }

    /// Retrieves recent session history.
    pub fn get_session_history(&self, limit: usize) -> Result<Vec<SessionHistoryRecord>, StorageError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT session_id, remote_device_id, remote_name, remote_address,
                    start_time_unix, end_time_unix, outcome, capabilities_granted
             FROM session_history ORDER BY start_time_unix DESC LIMIT ?1",
        )?;

        let rows = stmt.query_map(params![limit as i64], |row| {
            let session_id_str: String = row.get(0)?;
            let remote_dev_id: Option<String> = row.get(1)?;
            let remote_name: String = row.get(2)?;
            let addr_str: String = row.get(3)?;
            let start_time: i64 = row.get(4)?;
            let end_time: Option<i64> = row.get(5)?;
            let outcome: String = row.get(6)?;
            let caps_bits: u32 = row.get(7)?;

            Ok((
                session_id_str,
                remote_dev_id,
                remote_name,
                addr_str,
                start_time,
                end_time,
                outcome,
                caps_bits,
            ))
        })?;

        let mut records = Vec::new();
        for r in rows {
            let (sess_str, dev_opt, remote_name, addr_str, start, end, outcome, caps) = r?;
            let session_id = SessionId::from_str(&sess_str)?;
            let remote_device_id = match dev_opt {
                Some(s) => Some(DeviceId::new(s)?),
                None => None,
            };
            let remote_address = NetworkAddress::from_str(&addr_str)?;

            records.push(SessionHistoryRecord {
                session_id,
                remote_device_id,
                remote_name,
                remote_address,
                start_time_unix: start,
                end_time_unix: end,
                outcome,
                capabilities_granted: CapabilitySet::from_bits(caps),
            });
        }

        Ok(records)
    }

    /// Clears all session history.
    pub fn clear_history(&self) -> Result<(), StorageError> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM session_history", [])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_device_profile_crud() {
        let storage = Storage::open_in_memory().expect("Failed to open test in-memory storage");
        let dev_id = DeviceId::new("abcdef1234567890abcdef1234567890").unwrap();
        let addr = "192.168.1.50:47823".parse::<NetworkAddress>().unwrap();

        let profile = DeviceProfile {
            device_id: dev_id.clone(),
            friendly_name: "Lab Server".into(),
            last_known_address: addr.clone(),
            is_trusted: true,
            preferred_capabilities: CapabilitySet::FULL_CONTROL,
            created_at_unix: 1700000000,
            last_connected_unix: Some(1700000500),
            notes: "Main rack server".into(),
        };

        storage.save_device(&profile).unwrap();

        let devices = storage.get_saved_devices().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].friendly_name, "Lab Server");
        assert_eq!(devices[0].is_trusted, true);

        // Delete device
        assert!(storage.delete_device(&dev_id).unwrap());
        assert_eq!(storage.get_saved_devices().unwrap().len(), 0);
    }

    #[test]
    fn test_session_history() {
        let storage = Storage::open_in_memory().unwrap();
        let session_id = SessionId::new();
        let addr = "10.0.0.5:47823".parse::<NetworkAddress>().unwrap();

        storage
            .record_session_start(
                &session_id,
                None,
                "Workstation 1",
                &addr,
                1000,
                CapabilitySet::VIEW_ONLY,
            )
            .unwrap();

        let history = storage.get_session_history(10).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].outcome, "Active");

        storage
            .record_session_end(&session_id, 1500, "Clean Disconnect")
            .unwrap();

        let updated = storage.get_session_history(10).unwrap();
        assert_eq!(updated[0].outcome, "Clean Disconnect");
        assert_eq!(updated[0].end_time_unix, Some(1500));
    }
}
