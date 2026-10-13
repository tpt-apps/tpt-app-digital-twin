//! 1D heat diffusion in a rod, `u_t = alpha u_xx`, with both ends held at 0.
//!
//! Method of lines: `n` interior nodes on a uniform grid, second-order central
//! differences in space, and the `tpt-sci-ode` solver in time. The result is
//! checked against the closed-form decay of a sine mode in the tests.
//!
//! Required twin parameters: `thermal_diffusivity_m2_s` (> 0), `rod_length_m`
//! (> 0), `grid_nodes` (integer ≥ 2, interior nodes). The scenario gives the
//! initial profile as a half sine wave through `amplitude_k`.

use crate::SimError;
use tpt_sci_ode::{Method, OdeProblemBuilder};
use twin_core::{Scenario, Twin};

pub const DIFFUSIVITY: &str = "thermal_diffusivity_m2_s";
pub const LENGTH: &str = "rod_length_m";
pub const GRID_NODES: &str = "grid_nodes";
pub const AMPLITUDE0: &str = "amplitude_k";

/// Upper bound on interior nodes, to keep a bad parameter from exhausting memory.
const MAX_NODES: f64 = 100_000.0;

/// A sampled temperature field. `temps[i][j]` is the temperature at
/// `positions[j]` and `times[i]`. Positions are the interior nodes only.
#[derive(Debug, Clone, PartialEq)]
pub struct HeatTrace {
    pub times: Vec<f64>,
    pub positions: Vec<f64>,
    pub temps: Vec<Vec<f64>>,
}

/// Rod geometry and material, read from a twin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rod {
    pub diffusivity: f64,
    pub length: f64,
    pub nodes: usize,
}

impl Rod {
    pub fn from_twin(twin: &Twin) -> Result<Self, SimError> {
        let get = |name: &'static str| {
            twin.parameters
                .get(name)
                .copied()
                .ok_or(SimError::MissingParameter(name))
        };
        let diffusivity = get(DIFFUSIVITY)?;
        let length = get(LENGTH)?;
        let nodes = get(GRID_NODES)?;
        if !(diffusivity.is_finite() && diffusivity > 0.0) {
            return Err(SimError::InvalidParameter {
                name: DIFFUSIVITY,
                reason: "must be positive",
            });
        }
        if !(length.is_finite() && length > 0.0) {
            return Err(SimError::InvalidParameter {
                name: LENGTH,
                reason: "must be positive",
            });
        }
        if !(nodes.fract() == 0.0 && (2.0..=MAX_NODES).contains(&nodes)) {
            return Err(SimError::InvalidParameter {
                name: GRID_NODES,
                reason: "must be a whole number from 2 to 100000",
            });
        }
        Ok(Self {
            diffusivity,
            length,
            nodes: nodes as usize,
        })
    }

    /// Grid spacing.
    pub fn dx(&self) -> f64 {
        self.length / (self.nodes + 1) as f64
    }

    /// Interior node positions.
    pub fn positions(&self) -> Vec<f64> {
        let dx = self.dx();
        (1..=self.nodes).map(|i| i as f64 * dx).collect()
    }
}

/// Simulate heat diffusion for a scenario of `twin`, sampled at `times`.
pub fn simulate_heat(
    twin: &Twin,
    scenario: &Scenario,
    times: &[f64],
) -> Result<HeatTrace, SimError> {
    let rod = Rod::from_twin(twin)?;
    let amplitude = *scenario
        .initial_conditions
        .get(AMPLITUDE0)
        .ok_or(SimError::MissingInitialCondition(AMPLITUDE0))?;
    let positions = rod.positions();
    let initial: Vec<f64> = positions
        .iter()
        .map(|&x| amplitude * (std::f64::consts::PI * x / rod.length).sin())
        .collect();
    diffuse(&rod, initial, scenario.duration_s, times)
}

/// Diffuse an arbitrary initial profile (one value per interior node).
pub fn diffuse(
    rod: &Rod,
    initial: Vec<f64>,
    duration_s: f64,
    times: &[f64],
) -> Result<HeatTrace, SimError> {
    if initial.len() != rod.nodes {
        return Err(SimError::InvalidParameter {
            name: GRID_NODES,
            reason: "initial profile length must equal grid_nodes",
        });
    }
    crate::validate_times(times, duration_s)?;

    let k = rod.diffusivity / (rod.dx() * rod.dx());
    let n = rod.nodes;
    let rhs = move |_t: f64, y: &[f64], dydt: &mut [f64]| {
        for i in 0..n {
            let left = if i == 0 { 0.0 } else { y[i - 1] };
            let right = if i + 1 == n { 0.0 } else { y[i + 1] };
            dydt[i] = k * (left - 2.0 * y[i] + right);
        }
    };

    let later: Vec<f64> = times.iter().copied().filter(|&t| t > 0.0).collect();
    let mut solved = if later.is_empty() {
        Vec::new().into_iter()
    } else {
        OdeProblemBuilder::new(rhs, initial.clone(), 0.0)
            .rtol(1e-10)
            .atol(1e-10)
            .build()?
            .solve_dense(Method::Tsit45, &later)?
            .into_iter()
    };

    let temps = times
        .iter()
        .map(|&t| {
            if t == 0.0 {
                initial.clone()
            } else {
                solved.next().expect("one solved state per later time")
            }
        })
        .collect();
    Ok(HeatTrace {
        times: times.to_vec(),
        positions: rod.positions(),
        temps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn twin_with(alpha: f64, length: f64, nodes: f64) -> Twin {
        let mut twin = Twin::new("rod").unwrap();
        twin.set_parameter(DIFFUSIVITY, alpha).unwrap();
        twin.set_parameter(LENGTH, length).unwrap();
        twin.set_parameter(GRID_NODES, nodes).unwrap();
        twin
    }

    fn scenario(amplitude: f64, duration: f64) -> Scenario {
        Scenario {
            name: "cool".into(),
            duration_s: duration,
            initial_conditions: BTreeMap::from([(AMPLITUDE0.to_string(), amplitude)]),
            controller: None,
        }
    }

    #[test]
    fn sine_mode_matches_closed_form() {
        // u(x,t) = A exp(-alpha (pi/L)^2 t) sin(pi x / L)
        let (alpha, length, amp) = (0.5, 2.0, 10.0);
        let twin = twin_with(alpha, length, 40.0);
        let times = [0.0, 0.5, 1.0, 2.0];
        let trace = simulate_heat(&twin, &scenario(amp, 2.0), &times).unwrap();
        let pi = std::f64::consts::PI;
        let mut worst = 0.0f64;
        for (&t, row) in times.iter().zip(&trace.temps) {
            for (&x, &u) in trace.positions.iter().zip(row) {
                let exact =
                    amp * (-alpha * (pi / length).powi(2) * t).exp() * (pi * x / length).sin();
                worst = worst.max((u - exact).abs());
            }
        }
        // Second-order spatial error with 40 nodes; the time error is far below it.
        assert!(worst < 5e-3, "worst error {worst}");
    }

    #[test]
    fn error_shrinks_with_finer_grid() {
        let err = |nodes: f64| {
            let twin = twin_with(1.0, 1.0, nodes);
            let trace = simulate_heat(&twin, &scenario(1.0, 0.1), &[0.1]).unwrap();
            let pi = std::f64::consts::PI;
            trace
                .positions
                .iter()
                .zip(&trace.temps[0])
                .map(|(&x, &u)| (u - (-pi * pi * 0.1).exp() * (pi * x).sin()).abs())
                .fold(0.0, f64::max)
        };
        assert!(err(40.0) < err(10.0) / 8.0);
    }

    #[test]
    fn heat_only_decays_with_zero_boundaries() {
        let twin = twin_with(0.3, 1.0, 20.0);
        let trace = simulate_heat(&twin, &scenario(5.0, 3.0), &[0.0, 1.0, 2.0, 3.0]).unwrap();
        let peaks: Vec<f64> = trace
            .temps
            .iter()
            .map(|r| r.iter().cloned().fold(f64::MIN, f64::max))
            .collect();
        assert!(peaks.windows(2).all(|w| w[1] < w[0]));
    }

    #[test]
    fn runs_are_deterministic() {
        let twin = twin_with(0.3, 1.0, 20.0);
        let sc = scenario(5.0, 1.0);
        assert_eq!(
            simulate_heat(&twin, &sc, &[0.0, 0.5, 1.0]).unwrap(),
            simulate_heat(&twin, &sc, &[0.0, 0.5, 1.0]).unwrap()
        );
    }

    #[test]
    fn bad_parameters_are_rejected() {
        let sc = scenario(1.0, 1.0);
        for (a, l, n, name) in [
            (0.0, 1.0, 10.0, DIFFUSIVITY),
            (1.0, -1.0, 10.0, LENGTH),
            (1.0, 1.0, 1.0, GRID_NODES),
            (1.0, 1.0, 10.5, GRID_NODES),
            (1.0, 1.0, 1e9, GRID_NODES),
        ] {
            let err = simulate_heat(&twin_with(a, l, n), &sc, &[0.0]).unwrap_err();
            assert!(matches!(err, SimError::InvalidParameter { name: got, .. } if got == name));
        }
    }

    #[test]
    fn missing_amplitude_and_bad_times_are_reported() {
        let twin = twin_with(1.0, 1.0, 5.0);
        let mut sc = scenario(1.0, 1.0);
        assert_eq!(
            simulate_heat(&twin, &sc, &[2.0]),
            Err(SimError::InvalidTimes)
        );
        sc.initial_conditions.clear();
        assert_eq!(
            simulate_heat(&twin, &sc, &[0.0]),
            Err(SimError::MissingInitialCondition(AMPLITUDE0))
        );
    }

    #[test]
    fn wrong_profile_length_is_rejected() {
        let rod = Rod::from_twin(&twin_with(1.0, 1.0, 5.0)).unwrap();
        assert!(diffuse(&rod, vec![0.0; 4], 1.0, &[0.0]).is_err());
    }
}
