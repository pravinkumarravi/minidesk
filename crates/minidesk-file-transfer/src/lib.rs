//! Safe chunked file transfer engine with SHA-256 verification and path sanitization.
//!
//! Provides bounded-memory streaming, path traversal prevention, Windows reserved name
//! filtering, safe temporary file isolation (`.part`), and integrity verification.

use minidesk_protocol::FileTransferMessage;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Default transfer chunk size: 64 KB.
pub const DEFAULT_CHUNK_SIZE: usize = 64 * 1024;

/// Maximum allowed single file transfer size (2 GB by default for safety).
pub const MAX_FILE_SIZE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Error, Debug)]
pub enum FileTransferError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Security error: invalid or unsafe filename '{0}': {1}")]
    InvalidPath(String, &'static str),

    #[error("File size {0} exceeds maximum limit {1}")]
    FileTooLarge(u64, u64),

    #[error("Checksum mismatch: expected {0}, computed {1}")]
    HashMismatch(String, String),

    #[error("Invalid transfer state: {0}")]
    InvalidState(&'static str),

    #[error("Chunk order error: expected index {0}, got {1}")]
    ChunkOrderError(u64, u64),
}

/// Sanitizes a filename from an incoming transfer request to prevent path traversal
/// and dangerous Windows device name hijacking.
pub fn sanitize_filename(raw: &str) -> Result<String, FileTransferError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(FileTransferError::InvalidPath(
            raw.to_string(),
            "Filename cannot be empty",
        ));
    }

    // Reject path separators
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains(':') {
        return Err(FileTransferError::InvalidPath(
            raw.to_string(),
            "Path separators and drive designators are not allowed",
        ));
    }

    // Reject directory navigation
    if trimmed == "." || trimmed == ".." {
        return Err(FileTransferError::InvalidPath(
            raw.to_string(),
            "Relative directory traversal navigation is not allowed",
        ));
    }

    // Reject control characters & null bytes
    if trimmed.chars().any(|c| c.is_control() || c == '\0') {
        return Err(FileTransferError::InvalidPath(
            raw.to_string(),
            "Filename contains invalid control characters or null bytes",
        ));
    }

    // Check Windows reserved device names (CON, PRN, AUX, NUL, COM1..9, LPT1..9)
    let stem = Path::new(trimmed)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(trimmed);

    let upper_stem = stem.to_ascii_uppercase();
    let reserved = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];

    if reserved.contains(&upper_stem.as_str()) {
        return Err(FileTransferError::InvalidPath(
            raw.to_string(),
            "Filename conflicts with reserved OS device names",
        ));
    }

    if trimmed.len() > 255 {
        return Err(FileTransferError::InvalidPath(
            raw.to_string(),
            "Filename exceeds 255 characters",
        ));
    }

    Ok(trimmed.to_string())
}

/// Outgoing file sender that streams file data in bounded chunks.
pub struct FileSender {
    pub transfer_id: u64,
    pub filename: String,
    pub file_size_bytes: u64,
    pub sha256_hash: [u8; 32],
    file: File,
    chunk_size: usize,
    current_chunk_index: u64,
}

impl FileSender {
    pub fn open(path: &Path) -> Result<Self, FileTransferError> {
        let metadata = fs::metadata(path)?;
        let file_size_bytes = metadata.len();
        if file_size_bytes > MAX_FILE_SIZE_BYTES {
            return Err(FileTransferError::FileTooLarge(
                file_size_bytes,
                MAX_FILE_SIZE_BYTES,
            ));
        }

        let raw_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                FileTransferError::InvalidPath(
                    path.display().to_string(),
                    "Missing or non-UTF8 filename",
                )
            })?;
        let filename = sanitize_filename(raw_name)?;

        // Precompute SHA-256 in a streaming pass
        let mut file = File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; DEFAULT_CHUNK_SIZE];
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }

        let hash_result = hasher.finalize();
        let mut sha256_hash = [0u8; 32];
        sha256_hash.copy_from_slice(&hash_result);

        // Reset file offset to beginning for sending
        file.seek(SeekFrom::Start(0))?;

        let transfer_id = rand::random::<u64>();

        Ok(Self {
            transfer_id,
            filename,
            file_size_bytes,
            sha256_hash,
            file,
            chunk_size: DEFAULT_CHUNK_SIZE,
            current_chunk_index: 0,
        })
    }

    /// Generates initial transfer handshake request.
    pub fn request_message(&self) -> FileTransferMessage {
        FileTransferMessage::Request {
            transfer_id: self.transfer_id,
            filename: self.filename.clone(),
            file_size_bytes: self.file_size_bytes,
            sha256_hash: self.sha256_hash,
        }
    }

    /// Reads and returns the next chunk message. Returns None on EOF.
    pub fn next_chunk(&mut self) -> Result<Option<FileTransferMessage>, FileTransferError> {
        let mut buffer = vec![0u8; self.chunk_size];
        let bytes_read = self.file.read(&mut buffer)?;
        if bytes_read == 0 {
            return Ok(None);
        }

        buffer.truncate(bytes_read);
        let msg = FileTransferMessage::Chunk {
            transfer_id: self.transfer_id,
            chunk_index: self.current_chunk_index,
            data: buffer,
        };
        self.current_chunk_index += 1;
        Ok(Some(msg))
    }

    /// Generates the completion notification.
    pub fn complete_message(&self) -> FileTransferMessage {
        FileTransferMessage::Complete {
            transfer_id: self.transfer_id,
        }
    }
}

/// Incoming file receiver that validates integrity and isolates partial data.
pub struct FileReceiver {
    pub transfer_id: u64,
    pub filename: String,
    pub file_size_bytes: u64,
    pub expected_hash: [u8; 32],
    dest_path: PathBuf,
    part_path: PathBuf,
    part_file: Option<File>,
    hasher: Sha256,
    bytes_written: u64,
    expected_chunk_index: u64,
    is_finished: bool,
}

impl FileReceiver {
    pub fn new(
        dest_dir: &Path,
        transfer_id: u64,
        filename: &str,
        file_size_bytes: u64,
        expected_hash: [u8; 32],
    ) -> Result<Self, FileTransferError> {
        if file_size_bytes > MAX_FILE_SIZE_BYTES {
            return Err(FileTransferError::FileTooLarge(
                file_size_bytes,
                MAX_FILE_SIZE_BYTES,
            ));
        }

        let sanitized = sanitize_filename(filename)?;
        fs::create_dir_all(dest_dir)?;

        let dest_path = dest_dir.join(&sanitized);
        let part_filename = format!("{}.part.{}", sanitized, transfer_id);
        let part_path = dest_dir.join(part_filename);

        let part_file = File::create(&part_path)?;

        Ok(Self {
            transfer_id,
            filename: sanitized,
            file_size_bytes,
            expected_hash,
            dest_path,
            part_path,
            part_file: Some(part_file),
            hasher: Sha256::new(),
            bytes_written: 0,
            expected_chunk_index: 0,
            is_finished: false,
        })
    }

    /// Writes a chunk of received data and updates the streaming SHA-256 hasher.
    pub fn write_chunk(
        &mut self,
        chunk_index: u64,
        data: &[u8],
    ) -> Result<(), FileTransferError> {
        if self.is_finished {
            return Err(FileTransferError::InvalidState(
                "Cannot write to already finalized transfer",
            ));
        }

        if chunk_index != self.expected_chunk_index {
            return Err(FileTransferError::ChunkOrderError(
                self.expected_chunk_index,
                chunk_index,
            ));
        }

        let file = self
            .part_file
            .as_mut()
            .ok_or(FileTransferError::InvalidState("Partial file handle closed"))?;

        file.write_all(data)?;
        self.hasher.update(data);
        self.bytes_written += data.len() as u64;
        self.expected_chunk_index += 1;

        Ok(())
    }

    /// Progress percentage from 0.0 to 1.0.
    pub fn progress(&self) -> f32 {
        if self.file_size_bytes == 0 {
            1.0
        } else {
            (self.bytes_written as f32 / self.file_size_bytes as f32).clamp(0.0, 1.0)
        }
    }

    /// Finalizes the transfer, verifies the SHA-256 integrity hash, and atomically
    /// moves the temporary `.part` file to the destination path.
    pub fn finalize(&mut self) -> Result<PathBuf, FileTransferError> {
        if self.is_finished {
            return Ok(self.dest_path.clone());
        }

        // Flush and close file handle
        if let Some(mut file) = self.part_file.take() {
            file.flush()?;
        }

        let computed_hash = self.hasher.clone().finalize();
        let mut computed_bytes = [0u8; 32];
        computed_bytes.copy_from_slice(&computed_hash);

        if computed_bytes != self.expected_hash {
            // Cleanup partial file on corruption
            let _ = fs::remove_file(&self.part_path);
            return Err(FileTransferError::HashMismatch(
                hex_encode(&self.expected_hash),
                hex_encode(&computed_bytes),
            ));
        }

        // Atomically rename .part file to final destination
        fs::rename(&self.part_path, &self.dest_path)?;
        self.is_finished = true;

        Ok(self.dest_path.clone())
    }

    /// Aborts the transfer and deletes any partial temporary files.
    pub fn abort(&mut self) {
        self.part_file = None;
        let _ = fs::remove_file(&self.part_path);
        self.is_finished = true;
    }
}

impl Drop for FileReceiver {
    fn drop(&mut self) {
        if !self.is_finished {
            self.abort();
        }
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_sanitize_filename_attacks() {
        // Path traversal
        assert!(sanitize_filename("../../etc/passwd").is_err());
        assert!(sanitize_filename("..\\Windows\\System32\\cmd.exe").is_err());
        assert!(sanitize_filename("/root/.ssh/id_rsa").is_err());
        assert!(sanitize_filename("C:\\autoexec.bat").is_err());
        assert!(sanitize_filename(".").is_err());
        assert!(sanitize_filename("..").is_err());
        assert!(sanitize_filename("").is_err());

        // Control characters & null bytes
        assert!(sanitize_filename("evil\0file.txt").is_err());
        assert!(sanitize_filename("newline\nfile.txt").is_err());

        // Reserved Windows names
        assert!(sanitize_filename("CON").is_err());
        assert!(sanitize_filename("con.txt").is_err());
        assert!(sanitize_filename("prn.doc").is_err());
        assert!(sanitize_filename("aux").is_err());
        assert!(sanitize_filename("NUL.exe").is_err());
        assert!(sanitize_filename("com1.bat").is_err());
        assert!(sanitize_filename("lpt3.zip").is_err());

        // Valid filenames
        assert_eq!(
            sanitize_filename("report-2026.pdf").unwrap(),
            "report-2026.pdf"
        );
        assert_eq!(
            sanitize_filename("my photo (1).jpeg").unwrap(),
            "my photo (1).jpeg"
        );
    }

    #[test]
    fn test_sender_and_receiver_roundtrip() {
        let temp_dir = tempdir().unwrap();
        let src_path = temp_dir.path().join("source_document.txt");
        let dest_dir = temp_dir.path().join("received_downloads");

        let test_payload = b"Hello, secure miniDesk chunked file transfer with SHA-256 integrity verification!".repeat(500);
        fs::write(&src_path, &test_payload).unwrap();

        // Sender initialization
        let mut sender = FileSender::open(&src_path).unwrap();
        assert_eq!(sender.filename, "source_document.txt");
        assert_eq!(sender.file_size_bytes, test_payload.len() as u64);

        // Receiver initialization from sender's metadata
        let mut receiver = FileReceiver::new(
            &dest_dir,
            sender.transfer_id,
            &sender.filename,
            sender.file_size_bytes,
            sender.sha256_hash,
        )
        .unwrap();

        // Stream all chunks
        while let Some(msg) = sender.next_chunk().unwrap() {
            if let FileTransferMessage::Chunk {
                chunk_index, data, ..
            } = msg
            {
                receiver.write_chunk(chunk_index, &data).unwrap();
            }
        }

        assert_eq!(receiver.progress(), 1.0);

        // Finalize and verify
        let dest_file = receiver.finalize().unwrap();
        assert!(dest_file.exists());

        let received_content = fs::read(&dest_file).unwrap();
        assert_eq!(received_content, test_payload);
    }

    #[test]
    fn test_corrupted_chunk_fails_hash_verification() {
        let temp_dir = tempdir().unwrap();
        let src_path = temp_dir.path().join("test_file.bin");
        let dest_dir = temp_dir.path().join("corrupted_dest");

        let test_payload = b"Payload for hash mismatch test";
        fs::write(&src_path, test_payload).unwrap();

        let mut sender = FileSender::open(&src_path).unwrap();
        let mut receiver = FileReceiver::new(
            &dest_dir,
            sender.transfer_id,
            &sender.filename,
            sender.file_size_bytes,
            sender.sha256_hash,
        )
        .unwrap();

        let chunk = sender.next_chunk().unwrap().unwrap();
        if let FileTransferMessage::Chunk {
            chunk_index,
            mut data,
            ..
        } = chunk
        {
            // Corrupt one byte
            data[0] ^= 0xFF;
            receiver.write_chunk(chunk_index, &data).unwrap();
        }

        // Finalization must fail with HashMismatch
        let res = receiver.finalize();
        assert!(res.is_err());
        match res.unwrap_err() {
            FileTransferError::HashMismatch(..) => {}
            err => panic!("Expected HashMismatch, got {:?}", err),
        }

        // Must not leave destination file or .part file behind
        let expected_dest = dest_dir.join("test_file.bin");
        assert!(!expected_dest.exists());
    }

    #[test]
    fn test_abort_cleans_up_part_file() {
        let temp_dir = tempdir().unwrap();
        let dest_dir = temp_dir.path().join("abort_dest");
        let mut receiver = FileReceiver::new(
            &dest_dir,
            9999,
            "unfinished.txt",
            1024,
            [0u8; 32],
        )
        .unwrap();

        let part_path = receiver.part_path.clone();
        assert!(part_path.exists());

        receiver.abort();
        assert!(!part_path.exists());
    }
}
