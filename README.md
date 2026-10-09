# Twin — Digital Twin Workbench

An offline-first desktop environment for designing, simulating and collaborating on physical digital twins. Parametric CAD, simulation, sensor fusion and LAN collaboration in one local application. No cloud account, no telemetry.

Status: design and early scaffolding. See [TODO.md](TODO.md) and [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). Spec: [spec.txt](spec.txt).

## Build

Requires the Rust stable toolchain (`rust-toolchain.toml` installs it with clippy and rustfmt).

```sh
cargo build --workspace
cargo test --workspace
cargo run -p twin-app
```

Foundation crates (`tpt-vertex`, `tpt-keystone-db`, `tpt-appfront`, ...) are not wired in yet. See the Phase 0 items in [TODO.md](TODO.md).

## Licence

Dual-licensed under MIT OR Apache-2.0. See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).
