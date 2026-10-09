# Twin (Digital Twin Workbench) — TODO

Source: `spec.txt` (section refs in parentheses). Tick boxes as you go: `- [x]`.

## Decisions locked in

- Layout: single Cargo workspace, crates under `crates/` (`twin-core`, `twin-store`, `twin-geometry`, `twin-collab`, `twin-sim`, `twin-sensors`, `twin-package`, `twin-license`, `twin-app`).
- Foundation crates come from git, pinned to a commit `rev` (as in `tpt-app-policy`). Org: `github.com/TPT-Solutions/<crate>`. `tpt-math` / `tpt-science` live under `tpt-solutions`; confirm each URL.
- Desktop shell: pure `tpt-appfront` native, no Tauri (§10).
- LAN CRDT transport: whatever `tpt-vertex` provides (§10). If it has none, see Phase 3.
- MVP simulation: rigid-body plus a simple 1D/2D continuum (e.g. heat diffusion or beam deflection) (§10).
- Package format: zip-style container, extension `.twin`: manifest + geometry + Keystone snapshot + results; ed25519-signed; optional encryption (§10).
- Licence key: offline ed25519-signed key file, verified locally. The open-source build stays fully functional without it (§7, `GUMROAD.md`).
- Platforms: Windows, macOS, Linux (§4). Dual licence MIT OR Apache-2.0; no Apache-only deps (§5).
- No telemetry, no accounts, no phone-home; collaboration is LAN-only by default (§6).

## Open decisions (default shown; change if wrong)

- [x] Exact foundation-crate URLs and commits — `github.com/TPT-Solutions/<crate>`, pinned to master on 2026-10-10 (see `docs/ARCHITECTURE.md`)
- [ ] Rust edition / MSRV — default: edition 2021, latest stable (note `tpt-science` uses 2024 / 1.85)
- [ ] What the key-file licence does in official binaries (banner only vs. feature gate) — default: banner only
- [x] Encryption scheme for packages — ChaCha20-Poly1305 with an Argon2id passphrase KDF (implemented in `twin-package`)
- [ ] Rendering backend for the 3D viewport — depends on what `tpt-appfront` offers
- [ ] Dependency licence checker — default: `cargo-deny`

## Phase 0 — Workspace & groundwork

- [x] Cargo workspace root, `rust-toolchain.toml`, `.gitignore`
- [x] `LICENSE-MIT`, `LICENSE-APACHE`
- [x] Stub crates (build, clippy and test pass)
- [x] CI workflow for Windows, macOS, Linux (`.github/workflows/ci.yml`; not yet run on GitHub)
- [x] `git init` (local repo, nothing committed)
- [ ] First commit (waiting for your go-ahead)
- [ ] Create the GitHub remote (waiting for your go-ahead; org name to confirm, `Cargo.toml` says `tpt-apps`, the foundation crates are under `TPT-Solutions`)
- [x] Clone and audit the foundation crates: `tpt-vertex`, `tpt-keystone-db`, `tpt-appfront`, `tpt-science`, `tpt-engineering`, `tpt-perception`, `tpt-control`, `tpt-math`, `tpt-primitives` (all exist, all MIT OR Apache-2.0)
- [x] Record their public APIs and gaps in `docs/ARCHITECTURE.md`
- [x] Pin the git dependencies by `rev` (in `Cargo.toml` comments; not yet in `[workspace.dependencies]`, because nothing consumes them yet, and they must compile first)
- [x] Licence check: `deny.toml` added, `cargo deny check licenses` passes; CI job added (not yet run on GitHub)
- [x] README with build instructions
- [x] Confirm `tpt-keystone` and `tpt-vertex-collab` build on Windows as git deps (5m14s cold build; `default-features = false` keeps S3, OTLP, wasmtime and wgpu out)
- [x] Confirm the Phase 4 crates build on Windows as git deps. `tpt-math-numeric`, `tpt-math-linalg-dense`, `tpt-engineering`, `tpt-sci-grid`, `tpt-sci-physics-rigid` pass. **`tpt-sci-ode` did not compile** at the pinned commit (cranelift version clash, `docs/ARCHITECTURE.md` finding 6). **Fixed locally:** vendored in `vendor/tpt-sci-ode` (cranelift 0.135, `finalize` API), tests pass
- [x] Fix `tpt-sci-ode`: vendored with the fix (`vendor/tpt-sci-ode/VENDORED.md`). Upstream patch in `docs/upstream/tpt-sci-ode-cranelift.patch`, verified to apply to upstream master
- [ ] Submit the `tpt-sci-ode` patch upstream. Waiting for your go-ahead, since it's a change to a repo you don't own yet (or confirm you maintain `tpt-science`)
- [x] Licence: allow `Apache-2.0 WITH LLVM-exception` (cranelift crates) in `deny.toml`
- [ ] Decide whether `tpt-sci-ode` stays vendored or moves to the git dep once upstream merges the fix
- [x] LAN default binding: `sync_server` binds loopback by default. **Problem:** its default token is the hardcoded `dev`; Twin must not use that binary as-is (`docs/ARCHITECTURE.md` finding 6)
- [ ] Verify macOS and Linux builds of the git deps (CI)
- [ ] Decide the org for the GitHub remote, then create it
- [ ] Spike: 3D viewport renderer through egui paint callbacks (`glow` vs `wgpu`) — see `docs/ARCHITECTURE.md` finding 2

## Phase 1 — Twin document & storage (`twin-core`, `twin-store`)

- [x] Define the Twin document: parameters, scenarios, opaque refs to geometry and sensors (§3). Geometry and sensor data stay in their own crates
- [x] Stable IDs (UUID) and serialisation (serde); `schema_version` field, refuses newer schemas
- [x] Keystone-backed store for twin documents (`twin-store`, table `twins`, validated JSON). Simulation results, sensor history and version metadata are not yet stored (Phases 2, 4, 5)
- [x] Open / create / save / load / list / delete a twin in a local directory (`TwinStore::open`)
- [x] Migration hook (`migrate` in `twin-core`). Only v1 exists, so it is a passthrough
- [x] Unit tests: 8 in `twin-core` (round-trip, validation, NaN, schema refusal), 5 in `twin-store` against the real Keystone engine (including quoted names)
- [ ] Geometry field (`Twin.geometry_ref`) is an opaque ID until Phase 2 defines what it points to

## Phase 2 — Parametric geometry & history (`twin-geometry`)

- [ ] Wrap `tpt-vertex`: primitives, sketches/features, parameters (§4). Not started. Needs a look at the `tpt-vertex-kernel` API
- [ ] Parameter edits trigger a rebuild. Not started (depends on the kernel wrap)
- [x] Git-like history for parameters (`twin-geometry::History`): content-addressed commits (SHA-256), branches, switch, log, diff, and three-way merge that reports conflicts and applies nothing when there are any. 10 tests. **Geometry** history waits on the kernel wrap, using the same graph
- [ ] Persist history in Keystone. Not started; `History` is in memory only
- [x] Tests for branch and merge (three-way merge, same change on both sides, conflict, self-merge). Rebuild determinism waits on the kernel wrap

## Phase 3 — LAN collaboration (`twin-collab`)

- [ ] Check what real-time sync `tpt-vertex` provides; if none, spike a transport (QUIC / WebSocket + Noise) and record the choice
- [ ] Peer discovery on the LAN (e.g. mDNS)
- [ ] Join / host a session; show peers
- [ ] CRDT sync of geometry and parameters; offline edits converge on reconnect
- [ ] LAN-only by default; an explicit opt-in setting for anything else (§6)
- [ ] Encrypted and authenticated peers
- [ ] Multi-process convergence test; a test for network partition and heal

## Phase 4 — Simulation (`twin-sim`)

- [x] Scenario runner over `tpt-sci-ode`: `twin_sim::simulate(twin, scenario, times)` reads parameters from the Twin and returns a sampled trace
- [ ] Rigid-body from twin geometry: **partial.** A 1-DOF mass-spring-damper runs from twin parameters (`mass_kg`, `stiffness_n_per_m`, `damping_n_s_per_m`). Multi-body dynamics from geometry is not built
- [ ] Simple 1D/2D continuum solver (pick one: heat diffusion or beam deflection). Not started
- [x] Scenario definition: initial conditions (`position_m`, `velocity_m_s`), duration, sample times (validated: strictly increasing, within duration)
- [ ] Results stored in Keystone and linked to the geometry version. Not started (needs Phase 2)
- [x] Deterministic runs (checked by a test); regression tests against closed-form solutions for undamped, under-, critically and over-damped cases (all within 1e-6)
- [ ] Basic controller hook via `tpt-control` (optional for MVP). Not started
- [ ] Wire `tpt-engineering` and `tpt-math` into the runner. Not started; the solver uses only `tpt-math-numeric`
- [ ] Solver tolerance: `simulate` sets rtol and atol to 1e-10. The solver default of 1e-6 was too loose for the analytic checks (error grew over several oscillations)

## Phase 5 — Sensors & fusion (`twin-sensors`)

- [ ] Sensor model types (e.g. camera, lidar, IMU) attachable to geometry
- [ ] Synthetic data generation from simulation
- [ ] Basic fusion via `tpt-perception`; point-cloud registration
- [ ] Data for the visualisation layer
- [ ] Sensor history stored in Keystone

## Phase 6 — Package export / import (`twin-package`)

- [x] Spec the `.twin` container layout in `docs/PACKAGE_FORMAT.md`
- [x] Manifest with versions and SHA-256 per entry, signed with Ed25519 (`twin-package`)
- [ ] Export: geometry + simulation setup + results + Keystone snapshot. Export and import of the twin document and extra files work; geometry, results and the Keystone snapshot wait on Phases 2, 4 and a Keystone snapshot API
- [x] ed25519 signing and verification. Import rejects bad signatures, unlisted entries, hash mismatches and unsafe paths
- [x] Optional encryption on export: ChaCha20-Poly1305, key from Argon2id (`twin-package::encrypt`)
- [x] Import integrity checks; clear errors on tamper, wrong key and version mismatch. 10 tests
- [ ] Round-trip test between two machines / two directories (success criterion §9). Not done; the single-process test covers the format, not a real move
- [ ] **Open:** who signs a user's own export. The format needs a key the recipient trusts, and a vendor key isn't right for users (`docs/PACKAGE_FORMAT.md` open items)

## Phase 7 — Desktop app (`twin-app`)

- [ ] `tpt-appfront` shell: window, menus, project open/save
- [ ] 3D viewport with orbit / pan / zoom and selection
- [ ] Parameter panel and feature tree
- [ ] History panel (branches, commits, diff)
- [ ] Collaboration panel (peers, join/host)
- [ ] Simulation panel (set up, run, view results)
- [ ] Sensor panel and fusion visualisation
- [ ] Export / import dialogs, including encryption options
- [ ] Settings: network mode, data location. Nothing leaves the machine by default
- [ ] Example twin projects

## Phase 8 — Licence key & distribution (`twin-license`)

- [x] Key-file format and ed25519 offline verification (`twin-license`): JSON payload + base64 Ed25519 signature over the payload bytes. Checks signature, then product. 6 tests. Accepts a public key passed in; no key is compiled in
- [ ] **Vendor public key:** generate the Ed25519 keypair privately and supply the public key. Not done here, so no key is fabricated or stored in the repo
- [ ] Key-issuing tool (kept out of the public build or in a private tool)
- [ ] Banner behaviour in official binaries; the source build works without a key
- [ ] Release workflow: build for Windows, macOS, Linux
- [ ] Code signing and installers
- [ ] Zip bundle: binaries + licence + quick-start guide (PDF) + examples (`GUMROAD.md`)
- [ ] `SHA256SUMS`
- [ ] Gumroad listing check against `GUMROAD.md` (price $349, 14-day refund)
- [ ] Doc for submitting error reports

## Phase 9 — Quality & release criteria (§9)

- [ ] A small team edits one twin over a LAN with no cloud account
- [ ] A full twin moves between machines cleanly
- [ ] The app works with no network at all
- [ ] Confirm there are zero outbound connections (network capture)
- [ ] Binary size and startup/frame-time budgets set and measured
- [ ] Smoke tests on all three platforms
- [ ] Update `GUMROAD.md` system requirements to match measured numbers
- [ ] v1.0 tag

## Later (no commitment, §4)

- [ ] Richer solvers
- [ ] Advanced perception pipelines
- [ ] Controller design, hardware-in-the-loop
- [ ] Headless simulation server
- [ ] Formal claims about twin properties
