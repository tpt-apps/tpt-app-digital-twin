//! Forward sensitivity analysis for ODE models.
//!
//! Given `dy/dt = f(t, y, p)`, forward sensitivity computes the derivative of
//! the solution with respect to the parameters, `S = ∂y/∂p`, by integrating the
//! *augmented* system
//!
//! ```text
//! d/dt [ y ]   [ f(t, y, p)                  ]
//!      [ S ] = [ (∂f/∂y)·S + ∂f/∂p           ]
//! ```
//!
//! alongside the original ODE. The Jacobian `∂f/∂y` and the parameter gradient
//! `∂f/∂p` are approximated with forward finite differences each step
//! (reusing the dense [`DMat`](crate::linalg) for the matrix product);
//! the augmented system itself is integrated with the existing in-house
//! [`Method`] solvers, so sensitivities inherit the same adaptive error
//! control as the base trajectory.
//!
//! This is the standard "forward sensitivity" (first-order variational)
//! approach — see, e.g., Hindmarsh et al., *ASC 2005* (CVODES) — and reuses
//! the Phase 7 engine rather than pulling in a sensitivity-capable external
//! solver (todo.md, Phase 9a).

use crate::OdeError;
use crate::RhsCallable;
use crate::linalg::DMat;
use crate::problem::OdeProblemBuilder;
use crate::solver::Method;

/// Sensitivity result: the trajectory together with the parameter Jacobian
/// `∂y/∂p` at each requested output time.
pub struct SensitivityResult {
    /// Times at which outputs are reported (empty if only the final state was
    /// requested, in which case `states`/`sensitivities` hold one entry each).
    pub t: Vec<f64>,
    /// State vectors, one per output time.
    pub states: Vec<Vec<f64>>,
    /// Sensitivity matrices, one per output time. Each matrix is row-major
    /// `n_states × n_params`, i.e. `sensitivities[k][i * np + j] = ∂y_i/∂p_j`
    /// at output time `k`.
    pub sensitivities: Vec<Vec<f64>>,
}

/// Solve the augmented forward-sensitivity system for `dy/dt = f(t, y, p)`.
///
/// `y0`/`t0`/`t_final` mirror [`OdeProblem::solve`](crate::OdeProblem::solve);
/// `p` is the parameter vector; `t_eval` (if `Some`) selects output times
/// (otherwise only the final state + sensitivities are returned). The base
/// tolerances `rtol`/`atol` apply to the augmented system (which is
/// `n_states·(1 + n_params)`-dimensional), so they should be chosen
/// accordingly.
///
/// # Errors
///
/// Propagates [`OdeError`] from the underlying integrator (non-convergent
/// Newton step, collapsed step, or step budget exceeded).
#[allow(clippy::too_many_arguments)]
pub fn forward_sensitivities<F>(
    f: F,
    y0: &[f64],
    p: &[f64],
    t0: f64,
    t_final: f64,
    method: Method,
    rtol: f64,
    atol: f64,
    t_eval: Option<&[f64]>,
) -> Result<SensitivityResult, OdeError>
where
    F: Fn(f64, &[f64], &[f64], &mut [f64]) + 'static,
{
    let n = y0.len();
    let np = p.len();
    let aug = n + n * np; // [y (n); S (n·np)]
    let p_owned = p.to_vec();

    // Wrapper exposing `f(t, y, p)` as a plain `RhsCallable` over the augmented
    // state, owning its own copy of `p`.
    struct AugRhs<F> {
        f: F,
        p: Vec<f64>,
        n: usize,
        np: usize,
    }
    impl<F> RhsCallable for AugRhs<F>
    where
        F: Fn(f64, &[f64], &[f64], &mut [f64]),
    {
        fn nstates(&self) -> usize {
            0
        }
        fn call(&self, t: f64, z: &[f64], dydz: &mut [f64]) -> Result<(), OdeError> {
            let n = self.n;
            let np = self.np;
            let y = &z[..n];
            let s = &z[n..];
            // Base RHS.
            let mut dydt = vec![0.0; n];
            (self.f)(t, y, &self.p, &mut dydt);
            dydz[..n].copy_from_slice(&dydt);
            // Finite-difference Jacobian ∂f/∂y (n×n) and gradient ∂f/∂p (n×np).
            let sqrt_eps = f64::sqrt(f64::EPSILON);
            let mut jac = DMat::new(n, n);
            for c in 0..n {
                let dy = sqrt_eps * y[c].abs().max(1.0);
                let mut yp = y.to_vec();
                yp[c] += dy;
                let mut fp = vec![0.0; n];
                (self.f)(t, &yp, &self.p, &mut fp);
                let inv = 1.0 / dy;
                for r in 0..n {
                    jac.set(r, c, (fp[r] - dydt[r]) * inv);
                }
            }
            let mut fp_grad = DMat::new(n, np);
            for c in 0..np {
                let dp = sqrt_eps * self.p[c].abs().max(1.0);
                let mut pp = self.p.clone();
                pp[c] += dp;
                let mut fp = vec![0.0; n];
                (self.f)(t, y, &pp, &mut fp);
                let inv = 1.0 / dp;
                for r in 0..n {
                    fp_grad.set(r, c, (fp[r] - dydt[r]) * inv);
                }
            }
            // Sensitivity RHS: Ṡ = J·S + ∂f/∂p.  s is row-major n×np.
            for i in 0..n {
                for k in 0..np {
                    let mut acc = fp_grad.get(i, k);
                    for j in 0..n {
                        acc += jac.get(i, j) * s[j * np + k];
                    }
                    dydz[n + i * np + k] = acc;
                }
            }
            Ok(())
        }
    }

    let aug_rhs = AugRhs {
        f,
        p: p_owned,
        n,
        np,
    };
    let mut z0 = vec![0.0; aug];
    z0[..n].copy_from_slice(y0);
    // S0 = 0 (sensitivities at t0 are zero: y(t0) = y0 independent of p).

    let eval_times: Vec<f64> = match t_eval {
        Some(te) => te.to_vec(),
        None => vec![t_final],
    };
    let prob = OdeProblemBuilder::from_rhs(aug_rhs, z0, t0)
        .rtol(rtol)
        .atol(atol)
        .build()?;
    let aug_traj = prob.solve_dense(method, &eval_times)?;

    let mut t = Vec::new();
    let mut states = Vec::new();
    let mut sensitivities = Vec::new();
    for (idx, z) in aug_traj.iter().enumerate() {
        t.push(eval_times[idx]);
        states.push(z[..n].to_vec());
        sensitivities.push(z[n..].to_vec());
    }
    Ok(SensitivityResult {
        t,
        states,
        sensitivities,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitivity_of_exponential_decay() {
        // dy/dt = -p*y,  y(0) = 1.  Solution y(t) = exp(-p t).
        // ∂y/∂p = -t·exp(-p t) = -t·y.
        let res = forward_sensitivities(
            |_t, y, p, dydt| {
                dydt[0] = -p[0] * y[0];
            },
            &[1.0],
            &[2.0],
            0.0,
            1.0,
            Method::Tsit45,
            1e-8,
            1e-8,
            Some(&[1.0]),
        )
        .unwrap();
        let y = res.states[0][0];
        let expected_y = (-2.0_f64).exp();
        assert!((y - expected_y).abs() < 1e-5);
        let s = res.sensitivities[0][0];
        let expected_s = -expected_y; // -t·y at t=1
        assert!((s - expected_s).abs() < 1e-4);
    }

    #[test]
    fn sensitivity_of_linear_param() {
        // dy/dt = a, y(0)=0  => y = a t,  ∂y/∂a = t.
        let res = forward_sensitivities(
            |_t, _y, p, dydt| {
                dydt[0] = p[0];
            },
            &[0.0],
            &[3.0],
            0.0,
            2.0,
            Method::Tsit45,
            1e-8,
            1e-8,
            Some(&[2.0]),
        )
        .unwrap();
        assert!((res.states[0][0] - 6.0).abs() < 1e-6);
        assert!((res.sensitivities[0][0] - 2.0).abs() < 1e-6); // ∂y/∂a = t = 2
    }
}
