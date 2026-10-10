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
- [x] Licence check: `deny.toml` added, `cargo deny check licenses` passes (before the Phase 7 shell; see the shell item for the current failure)
- [x] README with build instructions
- [x] Confirm `tpt-keystone` and `tpt-vertex-collab` build on Windows as git deps (5m14s cold build; `default-features = false` keeps S3, OTLP, wasmtime and wgpu out)
- [x] Confirm the Phase 4 crates build on Windows as git deps. `tpt-math-numeric`, `tpt-math-linalg-dense`, `tpt-engineering`, `tpt-sci-grid`, `tpt-sci-physics-rigid` pass. **`tpt-sci-ode` did not compile** at the pinned commit (cranelift version clash, `docs/ARCHITECTURE.md` finding 6). **Fixed locally:** vendored in `vendor/tpt-sci-ode` (cranelift 0.135, `finalize` API), tests pass
- [x] Fix `tpt-sci-ode`: vendored with the fix (`vendor/tpt-sci-ode/VENDORED.md`). Upstream patch in `docs/upstream/tpt-sci-ode-cranelift.patch`, verified to apply to upstream master
- [~] Submit the `tpt-sci-ode` patch upstream. **Prepared, not pushed.** Commit `6a4fa1c` (no co-author trailer, amended) is on local branch `fix/align-cranelift-0.135` in the scratch clone of `tpt-science`. PR text: `docs/upstream/PR_BODY.md`. Push and PR need GitHub auth, which this session doesn't have (`gh` not installed, no git credentials). Run `gh auth login`, then the push and `gh pr create` steps
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
- [x] Geometry field (`Twin.geometry_ref`) now holds the head commit ID of a `Design` (`Design::link_twin`)

## Phase 2 — Parametric geometry & history (`twin-geometry`)

- [x] Wrap `tpt-vertex-kernel` (`twin_geometry::model`): `ModelSpec` of block, cylinder, boolean and translate steps whose dimensions are constants or named parameters; builds a kernel `FeatureTree`. Kernel has no named parameters, so this layer adds them. Not yet wrapped: revolve, sweep, loft, fillet, chamfer, rotation, free sketches, `tpt-vertex-versioning`
- [x] Parameter edits trigger a rebuild: `Model::rebuild(params)` (unchanged on error); `geometry_hash()` is equal for equal parameters. Rebuilds the whole tree, no incremental cache. 8 tests
- [x] Git-like history for parameters (`twin-geometry::History`): content-addressed commits (SHA-256), branches, switch, log, diff, and three-way merge that reports conflicts and applies nothing when there are any. 10 tests. **Geometry** history waits on the kernel wrap, using the same graph
- [x] Persist history in Keystone (serde_json now uses `float_roundtrip` workspace-wide, because the default parser can change an f64 by one ulp, which would change commit IDs): `History::to_json` / `from_json` (recomputes commit IDs, checks parents, branches and head, so tampering is refused) and `TwinStore::save_history` / `load_history` (table `histories`, removed with the twin). 5 new tests incl. reopen. Geometry snapshots wait on the kernel wrap
- [x] Geometry in history: `Commit.geometry` (hash, part of the commit ID when present) and `twin_geometry::design::Design` (`ModelSpec` + `History`). `commit` and `merge` build the model first and record its hash, and refuse if the parameters don't build; `verify(commit)` rebuilds and compares. Stored with `TwinStore::save_design` / `load_design` (table `designs`). 6 + 1 tests. `Design::link_twin` copies the working parameters into a `Twin` and sets `geometry_ref` to the head commit ID; `is_linked` reports when the design has moved on
- [x] Tests for branch and merge (three-way merge, same change on both sides, conflict, self-merge). Rebuild determinism waits on the kernel wrap

## Phase 3 — LAN collaboration (`twin-collab`)

- [x] Check what real-time sync `tpt-vertex` provides: `tpt-vertex-collab` already has a CRDT, a `SyncHub` and a WebSocket server, so no new transport protocol was needed (see `docs/ARCHITECTURE.md`)
- [ ] Peer discovery on the LAN (e.g. mDNS). Not started; no discovery until the opt-in LAN setting exists
- [~] Join / host a session: `twin_collab::hub` runs sessions through `tpt-vertex-collab`'s `SyncHub` in-process. Only the owner hosts; joiners to an empty room are refused; viewers cannot edit; offline edits are kept and sent after reconnecting; an offline edit that loses to a newer write is reported by `Client::lost_on_reconnect`. Tested with 11 tests. **Not done:** showing peers (presence)
- [~] CRDT sync of **parameters** (`twin-collab`): `TwinReplica` keeps an op log over `tpt-vertex-collab` LWW registers; `sync` / `pull` exchange logs in any order. Tested: different and same-parameter edits, repeated pulls, a partition with edits on both sides then heal, equal-clock tie-break by replica ID, three replicas. **Not done:** geometry (feature tree) sync; a network transport (the `SyncHub` in `tpt-vertex-collab`)
- [~] LAN-only by default: `twin_collab::serve` binds loopback and refuses any other address unless `allow_lan` is passed (tested). **Not done:** the user-facing setting, and TLS, which LAN use needs
- [~] Authenticated peers: tokens checked by the hub's `MemoryAuth`, with per-room grants. **Not done:** encryption, signed tokens or a persistent auth store (the in-memory table is for single-node use)
- [x] Partition and heal, multi-process: `tests/multi_process.rs` runs the server as a child process (`twin_collab_serve`) and drives three peers over real sockets. A partition is a closed socket; offline edits are sent after reconnecting; all peers converge. Passed in 5 runs. **Not done:** peers on different machines, and an unreliable network (loss, reordering, delay)

## Phase 4 — Simulation (`twin-sim`)

- [x] Scenario runner over `tpt-sci-ode`: `twin_sim::simulate(twin, scenario, times)` reads parameters from the Twin and returns a sampled trace
- [ ] Rigid-body from twin geometry: **partial.** A 1-DOF mass-spring-damper runs from twin parameters (`mass_kg`, `stiffness_n_per_m`, `damping_n_s_per_m`). Multi-body dynamics from geometry is not built
- [x] Simple 1D continuum solver: heat diffusion in a rod (`twin_sim::heat`), method of lines on `tpt-sci-ode`, zero-temperature ends. Checked against the closed-form sine-mode decay, grid-refinement convergence, and monotone decay; 7 tests. Uniform grid, Dirichlet ends only, explicit solver (stiff for very fine grids)
- [x] Scenario definition: initial conditions (`position_m`, `velocity_m_s`), duration, sample times (validated: strictly increasing, within duration)
- [x] Results stored in Keystone and linked to the geometry version. `TwinStore::run_scenario` simulates a scenario and saves it with the twin's `geometry_ref`; with a design it refuses (`StaleGeometry`) unless the twin is linked to the design's head. `run_is_current` flags runs made at an older commit. 3 tests. Runs are not saved automatically on parameter edits
- [x] Deterministic runs (checked by a test); regression tests against closed-form solutions for undamped, under-, critically and over-damped cases (all within 1e-6)
- [ ] Basic controller hook via `tpt-control` (optional for MVP). Not started
- [ ] Wire `tpt-engineering` and `tpt-math` into the runner. Not started; the solver uses only `tpt-math-numeric`
- [ ] Solver tolerance: `simulate` sets rtol and atol to 1e-10. The solver default of 1e-6 was too loose for the analytic checks (error grew over several oscillations)

## Phase 5 — Sensors & fusion (`twin-sensors`)

- [x] Sensor model types: IMU, camera and lidar, each with a mount pose and validated settings (`twin-sensors`, 5 tests). Stored per twin with `TwinStore::save_sensors` / `load_sensors` and included in exports. `Twin.sensor_refs` is not filled in yet; the sensor list is the source of truth
- [~] Synthetic data generation from simulation: noise-free IMU readings from the mass-spring-damper (`twin_sensors::imu_from_trace`, sample times via `sample_times`). Camera and lidar output and noise models are not started
- [~] Point-cloud registration via `tpt-perception` (ICP, point-to-point): `twin_sensors::register_point_clouds(source, target, prior, max_dist)`. Tested on an L-shape (known motion) and on lidar scans of a cube taken from a moving sensor (4 cm, 2°) with a prior. An identity start stalls on that motion (documented in the function). **Synthetic lidar:** `sample_surface` and `lidar_returns` (range-limited, sensor frame, no occlusion or beam pattern). **IMU prior:** `integrate_imu` double-integrates IMU acceleration (trapezoid rule, no drift correction); its translation feeds registration as the prior, tested against the simulated motion. **Not done:** rotation prior (no gyro model), so the IMU prior works only without yaw; sensor fusion beyond this (EKF/UKF from `tpt-perception` unused); point-to-plane ICP; occlusion and beam patterns in the lidar model. **`tpt-perception` is vendored** in `vendor/tpt-perception` (its `tpt-math` path dependencies can't resolve as a git dependency; see `VENDORED.md`)
- [ ] Data for the visualisation layer
- [~] Sensor history stored in Keystone: `TwinStore::save_recording` / `load_recording` (table `sensor_readings`, one recording per twin and sensor, validated, removed with the twin; 1 test). **Not done:** saving readings from a scenario run automatically, and including recordings in the `.twin` export

## Phase 6 — Package export / import (`twin-package`)

- [x] Spec the `.twin` container layout in `docs/PACKAGE_FORMAT.md`
- [x] Manifest with versions and SHA-256 per entry, signed with Ed25519 (`twin-package`)
- [~] Export: `TwinStore::export_twin` / `import_twin` write and read a signed (optionally encrypted) `.twin` with the twin, its design (`design.json`: model spec and history, geometry hashes included) and its stored runs (`runs/<n>.json`). **Not done:** the Keystone snapshot (no snapshot API in `tpt-keystone`); sensor history (Phase 5); the simulation setup is only the twin's scenarios, not a separate record
- [x] ed25519 signing and verification. Import rejects bad signatures, unlisted entries, hash mismatches and unsafe paths
- [x] Optional encryption on export: ChaCha20-Poly1305, key from Argon2id (`twin-package::encrypt`)
- [x] Import integrity checks; clear errors on tamper, wrong key and version mismatch. 10 tests
- [~] Round-trip test: the store test exports from one store and imports into another in the same process (separate directories, separate keys, wrong-key and tamper rejection). **Not done:** a real move between two machines
- [ ] **Open:** who signs a user's own export. The format needs a key the recipient trusts, and a vendor key isn't right for users (`docs/PACKAGE_FORMAT.md` open items)

## Phase 7 — Desktop app (`twin-app`)

- [~] `tpt-appfront` shell: window and project open/save. `twin-app` uses `tpt-appfront-canvas` (egui + glow), pinned to `61465269` and building on Windows. `twin_app::Project` owns the store, twin list and open twin, with 5 tests; `main.rs` is a view over it with New, Save and Open buttons. **Not done:** menus, a native folder picker, and the rebuild and results views (parameter editing is now in the window). Licences resolved: BSL-1.0 (clipboard crates), OFL-1.1 and Ubuntu-font-1.0 (bundled egui fonts) are on the allow list in `deny.toml`, and `cargo deny check licenses` passes. The Ubuntu Font review stays open under Phase 8
- [x] Window theme: light palette, rounded controls, card sections, scrolling (`crates/twin-app/src/theme.rs`). Checked by screenshot. **Limitation:** the canvas has no root scrolling and lays out to the height it is given, so the content sits in a fixed 2400 px scroll region (blank space below short content). Proper fix: auto-height root in `tpt-appfront-canvas` (upstream)
- [ ] 3D viewport with orbit / pan / zoom and selection
- [~] Parameter panel and feature tree: **parameter panel done.** Each parameter of the open twin has an editable field; numbers apply at once (not saved until Save), and text that is not a number is refused and blocks Save (`Project::edit_parameter`, 1 test). **Not done:** the feature tree, and field-level validation ranges
- [~] History panel (branches, commits, diff): **parameter history done.** Start history, commit the saved parameters, create and switch branches, see the log and the head commit's changes (`Project` history methods, 1 test). Commit and switch are refused while the twin has unsaved edits. **Not done:** merge, diff between any two commits, and geometry history (the design is not yet wired into the app)
- [ ] Collaboration panel (peers, join/host)
- [~] Simulation panel (set up, run, view results): **run and summary done.** Add an example mass-spring-damper model and scenario, run a scenario of a saved twin (stored as a run), and see a summary of the latest run (`Project::run_scenario` / `run_summary`, 1 test). **Not done:** scenario editing (duration, initial conditions), plots, and a stale-run flag when parameters change after a run
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
- [ ] Review the Ubuntu Font Licence (bundled egui font, allowed in `deny.toml`) before v1.0. Unmodified fonts only
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
