# tpt-sci-ode

ODE/DAE solving substrate for the `tpt-science` pillar.

This crate implements its own ODE integrators **from scratch** — no
[`diffsol`](https://crates.io/crates/diffsol), `nalgebra`, or `faer` in the
shipped dependency graph — so the whole crate is TPT-owned code under
`MIT OR Apache-2.0`. It carries its own dual-licensed dense linear algebra
(`DMat` + LU with partial pivoting) for the implicit-method linear solves, and
depends on `tpt-math-numeric` for the `Scalar` numeric trait.

Four methods are provided (see [`Method`](https://docs.rs/tpt-sci-ode/latest/tpt_sci_ode/enum.Method.html)):

* [`Method::Tsit45`] — explicit Runge–Kutta 4(5), non-stiff.
* [`Method::TrBdf2`] — 2-stage SDIRK (TR-BDF2), A-stable, stiff.
* [`Method::Esdirk34`] — 4-stage ESDIRK order 3(4), A-/L-stable, stiff.
* [`Method::Bdf`] — variable-order (1–5) backward differentiation, stiff.

All use a shared adaptive-step driver with Hermite dense output, so
[`OdeProblem::solve_dense`] returns exact states at the requested times.
`diffsol` is retained only as an optional, dev-only verification oracle (feature
`verify-diffsol`, excluded from license scanning).

Depends on `tpt-math-numeric` (published, `tpt-math` repo).

## Features

* Closure-first problem API: `OdeProblem::new(|t, y, dydt| { .. }, y0, t0)`.
* Adaptive step-size control with dense (Hermite) output for `solve_dense`.
* Stiff and non-stiff methods behind one `Method` enum and one `solve` entry point.
* Optional Cranelift JIT right-hand side (`JitRhs`) behind the `jit` module,
  pluggable into the same `RhsCallable` trait as a plain closure.

## Example

```rust
use tpt_sci_ode::{OdeProblem, Method};

// dy/dt = -y,  y(0) = 1  ->  y(1) = e^-1
let prob = OdeProblem::new(
    |_t, y, dydt| { dydt[0] = -y[0]; },
    vec![1.0],
    0.0,
).unwrap();
let y = prob.solve(Method::Tsit45, 1.0).unwrap();
assert!((y[0] - std::f64::consts::E.recip()).abs() < 1e-5);
```

## Scope

v1 covers deterministic initial-value ODEs (non-stiff and stiff) with dense
output. In addition to the dense [`DMat`](https://docs.rs/tpt-sci-ode/latest/tpt_sci_ode/linalg/struct.DMat.html)
linear algebra used by the implicit methods, the crate also ships:

* A sparse counterpart
  ([`CsrMatrix`](https://docs.rs/tpt-sci-ode/latest/tpt_sci_ode/sparse/struct.CsrMatrix.html)
  + sparse LU with partial pivoting) for large, sparse Jacobians.
* Forward sensitivity analysis
  ([`forward_sensitivities`](https://docs.rs/tpt-sci-ode/latest/tpt_sci_ode/fn.forward_sensitivities.html))
  of `dy/dt = f(t, y, p)` via the augmented variational system.
* A `Scalar`-generic (`f32`/`f64`) dense-LA + RK4 core
  ([`scalar`](https://docs.rs/tpt-sci-ode/latest/tpt_sci_ode/scalar/index.html))
  proving the engine math at both precisions; the public [`OdeProblem`] API
  remains `f64`-instantiated (the v1 supported precision).

DAE index reduction and adjoint (reverse) sensitivity analysis remain out of
scope.

## License

Licensed under either of [MIT](../../LICENSE-MIT) or
[Apache-2.0](../../LICENSE-APACHE) at your option.
