# Keys

`twin-vendor.pub` is the Ed25519 public key that verifies licence key files
(see `crates/twin-license`). It is public and safe to commit.

The matching private key is **not** in this repository. It is kept outside the
repo, under the vendor's own control (`~/.tpt-keys/twin-vendor.seed` on the
machine that generated it), and is needed only to sign licence keys. Never
commit it.
