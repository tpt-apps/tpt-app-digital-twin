//! The `.twin` package container: manifest, signing and optional encryption.
//!
//! A package is a zip archive with these entries:
//!
//! - `manifest.json`: format version, twin ID, and the SHA-256 and size of every
//!   other entry.
//! - `manifest.sig`: Ed25519 signature over the exact `manifest.json` bytes.
//! - `twin.json`: the Twin document.
//! - any extra entries the manifest lists (geometry, results, sensor history).
//!
//! Import checks the signature first, then each listed file's hash and size.
//! It rejects any zip entry the manifest does not list. A package can also be
//! wrapped in passphrase encryption (see [`encrypt`]). The wrapper is ChaCha20-Poly1305
//! with a key derived by Argon2id.
//!
//! The package is signed, not secret. Encryption is optional and separate.

use std::io::{Cursor, Read, Write};

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use twin_core::Twin;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// Version of this container layout.
pub const FORMAT_VERSION: u32 = 1;

/// Largest total uncompressed size accepted on import. Stops zip bombs.
pub const MAX_UNPACKED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

const MANIFEST: &str = "manifest.json";
const SIGNATURE: &str = "manifest.sig";
const TWIN: &str = "twin.json";
const ENCRYPTED_MAGIC: &[u8; 8] = b"TWINENC1";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;

#[derive(Debug, Error)]
pub enum PackageError {
    #[error("package is not a valid zip archive: {0}")]
    Archive(String),
    #[error("package is missing {0}")]
    Missing(&'static str),
    #[error("manifest is malformed: {0}")]
    Manifest(String),
    #[error("manifest signature is invalid")]
    BadSignature,
    #[error("format version {found} is not supported (this build reads {supported})")]
    UnsupportedFormat { found: u32, supported: u32 },
    #[error("file `{path}` does not match its manifest entry (tampered or truncated)")]
    HashMismatch { path: String },
    #[error("archive contains `{0}`, which the manifest does not list")]
    Unlisted(String),
    #[error("archive would unpack to more than {MAX_UNPACKED_BYTES} bytes")]
    TooLarge,
    #[error("entry name `{0}` is not a safe relative path")]
    UnsafePath(String),
    #[error("encrypted package needs a passphrase")]
    PassphraseRequired,
    #[error("wrong passphrase or corrupted encrypted package")]
    Decrypt,
    #[error("key derivation failed")]
    Kdf,
    #[error("random number generator failed")]
    Random,
    #[error(transparent)]
    Twin(#[from] twin_core::TwinError),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub path: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    pub twin_id: String,
    pub created: String,
    /// The signer's Ed25519 public key, as 64 hex digits. Signed along with
    /// the rest of the manifest. Packages written before this field existed
    /// have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer: Option<String>,
    pub entries: Vec<ManifestEntry>,
}

/// A verified package: the twin plus any extra entries, as raw bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct Package {
    pub twin: Twin,
    /// Extra entries by path, for example `geometry/model.bin`.
    pub extras: Vec<(String, Vec<u8>)>,
}

/// Build a signed package. `created` is an ISO-8601 timestamp.
pub fn export(
    twin: &Twin,
    extras: &[(String, Vec<u8>)],
    created: &str,
    signer: &SigningKey,
) -> Result<Vec<u8>, PackageError> {
    let twin_json = twin.to_json()?.into_bytes();

    let mut files: Vec<(String, Vec<u8>)> = vec![(TWIN.to_string(), twin_json)];
    for (path, data) in extras {
        check_safe_path(path)?;
        files.push((path.clone(), data.clone()));
    }

    let manifest = Manifest {
        format_version: FORMAT_VERSION,
        twin_id: twin.id.to_string(),
        created: created.to_string(),
        signer: Some(hex(signer.verifying_key().as_bytes())),
        entries: files
            .iter()
            .map(|(path, data)| ManifestEntry {
                path: path.clone(),
                sha256: hex(&Sha256::digest(data)),
                size: data.len() as u64,
            })
            .collect(),
    };
    let manifest_bytes =
        serde_json::to_vec_pretty(&manifest).map_err(|e| PackageError::Manifest(e.to_string()))?;
    let signature = signer.sign(&manifest_bytes);

    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    write_entry(&mut writer, MANIFEST, &manifest_bytes, options)?;
    write_entry(&mut writer, SIGNATURE, &signature.to_bytes(), options)?;
    for (path, data) in &files {
        write_entry(&mut writer, path, data, options)?;
    }
    let cursor = writer
        .finish()
        .map_err(|e| PackageError::Archive(e.to_string()))?;
    Ok(cursor.into_inner())
}

/// The public key that signed `bytes`, as recorded in its manifest. This is
/// not a verification: `import` with this key does the checking. Callers use
/// it to decide whether to trust the signer before importing.
pub fn signer_key(bytes: &[u8]) -> Result<VerifyingKey, PackageError> {
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|e| PackageError::Archive(e.to_string()))?;
    let manifest_bytes =
        read_entry(&mut archive, MANIFEST)?.ok_or(PackageError::Missing(MANIFEST))?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| PackageError::Manifest(e.to_string()))?;
    let hex_key = manifest
        .signer
        .ok_or_else(|| PackageError::Manifest("package does not name its signer".into()))?;
    let bytes: [u8; 32] = (0..64)
        .step_by(2)
        .map(|i| u8::from_str_radix(hex_key.get(i..i + 2).unwrap_or("zz"), 16))
        .collect::<Result<Vec<u8>, _>>()
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or_else(|| PackageError::Manifest("signer key is not 64 hex digits".into()))?;
    VerifyingKey::from_bytes(&bytes)
        .map_err(|_| PackageError::Manifest("signer key is invalid".into()))
}

/// Verify and open a package. Fails on any signature, hash or path problem.
pub fn import(bytes: &[u8], verifier: &VerifyingKey) -> Result<Package, PackageError> {
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|e| PackageError::Archive(e.to_string()))?;

    let manifest_bytes =
        read_entry(&mut archive, MANIFEST)?.ok_or(PackageError::Missing(MANIFEST))?;
    let signature_bytes =
        read_entry(&mut archive, SIGNATURE)?.ok_or(PackageError::Missing(SIGNATURE))?;
    let signature =
        Signature::from_slice(&signature_bytes).map_err(|_| PackageError::BadSignature)?;
    verifier
        .verify(&manifest_bytes, &signature)
        .map_err(|_| PackageError::BadSignature)?;

    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| PackageError::Manifest(e.to_string()))?;
    if manifest.format_version != FORMAT_VERSION {
        return Err(PackageError::UnsupportedFormat {
            found: manifest.format_version,
            supported: FORMAT_VERSION,
        });
    }

    let listed: std::collections::BTreeSet<&str> =
        manifest.entries.iter().map(|e| e.path.as_str()).collect();
    for name in archive_names(&mut archive)? {
        if name != MANIFEST && name != SIGNATURE && !listed.contains(name.as_str()) {
            return Err(PackageError::Unlisted(name));
        }
    }

    let mut total: u64 = 0;
    let mut files = std::collections::BTreeMap::new();
    for entry in &manifest.entries {
        check_safe_path(&entry.path)?;
        total = total.saturating_add(entry.size);
        if total > MAX_UNPACKED_BYTES {
            return Err(PackageError::TooLarge);
        }
        let data = read_entry(&mut archive, &entry.path)?
            .ok_or(PackageError::Missing("a listed entry"))?;
        if data.len() as u64 != entry.size || hex(&Sha256::digest(&data)) != entry.sha256 {
            return Err(PackageError::HashMismatch {
                path: entry.path.clone(),
            });
        }
        files.insert(entry.path.clone(), data);
    }

    let twin_bytes = files.remove(TWIN).ok_or(PackageError::Missing(TWIN))?;
    let twin_text =
        std::str::from_utf8(&twin_bytes).map_err(|e| PackageError::Manifest(e.to_string()))?;
    let twin = Twin::from_json(twin_text)?;
    if twin.id.to_string() != manifest.twin_id {
        return Err(PackageError::Manifest(
            "twin ID in manifest does not match twin.json".into(),
        ));
    }

    Ok(Package {
        twin,
        extras: files.into_iter().collect(),
    })
}

/// Wrap a package in passphrase encryption: magic, salt, nonce, ciphertext.
pub fn encrypt(package: &[u8], passphrase: &str) -> Result<Vec<u8>, PackageError> {
    let mut salt = [0u8; SALT_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut salt).map_err(|_| PackageError::Random)?;
    getrandom::fill(&mut nonce).map_err(|_| PackageError::Random)?;

    let key = derive_key(passphrase, &salt)?;
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), package)
        .map_err(|_| PackageError::Decrypt)?;

    let mut out =
        Vec::with_capacity(ENCRYPTED_MAGIC.len() + SALT_LEN + NONCE_LEN + ciphertext.len());
    out.extend_from_slice(ENCRYPTED_MAGIC);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// True if the bytes carry the encrypted wrapper.
pub fn is_encrypted(bytes: &[u8]) -> bool {
    bytes.starts_with(ENCRYPTED_MAGIC)
}

/// Undo [`encrypt`]. A passphrase is needed only for encrypted input. Plain
/// packages pass through unchanged.
pub fn decrypt(bytes: &[u8], passphrase: Option<&str>) -> Result<Vec<u8>, PackageError> {
    if !is_encrypted(bytes) {
        return Ok(bytes.to_vec());
    }
    let passphrase = passphrase.ok_or(PackageError::PassphraseRequired)?;
    let header = ENCRYPTED_MAGIC.len() + SALT_LEN + NONCE_LEN;
    if bytes.len() < header {
        return Err(PackageError::Decrypt);
    }
    let salt = &bytes[ENCRYPTED_MAGIC.len()..ENCRYPTED_MAGIC.len() + SALT_LEN];
    let nonce = &bytes[ENCRYPTED_MAGIC.len() + SALT_LEN..header];
    let key = derive_key(passphrase, salt)?;
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
    cipher
        .decrypt(Nonce::from_slice(nonce), &bytes[header..])
        .map_err(|_| PackageError::Decrypt)
}

fn derive_key(passphrase: &str, salt: &[u8]) -> Result<[u8; 32], PackageError> {
    let mut key = [0u8; 32];
    argon2::Argon2::default()
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|_| PackageError::Kdf)?;
    Ok(key)
}

fn write_entry(
    writer: &mut ZipWriter<Cursor<Vec<u8>>>,
    path: &str,
    data: &[u8],
    options: SimpleFileOptions,
) -> Result<(), PackageError> {
    writer
        .start_file(path, options)
        .map_err(|e| PackageError::Archive(e.to_string()))?;
    writer
        .write_all(data)
        .map_err(|e| PackageError::Archive(e.to_string()))
}

fn read_entry(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    path: &str,
) -> Result<Option<Vec<u8>>, PackageError> {
    let mut file = match archive.by_name(path) {
        Ok(file) => file,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(PackageError::Archive(e.to_string())),
    };
    let mut data = Vec::new();
    file.by_ref()
        .take(MAX_UNPACKED_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|e| PackageError::Archive(e.to_string()))?;
    if data.len() as u64 > MAX_UNPACKED_BYTES {
        return Err(PackageError::TooLarge);
    }
    Ok(Some(data))
}

fn archive_names(archive: &mut ZipArchive<Cursor<&[u8]>>) -> Result<Vec<String>, PackageError> {
    let mut names = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let file = archive
            .by_index(i)
            .map_err(|e| PackageError::Archive(e.to_string()))?;
        names.push(file.name().to_string());
    }
    Ok(names)
}

/// Reject absolute paths, parent-directory escapes, and backslashes.
fn check_safe_path(path: &str) -> Result<(), PackageError> {
    let unsafe_path = path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains(':')
        || path.split('/').any(|part| part.is_empty() || part == "..");
    if unsafe_path {
        Err(PackageError::UnsafePath(path.to_string()))
    } else {
        Ok(())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    #[test]
    fn the_signer_is_recorded_and_can_be_read_before_import() {
        let bytes = export(&twin(), &extras(), "2026-10-10T00:00:00Z", &signer()).unwrap();
        assert_eq!(signer_key(&bytes).unwrap(), signer().verifying_key());
        // The recorded signer is the key that import checks against.
        assert!(import(&bytes, &signer_key(&bytes).unwrap()).is_ok());
        let other = SigningKey::from_bytes(&[8u8; 32]).verifying_key();
        assert!(import(&bytes, &other).is_err());
    }

    fn signer() -> SigningKey {
        SigningKey::from_bytes(&[3u8; 32])
    }

    /// A fixed ID, so repeated calls compare equal.
    fn twin() -> Twin {
        let mut t = Twin::new("Robot arm").unwrap();
        t.id = twin_core::TwinId::parse("6f1c2a3e-0000-4000-8000-000000000001").unwrap();
        t.set_parameter("mass_kg", 2.5).unwrap();
        t
    }

    fn extras() -> Vec<(String, Vec<u8>)> {
        vec![("geometry/model.bin".into(), vec![1, 2, 3, 4])]
    }

    fn pkg() -> Vec<u8> {
        export(&twin(), &extras(), "2026-10-10T00:00:00Z", &signer()).unwrap()
    }

    fn verifier() -> VerifyingKey {
        signer().verifying_key()
    }

    #[test]
    fn export_then_import_round_trips() {
        let imported = import(&pkg(), &verifier()).unwrap();
        assert_eq!(imported.twin, twin());
        assert_eq!(imported.extras, extras());
    }

    #[test]
    fn rejects_a_package_signed_by_someone_else() {
        let other = SigningKey::from_bytes(&[8u8; 32]).verifying_key();
        assert!(matches!(
            import(&pkg(), &other),
            Err(PackageError::BadSignature)
        ));
    }

    #[test]
    fn rejects_an_entry_listed_in_the_manifest_but_changed() {
        // Build an archive whose manifest is signed but whose payload is swapped.
        let good = pkg();
        let mut archive = ZipArchive::new(Cursor::new(&good[..])).unwrap();
        let manifest = read_entry(&mut archive, MANIFEST).unwrap().unwrap();
        let sig = read_entry(&mut archive, SIGNATURE).unwrap().unwrap();
        let twin_json = read_entry(&mut archive, TWIN).unwrap().unwrap();
        let mut forged = Vec::new();
        {
            let mut w = ZipWriter::new(Cursor::new(&mut forged));
            let opts = SimpleFileOptions::default();
            w.start_file(MANIFEST, opts).unwrap();
            w.write_all(&manifest).unwrap();
            w.start_file(SIGNATURE, opts).unwrap();
            w.write_all(&sig).unwrap();
            w.start_file("geometry/model.bin", opts).unwrap();
            w.write_all(&[9, 9, 9, 9]).unwrap();
            w.start_file(TWIN, opts).unwrap();
            w.write_all(&twin_json).unwrap();
            w.finish().unwrap();
        }
        assert!(matches!(
            import(&forged, &verifier()),
            Err(PackageError::HashMismatch { .. })
        ));
    }

    #[test]
    fn rejects_an_unlisted_entry() {
        let good = pkg();
        let mut archive = ZipArchive::new(Cursor::new(&good[..])).unwrap();
        let names = archive_names(&mut archive).unwrap();
        let mut smuggled = Vec::new();
        {
            let mut w = ZipWriter::new(Cursor::new(&mut smuggled));
            let opts = SimpleFileOptions::default();
            for name in &names {
                let data = read_entry(&mut archive, name).unwrap().unwrap();
                w.start_file(name.as_str(), opts).unwrap();
                w.write_all(&data).unwrap();
            }
            w.start_file("extra.txt", opts).unwrap();
            w.write_all(b"smuggled").unwrap();
            w.finish().unwrap();
        }
        assert!(matches!(
            import(&smuggled, &verifier()),
            Err(PackageError::Unlisted(name)) if name == "extra.txt"
        ));
    }

    #[test]
    fn refuses_unsafe_extra_paths_on_export() {
        for bad in ["../escape", "/abs", "a\\b", "C:x", "a//b", ""] {
            let result = export(
                &twin(),
                &[(bad.to_string(), vec![0])],
                "2026-10-10T00:00:00Z",
                &signer(),
            );
            assert!(
                matches!(result, Err(PackageError::UnsafePath(_))),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn encrypted_package_round_trips_with_right_passphrase() {
        let wrapped = encrypt(&pkg(), "correct horse").unwrap();
        assert!(is_encrypted(&wrapped));
        let plain = decrypt(&wrapped, Some("correct horse")).unwrap();
        assert_eq!(import(&plain, &verifier()).unwrap().twin, twin());
    }

    #[test]
    fn wrong_passphrase_is_rejected() {
        let wrapped = encrypt(&pkg(), "correct horse").unwrap();
        assert!(matches!(
            decrypt(&wrapped, Some("battery staple")),
            Err(PackageError::Decrypt)
        ));
    }

    #[test]
    fn encrypted_package_needs_a_passphrase() {
        let wrapped = encrypt(&pkg(), "pw").unwrap();
        assert!(matches!(
            decrypt(&wrapped, None),
            Err(PackageError::PassphraseRequired)
        ));
    }

    #[test]
    fn plain_package_passes_through_decrypt() {
        let plain = pkg();
        assert_eq!(decrypt(&plain, None).unwrap(), plain);
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let mut wrapped = encrypt(&pkg(), "pw").unwrap();
        let last = wrapped.len() - 1;
        wrapped[last] ^= 0x01;
        assert!(matches!(
            decrypt(&wrapped, Some("pw")),
            Err(PackageError::Decrypt)
        ));
    }
}
