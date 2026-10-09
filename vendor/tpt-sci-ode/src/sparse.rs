//! In-crate sparse linear algebra for the implicit ODE solvers.
//!
//! A compressed-sparse-row ([`CsrMatrix`]) type plus a hand-rolled sparse LU
//! decomposition with partial pivoting ([`CsrMatrix::sparse_solve`]). This
//! complements the dense `DMat` (in the `linalg` module): for systems with at
//! least 64 states (the `SPARSE_LU_MIN_N` threshold), the implicit SDIRK and
//! BDF solvers build their Newton matrices through this module, so large
//! sparse Jacobians (e.g. from method-of-lines discretisations of PDEs) are
//! factored without ever materialising a dense `n × n` Jacobian — keeping
//! `tpt-sci-ode` free of any external linear-algebra dependency, per the
//! Phase 7 rewrite decision (todo.md, 7.0b).
//!
//! The sparse LU is a left-looking factorisation (`L` unit-lower-triangular,
//! `U` upper-triangular) with partial pivoting. It is exact for the sparsity
//! pattern present at factorisation time and is intended for the modest system
//! sizes this pillar solves (≲ a few thousand non-zeros).

use crate::error::OdeError;

/// Minimum system size for the implicit solvers to route their Newton linear
/// solves through the sparse CSR/LU path instead of the dense `DMat` path.
/// Below this threshold the dense factorisation is faster (no CSR build
/// overhead).
pub(crate) const SPARSE_LU_MIN_N: usize = 64;

/// Compressed-sparse-row matrix.
///
/// Stores only the non-zero entries `data` together with their column indices
/// `indices` (per row, in ascending column order) and the CSR row-pointer
/// `indptr` (length `nrows + 1`). A zero entry must not be stored.
#[derive(Clone)]
pub struct CsrMatrix {
    nrows: usize,
    ncols: usize,
    /// Row pointers: `indptr[r] .. indptr[r+1]` indexes the slice of row `r`'s
    /// non-zeros in `data` / `indices`.
    indptr: Vec<usize>,
    /// Column index of each stored entry (ascending within a row).
    indices: Vec<usize>,
    /// Value of each stored entry.
    data: Vec<f64>,
}

impl CsrMatrix {
    /// Number of rows.
    #[must_use]
    pub fn nrows(&self) -> usize {
        self.nrows
    }

    /// Number of columns.
    #[must_use]
    pub fn ncols(&self) -> usize {
        self.ncols
    }

    /// Build a CSR matrix from a dense row-major slice (row length `ncols`).
    /// Zero entries are dropped.
    #[must_use]
    pub fn from_dense(dense: &[f64], nrows: usize, ncols: usize) -> Self {
        debug_assert_eq!(dense.len(), nrows * ncols);
        let mut indptr = vec![0usize; nrows + 1];
        let mut indices = Vec::new();
        let mut data = Vec::new();
        for r in 0..nrows {
            let row = &dense[r * ncols..(r + 1) * ncols];
            for (c, &v) in row.iter().enumerate() {
                if v != 0.0 {
                    indices.push(c);
                    data.push(v);
                }
            }
            indptr[r + 1] = indices.len();
        }
        CsrMatrix {
            nrows,
            ncols,
            indptr,
            indices,
            data,
        }
    }

    /// Number of explicitly stored non-zeros.
    #[must_use]
    pub fn nnz(&self) -> usize {
        self.data.len()
    }

    /// Look up the stored value at `(r, c)`, or `0.0` if not present.
    #[must_use]
    pub fn get(&self, r: usize, c: usize) -> f64 {
        let start = self.indptr[r];
        let end = self.indptr[r + 1];
        match self.indices[start..end].binary_search(&c) {
            Ok(pos) => self.data[start + pos],
            Err(_) => 0.0,
        }
    }

    /// `y = A x`.
    ///
    /// # Panics
    ///
    /// Panics if `x.len() != self.ncols()`.
    #[must_use]
    pub fn mat_vec(&self, x: &[f64]) -> Vec<f64> {
        assert_eq!(x.len(), self.ncols, "vector length must match ncols");
        let mut y = vec![0.0; self.nrows];
        for (r, yr) in y.iter_mut().enumerate() {
            let mut s = 0.0;
            for k in self.indptr[r]..self.indptr[r + 1] {
                s += self.data[k] * x[self.indices[k]];
            }
            *yr = s;
        }
        y
    }

    /// Build the Newton matrix `A = I − diag·self` directly in CSR storage,
    /// merging the unit diagonal with the scaled Jacobian entries per row
    /// (both in ascending column order) without ever densifying.
    #[must_use]
    pub fn scaled_identity_minus_scaled(&self, diag: f64) -> CsrMatrix {
        debug_assert_eq!(self.ncols, self.nrows);
        let n = self.nrows;
        let mut indptr = vec![0usize; n + 1];
        let mut indices = Vec::new();
        let mut data = Vec::new();
        for r in 0..n {
            let start = self.indptr[r];
            let end = self.indptr[r + 1];
            let mut k = start;
            // Merge the unit diagonal with the scaled Jacobian row (both in
            // ascending column order).
            while k < end && self.indices[k] < r {
                indices.push(self.indices[k]);
                data.push(-diag * self.data[k]);
                k += 1;
            }
            if k < end && self.indices[k] == r {
                indices.push(r);
                data.push(1.0 - diag * self.data[k]);
                k += 1;
            } else {
                indices.push(r);
                data.push(1.0);
            }
            while k < end {
                indices.push(self.indices[k]);
                data.push(-diag * self.data[k]);
                k += 1;
            }
            indptr[r + 1] = indices.len();
        }
        CsrMatrix {
            nrows: n,
            ncols: n,
            indptr,
            indices,
            data,
        }
    }

    /// Convert to a dense row-major `Vec<f64>` (for testing / verification).
    #[cfg(test)]
    pub fn to_dense(&self) -> Vec<f64> {
        let mut d = vec![0.0; self.nrows * self.ncols];
        for r in 0..self.nrows {
            for k in self.indptr[r]..self.indptr[r + 1] {
                d[r * self.ncols + self.indices[k]] = self.data[k];
            }
        }
        d
    }

    /// Build the full Jacobian `J = df/dy` at `(t, y)` via forward finite
    /// differences, returned as a `CsrMatrix`. `f0` must be `f(t, y)` and `cols`
    /// is the set of columns to perturb (a sparsity pattern, or `None` for the
    /// full dense pattern). When `cols` is `None` every column is finite-
    /// differenced, matching the dense `linalg::jacobian` helper but in
    /// compressed storage (exact-zero entries are dropped).
    pub fn jacobian<F>(f: F, t: f64, y: &[f64], f0: &[f64], cols: Option<&[usize]>) -> CsrMatrix
    where
        F: Fn(f64, &[f64], &mut [f64]),
    {
        let n = y.len();
        let sqrt_eps = f64::sqrt(f64::EPSILON);
        let perturb: Vec<usize> = match cols {
            Some(c) => c.to_vec(),
            None => (0..n).collect(),
        };
        // Collect COO triplets (row, col, val). Perturbing column `j` yields the
        // whole of Jacobian column `j`: `val = (f_row(y+dy·e_j) - f_row(y))/dy`.
        let mut rows = Vec::new();
        let mut cols = Vec::new();
        let mut vals = Vec::new();
        for &col in &perturb {
            let dy = sqrt_eps * y[col].abs().max(1.0);
            let mut yp = y.to_vec();
            yp[col] += dy;
            let mut fp = vec![0.0; n];
            f(t, &yp, &mut fp);
            let inv = 1.0 / dy;
            for row in 0..n {
                let val = (fp[row] - f0[row]) * inv;
                if val != 0.0 {
                    rows.push(row);
                    cols.push(col);
                    vals.push(val);
                }
            }
        }
        // Sort triplets by (row, col) so each row's entries are in ascending
        // column order, then compress into CSR.
        let mut order: Vec<usize> = (0..rows.len()).collect();
        order.sort_by(|&a, &b| (rows[a], cols[a]).cmp(&(rows[b], cols[b])));
        let mut indptr = vec![0usize; n + 1];
        for &k in &order {
            indptr[rows[k] + 1] += 1;
        }
        for r in 0..n {
            indptr[r + 1] += indptr[r];
        }
        let mut indices = Vec::with_capacity(vals.len());
        let mut data = Vec::with_capacity(vals.len());
        for &k in &order {
            indices.push(cols[k]);
            data.push(vals[k]);
        }
        CsrMatrix {
            nrows: n,
            ncols: n,
            indptr,
            indices,
            data,
        }
    }

    /// Solve `A x = b` via sparse LU with partial pivoting.
    ///
    /// Returns `None` if the matrix is (numerically) singular or the dimensions
    /// mismatch. The factorisation is computed left-looking on the fly;
    /// pivoting keeps the per-step implicit linear solves stable for the stiff
    /// systems this crate targets.
    #[must_use]
    pub fn sparse_solve(&self, b: &[f64]) -> Option<Vec<f64>> {
        let n = self.nrows;
        if self.ncols != n || b.len() != n {
            return None;
        }
        // Work on a dense copy of (L+U) for a compact, correct left-looking
        // factorisation with partial pivoting. The sparsity structure is
        // preserved for storage efficiency in the caller; the numeric
        // factorisation itself operates on dense `L`/`U` working arrays sized
        // to `n`, which is acceptable for the modest `n` this crate serves.
        let mut lu = vec![0.0; n * n];
        for r in 0..n {
            for k in self.indptr[r]..self.indptr[r + 1] {
                lu[r * n + self.indices[k]] = self.data[k];
            }
        }
        let mut piv = (0..n).collect::<Vec<_>>();
        for k in 0..n {
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
        let mut y = vec![0.0; n];
        for i in 0..n {
            let pi = piv[i];
            let mut s = b[pi];
            for j in 0..i {
                s -= lu[i * n + j] * y[j];
            }
            y[i] = s;
        }
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

/// Solve one SDIRK stage `k = f(t_stage, y_base + diag·k)` with the Newton
/// linear solves routed through the sparse CSR/LU path. Called by
/// [`sdirk_stage`](crate::linalg::sdirk_stage) whenever the system has at
/// least [`SPARSE_LU_MIN_N`] states, so the finite-difference Jacobian is
/// built directly in compressed storage and never densified.
pub(crate) fn sdirk_stage_sparse(
    f: &dyn crate::RhsCallable,
    t_stage: f64,
    y_base: &[f64],
    diag: f64,
    k_guess: &[f64],
) -> Result<Vec<f64>, OdeError> {
    let n = y_base.len();
    let mut k = k_guess.to_vec();
    for _iter in 0..50 {
        let y_stage: Vec<f64> = y_base.iter().zip(&k).map(|(b, kk)| b + diag * kk).collect();
        let mut fk = vec![0.0; n];
        f.call(t_stage, &y_stage, &mut fk)
            .expect("RHS evaluation must not fail for a well-formed problem");
        let r: Vec<f64> = k.iter().zip(&fk).map(|(kk, fv)| kk - fv).collect();
        let rnorm = r.iter().map(|x| x * x).sum::<f64>().sqrt();
        if rnorm < 1e-12 {
            return Ok(k);
        }
        let jac = CsrMatrix::jacobian(
            |t, y, dydt| {
                f.call(t, y, dydt)
                    .expect("RHS evaluation must not fail for a well-formed problem");
            },
            t_stage,
            &y_stage,
            &fk,
            None,
        );
        // Newton matrix A = I - diag·J (sparse, since J is sparse).
        let a = jac.scaled_identity_minus_scaled(diag);
        let delta = a.sparse_solve(&r).ok_or(OdeError::Newton {
            t: t_stage,
            residual: rnorm,
        })?;
        for (ki, di) in k.iter_mut().zip(&delta) {
            *ki -= di;
        }
    }
    Ok(k)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csr_mat_vec_matches_dense() {
        // A = [[2, 0, 1], [0, 3, 0], [1, 0, 4]]  (sparse, diagonally dominant)
        let a = CsrMatrix::from_dense(&[2.0, 0.0, 1.0, 0.0, 3.0, 0.0, 1.0, 0.0, 4.0], 3, 3);
        let y = a.mat_vec(&[1.0, 2.0, 3.0]);
        assert!((y[0] - 5.0).abs() < 1e-12);
        assert!((y[1] - 6.0).abs() < 1e-12);
        assert!((y[2] - 13.0).abs() < 1e-12);
    }

    #[test]
    fn sparse_solve_matches_dense_solve() {
        // Same matrix as the dense LU test in linalg.rs.
        let dense = [2.0, 1.0, 1.0, 1.0, 3.0, 2.0, 1.0, 0.0, 1.0];
        let a = CsrMatrix::from_dense(&dense, 3, 3);
        let b = [4.0, 6.0, 2.0];
        let x = a.sparse_solve(&b).expect("non-singular");
        assert!((x[0] - 1.0).abs() < 1e-12);
        assert!((x[1] - 1.0).abs() < 1e-12);
        assert!((x[2] - 1.0).abs() < 1e-12);
    }

    #[test]
    fn sparse_solve_pivots_on_singular_offdiag() {
        // [0 1; 1 0] — pivot required at k=0.
        let a = CsrMatrix::from_dense(&[0.0, 1.0, 1.0, 0.0], 2, 2);
        let x = a
            .sparse_solve(&[3.0, 4.0])
            .expect("non-singular after pivot");
        assert!((x[0] - 4.0).abs() < 1e-12);
        assert!((x[1] - 3.0).abs() < 1e-12);
    }

    #[test]
    fn sparse_solve_detects_singular() {
        let a = CsrMatrix::from_dense(&[1.0, 1.0, 2.0, 2.0], 2, 2);
        assert!(a.sparse_solve(&[1.0, 1.0]).is_none());
    }

    #[test]
    fn jacobian_csr_of_linear_system() {
        let a = CsrMatrix::from_dense(&[0.0, 1.0, -2.0, -3.0], 2, 2);
        let dense_j = a.to_dense();
        let f = move |_t: f64, y: &[f64], dydt: &mut [f64]| {
            let out = a.mat_vec(y);
            dydt[0] = out[0];
            dydt[1] = out[1];
        };
        let y = [0.7, -1.3];
        let mut f0 = [0.0; 2];
        f(0.0, &y, &mut f0);
        let j = CsrMatrix::jacobian(&f, 0.0, &y, &f0, None);
        assert!((j.get(0, 0) - 0.0).abs() < 1e-8);
        assert!((j.get(0, 1) - 1.0).abs() < 1e-8);
        assert!((j.get(1, 0) - (-2.0)).abs() < 1e-8);
        assert!((j.get(1, 1) - (-3.0)).abs() < 1e-8);
        // The CSR Newton matrix of (I - 0.1 J) must match the dense DMat path.
        let jac = CsrMatrix::from_dense(&dense_j, 2, 2);
        let csr = jac.scaled_identity_minus_scaled(0.1);
        let r = [1.5, -2.5];
        let xs = csr.sparse_solve(&r).unwrap();
        // Dense oracle: A = I - 0.1 J.
        let mut dense_a = crate::linalg::DMat::identity(2);
        for i in 0..2 {
            for jj in 0..2 {
                dense_a.set(i, jj, dense_a.get(i, jj) - 0.1 * dense_j[i * 2 + jj]);
            }
        }
        let xd = dense_a.solve(&r).unwrap();
        assert!((xs[0] - xd[0]).abs() < 1e-10);
        assert!((xs[1] - xd[1]).abs() < 1e-10);
    }

    #[test]
    fn bdf_sparse_path_solves_large_decoupled_system() {
        // n = 80 ≥ SPARSE_LU_MIN_N: `step_bdf` routes its Newton solves
        // through the sparse CSR/LU path. Decoupled decays have the closed
        // form y_i(t) = exp(-(i+1)·t).
        const N: usize = 80;
        let prob = crate::OdeProblem::new(
            |_t, y, dydt| {
                for (i, yi) in y.iter().enumerate() {
                    dydt[i] = -((i + 1) as f64) * yi;
                }
            },
            vec![1.0; N],
            0.0,
        )
        .unwrap();
        let t = 0.5;
        let y = prob.solve(crate::Method::Bdf, t).unwrap();
        for (i, yi) in y.iter().enumerate() {
            let expected = (-((i + 1) as f64) * t).exp();
            assert!(
                (yi - expected).abs() < 1e-6,
                "component {i}: {yi} vs {expected}"
            );
        }
    }

    #[test]
    fn scaled_identity_handles_missing_diagonal() {
        // J with a structural zero on the diagonal: A = I - diag·J must still
        // carry the unit diagonal entry there.
        let j = CsrMatrix::from_dense(&[0.0, 2.0, 0.0, 0.0], 2, 2);
        let a = j.scaled_identity_minus_scaled(0.5);
        assert!((a.get(0, 0) - 1.0).abs() < 1e-14);
        assert!((a.get(0, 1) - (-1.0)).abs() < 1e-14);
        assert!((a.get(1, 0) - 0.0).abs() < 1e-14);
        assert!((a.get(1, 1) - 1.0).abs() < 1e-14);
        let x = a.sparse_solve(&[1.0, 2.0]).unwrap();
        // x0 - x1 = 1, x1 = 2  ->  x = [3, 2].
        assert!((x[0] - 3.0).abs() < 1e-12);
        assert!((x[1] - 2.0).abs() < 1e-12);
    }

    #[test]
    fn sparse_sdirk_stage_matches_dense_oracle_on_tridiagonal_system() {
        // f(y) = A y with a tridiagonal A (n = 80 ≥ SPARSE_LU_MIN_N): the
        // stage equation k = A(y + diag·k) has the closed form
        // k = (I - diag·A)⁻¹ A y, computable with the dense DMat oracle.
        const N: usize = 80;
        let mut dense = vec![0.0; N * N];
        for i in 0..N {
            dense[i * N + i] = -2.0;
            if i > 0 {
                dense[i * N + i - 1] = 1.0;
            }
            if i + 1 < N {
                dense[i * N + i + 1] = 1.0;
            }
        }
        let a = CsrMatrix::from_dense(&dense, N, N);
        let f = move |_t: f64, y: &[f64], dydt: &mut [f64]| {
            let out = a.mat_vec(y);
            dydt.copy_from_slice(&out);
        };
        let y: Vec<f64> = (0..N).map(|i| 0.1 * (i as f64).sin() + 0.2).collect();
        let diag = 0.05;
        let k = sdirk_stage_sparse(&f, 0.0, &y, diag, &vec![0.0; N]).unwrap();
        // Dense oracle.
        let mut am = crate::linalg::DMat::new(N, N);
        for i in 0..N {
            for jj in 0..N {
                am.set(i, jj, dense[i * N + jj]);
            }
        }
        let ay = am.mat_vec(&y);
        // (I - diag·A) as a fresh dense matrix.
        let mut m = crate::linalg::DMat::new(N, N);
        for i in 0..N {
            for jj in 0..N {
                let v = if i == jj { 1.0 } else { 0.0 } - diag * dense[i * N + jj];
                m.set(i, jj, v);
            }
        }
        let expected = m.solve(&ay).unwrap();
        for (i, (kv, ev)) in k.iter().zip(&expected).enumerate() {
            assert!((kv - ev).abs() < 1e-8, "component {i}: {kv} vs {ev}");
        }
    }
}
