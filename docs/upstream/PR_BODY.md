**Title:** fix(tpt-sci-ode): align cranelift crates to 0.135 so the crate builds

## Problem

`tpt-sci-ode` does not compile at `master` (3aabe71). `crates/tpt-sci-ode/Cargo.toml` pins:

- `cranelift-codegen` = 0.135 (Dependabot bump in #12)
- `cranelift-frontend`, `cranelift-module`, `cranelift-jit` = 0.109

Two `cranelift-codegen` versions end up in the graph, and `src/jit.rs` fails with mismatched `TargetIsa` and `Signature` errors. Turning off default features does not help, because `jit.rs` compiles regardless.

## Change

- Bump `cranelift-frontend`, `cranelift-module` and `cranelift-jit` to 0.135, to match `cranelift-codegen`.
- `FunctionBuilder::finalize` takes a `TargetFrontendConfig` in 0.135. Both call sites now pass `module.isa().frontend_config()`.
- `benches/solve.rs`: use `std::hint::black_box`. `criterion::black_box` is deprecated and fails `clippy -D warnings`.

The diff is 7 insertions and 6 deletions across three files.

## Verification

- `cargo test -p tpt-sci-ode` passes: 23 + 6 + 1 + 1 tests, with 2 ignored.
- Clippy with `-D warnings` is clean for a workspace that includes this crate.
- Checked on Windows with stable Rust. Linux and macOS not run.

## Notes for review

- I have not checked whether an older commit would avoid the clash. The version alignment is the smallest change I found.
- Dependabot's #12 bumped only `cranelift-codegen`. It looks like the other cranelift crates should have moved with it. Dependabot may need a group rule so the cranelift crates update together.

---

🤖 Generated with [Claude Code](https://claude.com/claude-code)
