//! # tpt-sci-ode
//!
//! ODE/DAE solving substrate for the `tpt-science` pillar.
//!
//! This crate implements its own ODE integrators **from scratch** — no
//! [`diffsol`](https://crates.io/crates/diffsol), `nalgebra`, or `faer` in the
//! shipped dependency graph — so the whole crate is TPT-owned code under
//! `MIT OR Apache-2.0`. It carries its own dual-licensed dense linear algebra
//! (`DMat` + LU with partial pivoting, in `linalg`) for the implicit-method
//! linear solves, and depends on `tpt-math-numeric` only for the `Scalar`
//! numeric trait.
//!
//! Four methods are provided (see [`Method`]):
//! * [`Method::Tsit45`] — explicit Runge–Kutta 4(5), non-stiff.
//! * [`Method::TrBdf2`] — 2-stage SDIRK (TR-BDF2), A-stable, stiff.
//! * [`Method::Esdirk34`] — 4-stage ESDIRK order 3(4), A-/L-stable, stiff.
//! * [`Method::Bdf`] — variable-order (1–5) backward differentiation, stiff.
//!
//! All use a shared adaptive-step driver with Hermite dense output, so
//! [`OdeProblem::solve_dense`] returns exact states at the requested times.
//! For systems with at least 64 states, the implicit methods build their
//! Newton matrices through the in-crate sparse CSR/LU path ([`sparse`]),
//! never materialising a dense Jacobian.
//! `diffsol` is retained only as an optional, dev-only verification oracle
//! (feature `verify-diffsol`, excluded from license scanning).
//!
//! ## Example
//!
//! ```rust
//! use tpt_sci_ode::{OdeProblem, Method};
//!
//! // dy/dt = -y,  y(0) = 1  ->  y(1) = e^-1
//! let prob = OdeProblem::new(
//!     |_t, y, dydt| { dydt[0] = -y[0]; },
//!     vec![1.0],
//!     0.0,
//! ).unwrap();
//! let y = prob.solve(Method::Tsit45, 1.0).unwrap();
//! assert!((y[0] - std::f64::consts::E.recip()).abs() < 1e-5);
//! ```

pub mod error;
pub mod jit;
pub mod linalg;
pub mod problem;
pub mod scalar;
pub mod sensitivity;
pub mod solver;
pub mod sparse;

pub use error::OdeError;
pub use jit::{JitRhs, JitRhsBuilder, compile_rhs};
pub use problem::{OdeProblem, OdeProblemBuilder, Rhs};
pub use sensitivity::{SensitivityResult, forward_sensitivities};
pub use solver::Method;

/// A callable right-hand side for the ODE solvers.
///
/// This is implemented automatically for any `Fn(f64, &[f64], &mut [f64])`
/// closure (the common way to define a problem, e.g. `|t, y, dydt| { .. }`),
/// and explicitly for [`JitRhs`] so a Cranelift JIT-compiled right-hand side
/// can be used interchangeably with a plain closure.
///
/// [`OdeProblem`] and the integrators store the RHS behind an `Rc<dyn
/// RhsCallable>`, so both representations share the exact same solving
/// pipeline.
pub trait RhsCallable {
    /// Number of state variables this RHS operates on.
    fn nstates(&self) -> usize;

    /// Evaluate `dy/dt = f(t, y)` into `dydt`.
    ///
    /// # Errors
    ///
    /// Returns [`OdeError`] if the evaluation fails (e.g. the RHS reports an
    /// invalid state). The in-house closure-backed implementation never fails.
    fn call(&self, t: f64, y: &[f64], dydt: &mut [f64]) -> Result<(), OdeError>;
}

impl<F> RhsCallable for F
where
    F: Fn(f64, &[f64], &mut [f64]),
{
    fn nstates(&self) -> usize {
        // A bare closure does not carry its dimension; the problem size is
        // taken from `y0` instead.
        0
    }

    fn call(&self, t: f64, y: &[f64], dydt: &mut [f64]) -> Result<(), OdeError> {
        self(t, y, dydt);
        Ok(())
    }
}

// Re-export the numeric scalar trait from the tpt-math substrate that this
// crate depends on, so downstream science crates share one scalar vocabulary.
pub use tpt_math_numeric::Scalar;
