# Vendored: tpt-sci-ode

Copy of `tpt-sci-ode` from `github.com/TPT-Solutions/tpt-science` at commit `3aabe71aa22e0413558da828906ba0605f1ec36b` (master, 2026-10-10), with a fix for a build failure.

## Why it is vendored

At that commit the crate does not compile. Its `Cargo.toml` mixes `cranelift-codegen` 0.135 with `cranelift-frontend`, `cranelift-module` and `cranelift-jit` at 0.109. The two `cranelift-codegen` versions clash in `src/jit.rs`. See `docs/ARCHITECTURE.md` finding 6.

## Changes from upstream

1. `Cargo.toml`: `cranelift-frontend`, `cranelift-module` and `cranelift-jit` bumped from 0.109 to 0.135, matching `cranelift-codegen`.
2. `src/jit.rs`: `FunctionBuilder::finalize()` now takes a `TargetFrontendConfig` in 0.135. Both calls pass `module.isa().frontend_config()`.
3. `benches/solve.rs`: `criterion::black_box` is deprecated (and fails `clippy -D warnings`). Replaced with `std::hint::black_box`.
4. `Cargo.toml`: workspace inheritance (`*.workspace = true`) resolved to the values from the upstream root. Twin's workspace does not inherit from `tpt-science`.

Nothing else changed. `src/`, `tests/` and `examples/` match upstream.

## Verification

- `cargo test -p tpt-sci-ode` passes: 23 + 6 + 1 + 1 tests, plus 2 ignored.
- `cargo clippy --workspace --all-targets -- -D warnings` passes with this crate in the workspace.

## Upstream

The same fix is in `docs/upstream/tpt-sci-ode-cranelift.patch`, paths relative to the `tpt-science` repo root. It applies cleanly to upstream master (`git apply --check -p1`). It has not been submitted. Submitting it is a decision for the maintainer, and the vendored copy can be dropped once upstream fixes it.

## Licence

Upstream is `MIT OR Apache-2.0`. `LICENSE-MIT` and `LICENSE-APACHE` are copied here.
