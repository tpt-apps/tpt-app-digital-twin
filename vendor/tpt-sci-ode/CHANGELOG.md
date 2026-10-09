# Changelog

All notable changes to `tpt-sci-ode` are documented here. This project adheres
to [Semantic Versioning](https://semver.org).

## [0.1.0] — 2026-08-22

Initial release of `tpt-sci-ode` to crates.io.

### Added

- From-scratch ODE/DAE integrators on the dual-licensed `tpt-math` dense
  linear algebra:
  - `Method::Tsit45` — explicit Runge–Kutta 4(5), non-stiff.
  - `Method::TrBdf2` — 2-stage SDIRK (TR-BDF2), A-stable, stiff.
  - `Method::Esdirk34` — 4-stage ESDIRK order 3(4), A-/L-stable, stiff.
  - `Method::Bdf` — variable-order (1–5) backward differentiation, stiff, with a
    Nordsieck predictor-corrector and Hermite dense output.
- Closure-first `OdeProblem` API (`Rhs = Fn(f64, &[f64], &mut [f64])`) and an
  `Rc<dyn RhsCallable>` pipeline shared by closures and JIT RHS.
- Adaptive-step driver with Hermite dense output so `solve_dense` lands exactly on
  requested `t_eval` points.
- `JitRhs` / `JitRhsBuilder` / `compile_rhs` — an optional Cranelift JIT-compiled
  right-hand side that plugs into the same `RhsCallable` trait as a plain closure.
- `verify-diffsol` dev feature — retains `diffsol` only as an optional, dev-only
  verification oracle (excluded from `cargo deny` license scanning).
- Sparse Newton path for the implicit solvers: for systems with ≥ 64 states,
  `sdirk_stage` (TR-BDF2 / ESDIRK34) and the BDF corrector build their
  finite-difference Jacobians directly in compressed CSR storage and factor
  the Newton matrix `I − γ·J` with the in-crate sparse LU (`sparse` module),
  never materialising a dense Jacobian. Small systems keep the dense path.
  The `CsrMatrix` API is public (`nrows`, `ncols`, `nnz`, `get`, `mat_vec`,
  `jacobian`, `scaled_identity_minus_scaled`, `sparse_solve`).

### Changed

- **Replaced the `diffsol` wrap with an in-house, dual-licensed ODE engine**
  (`linalg` module: row-major `DMat`, LU with partial pivoting, finite-difference
  Jacobian). No `diffsol` / `nalgebra` / `faer` remains in the shipped dependency
  graph. The public API (`OdeProblem`, `OdeProblemBuilder`, `Method`, `solve`,
  `solve_dense`) is unchanged so downstream crates need no edits.
- `solve_dense` now builds the solver once and walks a single trajectory
  (`solver.solve_dense(t_eval)`), removing the previous O(n) redundant
  re-integration from `t0` for each evaluation point.

[0.1.0]: https://github.com/tpt-solutions/tpt-science/releases/tag/v0.1.0
