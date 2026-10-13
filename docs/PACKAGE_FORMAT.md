# `.twin` package format

Version 1. Implemented in `crates/twin-package`.

A `.twin` file is a zip archive, optionally wrapped in passphrase encryption. Spec §10 and §9 call for a package that moves a complete twin (geometry, simulation setup, results) between machines cleanly.

## Plain package (zip)

| Entry | Contents |
|-------|----------|
| `manifest.json` | JSON manifest (below) |
| `manifest.sig` | 64-byte Ed25519 signature over the exact `manifest.json` bytes |
| `twin.json` | The Twin document (`twin-core`, schema version 1) |
| *extras* | Any other files the manifest lists, such as `geometry/…`, `results/…`, `sensors/…` |

### Manifest

```json
{
  "format_version": 1,
  "twin_id": "6f1c2a3e-…",
  "created": "2026-10-10T00:00:00Z",
  "signer": "<64 hex digits: the signer's Ed25519 public key>",
  "entries": [
    { "path": "twin.json", "sha256": "<hex>", "size": 412 },
    { "path": "geometry/model.bin", "sha256": "<hex>", "size": 4 }
  ]
}
```

- `twin.json` is always listed. Its `id` must equal `twin_id`.
- Every entry is listed, with its SHA-256 and byte size.
- `signer` is the public key that signed the manifest. It is covered by the signature. Packages made before it existed have no `signer`; they can be read only with a key that is supplied explicitly.

### Trust on first use

The app pins the signer of each twin the first time it imports that twin (the pins are kept in `trusted-signers.txt` in the project folder). A later package for the same twin signed by a different key is refused. Each user signs their own exports with a key created on first use in their settings folder: `%APPDATA%\twin\export-key.seed` on Windows, or `~/.config/twin/export-key.seed` elsewhere. Keep that file private. To accept a new key for a twin, delete its line in `trusted-signers.txt`; do this only if you trust the new key.

### Import rules

Import happens in this order. The first failure stops it.

1. Read `manifest.json` and `manifest.sig`. Verify the signature against the supplied public key. Otherwise: `BadSignature`.
2. Check `format_version` equals 1. Otherwise: `UnsupportedFormat`.
3. Reject any zip entry that the manifest does not list (other than the two manifest files). Otherwise: `Unlisted`.
4. For each listed entry, check the path is a safe relative path (no leading `/`, no `\`, no `:`, no empty or `..` segment). Then check the size and SHA-256. Otherwise: `UnsafePath` or `HashMismatch`.
5. Decode `twin.json` through `twin-core`. That checks the schema version and every invariant. Check `twin_id` matches.
6. Total unpacked size is capped at 2 GiB (`MAX_UNPACKED_BYTES`).

The signature is checked before any file is parsed, and every listed file is hashed before it is used.

## Encrypted package

Wrapping is applied to the whole plain package.

| Offset | Length | Field |
|--------|--------|-------|
| 0 | 8 | Magic `TWINENC1` |
| 8 | 16 | Argon2id salt (random) |
| 24 | 12 | ChaCha20-Poly1305 nonce (random) |
| 36 | rest | Ciphertext with 16-byte Poly1305 tag |

- Key: 32 bytes from Argon2id (`argon2` crate defaults), over the passphrase and salt.
- A plain zip starts with `PK`. A file starting with `TWINENC1` is encrypted.
- Wrong passphrase and tampered ciphertext both fail with `Decrypt`. They are not distinguished.

## Trust model

- The signature proves the package came from the holder of the vendor or author key. The key is supplied by the caller, and the crate ships no key.
- Encryption protects contents at rest and in transit. It does not sign anything.
- Every package is signed, and import always requires a valid signature. There is no unsigned mode.

## Open items

- Which key signs user exports: decided. Each user has a key of their own, and imports pin the signer per twin (see Trust on first use). A key that is lost cannot sign new packages for the same twin, so the pin must be reset by hand.
- Keystone snapshot entries (`keystone/…`). Phase 1 stores the twin in Keystone but has no snapshot export yet.
- Geometry and result entries: their formats are defined by Phases 2 and 4.
