//! Rigid-body and simple continuum simulation.
//!
//! This first slice covers one rigid body: a 1-DOF mass-spring-damper,
//! `m x'' + c x' + k x = 0`. It reads its parameters from a [`Twin`] and
//! integrates with the vendored `tpt-sci-ode` solver. The result is checked
//! against closed-form solutions in the tests.
//!
//! Required twin parameters: `mass_kg` (> 0), `stiffness_n_per_m` (≥ 0),
//! `damping_n_s_per_m` (≥ 0). Initial conditions come from the scenario:
//! `position_m` and `velocity_m_s`.
//!
//! A scenario may carry a [`twin_core::PidController`] on position. The
//! controller adds one state (the integral of the error) to the same solver.

pub mod heat;

use thiserror::Error;
use tpt_sci_ode::{Method, OdeError, OdeProblemBuilder};
use twin_core::{Scenario, Twin};

pub const MASS: &str = "mass_kg";
pub const STIFFNESS: &str = "stiffness_n_per_m";
pub const DAMPING: &str = "damping_n_s_per_m";
pub const POSITION0: &str = "position_m";
pub const VELOCITY0: &str = "velocity_m_s";

#[derive(Debug, Error, PartialEq)]
pub enum SimError {
    #[error("twin is missing parameter `{0}`")]
    MissingParameter(&'static str),
    #[error("parameter `{name}` is invalid: {reason}")]
    InvalidParameter {
        name: &'static str,
        reason: &'static str,
    },
    #[error("scenario is missing initial condition `{0}`")]
    MissingInitialCondition(&'static str),
    #[error("sample times must be strictly increasing, within [0, duration]")]
    InvalidTimes,
    #[error("solver failed: {0}")]
    Solver(String),
}

impl From<OdeError> for SimError {
    fn from(e: OdeError) -> Self {
        SimError::Solver(format!("{e:?}"))
    }
}

/// Physical constants of the mass-spring-damper, read from a twin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MassSpringDamper {
    pub mass: f64,
    pub stiffness: f64,
    pub damping: f64,
}

impl MassSpringDamper {
    pub fn from_twin(twin: &Twin) -> Result<Self, SimError> {
        let get = |name: &'static str| {
            twin.parameters
                .get(name)
                .copied()
                .ok_or(SimError::MissingParameter(name))
        };
        let mass = get(MASS)?;
        let stiffness = get(STIFFNESS)?;
        let damping = get(DAMPING)?;
        if mass.is_nan() || mass <= 0.0 {
            return Err(SimError::InvalidParameter {
                name: MASS,
                reason: "must be positive",
            });
        }
        if stiffness < 0.0 {
            return Err(SimError::InvalidParameter {
                name: STIFFNESS,
                reason: "must not be negative",
            });
        }
        if damping < 0.0 {
            return Err(SimError::InvalidParameter {
                name: DAMPING,
                reason: "must not be negative",
            });
        }
        Ok(Self {
            mass,
            stiffness,
            damping,
        })
    }

    /// Natural angular frequency, `sqrt(k / m)`.
    pub fn natural_frequency(&self) -> f64 {
        (self.stiffness / self.mass).sqrt()
    }

    /// Damping ratio, `c / (2 sqrt(k m))`. Zero when there is no stiffness.
    pub fn damping_ratio(&self) -> f64 {
        if self.stiffness == 0.0 {
            return f64::INFINITY;
        }
        self.damping / (2.0 * (self.stiffness * self.mass).sqrt())
    }
}

/// A sampled trajectory. `states[i]` is `[position, velocity]` at `times[i]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Trace {
    pub times: Vec<f64>,
    pub states: Vec<[f64; 2]>,
}

/// Simulate a scenario of `twin` and sample it at `times`.
///
/// Scenario duration bounds the run: every sample time must be within
/// `[0, duration_s]`. Output is deterministic for the same inputs.
pub fn simulate(twin: &Twin, scenario: &Scenario, times: &[f64]) -> Result<Trace, SimError> {
    let body = MassSpringDamper::from_twin(twin)?;
    let x0 = *scenario
        .initial_conditions
        .get(POSITION0)
        .ok_or(SimError::MissingInitialCondition(POSITION0))?;
    let v0 = *scenario
        .initial_conditions
        .get(VELOCITY0)
        .ok_or(SimError::MissingInitialCondition(VELOCITY0))?;

    let MassSpringDamper {
        mass,
        stiffness,
        damping,
    } = body;
    // With a controller the state also holds the integral of the position
    // error, so the ODE has three states; the output still has two.
    let controller = scenario.controller;
    let rhs = move |_t: f64, y: &[f64], dydt: &mut [f64]| {
        let force = match controller {
            Some(c) => {
                let error = c.setpoint_m - y[0];
                dydt[2] = error;
                c.kp * error + c.ki * y[2] - c.kd * y[1]
            }
            None => 0.0,
        };
        dydt[0] = y[1];
        dydt[1] = (force - damping * y[1] - stiffness * y[0]) / mass;
    };
    let y0 = if controller.is_some() {
        vec![x0, v0, 0.0]
    } else {
        vec![x0, v0]
    };

    validate_times(times, scenario.duration_s)?;

    // The solver needs strictly-later evaluation times, so t = 0 is the
    // initial state and is not passed to it.
    let later: Vec<f64> = times.iter().copied().filter(|&t| t > 0.0).collect();
    // The solver's default tolerance (1e-6) lets error grow over several
    // oscillations. Tighter tolerances keep the comparison with closed forms
    // meaningful.
    let problem = OdeProblemBuilder::new(rhs, y0, 0.0)
        .rtol(1e-10)
        .atol(1e-10)
        .build()?;
    let mut solved = if later.is_empty() {
        Vec::new().into_iter()
    } else {
        problem.solve_dense(Method::Tsit45, &later)?.into_iter()
    };

    let states = times
        .iter()
        .map(|&t| {
            if t == 0.0 {
                [x0, v0]
            } else {
                let s = solved.next().expect("one solved state per later time");
                [s[0], s[1]]
            }
        })
        .collect();
    Ok(Trace {
        times: times.to_vec(),
        states,
    })
}

pub(crate) fn validate_times(times: &[f64], duration_s: f64) -> Result<(), SimError> {
    let mut prev = f64::NEG_INFINITY;
    for &t in times {
        if !t.is_finite() || t < 0.0 || t > duration_s || t <= prev {
            return Err(SimError::InvalidTimes);
        }
        prev = t;
    }
    Ok(())
}

/// Closed-form position of the mass-spring-damper, for checking the solver.
///
/// Handles under-, critically and over-damped cases.
pub fn analytic_position(body: &MassSpringDamper, x0: f64, v0: f64, t: f64) -> f64 {
    let wn = body.natural_frequency();
    let zeta = body.damping_ratio();
    if zeta < 1.0 {
        let wd = wn * (1.0 - zeta * zeta).sqrt();
        let a = x0;
        let b = (v0 + zeta * wn * x0) / wd;
        (-zeta * wn * t).exp() * (a * (wd * t).cos() + b * (wd * t).sin())
    } else if zeta == 1.0 {
        (x0 + (v0 + wn * x0) * t) * (-wn * t).exp()
    } else {
        let s = wn * (zeta * zeta - 1.0).sqrt();
        let r1 = -zeta * wn + s;
        let r2 = -zeta * wn - s;
        let c2 = (v0 - r1 * x0) / (r2 - r1);
        let c1 = x0 - c2;
        c1 * (r1 * t).exp() + c2 * (r2 * t).exp()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use twin_core::PidController;

    fn twin_with(mass: f64, stiffness: f64, damping: f64) -> Twin {
        let mut twin = Twin::new("spring").unwrap();
        twin.set_parameter(MASS, mass).unwrap();
        twin.set_parameter(STIFFNESS, stiffness).unwrap();
        twin.set_parameter(DAMPING, damping).unwrap();
        twin
    }

    fn scenario(x0: f64, v0: f64, duration: f64) -> Scenario {
        Scenario {
            name: "test".into(),
            duration_s: duration,
            initial_conditions: BTreeMap::from([
                (POSITION0.to_string(), x0),
                (VELOCITY0.to_string(), v0),
            ]),
            controller: None,
        }
    }

    fn sample_times(duration: f64, n: usize) -> Vec<f64> {
        (0..=n).map(|i| duration * i as f64 / n as f64).collect()
    }

    /// Max absolute error of the simulated position against the closed form.
    fn max_error(twin: &Twin, sc: &Scenario, duration: f64) -> f64 {
        let times = sample_times(duration, 50);
        let trace = simulate(twin, sc, &times).unwrap();
        let body = MassSpringDamper::from_twin(twin).unwrap();
        let x0 = sc.initial_conditions[POSITION0];
        let v0 = sc.initial_conditions[VELOCITY0];
        times
            .iter()
            .zip(&trace.states)
            .map(|(&t, s)| (s[0] - analytic_position(&body, x0, v0, t)).abs())
            .fold(0.0, f64::max)
    }

    #[test]
    fn undamped_matches_closed_form() {
        // omega = sqrt(4) = 2 rad/s, so period ~ 3.14 s.
        let twin = twin_with(1.0, 4.0, 0.0);
        let sc = scenario(1.0, 0.0, 10.0);
        assert!(max_error(&twin, &sc, 10.0) < 1e-6);
    }

    #[test]
    fn underdamped_matches_closed_form() {
        let twin = twin_with(1.0, 4.0, 0.4);
        let sc = scenario(1.0, 0.5, 10.0);
        assert!(max_error(&twin, &sc, 10.0) < 1e-6);
    }

    #[test]
    fn overdamped_matches_closed_form() {
        let twin = twin_with(1.0, 4.0, 6.0);
        let sc = scenario(1.0, 0.0, 10.0);
        assert!(max_error(&twin, &sc, 10.0) < 1e-6);
    }

    #[test]
    fn critically_damped_matches_closed_form() {
        // zeta = c / (2 sqrt(k m)) = 1 => c = 2 * sqrt(4) = 4
        let twin = twin_with(1.0, 4.0, 4.0);
        let body = MassSpringDamper::from_twin(&twin).unwrap();
        assert!((body.damping_ratio() - 1.0).abs() < 1e-12);
        let sc = scenario(1.0, 0.0, 10.0);
        assert!(max_error(&twin, &sc, 10.0) < 1e-6);
    }

    #[test]
    fn same_inputs_give_identical_output() {
        let twin = twin_with(2.0, 3.0, 0.2);
        let sc = scenario(0.3, -0.1, 5.0);
        let times = sample_times(5.0, 20);
        assert_eq!(
            simulate(&twin, &sc, &times).unwrap(),
            simulate(&twin, &sc, &times).unwrap()
        );
    }

    #[test]
    fn unsorted_or_out_of_range_times_are_rejected() {
        let twin = twin_with(1.0, 1.0, 0.0);
        let sc = scenario(1.0, 0.0, 2.0);
        assert_eq!(
            simulate(&twin, &sc, &[1.0, 0.5]),
            Err(SimError::InvalidTimes)
        );
        assert_eq!(
            simulate(&twin, &sc, &[0.0, 3.0]),
            Err(SimError::InvalidTimes)
        );
        assert_eq!(
            simulate(&twin, &sc, &[-0.1, 1.0]),
            Err(SimError::InvalidTimes)
        );
    }

    #[test]
    fn missing_parameter_is_reported() {
        let mut twin = Twin::new("no damping").unwrap();
        twin.set_parameter(MASS, 1.0).unwrap();
        twin.set_parameter(STIFFNESS, 1.0).unwrap();
        let sc = scenario(1.0, 0.0, 1.0);
        assert_eq!(
            simulate(&twin, &sc, &[0.0, 1.0]),
            Err(SimError::MissingParameter(DAMPING))
        );
    }

    #[test]
    fn non_positive_mass_is_rejected() {
        let twin = twin_with(0.0, 1.0, 0.0);
        assert!(matches!(
            MassSpringDamper::from_twin(&twin),
            Err(SimError::InvalidParameter { name: MASS, .. })
        ));
    }

    #[test]
    fn missing_initial_condition_is_reported() {
        let twin = twin_with(1.0, 1.0, 0.0);
        let mut sc = scenario(1.0, 0.0, 1.0);
        sc.initial_conditions.remove(VELOCITY0);
        assert_eq!(
            simulate(&twin, &sc, &[0.0, 1.0]),
            Err(SimError::MissingInitialCondition(VELOCITY0))
        );
    }

    #[test]
    fn zero_gain_controller_matches_open_loop() {
        let twin = twin_with(1.0, 4.0, 0.4);
        let open = scenario(1.0, 0.5, 10.0);
        let mut closed = open.clone();
        closed.controller = Some(PidController {
            setpoint_m: 0.0,
            kp: 0.0,
            ki: 0.0,
            kd: 0.0,
        });
        let times = sample_times(10.0, 50);
        let a = simulate(&twin, &open, &times).unwrap();
        let b = simulate(&twin, &closed, &times).unwrap();
        for (x, y) in a.states.iter().zip(&b.states) {
            assert!((x[0] - y[0]).abs() < 1e-9 && (x[1] - y[1]).abs() < 1e-9);
        }
    }

    #[test]
    fn pid_drives_position_to_setpoint() {
        // Free mass (k = c = 0). Gains place the closed-loop poles at -1 (triple):
        // s^3 + kd s^2 + kp s + ki = (s + 1)^3.
        let twin = twin_with(1.0, 0.0, 0.0);
        let mut sc = scenario(0.0, 0.0, 40.0);
        sc.controller = Some(PidController {
            setpoint_m: 1.0,
            kp: 3.0,
            ki: 1.0,
            kd: 3.0,
        });
        let trace = simulate(&twin, &sc, &[40.0]).unwrap();
        assert!((trace.states[0][0] - 1.0).abs() < 1e-6);
        assert!(trace.states[0][1].abs() < 1e-6);
    }

    #[test]
    fn output_keeps_position_and_velocity_only() {
        let twin = twin_with(1.0, 1.0, 0.0);
        let mut sc = scenario(0.0, 0.0, 1.0);
        sc.controller = Some(PidController {
            setpoint_m: 0.5,
            kp: 1.0,
            ki: 0.0,
            kd: 0.0,
        });
        let trace = simulate(&twin, &sc, &[0.0, 1.0]).unwrap();
        assert!(trace.states.iter().all(|s| s.len() == 2));
    }
}
