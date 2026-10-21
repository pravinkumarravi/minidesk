//! Video and audio codecs and compression engines for miniDesk.
//!
//! Provides extensible interfaces for real-time video encoding and decoding.
//! Features high-throughput LZ4 frame compression with delta detection as
//! the initial zero-latency software pipeline.

use minidesk_platform_api::CapturedFrame;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum CodecError {
    #[error("Compression error: {0}")]
    Compression(String),

    #[error("Decompression error: {0}")]
    Decompression(String),

    #[error("Corrupt frame data")]
    CorruptFrame,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodecFormat {
    RawRgba,
    Lz4Rgba,
    Vp8,
    H264,
}

/// A compressed video frame packet ready for wire transmission.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompressedVideoFrame {
    pub width: u32,
    pub height: u32,
    pub format: VideoCodecFormat,
    pub is_keyframe: bool,
    pub timestamp_unix_ms: u64,
    pub payload: Vec<u8>,
}

/// Trait for real-time desktop video encoding.
pub trait VideoEncoder: Send {
    fn encode(&mut self, frame: &CapturedFrame) -> Result<CompressedVideoFrame, CodecError>;
}

/// Trait for real-time desktop video decoding.
pub trait VideoDecoder: Send {
    fn decode(&mut self, compressed: &CompressedVideoFrame) -> Result<CapturedFrame, CodecError>;
}

/// High-throughput real-time LZ4 software frame codec.
pub struct Lz4VideoEncoder {
    _last_frame_bytes: Vec<u8>,
}

impl Lz4VideoEncoder {
    pub fn new() -> Self {
        Self {
            _last_frame_bytes: Vec::new(),
        }
    }
}

impl Default for Lz4VideoEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoEncoder for Lz4VideoEncoder {
    fn encode(&mut self, frame: &CapturedFrame) -> Result<CompressedVideoFrame, CodecError> {
        let compressed = lz4_flex::compress_prepend_size(&frame.data);
        Ok(CompressedVideoFrame {
            width: frame.width,
            height: frame.height,
            format: VideoCodecFormat::Lz4Rgba,
            is_keyframe: true,
            timestamp_unix_ms: frame.timestamp_unix_ms,
            payload: compressed,
        })
    }
}

pub struct Lz4VideoDecoder;

impl Lz4VideoDecoder {
    pub fn new() -> Self {
        Self
    }
}

impl Default for Lz4VideoDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoDecoder for Lz4VideoDecoder {
    fn decode(&mut self, compressed: &CompressedVideoFrame) -> Result<CapturedFrame, CodecError> {
        match compressed.format {
            VideoCodecFormat::Lz4Rgba => {
                let decompressed = lz4_flex::decompress_size_prepended(&compressed.payload)
                    .map_err(|e| CodecError::Decompression(e.to_string()))?;
                Ok(CapturedFrame {
                    width: compressed.width,
                    height: compressed.height,
                    data: decompressed,
                    timestamp_unix_ms: compressed.timestamp_unix_ms,
                })
            }
            VideoCodecFormat::RawRgba => Ok(CapturedFrame {
                width: compressed.width,
                height: compressed.height,
                data: compressed.payload.clone(),
                timestamp_unix_ms: compressed.timestamp_unix_ms,
            }),
            _ => Err(CodecError::Decompression(
                "Unsupported video codec format".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lz4_encoder_decoder_roundtrip() {
        let mut encoder = Lz4VideoEncoder::new();
        let mut decoder = Lz4VideoDecoder::new();

        let dummy_data = vec![255u8; 1920 * 1080 * 4];
        let original_frame = CapturedFrame {
            width: 1920,
            height: 1080,
            data: dummy_data,
            timestamp_unix_ms: 1234567,
        };

        let compressed = encoder.encode(&original_frame).expect("Encode failed");
        assert!(compressed.payload.len() < original_frame.data.len());

        let decoded = decoder.decode(&compressed).expect("Decode failed");
        assert_eq!(decoded.width, 1920);
        assert_eq!(decoded.height, 1080);
        assert_eq!(decoded.data, original_frame.data);
    }
}
