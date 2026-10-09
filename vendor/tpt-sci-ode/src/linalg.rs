//! In-crate dense linear algebra for the implicit ODE solvers.
//!
//! A small, self-contained row-major dense matrix with an LU decomposition
//! (partial pivoting) and finite-difference Jacobian construction. This keeps
//! `tpt-sci-ode` free of any external linear-algebra dependency (no `faer`,
//! `nalgebra`, or even `tpt-math-linalg`), so the shipped crate is 100%
//! dual-licensed TPT-owned code — the explicit goal of the Phase 7 rewrite
//! (todo.md Phase 7, decision 7.0b).

use crate::error::OdeError;

/// RHS signature: any [`RhsCallable`](crate::RhsCallable) (closure or JIT).
pub(crate) type RhsFn<'a> = dyn crate::RhsCallable + 'a;

/// Evaluate `f(t, y)` into a fresh `Vec<f64>`.
pub(crate) fn eval(f: &RhsFn<'_>, t: f64, y: &[f64]) -> Vec<f64> {
    let mut out = vec![0.0; y.len()];
    f.call(t, y, &mut out)
        .expect("RHS evaluation must not fail for a well-formed problem");
    out
}

/// Row-major dense `nrows × ncols` matrix.
pub(crate) struct DMat {
    pub nrows: usize,
    pub ncols: usize,
    pub data: Vec<f64>,
}

impl DMat {
    pub fn new(nrows: usize, ncols: usize) -> Self {
        DMat {
            nrows,
            ncols,
            data: vec![0.0; nrows * ncols],
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn from_row_slice(nrows: usize, ncols: usize, data: &[f64]) -> Self {
        debug_assert_eq!(data.len(), nrows * ncols);
        DMat {
            nrows,
            ncols,
            data: data.to_vec(),
        }
    }

    #[inline]
    pub fn get(&self, i: usize, j: usize) -> f64 {
        self.data[i * self.ncols + j]
    }

    #[inline]
    pub fn set(&mut self, i: usize, j: usize, v: f64) {
        self.data[i * self.ncols + j] = v;
    }

    #[allow(dead_code)]
    pub fn identity(n: usize) -> Self {
        let mut m = DMat::new(n, n);
        for i in 0..n {
            m.data[i * n + i] = 1.0;
        }
        m
    }

    /// `self += gamma * I`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn add_scaled_identity(&mut self, gamma: f64) {
        debug_assert_eq!(self.nrows, self.ncols);
        let n = self.nrows;
        for i in 0..n {
            self.data[i * n + i] += gamma;
        }
    }

    /// `y = A x`.
    #[allow(dead_code, clippy::needless_range_loop)]
    pub fn mat_vec(&self, x: &[f64]) -> Vec<f64> {
        let mut y = vec![0.0; self.nrows];
        for i in 0..self.nrows {
            let row = &self.data[i * self.ncols..(i + 1) * self.ncols];
            let mut s = 0.0;
            for j in 0..self.ncols {
                s += row[j] * x[j];
            }
            y[i] = s;
        }
        y
    }

    /// Solve `A x = b` for square `A` via LU with partial pivoting. Returns
    /// `None` if the matrix is (numerically) singular.
    pub fn solve(&self, b: &[f64]) -> Option<Vec<f64>> {
        debug_assert_eq!(self.nrows, self.ncols);
        let n = self.nrows;
        let mut lu = self.data.clone();
        let mut piv = (0..n).collect::<Vec<_>>();
        for k in 0..n {
            // Partial pivot.
            let mut p = k;
            let mut max = lu[k * n + k].abs();
            for i in (k + 1)..n {
                let v = lu[i * n + k].abs();
                if v > max {
                    max = v;
                    p = i;
                }
            }
            if max < 1e-14 {
                return None;
            }
            if p != k {
                for j in 0..n {
                    lu.swap(k * n + j, p * n + j);
                }
                piv.swap(k, p);
            }
            let pivot = lu[k * n + k];
            for i in (k + 1)..n {
                let fac = lu[i * n + k] / pivot;
                lu[i * n + k] = fac;
                for j in (k + 1)..n {
                    lu[i * n + j] -= fac * lu[k * n + j];
                }
            }
        }
        // Forward substitution (unit-lower-triangular `lu`).
        let mut y = vec![0.0; n];
        for i in 0..n {
            let pi = piv[i];
            let mut s = b[pi];
            for j in 0..i {
                s -= lu[i * n + j] * y[j];
            }
            y[i] = s;
        }
        // Back substitution (upper-triangular `lu`).
        let mut x = vec![0.0; n];
        for i in (0..n).rev() {
            let mut s = y[i];
            for j in (i + 1)..n {
                s -= lu[i * n + j] * x[j];
            }
            x[i] = s / lu[i * n + i];
        }
        Some(x)
    }
}

/// Two-norm of a slice.
pub(crate) fn norm2(v: &[f64]) -> f64 {
    v.iter().map(|x| x * x).sum::<f64>().sqrt()
}

/// Full Jacobian `J = df/dy` at `(t, y)` via forward finite differences.
///
/// `f0` is `f(t, y)`; the step is scaled per component (`ε·|yᵢ| + ε`) so the
/// difference quotient stays well-conditioned near zero states. Systems solved
/// by this pillar are small (≲ a few hundred states), so the O(n²) evaluation
/// cost is negligible.
pub(crate) fn jacobian(f: &dyn crate::RhsCallable, t: f64, y: &[f64], f0: &[f64]) -> DMat {
    let n = y.len();
    let sqrt_eps = f64::sqrt(f64::EPSILON);
    let mut j = DMat::new(n, n);
    for col in 0..n {
        let dy = sqrt_eps * y[col].abs().max(1.0);
        let mut yp = y.to_vec();
        yp[col] += dy;
        let fp = eval(f, t, &yp);
        let inv = 1.0 / dy;
        for row in 0..n {
            j.set(row, col, (fp[row] - f0[row]) * inv);
        }
    }
    j
}

/// Solve one SDIRK stage `k = f(t_stage, y_base + diag·k)` by Newton iteration
/// with a freshly-computed finite-difference Jacobian each iteration. The
/// systems solved here are small, so recomputing the Jacobian per iteration is
/// cheap and avoids the staleness of a single frozen Jacobian on nonlinear
/// stages.
pub(crate) fn sdirk_stage(
    f: &dyn crate::RhsCallable,
    t_stage: f64,
    y_base: &[f64],
    diag: f64,
    k_guess: &[f64],
) -> Result<Vec<f64>, OdeError> {
    let n = y_base.len();
    // Large systems route their Newton linear solves through the sparse
    // CSR/LU path (`sparse` module): the finite-difference Jacobian is built
    // directly in compressed storage and never materialised densely.
    if n >= crate::sparse::SPARSE_LU_MIN_N {
        return crate::sparse::sdirk_stage_sparse(f, t_stage, y_base, diag, k_guess);
    }
    let mut k = k_guess.to_vec();
    for _iter in 0..50 {
        let y_stage: Vec<f64> = y_base.iter().zip(&k).map(|(b, kk)| b + diag * kk).collect();
        let fk = eval(f, t_stage, &y_stage);
        let r: Vec<f64> = k.iter().zip(&fk).map(|(kk, fv)| kk - fv).collect();
        if norm2(&r) < 1e-12 {
            return Ok(k);
        }
        let jac = jacobian(f, t_stage, &y_stage, &fk);
        // Newton matrix A = I - diag·J.
        let mut a = DMat::new(n, n);
        for i in 0..n {
            for j in 0..n {
                a.set(i, j, if i == j { 1.0 } else { 0.0 } - diag * jac.get(i, j));
            }
        }
        let delta = a.solve(&r).ok_or(OdeError::Newton {
            t: t_stage,
            residual: norm2(&r),
        })?;
        for i in 0..n {
            k[i] -= delta[i];
        }
    }
    Ok(k)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lu_solves_known_3x3_system() {
        // [2 1 1][x]   [4]        -> x = [1, 1, 1]
        // [1 3 2][y] = [6]
        // [1 0 1][z]   [2]
        let a = DMat::from_row_slice(3, 3, &[2.0, 1.0, 1.0, 1.0, 3.0, 2.0, 1.0, 0.0, 1.0]);
        let b = [4.0, 6.0, 2.0];
        let x = a.solve(&b).expect("non-singular");
        assert!((x[0] - 1.0).abs() < 1e-12);
        assert!((x[1] - 1.0).abs() < 1e-12);
        assert!((x[2] - 1.0).abs() < 1e-12);
    }

    #[test]
    fn mat_vec_and_add_scaled_identity() {
        let mut a = DMat::from_row_slice(2, 2, &[1.0, 2.0, 3.0, 4.0]);
        let y = a.mat_vec(&[1.0, 1.0]);
        assert!((y[0] - 3.0).abs() < 1e-12);
        assert!((y[1] - 7.0).abs() < 1e-12);
        a.add_scaled_identity(2.0);
        assert!((a.get(0, 0) - 3.0).abs() < 1e-12);
        assert!((a.get(1, 1) - 6.0).abs() < 1e-12);
    }

    #[test]
    fn jacobian_of_linear_system_is_constant() {
        // f(y) = A y,  A = [[0, 1], [-2, -3]]
        let a = DMat::from_row_slice(2, 2, &[0.0, 1.0, -2.0, -3.0]);
        let f = move |_t: f64, y: &[f64], dydt: &mut [f64]| {
            let out = a.mat_vec(y);
            dydt[0] = out[0];
            dydt[1] = out[1];
        };
        let y = [0.7, -1.3];
        let f0 = eval(&f, 0.0, &y);
        let j = jacobian(&f, 0.0, &y, &f0);
        assert!((j.get(0, 0) - 0.0).abs() < 1e-8);
        assert!((j.get(0, 1) - 1.0).abs() < 1e-8);
        assert!((j.get(1, 0) - (-2.0)).abs() < 1e-8);
        assert!((j.get(1, 1) - (-3.0)).abs() < 1e-8);
    }

    #[test]
    fn sdirk_stage_solves_linear_implicit_step() {
        // Stage equation k = f(t, y + diag*k) with f(y) = -2 y  (y constant).
        // => k = -2 (y + diag k) => k (1 + 2 diag) = -2 y.
        let y = [1.0, 2.0];
        let diag = 0.1;
        let f = |_t: f64, y: &[f64], dydt: &mut [f64]| {
            dydt[0] = -2.0 * y[0];
            dydt[1] = -2.0 * y[1];
        };
        let k = sdirk_stage(&f, 0.0, &y, diag, &[0.0, 0.0]).unwrap();
        let expected0 = -2.0 * y[0] / (1.0 + 2.0 * diag);
        let expected1 = -2.0 * y[1] / (1.0 + 2.0 * diag);
        assert!((k[0] - expected0).abs() < 1e-10);
        assert!((k[1] - expected1).abs() < 1e-10);
    }
}
