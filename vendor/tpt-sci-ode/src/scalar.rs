//! Generic (`f32`/`f64`) dense linear algebra and an explicit integrator.
//!
//! The public [`OdeProblem`](crate::OdeProblem) API is `f64`-only (the v1
//! instantiation, per todo.md Phase 9a — `f32` is a stretch goal). This module
//! exercises the *engine core* generically over
//! [`Scalar`] so the foundational arithmetic is proven to work
//! at both `f32` and `f64`. It is the substrate a fully-generic
//! [`OdeProblem`](crate::OdeProblem) would lift onto, and it is unit-tested at
//! both precisions
//! (todo.md Phase 9a: "instantiate the existing Scalar-generic core at `f32`
//! in addition to `f64`; test both").

use tpt_math_numeric::Scalar;

/// Generic row-major dense matrix over a [`Scalar`] type.
pub struct DMat<T> {
    pub nrows: usize,
    pub ncols: usize,
    pub data: Vec<T>,
}

impl<T: Scalar> DMat<T> {
    /// Build a zero `nrows × ncols` matrix.
    pub fn new(nrows: usize, ncols: usize) -> Self {
        DMat {
            nrows,
            ncols,
            data: vec![T::zero(); nrows * ncols],
        }
    }

    /// Element access (row-major).
    #[inline]
    pub fn get(&self, i: usize, j: usize) -> T {
        self.data[i * self.ncols + j]
    }

    /// Element mutation (row-major).
    #[inline]
    pub fn set(&mut self, i: usize, j: usize, v: T) {
        self.data[i * self.ncols + j] = v;
    }

    /// `self += gamma * I`.
    pub fn add_scaled_identity(&mut self, gamma: T) {
        debug_assert_eq!(self.nrows, self.ncols);
        let n = self.nrows;
        for i in 0..n {
            let idx = i * n + i;
            self.data[idx] = self.data[idx] + gamma;
        }
    }

    /// `y = A x`.
    #[allow(clippy::needless_range_loop)]
    pub fn mat_vec(&self, x: &[T]) -> Vec<T> {
        let mut y = vec![T::zero(); self.nrows];
        for i in 0..self.nrows {
            let row = &self.data[i * self.ncols..(i + 1) * self.ncols];
            let mut s = T::zero();
            for j in 0..self.ncols {
                s = s + row[j] * x[j];
            }
            y[i] = s;
        }
        y
    }

    /// Solve `A x = b` for square `A` via LU with partial pivoting. Returns
    /// `None` if the matrix is (numerically) singular.
    #[allow(clippy::missing_panics_doc)]
    pub fn solve(&self, b: &[T]) -> Option<Vec<T>> {
        debug_assert_eq!(self.nrows, self.ncols);
        let n = self.nrows;
        let mut lu = self.data.clone();
        let mut piv = (0..n).collect::<Vec<_>>();
        let eps = T::from(1e-14f64).unwrap();
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
            if max < eps {
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
                    lu[i * n + j] = lu[i * n + j] - fac * lu[k * n + j];
                }
            }
        }
        let mut y = vec![T::zero(); n];
        for i in 0..n {
            let pi = piv[i];
            let mut s = b[pi];
            for j in 0..i {
                s = s - lu[i * n + j] * y[j];
            }
            y[i] = s;
        }
        let mut x = vec![T::zero(); n];
        for i in (0..n).rev() {
            let mut s = y[i];
            for j in (i + 1)..n {
                s = s - lu[i * n + j] * x[j];
            }
            x[i] = s / lu[i * n + i];
        }
        Some(x)
    }
}

/// Classical fixed-step Runge–Kutta 4 for `dy/dt = f(t, y)`, generic over
/// [`Scalar`]. Returns the trajectory at the requested (evenly spaced)
/// evaluation times. Demonstrates the core integrator at arbitrary precision.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::missing_panics_doc)]
pub fn rk4_scalar<T, F>(mut f: F, y0: &[T], t0: T, t_final: T, n_steps: usize) -> Vec<Vec<T>>
where
    T: Scalar,
    F: FnMut(T, &[T], &mut [T]),
{
    let n = y0.len();
    let mut y = y0.to_vec();
    let span = t_final - t0;
    let h = span / T::from(n_steps).unwrap();
    let mut out = vec![y0.to_vec()];
    let mut t = t0;
    let two = T::from(2).unwrap();
    let six = T::from(6).unwrap();
    for _ in 0..n_steps {
        let mut k1 = vec![T::zero(); n];
        let mut k2 = vec![T::zero(); n];
        let mut k3 = vec![T::zero(); n];
        let mut k4 = vec![T::zero(); n];
        f(t, &y, &mut k1);
        let y2: Vec<T> = y
            .iter()
            .zip(&k1)
            .map(|(yi, k)| *yi + h / two * *k)
            .collect();
        f(t + h / two, &y2, &mut k2);
        let y3: Vec<T> = y
            .iter()
            .zip(&k2)
            .map(|(yi, k)| *yi + h / two * *k)
            .collect();
        f(t + h / two, &y3, &mut k3);
        let y4: Vec<T> = y.iter().zip(&k3).map(|(yi, k)| *yi + h * *k).collect();
        f(t + h, &y4, &mut k4);
        for i in 0..n {
            y[i] = y[i] + (h / six) * (k1[i] + two * k2[i] + two * k3[i] + k4[i]);
        }
        t = t + h;
        out.push(y.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lu_solves_known_3x3_f64() {
        let a = DMat::<f64>::new(3, 3);
        let mut a = a;
        // [2 1 1; 1 3 2; 1 0 1] · x = [4, 6, 2] -> x = [1, 1, 1]
        a.set(0, 0, 2.0);
        a.set(0, 1, 1.0);
        a.set(0, 2, 1.0);
        a.set(1, 0, 1.0);
        a.set(1, 1, 3.0);
        a.set(1, 2, 2.0);
        a.set(2, 0, 1.0);
        a.set(2, 1, 0.0);
        a.set(2, 2, 1.0);
        let x = a.solve(&[4.0, 6.0, 2.0]).unwrap();
        assert!((x[0] - 1.0).abs() < 1e-12);
        assert!((x[1] - 1.0).abs() < 1e-12);
        assert!((x[2] - 1.0).abs() < 1e-12);
    }

    #[test]
    fn rk4_exponential_decay_f64() {
        let traj = rk4_scalar(
            |_t, y, dydt| dydt[0] = -y[0],
            &[1.0_f64],
            0.0_f64,
            1.0_f64,
            200,
        );
        let y = traj.last().unwrap()[0];
        assert!((y - (-1.0_f64).exp()).abs() < 1e-4);
    }

    #[test]
    fn rk4_exponential_decay_f32() {
        let traj = rk4_scalar(
            |_t, y, dydt| dydt[0] = -y[0],
            &[1.0_f32],
            0.0_f32,
            1.0_f32,
            200,
        );
        let y = traj.last().unwrap()[0];
        assert!((y - (-1.0_f32).exp()).abs() < 1e-3);
    }

    #[test]
    fn generic_dmat_mat_vec_f32_f64() {
        let check = |y0: f64| {
            let mut a = DMat::<f64>::new(2, 2);
            a.set(0, 0, y0);
            a.set(0, 1, 2.0 * y0);
            a.set(1, 0, 3.0 * y0);
            a.set(1, 1, 4.0 * y0);
            let out = a.mat_vec(&[y0, y0]);
            assert!((out[0] - 3.0 * y0).abs() < 1e-9);
            assert!((out[1] - 7.0 * y0).abs() < 1e-9);
        };
        check(1.0_f64);
        let check_f32 = |y0: f32| {
            let mut a = DMat::<f32>::new(2, 2);
            a.set(0, 0, y0);
            a.set(0, 1, 2.0 * y0);
            a.set(1, 0, 3.0 * y0);
            a.set(1, 1, 4.0 * y0);
            let out = a.mat_vec(&[y0, y0]);
            assert!((out[0] - 3.0 * y0).abs() < 1e-4);
            assert!((out[1] - 7.0 * y0).abs() < 1e-4);
        };
        check_f32(1.0_f32);
    }
}
