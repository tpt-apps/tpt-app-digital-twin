# Twin — Architecture

Audit of the foundation crates and the layering that follows from it. Source for the design: `spec.txt`.

## Foundation crates (audited 2026-10-10)

All are from `github.com/TPT-Solutions/<crate>` at `master`. Shallow-cloned and read. `tpt-vertex-collab` and `tpt-keystone` are the only two wired into the workspace so far, and both check cleanly on Windows.

| Crate | Pinned master commit | Edition | Licence | What it gives Twin |
|-------|----------------------|---------|---------|--------------------|
| `tpt-vertex` | `898e6efd4befd9a9405d791d4d83570f8fe49a98` | 2021 | MIT OR Apache-2.0 | Parametric CAD kernel, feature tree, **CRDT collab** (`collab/`: HLC clocks, OR-Set + LWW registers, `SyncHub` with auth, access control and offline resync; WebSocket server), merge/branch/history vocabulary |
| `tpt-keystone-db` | `b861edd6f01e08061e0e1f3a86e6a2a1e1f2f449` | (monorepo; library crate `tpt-keystone`) | MIT OR Apache-2.0 (LICENSE-MIT / LICENSE-APACHE) | Multi-engine local database for twin state, results, sensor history, version metadata |
| `tpt-appfront` | `61465269be731aaf4babf6cd3ec87ecf664d7397` | 2021 | MIT OR Apache-2.0 | Native UI. `tpt-appfront-canvas` is an **egui + glow** backend (native and wasm32) |
| `tpt-engineering` | `69d6d62d294d6924342079d2eea8391c183d9a65` | 2024 | MIT OR Apache-2.0 | Applied engineering primitives (continuum / process) |
| `tpt-science` | `3aabe71aa22e0413558da828906ba0605f1ec36b` | 2024 | MIT OR Apache-2.0 | ODE/DAE, structured-grid PDE, multi-scale simulation substrate |
| `tpt-math` | `2b86a62c6f323d06c715c862304ae064f153c279` | 2021 | MIT OR Apache-2.0 | Numeric substrate for science and engineering |
| `tpt-perception` | `5b07204dee8bd9bc18ddc567d9163511a241aefe` | 2021 | MIT OR Apache-2.0 | Point clouds, registration, sensor fusion |
| `tpt-control` | `25d14f1c0679c835ea1eb544a4f440fd0923776f` | 2021 | MIT OR Apache-2.0 | Controllers, state estimation, trajectory helpers |
| `tpt-primitives` | `8f4e27c05db0ba06e38abf20670bacd4058d1554` | 2024 | MIT OR Apache-2.0 | Identity, capability, evidence |

The `tpt-primitives` commit matches the one `tpt-app-policy` already pins.

### Findings that change the plan

1. **CRDT sync already exists.** `tpt-vertex/collab` is transport-agnostic and includes a WebSocket server. Phase 3 should adopt it, not write a transport. The spec's open question on transport (§10) is answered as "whatever `tpt-vertex` provides". Its sync server binds loopback by default (see finding 6).
2. **The 3D viewport has no off-the-shelf path.** `tpt-appfront-canvas` is egui-based and draws 2D UI. Its `node-graph` example uses raw egui for GPU-style interaction. The viewport will need a custom GPU paint path, probably `glow` or `wgpu` through egui's paint callbacks. This is a real risk for Phase 7 and needs a spike.
3. **Keystone is a monorepo.** The Rust library is `tpt-keystone-db/tpt-keystone`. Depend on that crate by path or git subdirectory, not the repo root.
4. **Network code is compiled in.** `tpt-keystone` has `rustls` and `tokio` as normal dependencies, even with `default-features = false`. Its S3, OTLP, wasmtime and wgpu features are off in Twin, so no AWS, OpenTelemetry or WASM code is built. Twin only calls the embedded library and never opens a socket, but the TLS and async networking code is still present. Verified 2026-10-10: `cargo tree` shows `aws-lc-rs` only as a `rustls` backend. `cargo deny` passes. Add a network-capture test (Phase 9) to prove zero outbound connections.
5. **Edition mix.** `tpt-engineering`, `tpt-science` and `tpt-primitives` are edition 2024 with rust-version 1.85. The rest are 2021. Twin's workspace is 2021, which is fine, but MSRV must be at least 1.85.
6. **`tpt-sci-ode` does not compile at the pinned commit.** Its `Cargo.toml` pins `cranelift-codegen` to 0.135 but `cranelift-frontend`, `cranelift-module` and `cranelift-jit` to 0.109. The two `cranelift-codegen` versions then clash (`src/jit.rs`: mismatched `TargetIsa` and `Signature`). Turning off default features does not help, because `jit.rs` compiles anyway. Verified 2026-10-10 on Windows with `cargo check`. The other science and math crates Twin needs (`tpt-sci-grid`, `tpt-sci-physics-rigid`, `tpt-math-numeric`, `tpt-math-linalg-dense`) and `tpt-engineering` build cleanly. **Status: fixed locally.** The cranelift crates are aligned to 0.135, and the two `finalize` calls are updated to pass `module.isa().frontend_config()`. The fixed crate is vendored in `vendor/tpt-sci-ode` (see its `VENDORED.md`). Its own tests pass (23 + 6 + 1 + 1, plus 2 ignored), and the workspace clippy is clean. The upstream fix is in `docs/upstream/tpt-sci-ode-cranelift.patch`, and it applies to upstream master with `git apply --check -p1`. It is not submitted upstream, since that is a decision for the `tpt-science` maintainer.
7. **The sync server's default token is a known value.** `tpt-vertex-collab`'s `sync_server` binds `127.0.0.1` by default (good). But with no `--token` it uses the hardcoded token `dev`. If someone runs it bound to `0.0.0.0`, anyone on the LAN can join with that token. Twin must not ship that binary as-is. It should use the `SyncHub` library directly, with per-session tokens and access levels.

## Layering

```
twin-app (tpt-appfront shell, viewport, panels)
   │
   ├── twin-geometry ── tpt-vertex (kernel, feature tree, history)
   ├── twin-collab   ── tpt-vertex/collab (CRDT, SyncHub)
   ├── twin-sim      ── tpt-science + tpt-engineering + tpt-math (+ tpt-control)
   ├── twin-sensors  ── tpt-perception
   ├── twin-store    ── tpt-keystone (local DB)
   ├── twin-package  ── twin-store, twin-core (container, signing, encryption)
   ├── twin-license  (offline ed25519 key check; no network)
   └── twin-core     (Twin document model; shared IDs, schema version)
```

Dependencies only point down this graph. `twin-core` depends on no foundation crate, so the document format stays stable whatever the engines do.

## Decisions

- Desktop shell: pure `tpt-appfront` native, no Tauri (§10).
- Collaboration: `tpt-vertex/collab` CRDT, LAN-only by default (§6).
- Package: zip-style `.twin` container, ed25519-signed, optional encryption (§10). Format spec goes in `docs/PACKAGE_FORMAT.md` (Phase 6).
- Licence: offline signed key, verified locally. Source build is fully functional (§7, `GUMROAD.md`).
- Dependencies must be MIT OR Apache-2.0 compatible (§5). Enforced by `deny.toml`.
- No telemetry, no accounts, no phone-home (§6).

## Still open

- Build the rest of the foundation crates (`tpt-science`, `tpt-math`, `tpt-appfront`, `tpt-engineering`, `tpt-perception`, `tpt-control`, `tpt-primitives`) as git deps. Only `tpt-vertex-collab` and `tpt-keystone` are verified on Windows.
- macOS and Linux builds: not tested.
- Viewport renderer choice (`glow` vs `wgpu` via egui callbacks). Needs a spike.

## Recommendations for Phases 2 and 3 (2026-10-10)

**Phase 2: geometry.** The kernel already has what Phase 2 asks for. `tpt-vertex-kernel` provides `FeatureTree` (`add`, `evaluate`) and `Sketch`. `tpt-vertex/versioning` provides a `Repository` with branches, checkout, merge base, fast-forward, merge and commit. Recommendation: wrap those two. Do not build a second geometry history. The `twin-geometry::History` written here is parameter-only, so it should either be retired or used only for parameter maps that have no feature-tree home. Before wiring, confirm that the `versioning` crate's merge works on feature trees, and that `versioning` builds as a git dependency. Its deps are `tpt-vertex-kernel`, `serde` and `sha2 0.10`, which look light.

**Phase 3: collaboration.** Use the `tpt-vertex-collab` library: `CrdtDoc`, `LocalReplica`, and `SyncHub` with its authentication and access levels. Do not run the `sync_server` binary, because of its hardcoded `dev` token. Host the hub in-process, bind it to a LAN interface only when the user opts in, and give each session its own token. The next step is a spike with two in-process replicas that edit a shared feature tree and converge, including after a partition, before any networking. Peer discovery (mDNS) and TLS are separate steps after that.

**Viewport (Phase 7).** `spikes/viewport` draws a 3D triangle with raw glow inside an egui paint callback, with a perspective MVP and egui panels around it. It compiles and runs for 10 seconds without a startup error. It has not been checked visually, and it has no orbit camera or mesh input. The result shows the custom-GPU path is viable. It is not yet a viewport.
