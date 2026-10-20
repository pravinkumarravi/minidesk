//! Cryptographic security, identity, and verification for miniDesk.
//!
//! Implements Ed25519 device identities, digital signatures, peer verification,
//! and secure token generation with automatic zeroization.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use minidesk_domain::{DeviceId, DomainError};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop};

#[derive(Error, Debug)]
pub enum SecurityError {
    #[error("Cryptographic signature verification failed")]
    SignatureVerificationFailed,

    #[error("Invalid key format: {0}")]
    InvalidKey(String),

    #[error("Domain error: {0}")]
    Domain(#[from] DomainError),
}

/// A device identity backed by an Ed25519 keypair.
pub struct DeviceIdentity {
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
    device_id: DeviceId,
}

impl DeviceIdentity {
    /// Generates a new cryptographically secure device identity.
    pub fn generate() -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        let device_id = compute_device_id(&verifying_key);
        Self {
            signing_key,
            verifying_key,
            device_id,
        }
    }

    /// Loads an identity from raw 32-byte secret seed.
    pub fn from_secret_bytes(bytes: &[u8; 32]) -> Self {
        let signing_key = SigningKey::from_bytes(bytes);
        let verifying_key = signing_key.verifying_key();
        let device_id = compute_device_id(&verifying_key);
        Self {
            signing_key,
            verifying_key,
            device_id,
        }
    }

    /// Returns the raw 32 secret seed bytes.
    pub fn to_secret_bytes(&self) -> [u8; 32] {
        self.signing_key.to_bytes()
    }

    /// Returns the public verifying key.
    pub fn verifying_key(&self) -> &VerifyingKey {
        &self.verifying_key
    }

    /// Returns the public DeviceId fingerprint.
    pub fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    /// Signs an arbitrary message using the device's private key.
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        let signature = self.signing_key.sign(message);
        signature.to_bytes()
    }

    /// Verifies a signature against this device's public key.
    pub fn verify(&self, message: &[u8], signature_bytes: &[u8; 64]) -> Result<(), SecurityError> {
        let signature = Signature::from_bytes(signature_bytes);
        self.verifying_key
            .verify(message, &signature)
            .map_err(|_| SecurityError::SignatureVerificationFailed)
    }
}

/// Verifies a signature using an external peer's public key bytes.
pub fn verify_peer_signature(
    public_key_bytes: &[u8; 32],
    message: &[u8],
    signature_bytes: &[u8; 64],
) -> Result<DeviceId, SecurityError> {
    let verifying_key = VerifyingKey::from_bytes(public_key_bytes)
        .map_err(|e| SecurityError::InvalidKey(e.to_string()))?;
    let signature = Signature::from_bytes(signature_bytes);
    verifying_key
        .verify(message, &signature)
        .map_err(|_| SecurityError::SignatureVerificationFailed)?;

    Ok(compute_device_id(&verifying_key))
}

/// Computes the DeviceId fingerprint (SHA-256 hex string) of a public key.
fn compute_device_id(key: &VerifyingKey) -> DeviceId {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    let hash = hasher.finalize();
    let hex_str = hex::encode(hash);
    DeviceId::new(hex_str).expect("Valid device id format")
}

/// Ephemeral session challenge token.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct ChallengeToken([u8; 32]);

impl ChallengeToken {
    pub fn generate() -> Self {
        use rand::RngCore;
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_identity_sign_and_verify() {
        let identity = DeviceIdentity::generate();
        let message = b"miniDesk Handshake Auth Challenge";
        let sig = identity.sign(message);

        assert!(identity.verify(message, &sig).is_ok());

        // Tampered message must fail
        assert!(identity.verify(b"tampered message", &sig).is_err());
    }

    #[test]
    fn test_verify_peer_signature() {
        let alice = DeviceIdentity::generate();
        let message = b"Auth challenge from Bob";
        let sig = alice.sign(message);

        let peer_pub = alice.verifying_key().to_bytes();
        let device_id = verify_peer_signature(&peer_pub, message, &sig)
            .expect("Peer signature should verify");
        assert_eq!(&device_id, alice.device_id());
    }
}
