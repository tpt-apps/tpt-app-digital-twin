//! Free rigid body: one solid, moving under constant gravity.
//!
//! State: the centroid's position and velocity (world frame), the orientation
//! as a unit quaternion `[w, x, y, z]` (body to world), and the angular velocity
//! in the body frame. The equations are
//!
//! ```text
//! p' = v,   v' = g,   q' = ½ q ⊗ (0, ω),   I ω' = -ω × (I ω)
//! ```
//!
//! with the full inertia tensor `I` about the centroid, in body axes. The
//! integrator is the same `tpt-sci-ode` solver as the other simulations.
//! Orientation is renormalised in the returned states; the solver itself does
//! not enforce unit length.

use tpt_sci_ode::{Method, OdeProblemBuilder};

use crate::{SimError, validate_times};

/// A rigid body's constant properties, about its centroid in body axes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RigidBody {
    pub mass: f64,
    /// Inertia tensor about the centroid, in body axes. Symmetric and positive
    /// definite.
    pub inertia: [[f64; 3]; 3],
}

/// The state of a body at one time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BodyState {
    pub position: [f64; 3],
    pub velocity: [f64; 3],
    pub orientation: [f64; 4],
    /// Angular velocity in body axes, in rad/s.
    pub angular_velocity: [f64; 3],
}

/// Integrate `body` from `initial` under constant `gravity` (m/s²) and sample
/// the state at `times`, which must lie within `[0, duration]`.
pub fn simulate_body(
    body: &RigidBody,
    initial: &BodyState,
    gravity: [f64; 3],
    duration: f64,
    times: &[f64],
) -> Result<Vec<BodyState>, SimError> {
    if !(body.mass.is_finite() && body.mass > 0.0) {
        return Err(SimError::InvalidParameter {
            name: "mass",
            reason: "must be positive and finite",
        });
    }
    let inertia_inverse = invert(&body.inertia).ok_or(SimError::InvalidParameter {
        name: "inertia",
        reason: "must be invertible",
    })?;
    validate_times(times, duration)?;

    let inertia = body.inertia;
    let rhs = move |_t: f64, y: &[f64], dydt: &mut [f64]| {
        let q = [y[6], y[7], y[8], y[9]];
        let omega = [y[10], y[11], y[12]];
        dydt[0..3].copy_from_slice(&y[3..6]);
        dydt[3..6].copy_from_slice(&gravity);
        let dq = quat_mul(q, [0.0, omega[0], omega[1], omega[2]]);
        for k in 0..4 {
            dydt[6 + k] = 0.5 * dq[k];
        }
        let i_omega = mat_vec(&inertia, omega);
        let gyroscopic = cross(omega, i_omega);
        let domega = mat_vec(
            &inertia_inverse,
            [-gyroscopic[0], -gyroscopic[1], -gyroscopic[2]],
        );
        dydt[10..13].copy_from_slice(&domega);
    };

    let y0 = [
        initial.position[0],
        initial.position[1],
        initial.position[2],
        initial.velocity[0],
        initial.velocity[1],
        initial.velocity[2],
        initial.orientation[0],
        initial.orientation[1],
        initial.orientation[2],
        initial.orientation[3],
        initial.angular_velocity[0],
        initial.angular_velocity[1],
        initial.angular_velocity[2],
    ];

    // The solver needs strictly-later evaluation times; t = 0 is the initial
    // state and is not passed to it.
    let later: Vec<f64> = times.iter().copied().filter(|&t| t > 0.0).collect();
    let problem = OdeProblemBuilder::new(rhs, y0.to_vec(), 0.0)
        .rtol(1e-10)
        .atol(1e-10)
        .build()?;
    let mut solved = if later.is_empty() {
        Vec::new().into_iter()
    } else {
        problem.solve_dense(Method::Tsit45, &later)?.into_iter()
    };

    Ok(times
        .iter()
        .map(|&t| {
            let y = if t == 0.0 {
                y0.to_vec()
            } else {
                solved.next().expect("one solved state per later time")
            };
            BodyState {
                position: [y[0], y[1], y[2]],
                velocity: [y[3], y[4], y[5]],
                orientation: normalise([y[6], y[7], y[8], y[9]]),
                angular_velocity: [y[10], y[11], y[12]],
            }
        })
        .collect())
}

/// Hamilton product `a ⊗ b` for quaternions `[w, x, y, z]`.
fn quat_mul(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [
        a[0] * b[0] - a[1] * b[1] - a[2] * b[2] - a[3] * b[3],
        a[0] * b[1] + a[1] * b[0] + a[2] * b[3] - a[3] * b[2],
        a[0] * b[2] - a[1] * b[3] + a[2] * b[0] + a[3] * b[1],
        a[0] * b[3] + a[1] * b[2] - a[2] * b[1] + a[3] * b[0],
    ]
}

fn normalise(q: [f64; 4]) -> [f64; 4] {
    let n = q.iter().map(|x| x * x).sum::<f64>().sqrt();
    if n == 0.0 {
        return [1.0, 0.0, 0.0, 0.0];
    }
    q.map(|x| x / n)
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn mat_vec(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// Inverse of a 3x3 matrix by cofactors, or `None` if it is singular.
fn invert(m: &[[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let c00 = m[1][1] * m[2][2] - m[1][2] * m[2][1];
    let c01 = m[1][2] * m[2][0] - m[1][0] * m[2][2];
    let c02 = m[1][0] * m[2][1] - m[1][1] * m[2][0];
    let det = m[0][0] * c00 + m[0][1] * c01 + m[0][2] * c02;
    if !(det.is_finite() && det.abs() > 1e-300) {
        return None;
    }
    let c10 = m[0][2] * m[2][1] - m[0][1] * m[2][2];
    let c11 = m[0][0] * m[2][2] - m[0][2] * m[2][0];
    let c12 = m[0][1] * m[2][0] - m[0][0] * m[2][1];
    let c20 = m[0][1] * m[1][2] - m[0][2] * m[1][1];
    let c21 = m[0][2] * m[1][0] - m[0][0] * m[1][2];
    let c22 = m[0][0] * m[1][1] - m[0][1] * m[1][0];
    // The inverse is the transposed cofactor matrix over the determinant.
    let inv = [[c00, c10, c20], [c01, c11, c21], [c02, c12, c22]];
    Some(inv.map(|row| row.map(|x| x / det)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at_rest() -> BodyState {
        BodyState {
            position: [0.0; 3],
            velocity: [0.0; 3],
            orientation: [1.0, 0.0, 0.0, 0.0],
            angular_velocity: [0.0; 3],
        }
    }

    fn body(ixx: f64, iyy: f64, izz: f64) -> RigidBody {
        RigidBody {
            mass: 2.0,
            inertia: [[ixx, 0.0, 0.0], [0.0, iyy, 0.0], [0.0, 0.0, izz]],
        }
    }

    #[test]
    fn free_fall_matches_the_closed_form() {
        let g = [0.0, 0.0, -9.81];
        let start = BodyState {
            position: [1.0, 0.0, 10.0],
            velocity: [0.5, 0.0, 2.0],
            ..at_rest()
        };
        let times = [0.5, 1.0, 2.0];
        let states = simulate_body(&body(1.0, 2.0, 3.0), &start, g, 2.0, &times).unwrap();
        for (t, s) in times.iter().zip(&states) {
            assert!((s.position[0] - (1.0 + 0.5 * t)).abs() < 1e-8);
            assert!((s.position[2] - (10.0 + 2.0 * t - 0.5 * 9.81 * t * t)).abs() < 1e-8);
            assert!((s.velocity[2] - (2.0 - 9.81 * t)).abs() < 1e-8);
        }
    }

    #[test]
    fn spin_about_a_principal_axis_is_steady() {
        // No torque: ω along a principal axis stays put, and the orientation
        // turns about that axis only, by ω t.
        let spin = BodyState {
            angular_velocity: [0.0, 0.0, 3.0],
            ..at_rest()
        };
        let states = simulate_body(&body(1.0, 2.0, 3.0), &spin, [0.0; 3], 4.0, &[4.0]).unwrap();
        let s = states[0];
        assert!((s.angular_velocity[2] - 3.0).abs() < 1e-8);
        assert!(s.angular_velocity[0].abs() < 1e-8 && s.angular_velocity[1].abs() < 1e-8);
        let angle: f64 = 3.0 * 4.0 / 2.0; // the quaternion carries half the angle
        assert!((s.orientation[0] - angle.cos()).abs() < 1e-7);
        assert!((s.orientation[3] - angle.sin()).abs() < 1e-7);
    }

    #[test]
    fn torque_free_tumbling_conserves_angular_momentum_magnitude() {
        // An asymmetric body tumbling freely: |I ω| is constant.
        let tumble = BodyState {
            angular_velocity: [0.3, 1.0, 0.2],
            ..at_rest()
        };
        let b = body(1.0, 2.0, 3.0);
        let times: Vec<f64> = (1..=20).map(|i| i as f64 * 0.5).collect();
        let states = simulate_body(&b, &tumble, [0.0; 3], 10.0, &times).unwrap();
        let mag = |w: [f64; 3]| {
            let l = mat_vec(&b.inertia, w);
            (l[0] * l[0] + l[1] * l[1] + l[2] * l[2]).sqrt()
        };
        let start = mag(tumble.angular_velocity);
        for s in &states {
            assert!((mag(s.angular_velocity) - start).abs() < 1e-7);
        }
    }

    #[test]
    fn orientation_stays_unit_length() {
        let tumble = BodyState {
            angular_velocity: [0.3, 1.0, 0.2],
            ..at_rest()
        };
        let times = [2.0, 5.0, 9.0];
        let states = simulate_body(&body(1.0, 2.0, 3.0), &tumble, [0.0; 3], 9.0, &times).unwrap();
        for s in &states {
            let n: f64 = s.orientation.iter().map(|x| x * x).sum();
            assert!((n - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn bad_properties_are_refused() {
        let zero_mass = RigidBody {
            mass: 0.0,
            ..body(1.0, 1.0, 1.0)
        };
        assert!(matches!(
            simulate_body(&zero_mass, &at_rest(), [0.0; 3], 1.0, &[1.0]),
            Err(SimError::InvalidParameter { name: "mass", .. })
        ));
        let singular = RigidBody {
            mass: 1.0,
            inertia: [[1.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
        };
        assert!(matches!(
            simulate_body(&singular, &at_rest(), [0.0; 3], 1.0, &[1.0]),
            Err(SimError::InvalidParameter {
                name: "inertia",
                ..
            })
        ));
    }
}
