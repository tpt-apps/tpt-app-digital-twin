use thiserror::Error;

/// Errors raised while building or solving an [`OdeProblem`](crate::OdeProblem).
#[derive(Debug, Error)]
pub enum OdeError {
    /// The problem definition was invalid (e.g. empty initial state).
    #[error("invalid ODE problem: {0}")]
    Invalid(String),

    /// The nonlinear solver (Newton) failed to converge within the iteration
    /// budget, or the linear solve was singular. Usually a sign that the step
    /// size is too large for the current state; the integrator retries with a
    /// smaller step before surfacing this error.
    #[error("nonlinear solve failed to converge at t = {t} (last residual {residual:.3e})")]
    Newton {
        /// Time at which the failure occurred.
        t: f64,
        /// Magnitude of the residual on the final Newton iteration.
        residual: f64,
    },

    /// The step-size controller drove the step below the smallest representable
    /// increment (e.g. at a discontinuity or a very stiff region), so the
    /// requested `t_final` cannot be reached.
    #[error("step size collapsed below machine precision at t = {t}")]
    StepTooSmall {
        /// Time at which the step collapsed.
        t: f64,
    },

    /// The integrator exceeded its maximum step count (a safety rail against
    /// runaway step contraction); indicates the problem is far stiffer than the
    /// configured method can handle at the requested tolerances.
    #[error("maximum number of steps ({max_steps}) exceeded before reaching t = {t_final}")]
    MaxSteps {
        /// Target time that was not reached.
        t_final: f64,
        /// Configured step ceiling.
        max_steps: usize,
    },
}

impl OdeError {
    pub(crate) fn invalid(msg: impl Into<String>) -> Self {
        OdeError::Invalid(msg.into())
    }
}
