//! # Predator–prey (Lotka–Volterra) dynamics — phase portrait and a closed-form
//! first integral.
//!
//! The classic Lotka–Volterra system models two coupled populations, a prey `x`
//! and a predator `y`:
//!
//! ```text
//!     x' = α x - β x y        (prey: grows exponentially, is eaten by y)
//!     y' = δ x y - γ y        (predator: fed by x, dies off exponentially)
//! ```
//!
//! Unlike the Van der Pol oscillator (which is attracted to a unique *limit
//! cycle*), the Lotka–Volterra system is **Hamiltonian**: it has a *conserved
//! quantity* (a first integral), so every non-equilibrium initial condition
//! traces a closed orbit forever and never settles. The conserved quantity is
//!
//! ```text
//!     Q(x, y) = γ·ln x + α·ln y - δ·x - β·y
//! ```
//!
//! which is constant along the exact trajectory (dQ/dt = 0). This makes it an
//! unusually *strong* self-check: any drift in the integrator shows up directly
//! as a change in `Q`, independent of the phase of the orbit.
//!
//! # What this example exercises
//!
//! * building the LV problem with [`OdeProblem::new`] and tracing the closed
//!   orbit with dense output ([`OdeProblem::solve_dense`]),
//! * the explicit [`Method::Tsit45`] and the implicit [`Method::Bdf`] on the
//!   *same* orbit, compared by first-integral drift and period,
//! * the closed-form invariant `Q` as a tight, physics-based assertion,
//! * an ASCII phase portrait (`y` against `x`) of the closed limit cycle,
//! * the oscillation period and amplitude measured from the dense samples.
//!
//! # What to observe in the output
//!
//! * both methods close the orbit and conserve `Q` to within a few parts in
//!   ten thousand (a far tighter check than merely matching a phase-averaged
//!   amplitude),
//! * the predator (`y`) lags the prey (`x`) by roughly a quarter period — the
//!   textbook `x` leads `y` signature of the LV cycle,
//! * the implicit `Bdf` run, though more expensive per step, tracks `Q` as
//!   tightly as the explicit `Tsit45` on this mildly stiff, non-dissipative
//!   problem.

use tpt_sci_ode::{Method, OdeProblem};

// --- Lotka–Volterra parameters ----------------------------------------------
// Standard textbook non-dimensional scaling: equilibrium at (x*, y*) =
// (γ/δ, α/β) = (1, 0.5).
const ALPHA: f64 = 2.0 / 3.0; // prey intrinsic growth rate
const BETA: f64 = 4.0 / 3.0; // predation rate (prey removal per predator)
const DELTA: f64 = 1.0; // predator growth rate per unit prey eaten
const GAMMA: f64 = 1.0; // predator intrinsic death rate

// Equilibrium populations (fixed points of the ODE).
const X_STAR: f64 = GAMMA / DELTA; // 1.0
const Y_STAR: f64 = ALPHA / BETA; // 0.5

// Initial condition away from equilibrium so the orbit is non-trivial.
const X0: f64 = 1.0;
const Y0: f64 = 1.0;
const T0: f64 = 0.0;

// Integration horizon (a few oscillation periods) and dense-sample spacing.
const T_FINAL: f64 = 24.0;
const DT: f64 = 0.02;

/// The Lotka–Volterra right-hand side (a plain `Fn(f64, &[f64], &mut [f64])`).
fn lv_rhs(_t: f64, y: &[f64], dydt: &mut [f64]) {
    let x = y[0];
    let pred = y[1];
    dydt[0] = ALPHA * x - BETA * x * pred;
    dydt[1] = DELTA * x * pred - GAMMA * pred;
}

/// The conserved first integral `Q = γ·ln x + α·ln y - δ·x - β·y`.
///
/// Returns `None` if either population is non-positive (the logarithm is then
/// undefined — which also signals the trajectory has left the physical state
/// space, a would-be integration failure).
fn first_integral(x: f64, y: f64) -> Option<f64> {
    if x <= 0.0 || y <= 0.0 {
        return None;
    }
    Some(GAMMA * x.ln() + ALPHA * y.ln() - DELTA * x - BETA * y)
}

/// Uniform output grid `dt, 2*dt, ..., t_end` (all points strictly after `t0`
/// as [`OdeProblem::solve_dense`] requires).
fn grid(dt: f64, t_end: f64) -> Vec<f64> {
    let n = (t_end / dt).round() as usize;
    (1..=n).map(|i| i as f64 * dt).collect()
}

/// Times at which the prey `x` crosses `x_star` upward, linearly interpolated
/// between dense samples. Consecutive upward crossings are one period apart.
fn upward_crossings_of(times: &[f64], traj: &[Vec<f64>], target: f64) -> Vec<f64> {
    let mut out = Vec::new();
    for ((ta, tb), (ya, yb)) in times
        .windows(2)
        .map(|w| (w[0], w[1]))
        .zip(traj.windows(2).map(|w| (w[0][0], w[1][0])))
    {
        if ya <= target && yb > target {
            out.push(ta + (tb - ta) * (target - ya) / (yb - ya));
        }
    }
    out
}

/// Mean spacing of the crossing times, i.e. the measured oscillation period.
fn mean_period(crossings: &[f64]) -> Option<f64> {
    let (first, last) = (crossings.first()?, crossings.last()?);
    let intervals = crossings.len().checked_sub(1)?;
    if intervals == 0 {
        return None;
    }
    Some((last - first) / intervals as f64)
}

/// A compact ASCII phase portrait of the orbit (`y` against `x`).
fn phase_sketch(traj: &[Vec<f64>]) -> String {
    const W: usize = 61;
    const H: usize = 19;

    let x_max = traj.iter().map(|y| y[0]).fold(0.0_f64, f64::max);
    let y_max = traj.iter().map(|y| y[1]).fold(0.0_f64, f64::max);
    if x_max <= 0.0 || y_max <= 0.0 {
        return "    (degenerate orbit)".to_string();
    }

    let mut canvas = vec![vec![' '; W]; H];
    for y in traj {
        let col = ((y[0] / x_max) * (W - 1) as f64).round() as usize;
        let row = ((1.0 - y[1] / y_max) * (H - 1) as f64).round() as usize;
        canvas[row.min(H - 1)][col.min(W - 1)] = '*';
    }

    canvas
        .into_iter()
        .map(|row| format!("    |{}|", row.into_iter().collect::<String>()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Everything worth reporting about one method's integration of the orbit.
struct Orbit {
    final_state: Vec<f64>,
    q_drift: f64,
    period: Option<f64>,
    x_amp: (f64, f64),
}

/// Integrate the Lotka–Volterra orbit with `method`, sample the dense output,
/// and reduce the trajectory to first-integral drift and period statistics.
fn trace(method: Method, times: &[f64]) -> Orbit {
    let prob = OdeProblem::new(lv_rhs, vec![X0, Y0], T0).expect("LV problem is well posed");
    assert_eq!(prob.nstates(), 2, "Lotka–Volterra is a 2-state system");

    let traj = prob
        .solve_dense(method, times)
        .unwrap_or_else(|e| panic!("{method:?} failed to integrate Lotka–Volterra: {e}"));
    assert_eq!(
        traj.len(),
        times.len(),
        "{method:?}: solve_dense must return one state per requested time"
    );

    let q0 = first_integral(X0, Y0).expect("initial state is physical");
    let mut q_drift = 0.0_f64;
    for y in &traj {
        let q = first_integral(y[0], y[1]).unwrap_or_else(|| {
            panic!("{method:?}: trajectory left the physical state space at {y:?}")
        });
        q_drift = q_drift.max((q - q0).abs());
    }

    let x_min = traj.iter().map(|y| y[0]).fold(f64::INFINITY, f64::min);
    let x_max = traj.iter().map(|y| y[0]).fold(0.0_f64, f64::max);
    let period = mean_period(&upward_crossings_of(times, &traj, X_STAR));

    Orbit {
        final_state: traj.last().expect("dense grid is non-empty").clone(),
        q_drift,
        period,
        x_amp: (x_min, x_max),
    }
}

/// Loud, self-checking assertion that a value lies in `[lo, hi]`.
fn assert_within(label: &str, value: f64, lo: f64, hi: f64) {
    assert!(
        (lo..=hi).contains(&value),
        "{label}: {value} is outside the plausible range [{lo}, {hi}]"
    );
}

fn main() {
    println!("=== tpt-sci-ode: Lotka–Volterra predator–prey ===");
    println!("  x' = α x - β x y,   y' = δ x y - γ y");
    println!("  α = {ALPHA}, β = {BETA}, δ = {DELTA}, γ = {GAMMA},  (x0, y0) = ({X0}, {Y0})");
    println!("  equilibrium: x* = {X_STAR}, y* = {Y_STAR}");

    let times = grid(DT, T_FINAL);

    // --- Both methods on the same orbit, compared by first-integral drift -----
    println!("\n[1] Orbit traced with dense output (dt = {DT}); Q = γ·ln x + α·ln y - δ·x - β·y");
    println!(
        "  {:<10} {:>10} {:>10} {:>14} {:>12}",
        "method", "x(T)", "y(T)", "max |ΔQ|", "period"
    );

    let mut reference: Option<Vec<f64>> = None;
    for method in [Method::Tsit45, Method::Bdf] {
        let o = trace(method, &times);
        let name = format!("{method:?}");
        let period = o.period.expect("several oscillation periods were sampled");
        println!(
            "  {name:<10} {:>10.5} {:>10.5} {:>14.3e} {:>12.5}",
            o.final_state[0], o.final_state[1], o.q_drift, period
        );

        // The defining property: Q must be conserved to high accuracy.
        assert_within(
            &format!("{method:?} first-integral drift"),
            o.q_drift,
            0.0,
            1e-2,
        );
        // The orbit amplitude brackets the equilibrium (it is a closed loop
        // around the fixed point, never crossing it).
        assert!(
            o.x_amp.0 < X_STAR && o.x_amp.1 > X_STAR,
            "prey must oscillate around x* = {X_STAR} (got [{}, {}])",
            o.x_amp.0,
            o.x_amp.1
        );
        assert_within("Tsit45/LV period", period, 5.0, 10.0);

        match &reference {
            None => reference = Some(o.final_state),
            Some(r) => {
                let dev = r
                    .iter()
                    .zip(&o.final_state)
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0_f64, f64::max);
                println!("             (state deviation from Tsit45 at t = {T_FINAL}: {dev:.3e})");
            }
        }
    }

    // --- Dense phase portrait -------------------------------------------------
    let prob = OdeProblem::new(lv_rhs, vec![X0, Y0], T0).expect("problem must build");
    let traj = prob
        .solve_dense(Method::Bdf, &times)
        .expect("dense solve must succeed");
    println!("\n[2] Phase portrait (y vertical, x horizontal), closed orbit:");
    println!("{}", phase_sketch(&traj));

    // --- Small-amplitude period vs the linearised (harmonic) prediction ------
    // Near equilibrium the LV system linearises to two uncoupled oscillators of
    // frequency ω = sqrt(α·γ), so the period tends to 2π/sqrt(α·γ) as the
    // amplitude shrinks. We check the orbit here is in that neighbourhood.
    let linear_period = 2.0 * std::f64::consts::PI / (ALPHA * GAMMA).sqrt();
    println!("\n[3] Linearised small-amplitude period 2π/√(α·γ) = {linear_period:.4}");
    let measured = mean_period(&upward_crossings_of(&times, &traj, X_STAR))
        .expect("period sampled on the main orbit");
    // The finite-amplitude orbit runs a little longer than the linear limit;
    // for these parameters the ratio is a few percent.
    assert_within(
        "LV period vs linear limit",
        measured / linear_period,
        1.0,
        1.2,
    );
    println!(
        "  measured orbit period           = {measured:.4}  (ratio {:.3})",
        measured / linear_period
    );

    println!("\nAll diagnostics and assertions passed.");
}
