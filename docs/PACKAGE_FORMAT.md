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
  "entries": [
    { "path": "twin.json", "sha256": "<hex>", "size": 412 },
    { "path": "geometry/model.bin", "sha256": "<hex>", "size": 4 }
  ]
}
```

- `twin.json` is always listed. Its `id` must equal `twin_id`.
- Every entry is listed, with its SHA-256 and byte size.

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

- Which key signs user exports. Users need a per-user key or a shared trust store, not a vendor key. Not yet decided.
- Keystone snapshot entries (`keystone/…`). Phase 1 stores the twin in Keystone but has no snapshot export yet.
- Geometry and result entries: their formats are defined by Phases 2 and 4.
