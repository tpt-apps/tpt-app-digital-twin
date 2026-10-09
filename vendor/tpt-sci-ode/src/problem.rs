use std::rc::Rc;

use crate::RhsCallable;
use crate::error::OdeError;

/// Right-hand-side closure shape accepted by [`OdeProblemBuilder`].
///
/// `f(t, y, dydt)` must write the derivative `dy/dt` into `dydt`, given the
/// current time `t` and state `y`. The state vectors are plain `f64` slices,
/// so callers do not need to depend on `diffsol` directly.
pub type Rhs = dyn Fn(f64, &[f64], &mut [f64]);

/// Builder for an [`OdeProblem`]. Use [`OdeProblem::new`] for the common case
/// (default tolerances), or configure tolerances/step size here first.
pub struct OdeProblemBuilder {
    rhs: Rc<dyn RhsCallable>,
    y0: Vec<f64>,
    t0: f64,
    rtol: f64,
    atol: f64,
    h0: f64,
}

impl OdeProblemBuilder {
    /// Start building a problem for `dy/dt = rhs(t, y)` with initial state
    /// `y0` at time `t0`. The right-hand side is given as a closure
    /// `Fn(f64, &[f64], &mut [f64])`, which is the common case.
    pub fn new<F>(rhs: F, y0: Vec<f64>, t0: f64) -> Self
    where
        F: Fn(f64, &[f64], &mut [f64]) + 'static,
    {
        Self {
            rhs: Rc::new(rhs),
            y0,
            t0,
            rtol: 1e-6,
            atol: 1e-6,
            h0: 1.0,
        }
    }

    /// Start building a problem from any [`RhsCallable`] (e.g. a
    /// [`crate::JitRhs`] compiled by the Cranelift JIT). Use this instead of
    /// [`OdeProblemBuilder::new`] when the RHS is not a plain closure.
    pub fn from_rhs<R: RhsCallable + 'static>(rhs: R, y0: Vec<f64>, t0: f64) -> Self {
        Self {
            rhs: Rc::new(rhs),
            y0,
            t0,
            rtol: 1e-6,
            atol: 1e-6,
            h0: 1.0,
        }
    }

    /// Relative tolerance passed to the solver's adaptive error control.
    pub fn rtol(mut self, rtol: f64) -> Self {
        self.rtol = rtol;
        self
    }

    /// Absolute tolerance passed to the solver's adaptive error control.
    pub fn atol(mut self, atol: f64) -> Self {
        self.atol = atol;
        self
    }

    /// Initial (guessed) step size.
    pub fn h0(mut self, h0: f64) -> Self {
        self.h0 = h0;
        self
    }

    /// Finalise the builder into a ready-to-solve [`OdeProblem`].
    ///
    /// # Errors
    ///
    /// Returns [`OdeError::Invalid`] if `y0` is empty or if `rtol`/`atol` are
    /// not strictly positive.
    pub fn build(self) -> Result<OdeProblem, OdeError> {
        let n = self.y0.len();
        if n == 0 {
            return Err(OdeError::Invalid(
                "initial state y0 must be non-empty".to_string(),
            ));
        }
        if self.rtol <= 0.0 || self.atol <= 0.0 {
            return Err(OdeError::Invalid(
                "rtol and atol must be strictly positive".to_string(),
            ));
        }
        Ok(OdeProblem {
            rhs: self.rhs,
            y0: self.y0,
            t0: self.t0,
            rtol: self.rtol,
            atol: self.atol,
            h0: self.h0,
            nstates: n,
        })
    }
}

/// A built ODE problem, ready to be integrated to a target time with one of
/// the available solvers (see [`Method`](crate::Method) and
/// [`OdeProblem::solve`]).
pub struct OdeProblem {
    pub(crate) rhs: Rc<dyn RhsCallable>,
    pub(crate) y0: Vec<f64>,
    pub(crate) t0: f64,
    pub(crate) rtol: f64,
    pub(crate) atol: f64,
    pub(crate) h0: f64,
    pub(crate) nstates: usize,
}

impl OdeProblem {
    /// Build a problem with default tolerances (`rtol = atol = 1e-6`).
    ///
    /// # Errors
    ///
    /// Returns [`OdeError::Invalid`] if `y0` is empty (the default tolerances are
    /// always strictly positive, so they never trigger an error here).
    pub fn new<F>(rhs: F, y0: Vec<f64>, t0: f64) -> Result<Self, OdeError>
    where
        F: Fn(f64, &[f64], &mut [f64]) + 'static,
    {
        OdeProblemBuilder::new(rhs, y0, t0).build()
    }

    /// Build a problem from any [`RhsCallable`] (e.g. a [`crate::JitRhs`]).
    ///
    /// # Errors
    ///
    /// Returns [`OdeError::Invalid`] if `y0` is empty.
    pub fn from_rhs<R: RhsCallable + 'static>(
        rhs: R,
        y0: Vec<f64>,
        t0: f64,
    ) -> Result<Self, OdeError> {
        OdeProblemBuilder::from_rhs(rhs, y0, t0).build()
    }

    /// Number of state variables.
    pub fn nstates(&self) -> usize {
        self.nstates
    }

    /// Initial time of the problem (the `t0` passed at construction).
    pub fn t0(&self) -> f64 {
        self.t0
    }

    /// Initial state of the problem (the `y0` passed at construction).
    pub fn y0(&self) -> &[f64] {
        &self.y0
    }

    /// Produce a fresh problem that shares this problem's RHS and tolerances
    /// but starts from a new initial state `y0` at time `t0`. This is used by
    /// multi-scale orchestration (see `tpt-sci-sim-core`) to step an ODE
    /// forward incrementally from its current state rather than re-integrating
    /// from the original `t0` each time.
    ///
    /// # Errors
    ///
    /// Returns [`OdeError::Invalid`] if `y0` is empty (the shared tolerances are
    /// strictly positive, so they never trigger an error here).
    pub fn respawn(&self, y0: Vec<f64>, t0: f64) -> Result<Self, OdeError> {
        if y0.is_empty() {
            return Err(OdeError::Invalid(
                "initial state y0 must be non-empty".to_string(),
            ));
        }
        Ok(Self {
            rhs: self.rhs.clone(),
            y0,
            t0,
            rtol: self.rtol,
            atol: self.atol,
            h0: self.h0,
            nstates: self.nstates,
        })
    }
}
