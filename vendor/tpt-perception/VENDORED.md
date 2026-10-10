# Vendored: tpt-perception

Copy of `tpt-perception` from `github.com/TPT-Solutions/tpt-perception` at commit `5b07204dee8bd9bc18ddc567d9163511a241aefe` (master, 2026-10-10).

## Why it is vendored

Its workspace reaches the `tpt-math` crates through relative paths (`../tpt-math/crates/...`). That works inside a checkout of both repos, but a git dependency on `tpt-perception` alone cannot resolve them, so Cargo cannot fetch it directly.

## Changes from upstream

1. `Cargo.toml`: the five `tpt-math-*` path dependencies (`geometry`, `linalg-fixed`, `linalg-dense`, `numeric`, `units`) are git dependencies on `github.com/TPT-Solutions/tpt-math` at `2b86a62c6f323d06c715c862304ae064f153c279`. Nothing else changed.
2. `Cargo.lock` removed. Twin's workspace resolves the versions.

Source files, tests and the other crates match upstream.

## Verification

`cargo check -p tpt-percept-fusion -p tpt-percept-cloud -p tpt-percept-register` passes against the rewritten dependencies (scratch copy, before vendoring). Twin's own tests exercise `tpt-percept-cloud` and `tpt-percept-register` through `twin-sensors`.
