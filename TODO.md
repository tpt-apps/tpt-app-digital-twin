# Twin (Digital Twin Workbench) — TODO

GUI review (done): the window now has a header bar (product name, project folder, open twin), a left navigation with five sections (Project, Model, Geometry, Sensors, Share), and one section shown at a time, instead of one long scroll. The toolbar (status, New twin, Save) stays on every section. Chosen style: a light, Fusion/Onshape-like engineering look (cool grey page, white panels, one blue accent, selected section highlighted). Checked by screenshot: Project, Model, Geometry (start, feature tree, parameters, branches) and Sensors (add a camera, its settings) sections, and the Share hint. Also checked by screenshot: the Save-then-run flow (example model, run, Plot shows the position and velocity lines in the right panel), and a lidar added with its settings. Still unchecked by eye: the lidar scan panel (Save scan, Plot scan) and the Share cards with an open twin. Known layout limit: canvas text does not wrap, so long summary lines (camera and lidar) run under the plot panel when it is open. Visual check: the main window launches and its layout was checked by screenshot (New twin, Twins list, Parameters, History and Simulation cards in order). Driving the deeper cards by clicks did not work reliably, because the editor kept taking focus, so the Shared session, Geometry, Sensors, plot and scan panels are still unchecked by eye.

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
- [x] Rust edition / MSRV — **decided: edition 2024, `rust-version = "1.95"`.** The 1.85 default does not build: `tpt-sci-ode`'s cranelift needs rustc 1.95 (checked with `cargo +1.95 check --workspace --all-targets`). Clippy and tests pass on stable
- [x] What the key-file licence does in official binaries (banner only vs. feature gate) — **decided: banner only**, via the `official` feature (`Project::licence_banner`)
- [x] Encryption scheme for packages — ChaCha20-Poly1305 with an Argon2id passphrase KDF (implemented in `twin-package`)
- [x] Rendering backend for the 3D viewport — **decided: `glow`**, which the appfront canvas already uses. The spike below still has to confirm it
- [x] Dependency licence checker — default: `cargo-deny` (`deny.toml`; `cargo deny check licenses` passes)

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
- [ ] Decide whether `tpt-sci-ode` stays vendored or moves to the git dep once upstream merges the fix — **recommended: stay vendored until the upstream PR merges**
- [x] LAN default binding: `sync_server` binds loopback by default. **Problem:** its default token is the hardcoded `dev`; Twin must not use that binary as-is (`docs/ARCHITECTURE.md` finding 6)
- [ ] Verify macOS and Linux builds of the git deps (CI)
- [x] Decide the org for the GitHub remote — **decided: `tpt-apps`**, as `Cargo.toml` already says. Creating the remote is still open (publishing is excluded for now)
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
- [ ] Peer discovery on the LAN (e.g. mDNS). **Decided: manual address entry first; mDNS later.** Not started; no discovery until the opt-in LAN setting exists
- [~] Join / host a session: `twin_collab::hub` runs sessions through `tpt-vertex-collab`'s `SyncHub` in-process. Only the owner hosts; joiners to an empty room are refused; viewers cannot edit; offline edits are kept and sent after reconnecting; an offline edit that loses to a newer write is reported by `Client::lost_on_reconnect`. Tested with 11 tests. **Not done:** showing peers (presence)
- [~] CRDT sync of **parameters** (`twin-collab`): `TwinReplica` keeps an op log over `tpt-vertex-collab` LWW registers; `sync` / `pull` exchange logs in any order. Tested: different and same-parameter edits, repeated pulls, a partition with edits on both sides then heal, equal-clock tie-break by replica ID, three replicas. **Network transport:** `twin_collab::sync` shares parameters over TLS (`SyncHost` / `SyncClient`): a joiner sends the session code, gets a snapshot of the op log, and then both sides exchange ops, with the host relaying between peers. The host stops listening when it is dropped. 4 tests, including edits both ways between two peers. **Geometry sync (whole spec):** the design's `ModelSpec` (JSON) is shared as one text register (`GEOMETRY_KEY`), last writer wins as a whole, over the same op log and TLS transport (`SyncHost::set_geometry`, `SyncClient::set_geometry`; viewers are refused). The app has Share and Apply buttons in the Shared session card; applying is an uncommitted edit, and the status line shows the hash of the shape it builds. Tested: replica, TLS round trip with viewer refusal, and a two-project session. **Not done:** per-step merging, which needs stable step IDs (steps refer to each other by index today, so inserting a step would shift references); and sharing the hub's roles (owner, editor, viewer) over this transport
- [~] LAN-only by default: `twin_collab::serve` binds loopback and refuses any other address unless `allow_lan` is passed (tested). **TLS built:** `twin_collab::tls` serves over TLS with a self-signed certificate, and clients pin its fingerprint (trust on first use; see the module docs for the first-contact caveat). The plain transport now refuses every non-loopback address. The host identity is kept in the user config folder (`load_or_create_server_identity` in `twin-app`, 1 test). **User-facing setting done:** LAN sessions are off by default; the Settings section turns them on, and the choice is saved in the project folder (`settings.txt`). With it off, hosting or joining at a LAN address is refused before anything is bound or dialled (1 test; checked by screenshot). **Not done:** a per-machine default, so every new project starts off
- [~] Authenticated peers: tokens checked by the hub's `MemoryAuth`, with per-room grants. Encryption is now in place (TLS above). **Not done:** signed tokens or a persistent auth store (the in-memory table is for single-node use)
- [x] Partition and heal, multi-process: `tests/multi_process.rs` runs the server as a child process (`twin_collab_serve`) and drives three peers over real sockets. A partition is a closed socket; offline edits are sent after reconnecting; all peers converge. Passed in 5 runs. **Not done:** peers on different machines, and an unreliable network (loss, reordering, delay)

## Phase 4 — Simulation (`twin-sim`)

- [x] Scenario runner over `tpt-sci-ode`: `twin_sim::simulate(twin, scenario, times)` reads parameters from the Twin and returns a sampled trace
- [~] Rigid-body from twin geometry: **free body done.** The whole design is one solid body: `twin_geometry::mass` gives volume, mass, centroid and the inertia tensor from the mesh (divergence theorem; 5 tests, including a kernel block), with density from the twin's `density_kg_m3` (`Project::body_properties`). `twin_sim::rigid::simulate_body` integrates the free body under constant gravity with the full inertia tensor, on `tpt-sci-ode` (5 tests: free fall, steady spin, |Iω| conserved, unit quaternion). A 1-DOF mass-spring-damper also runs from twin parameters (`mass_kg`, `stiffness_n_per_m`, `damping_n_s_per_m`). **Deviation from the plan:** `tpt-sci-physics-rigid` was not used: its bodies are spheres with a scalar inertia, so it cannot hold a solid's inertia tensor. The Geometry section has a Body card: a density field, and the volume, mass, centre of mass and inertia (checked by screenshot against hand values for a 2 × 1 × 1 block). The Body card also has gravity (`gravity_m_s2`, 9.81 when not set) and starting spin (`spin_*_rad_s`, 0 when not set) fields, and "Drop body for 2 s" runs the body from rest at its centre of mass and shows the result in the status line (`Project::body_run`; checked by screenshot: centre z −19.120 m after 2 s, as hand calculation gives). **Not done:** joints and several bodies, and storing body results (the status line is the only output for now)
- [x] Simple 1D continuum solver: heat diffusion in a rod (`twin_sim::heat`), method of lines on `tpt-sci-ode`, zero-temperature ends. Checked against the closed-form sine-mode decay, grid-refinement convergence, and monotone decay; 7 tests. Uniform grid, Dirichlet ends only, explicit solver (stiff for very fine grids)
- [x] Scenario definition: initial conditions (`position_m`, `velocity_m_s`), duration, sample times (validated: strictly increasing, within duration)
- [x] Results stored in Keystone and linked to the geometry version. `TwinStore::run_scenario` simulates a scenario and saves it with the twin's `geometry_ref`; with a design it refuses (`StaleGeometry`) unless the twin is linked to the design's head. `run_is_current` flags runs made at an older commit. 3 tests. Runs are not saved automatically on parameter edits
- [x] Deterministic runs (checked by a test); regression tests against closed-form solutions for undamped, under-, critically and over-damped cases (all within 1e-6)
- [x] Basic controller hook: a declarative PID on position in the scenario (`twin_core::PidController`, `Scenario.controller`). The integral is an extra ODE state; the derivative uses measured velocity; zero gains match open loop; a stable PID reaches its setpoint (tests). Plain data, so a `.twin` file never runs code. **Not done:** editing it in the window, actuator limits, and using `tpt-control` (its API was not checked)
- [ ] Wire `tpt-engineering` and `tpt-math` into the runner. Not started; the solver uses only `tpt-math-numeric`
- [x] Solver tolerance: `simulate` sets rtol and atol to 1e-10. The solver default of 1e-6 was too loose for the analytic checks (error grew over several oscillations)

## Phase 5 — Sensors & fusion (`twin-sensors`)

- [x] Sensor model types: IMU, camera and lidar, each with a mount pose and validated settings (`twin-sensors`, 5 tests). Stored per twin with `TwinStore::save_sensors` / `load_sensors` and included in exports. `Twin.sensor_refs` is not filled in yet; the sensor list is the source of truth
- [~] Synthetic data generation from simulation: noise-free IMU readings from the mass-spring-damper (`twin_sensors::imu_from_trace`, sample times via `sample_times`). Lidar range noise: `twin_sensors::lidar_returns_with_noise` adds a zero-mean Gaussian error along each ray, from a seeded generator, so a scan is repeatable (3 tests: zero sigma is noise-free, same seed gives the same scan, measured bias and spread match the request). Not yet in the window, and the noise-free scan is still what the window shows. **Not done:** a noise model for the IMU and camera, dropouts and beam patterns on the lidar
- [~] Point-cloud registration via `tpt-perception` (ICP, point-to-point): `twin_sensors::register_point_clouds(source, target, prior, max_dist)`. Tested on an L-shape (known motion) and on lidar scans of a cube taken from a moving sensor (4 cm, 2°) with a prior. An identity start stalls on that motion (documented in the function). **Synthetic lidar:** `sample_surface` and `lidar_returns` (range-limited, sensor frame, no occlusion or beam pattern). **IMU prior:** `integrate_imu` double-integrates IMU acceleration (trapezoid rule, no drift correction); its translation feeds registration as the prior, tested against the simulated motion. **Not done:** rotation prior (no gyro model), so the IMU prior works only without yaw; sensor fusion beyond this (EKF/UKF from `tpt-perception` unused); point-to-plane ICP; occlusion and beam patterns in the lidar model. **`tpt-perception` is vendored** in `vendor/tpt-perception` (its `tpt-math` path dependencies can't resolve as a git dependency; see `VENDORED.md`)
- [ ] Data for the visualisation layer
- [~] Sensor history stored in Keystone: `TwinStore::save_recording` / `load_recording` (table `sensor_readings`, one recording per twin and sensor, validated, removed with the twin; 1 test). Recordings are included in the `.twin` export (`recordings/<n>.json`) and restored on import. Each scenario run also records the IMU sensors' noise-free readings (`TwinStore::run_scenario`, one recording per IMU, replaced by the next run; the recording does not say which scenario it came from). **Not done:** per-scenario recordings, and camera or lidar output

## Phase 6 — Package export / import (`twin-package`)

- [x] Spec the `.twin` container layout in `docs/PACKAGE_FORMAT.md`
- [x] Manifest with versions and SHA-256 per entry, signed with Ed25519 (`twin-package`)
- [~] Export: `TwinStore::export_twin` / `import_twin` write and read a signed (optionally encrypted) `.twin` with the twin, its design (`design.json`: model spec and history, geometry hashes included) and its stored runs (`runs/<n>.json`). **Not done:** the Keystone snapshot (no snapshot API in `tpt-keystone`); the simulation setup is only the twin's scenarios, not a separate record
- [x] ed25519 signing and verification. Import rejects bad signatures, unlisted entries, hash mismatches and unsafe paths
- [x] Optional encryption on export: ChaCha20-Poly1305, key from Argon2id (`twin-package::encrypt`)
- [x] Import integrity checks; clear errors on tamper, wrong key and version mismatch. 10 tests
- [~] Round-trip test: the store test exports from one store and imports into another in the same process (separate directories, separate keys, wrong-key and tamper rejection). **Not done:** a real move between two machines
- [x] **Who signs a user's own export — decided and built:** each user has their own key (created on first export in `%APPDATA%	win`), the signer is recorded in the signed manifest, and imports pin the signer per twin (trust on first use; a different key for the same twin is refused). `Project::export_twin` / `import_twin`, 2 tests, and a Packages card in the window. The **format is changed** (`signer` field); see `docs/PACKAGE_FORMAT.md`

## Phase 7 — Desktop app (`twin-app`)

- [~] `tpt-appfront` shell: window and project open/save. `twin-app` uses `tpt-appfront-canvas` (egui + glow), pinned to `61465269` and building on Windows. `twin_app::Project` owns the store, twin list and open twin, with 5 tests; `main.rs` is a view over it with New, Save and Open buttons. **Not done:** menus, a native folder picker, and the rebuild and results views (parameter editing is now in the window). Licences resolved: BSL-1.0 (clipboard crates), OFL-1.1 and Ubuntu-font-1.0 (bundled egui fonts) are on the allow list in `deny.toml`, and `cargo deny check licenses` passes. The Ubuntu Font review stays open under Phase 8
- [x] Window theme: light palette, rounded controls, card sections, scrolling (`crates/twin-app/src/theme.rs`). Checked by screenshot. **Limitation:** the canvas has no root scrolling and lays out to the height it is given, so the content sits in a fixed 2400 px scroll region (blank space below short content). Proper fix: auto-height root in `tpt-appfront-canvas` (upstream)
- [~] 3D viewport with orbit / pan / zoom and selection. **Orbit, pan and zoom done:** a right-side panel in the Geometry section draws the design's surface (working parameters, so unsaved edits show) with flat shading, back to front, using CPU projection into egui's painter (`twin_app::view3d`, 5 tests; `Project::view_triangles`). Drag to orbit, right-drag to pan, scroll to zoom. The camera is not saved. Checked by screenshot: the block draws correctly, and drag-to-orbit, right-drag pan and scroll zoom all respond. Faces are drawn as mesh triangles, not polygons, because egui's feathered polygons spiked on faces seen edge-on. **Selection done:** the kernel keeps no face provenance, so each step's own solid is kept (`Model::face_steps`, 2 tests), and a face belongs to the earliest step whose surface contains it. Clicking a face highlights every face of its step and names the step; clicking nothing clears it. Checked by screenshot on the one-block demo. **Limitation:** a face split by a later cut may match no single step and is then not selectable. **Not done:** a `glow` renderer for large models, and selection on a multi-step model checked by eye
- [~] Parameter panel and feature tree: **parameter panel done.** Each parameter of the open twin has an editable field; numbers apply at once (not saved until Save), and text that is not a number is refused and blocks Save (`Project::edit_parameter`, 1 test). The feature tree is listed read-only in the Geometry card (`describe_step`, `Project::feature_tree`). **Not done:** editing the tree, and field-level validation ranges
- [~] History panel (branches, commits, diff): **parameter history done.** Start history, commit the saved parameters, create and switch branches, merge a branch into the current one, compare any two commits (or a branch tip with the head) in the log, see the head commit's changes (`Project` history methods, 3 tests). Commit, switch and merge are refused while the twin has unsaved edits; a conflicting merge is refused and changes nothing. **Geometry history in the panel:** done in the Geometry card: commit, create, switch and merge branches (refused with uncommitted edits, and a conflicting merge changes nothing), list each branch's changes against the head, link, and gated runs (`Project` design methods, 2 tests). **Not done:** geometry commit messages typed in the window (fixed text for now)
- [ ] Collaboration panel (peers, join/host): **done for parameters.** The Shared session card hosts or joins with an address and a session code (a new code can be generated), shows the shared parameters, sets one, and applies them to the open twin as unsaved edits. A joiner pins the host's fingerprint per address on first use (`collab-pins.txt`). The host's identity is kept in the user config folder. Two projects sharing parameters are tested headlessly (`two_projects_share_parameters_over_a_session`). Two codes per session: an edit code and a view-only code. View-only peers receive changes but their edits are ignored by the host (`Access`, tested over a raw connection too). The card shows how many peers are connected. Peers give a name when they join (`SyncClient::connect_as`, `SyncMessage::Hello.name`), and the host lists the names. **Not done:** presence beyond the names list, signed tokens (the codes are shared secrets; signed tokens need a security review first), and checking the card by eye (not yet looked at in the running window)
- [~] Simulation panel (set up, run, view results): **run and summary done.** Add an example mass-spring-damper model and scenario, run a scenario of a saved twin (stored as a run), and see a summary of the latest run (`Project::run_scenario` / `run_summary`, 1 test). A run records the parameters it used (`StoredRun.parameters`, optional in older files), and its summary says "stale" when the twin's parameters have changed since. Scenario duration and initial conditions are editable (`Twin::set_scenario_duration` / `set_scenario_initial` in `twin-core`; the window's fields work like the parameter fields, and a refused value blocks Save). Plot: "Plot <scenario>" draws position and velocity of the stored run in a resizable right-hand panel (`Project::plot_series`, painted with egui in `main.rs`, since the canvas tree has no custom-drawing element). **Geometry stale flag:** a run is refused until the twin is linked to the design head, and its summary says "stale" when the geometry changed. **Not done:** checking the plot by eye (not yet looked at in the running window)
- [~] Sensor panel and fusion visualisation: **sensor list done.** Add an IMU (50 Hz), remove a sensor (its recording goes too), and see each sensor's stored recording summary (`Project::sensors` / `add_imu` / `remove_sensor` / `recording_summary`, 1 test). Camera and lidar sensors can be added in the window (default settings), and "Plot" draws an IMU's stored recording (`PlotChoice::Sensor`, `Project::plot_data`). Each sensor's settings are editable in the window (IMU rate, camera field of view and size, lidar range and beams, mount position x/y/z; `Project::edit_sensor`, 2 tests), and a refused value blocks Save. **Lidar scan:** a lidar row counts the design surface points within its range (`Model::triangles`, `Project::lidar_summary`, sampled from the linked design; no occlusion). **Lidar plot:** "Plot scan" draws a saved lidar scan from above (`Project::lidar_scan_xy`, `scan_card`). **Camera output:** "Render" writes what a camera sees of the linked design as a greyscale PGM in the project folder (`twin_sensors::camera_depth`, a software pinhole projection with a depth test; no lighting, no clipping of triangles that cross the camera plane). **Lidar data:** "Save scan" writes the in-range points to `lidar-<name>.xyz`. **Not done:** fusion visualisation, and plots of the camera and lidar outputs
- [~] Export / import dialogs, including encryption options: **basic Packages card done** (type a file path, export or import; the export key is the user's own, and imports pin signers). The window takes a passphrase for encrypted packages (session only, never saved). Export and import use native file dialogs (`rfd`, which needs GTK development libraries on Linux). **Not done:** a dedicated export/import dialog with its own options
- [~] Settings: network mode, data location. **Read-only Settings section done:** shows the project folder and the network rules (no telemetry, nothing leaves the machine unless you host or join a session, LAN only when a LAN address is typed). **Not done:** a user-facing network-mode setting (the LAN opt-in in Phase 3) and a way to change the data location
- [ ] Example twin projects

## Phase 8 — Licence key & distribution (`twin-license`)

- [x] Key-file format and ed25519 offline verification (`twin-license`): JSON payload + base64 Ed25519 signature over the payload bytes. Checks signature, then product. 6 tests. Accepts a public key passed in; no key is compiled in
- [x] **Vendor public key:** generated locally. The public key is committed at `keys/twin-vendor.pub` (`keys/README.md`); the private seed stays outside the repo in the vendor's `~/.tpt-keys/`. A test checks the key verifies. A licence signed with the private key was checked end to end (not committed)
- [ ] Key-issuing tool (kept out of the public build or in a private tool). Not built: licences are signed by hand for now
- [x] Banner behaviour in official binaries; the source build works without a key. Official builds (`--features official`) show a banner when `twin.license` is not in the project folder; `Project::open` never fails over a licence
- [ ] Release workflow: build for Windows, macOS, Linux
- [ ] Code signing and installers — **decided: defer until v1.0 is close** (certificate cost and timing)
- [ ] Zip bundle: binaries + licence + quick-start guide (PDF) + examples (`GUMROAD.md`)
- [ ] `SHA256SUMS`
- [~] Review the Ubuntu Font Licence (bundled egui font, allowed in `deny.toml`) before v1.0. Unmodified fonts only. **Decided: keep it** (the fonts are unmodified and `cargo deny` allows the licence). Final legal read still open
- [x] Gumroad listing check against `GUMROAD.md` (price $349, 14-day refund): matches `GUMROAD.md`
- [x] Doc for submitting error reports: `docs/ERROR_REPORTS.md`. Reports are sent by hand; nothing is sent automatically

## Phase 9 — Quality & release criteria (§9)

- [ ] A small team edits one twin over a LAN with no cloud account
- [ ] A full twin moves between machines cleanly
- [ ] The app works with no network at all
- [ ] Confirm there are zero outbound connections (network capture)
- [~] Binary size and startup/frame-time budgets set and measured: **size measured** at 13.6 MB (Windows release build of `twin-app`, `cargo build --release`, unstripped, default profile). **Not done:** a budget to compare against, and startup and frame-time measurements
- [ ] Smoke tests on all three platforms
- [ ] Update `GUMROAD.md` system requirements to match measured numbers
- [ ] v1.0 tag

## Later (no commitment, §4)

- [ ] Richer solvers
- [ ] Advanced perception pipelines
- [ ] Controller design, hardware-in-the-loop
- [ ] Headless simulation server
- [ ] Formal claims about twin properties
