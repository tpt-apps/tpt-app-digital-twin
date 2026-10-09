//! Offline verification of signed licence key files.
//!
//! A key file is JSON with two fields. `payload` holds the licence facts.
//! `signature` is an Ed25519 signature over the payload's exact JSON bytes,
//! base64-encoded. Verification uses only the vendor's public key, which is
//! compiled in or supplied by the caller. Nothing here touches the network.
//!
//! Signing is not in this crate. The private key stays with the vendor, and
//! the issuing tool is kept out of the public build.

use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The product this build accepts. Keys for other products are refused.
pub const PRODUCT: &str = "twin";

#[derive(Debug, Error, PartialEq)]
pub enum LicenseError {
    #[error("licence file is not valid JSON or has the wrong shape: {0}")]
    Malformed(String),
    #[error("signature encoding is invalid")]
    BadSignatureEncoding,
    #[error("public key is not a valid Ed25519 key")]
    BadPublicKey,
    #[error("signature does not match this licence file")]
    InvalidSignature,
    #[error("licence is for product `{0}`, not `{PRODUCT}`")]
    WrongProduct(String),
}

/// The licence facts, signed by the vendor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LicensePayload {
    pub product: String,
    /// Unique per purchase, from the vendor's order system.
    pub license_id: String,
    pub licensee: String,
    /// ISO-8601 date the key was issued.
    pub issued: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct KeyFile {
    payload: LicensePayload,
    signature: String,
}

/// A licence that has passed verification.
#[derive(Debug, Clone, PartialEq)]
pub struct License {
    pub payload: LicensePayload,
}

/// Verifies key files against one vendor public key.
pub struct LicenseVerifier {
    key: VerifyingKey,
}

impl LicenseVerifier {
    pub fn new(public_key: &[u8; 32]) -> Result<Self, LicenseError> {
        VerifyingKey::from_bytes(public_key)
            .map(|key| Self { key })
            .map_err(|_| LicenseError::BadPublicKey)
    }

    /// Verify a key file's signature and product, and return its licence.
    ///
    /// The signature is checked over the payload's serialised bytes, so the
    /// signer and this verifier must serialise the payload the same way. Both
    /// use `serde_json::to_vec` on [`LicensePayload`], which keeps field order.
    pub fn verify(&self, key_file_json: &str) -> Result<License, LicenseError> {
        let file: KeyFile = serde_json::from_str(key_file_json)
            .map_err(|e| LicenseError::Malformed(e.to_string()))?;

        let sig_bytes = base64::engine::general_purpose::STANDARD
            .decode(&file.signature)
            .map_err(|_| LicenseError::BadSignatureEncoding)?;
        let signature =
            Signature::from_slice(&sig_bytes).map_err(|_| LicenseError::BadSignatureEncoding)?;

        let payload_bytes = serde_json::to_vec(&file.payload)
            .map_err(|e| LicenseError::Malformed(e.to_string()))?;
        self.key
            .verify(&payload_bytes, &signature)
            .map_err(|_| LicenseError::InvalidSignature)?;

        if file.payload.product != PRODUCT {
            return Err(LicenseError::WrongProduct(file.payload.product));
        }
        Ok(License {
            payload: file.payload,
        })
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Test-only signing. This keeps the issuing path out of the shipped crate.
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    pub fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    pub fn sign(payload: &LicensePayload) -> String {
        let bytes = serde_json::to_vec(payload).unwrap();
        let sig = signing_key().sign(&bytes);
        let file = KeyFile {
            payload: payload.clone(),
            signature: base64::engine::general_purpose::STANDARD.encode(sig.to_bytes()),
        };
        serde_json::to_string(&file).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use ed25519_dalek::SigningKey;

    fn payload() -> LicensePayload {
        LicensePayload {
            product: PRODUCT.into(),
            license_id: "GR-0001".into(),
            licensee: "Test Buyer".into(),
            issued: "2026-10-10".into(),
        }
    }

    fn verifier() -> LicenseVerifier {
        let public = signing_key().verifying_key().to_bytes();
        LicenseVerifier::new(&public).unwrap()
    }

    #[test]
    fn accepts_a_correctly_signed_key() {
        let key = sign(&payload());
        let license = verifier().verify(&key).unwrap();
        assert_eq!(license.payload, payload());
    }

    #[test]
    fn rejects_a_tampered_payload() {
        let key = sign(&payload());
        let tampered = key.replace("Test Buyer", "Someone Else");
        assert_eq!(
            verifier().verify(&tampered),
            Err(LicenseError::InvalidSignature)
        );
    }

    #[test]
    fn rejects_a_key_from_another_signer() {
        let other = SigningKey::from_bytes(&[9u8; 32]);
        let public = other.verifying_key().to_bytes();
        let key = sign(&payload());
        assert_eq!(
            LicenseVerifier::new(&public).unwrap().verify(&key),
            Err(LicenseError::InvalidSignature)
        );
    }

    #[test]
    fn rejects_a_key_for_another_product() {
        let mut other = payload();
        other.product = "data".into();
        let key = sign(&other);
        assert_eq!(
            verifier().verify(&key),
            Err(LicenseError::WrongProduct("data".into()))
        );
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(matches!(
            verifier().verify("{not json"),
            Err(LicenseError::Malformed(_))
        ));
    }

    #[test]
    fn rejects_bad_signature_encoding() {
        let json = r#"{"payload":{"product":"twin","license_id":"x","licensee":"y","issued":"2026-10-10"},"signature":"!!!"}"#;
        assert_eq!(
            verifier().verify(json),
            Err(LicenseError::BadSignatureEncoding)
        );
    }
}
