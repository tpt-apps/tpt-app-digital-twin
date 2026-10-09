//! Van der Pol oscillator — a guided tour of the `tpt-sci-ode` public API.
//!
//! # The problem
//!
//! The Van der Pol oscillator is the textbook non-conservative, self-exciting
//! system `y'' - mu (1 - y^2) y' + y = 0`, written here as a first-order pair:
//!
//! ```text
//!     y0' = y1
//!     y1' = mu (1 - y0^2) y1 - y0
//! ```
//!
//! The nonlinear damping term `mu (1 - y0^2)` *pumps energy in* when `|y0| < 1`
//! and *bleeds it out* when `|y0| > 1`, so every non-zero initial condition is
//! attracted to the same closed orbit: a **limit cycle**. For `mu = 1` that
//! cycle has amplitude `max|y0| ~ 2.009` and period `~ 6.6633`. As `mu -> 0`
//! the damping vanishes and the system degenerates to the harmonic oscillator
//! `y'' + y = 0` with period `2*pi`; as `mu` grows large the orbit develops
//! near-vertical relaxation jumps and the problem becomes genuinely stiff,
//! which is why this single equation is the standard stress test for both
//! explicit and implicit integrators.
//!
//! # What this example exercises
//!
//! * both problem constructors ([`OdeProblem::new`], [`OdeProblem::from_rhs`])
//!   and both builder entry points ([`OdeProblemBuilder::new`],
//!   [`OdeProblemBuilder::from_rhs`]) with `rtol`/`atol`/`h0`,
//! * a boxed [`Rhs`] closure *and* a hand-written [`RhsCallable`] impl that
//!   counts right-hand-side evaluations (the solvers expose no step counter, so
//!   RHS evaluations are used as the cost proxy),
//! * all four [`Method`] variants (`Tsit45`, `TrBdf2`, `Esdirk34`, `Bdf`),
//! * [`OdeProblem::solve`], [`OdeProblem::solve_dense`] (Hermite dense output)
//!   and [`OdeProblem::respawn`] for chunked/incremental integration,
//! * the Cranelift JIT right-hand side ([`JitRhsBuilder`], [`compile_rhs`]),
//! * the [`OdeError`] surface, and the re-exported [`Scalar`] numeric trait.
//!
//! # What to observe in the output
//!
//! * every method lands on the *same* limit cycle (amplitude and period agree
//!   to a few parts in a thousand) even though the individual states at the
//!   final time differ by an accumulated phase error,
//! * `Tsit45` (explicit) needs the fewest RHS evaluations on this non-stiff
//!   `mu = 1` problem, while the implicit methods pay for Newton iterations and
//!   finite-difference Jacobians — and the stiff `mu = 1e5` run reverses that
//!   verdict completely: the explicit method is throttled by stability and
//!   needs orders of magnitude more work than the implicit ones,
//! * tightening `rtol`/`atol` moves the answer by far less than it costs,
//! * the `mu = 0.05` run reproduces the harmonic `2*pi` period,
//! * the JIT-compiled RHS and the chunked `respawn` integration reproduce the
//!   plain closure trajectory.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tpt_sci_ode::{
    JitRhsBuilder, Method, OdeError, OdeProblem, OdeProblemBuilder, Rhs, RhsCallable, Scalar,
    compile_rhs,
};

/// Stiffness/nonlinearity parameter of the main run.
const MU: f64 = 1.0;
/// Nearly-harmonic parameter used for the small-`mu` cross-check.
const MU_SMALL: f64 = 0.05;
/// Genuinely stiff parameter used for the explicit-vs-implicit comparison.
const MU_STIFF: f64 = 1e5;
/// Final time for the stiff run (kept short: the explicit method has to crawl).
const T_STIFF: f64 = 0.1;
/// Initial state, well outside the limit cycle so the transient is visible.
const Y0: [f64; 2] = [2.0, 0.0];
/// Initial time.
const T0: f64 = 0.0;
/// Final time of the main survey (~4.5 limit-cycle periods for `mu = 1`).
const T_FINAL: f64 = 30.0;
/// Dense-output sampling interval for the main survey.
const DT: f64 = 0.02;
/// Time after which the trajectory is treated as being on the limit cycle.
const SETTLED_AFTER: f64 = 7.0;
/// Reference values for the `mu = 1` limit cycle (standard literature values).
const REF_PERIOD: f64 = 6.66329;
const REF_AMPLITUDE: f64 = 2.00862;

/// All four methods shipped by the crate.
const METHODS: [Method; 4] = [
    Method::Tsit45,
    Method::TrBdf2,
    Method::Esdirk34,
    Method::Bdf,
];

/// Shared right-hand-side evaluation counter (single-threaded, so `Cell` is
/// enough).
type EvalCounter = Rc<Cell<usize>>;

/// RHS evaluations performed through the JIT-compiled path.
static JIT_EVALS: AtomicUsize = AtomicUsize::new(0);

// ---------------------------------------------------------------------------
// Two ways of expressing the same right-hand side.
// ---------------------------------------------------------------------------

/// The Van der Pol RHS as a boxed [`Rhs`] trait object, i.e. the plain
/// `Fn(f64, &[f64], &mut [f64])` shape that [`OdeProblem::new`] accepts.
fn vdp_rhs(mu: f64) -> Box<Rhs> {
    Box::new(move |_t, y, dydt| {
        dydt[0] = y[1];
        dydt[1] = mu * (1.0 - y[0] * y[0]) * y[1] - y[0];
    })
}

/// The same RHS as an explicit [`RhsCallable`] implementation, which lets it
/// report its own dimension, validate its input, and count evaluations.
struct CountingVdp {
    mu: f64,
    evals: EvalCounter,
}

impl CountingVdp {
    fn new(mu: f64, evals: EvalCounter) -> Self {
        Self { mu, evals }
    }
}

impl RhsCallable for CountingVdp {
    fn nstates(&self) -> usize {
        2
    }

    fn call(&self, _t: f64, y: &[f64], dydt: &mut [f64]) -> Result<(), OdeError> {
        if y.len() != 2 || dydt.len() != 2 {
            return Err(OdeError::Invalid(format!(
                "van der Pol needs 2 states, got y={} dydt={}",
                y.len(),
                dydt.len()
            )));
        }
        self.evals.set(self.evals.get() + 1);
        dydt[0] = y[1];
        dydt[1] = self.mu * (1.0 - y[0] * y[0]) * y[1] - y[0];
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Trajectory analysis helpers.
// ---------------------------------------------------------------------------

/// Period of the `mu -> 0` (harmonic) limit, written generically over the
/// [`Scalar`] trait that `tpt-sci-ode` re-exports from the `tpt-math`
/// substrate.
fn harmonic_period<T: Scalar>() -> T {
    T::PI() + T::PI()
}

/// Uniform output grid `dt, 2*dt, ..., t_end` (all points strictly after `t0`,
/// as [`OdeProblem::solve_dense`] requires).
fn grid(dt: f64, t_end: f64) -> Vec<f64> {
    let n = (t_end / dt).round() as usize;
    (1..=n).map(|i| i as f64 * dt).collect()
}

/// Times at which `y0` crosses zero upwards, linearly interpolated between
/// samples. Consecutive upward crossings are one period apart.
fn upward_crossings(times: &[f64], traj: &[Vec<f64>], after: f64) -> Vec<f64> {
    let mut out = Vec::new();
    for ((ta, tb), (ya, yb)) in times
        .windows(2)
        .map(|w| (w[0], w[1]))
        .zip(traj.windows(2).map(|w| (w[0][0], w[1][0])))
    {
        if ta >= after && ya <= 0.0 && yb > 0.0 {
            out.push(ta + (tb - ta) * (-ya / (yb - ya)));
        }
    }
    out
}

/// Mean spacing of the crossing times, i.e. the measured limit-cycle period.
fn mean_period(crossings: &[f64]) -> Option<f64> {
    let (first, last) = (crossings.first()?, crossings.last()?);
    let intervals = crossings.len().checked_sub(1)?;
    if intervals == 0 {
        return None;
    }
    Some((last - first) / intervals as f64)
}

/// Largest `|y0|` reached after the transient has died away.
fn settled_amplitude(times: &[f64], traj: &[Vec<f64>], after: f64) -> f64 {
    times
        .iter()
        .zip(traj)
        .filter(|(t, _)| **t >= after)
        .map(|(_, y)| y[0].abs())
        .fold(0.0_f64, f64::max)
}

/// Everything worth reporting about one method's run.
struct Survey {
    final_state: Vec<f64>,
    amplitude: f64,
    period: Option<f64>,
    evals: usize,
}

/// Integrate the `mu = 1` oscillator with `method`, sampling the dense output
/// on `times`, and reduce the trajectory to limit-cycle statistics.
fn survey(method: Method, times: &[f64]) -> Survey {
    let evals: EvalCounter = Rc::new(Cell::new(0));
    let prob = OdeProblem::from_rhs(CountingVdp::new(MU, Rc::clone(&evals)), Y0.to_vec(), T0)
        .expect("van der Pol problem is well posed");
    assert_eq!(prob.nstates(), 2, "van der Pol is a 2-state system");

    let traj = prob
        .solve_dense(method, times)
        .unwrap_or_else(|e| panic!("{method:?} failed to integrate van der Pol: {e}"));
    assert_eq!(
        traj.len(),
        times.len(),
        "{method:?}: solve_dense must return one state per requested time"
    );
    let final_state = traj.last().expect("dense grid is non-empty").clone();
    assert_finite(&format!("{method:?} final state"), &final_state);

    Survey {
        amplitude: settled_amplitude(times, &traj, SETTLED_AFTER),
        period: mean_period(&upward_crossings(times, &traj, SETTLED_AFTER)),
        evals: evals.get(),
        final_state,
    }
}

/// A compact ASCII phase-plane sketch of the settled orbit (`y1` against `y0`).
fn phase_sketch(times: &[f64], traj: &[Vec<f64>], after: f64) -> String {
    const W: usize = 61;
    const H: usize = 19;

    let settled: Vec<&Vec<f64>> = times
        .iter()
        .zip(traj)
        .filter(|(t, _)| **t >= after)
        .map(|(_, y)| y)
        .collect();
    let x_max = settled.iter().map(|y| y[0].abs()).fold(0.0_f64, f64::max);
    let v_max = settled.iter().map(|y| y[1].abs()).fold(0.0_f64, f64::max);
    if x_max <= 0.0 || v_max <= 0.0 {
        return "    (degenerate orbit)".to_string();
    }

    let mut canvas = vec![vec![' '; W]; H];
    for y in settled {
        let col = ((y[0] / x_max * 0.5 + 0.5) * (W - 1) as f64).round() as usize;
        let row = ((0.5 - y[1] / v_max * 0.5) * (H - 1) as f64).round() as usize;
        canvas[row.min(H - 1)][col.min(W - 1)] = '*';
    }

    canvas
        .into_iter()
        .map(|row| format!("    |{}|", row.into_iter().collect::<String>()))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Assertions used as loud, self-checking diagnostics.
// ---------------------------------------------------------------------------

fn assert_finite(label: &str, state: &[f64]) {
    assert!(
        state.iter().all(|v| v.is_finite()),
        "{label}: expected a finite state, got {state:?}"
    );
}

fn assert_within(label: &str, value: f64, lo: f64, hi: f64) {
    assert!(
        (lo..=hi).contains(&value),
        "{label}: {value} is outside the plausible range [{lo}, {hi}]"
    );
}

/// Assert that a call was rejected with [`OdeError::Invalid`] and show the
/// message the crate produced.
fn expect_invalid<T>(label: &str, result: Result<T, OdeError>) {
    match result {
        Err(err @ OdeError::Invalid(_)) => println!("  {label:<26} -> rejected: {err}"),
        Err(other) => panic!("{label}: expected OdeError::Invalid, got {other}"),
        Ok(_) => panic!("{label}: expected an error, but the call succeeded"),
    }
}

// ---------------------------------------------------------------------------
// The tour.
// ---------------------------------------------------------------------------

fn main() {
    println!("=== tpt-sci-ode: Van der Pol oscillator tour ===");
    println!("  y0' = y1,   y1' = mu (1 - y0^2) y1 - y0");
    println!("  mu = {MU}, y({T0}) = {Y0:?}, integrating to t = {T_FINAL}");

    constructors();
    let times = grid(DT, T_FINAL);
    method_comparison(&times);
    dense_output(&times);
    harmonic_limit();
    incremental_respawn();
    jit_rhs();
    stiff_regime();
    error_surface();

    println!("\nAll diagnostics and assertions passed.");
}

/// `OdeProblem::new` (defaults) versus `OdeProblemBuilder` (tuned tolerances).
fn constructors() {
    println!("\n[1] Constructors: OdeProblem::new vs OdeProblemBuilder");

    let default_prob =
        OdeProblem::new(vdp_rhs(MU), Y0.to_vec(), T0).expect("default problem must build");
    println!(
        "  OdeProblem::new       nstates = {}, t0 = {}, y0 = {:?}  (rtol = atol = 1e-6)",
        default_prob.nstates(),
        default_prob.t0(),
        default_prob.y0()
    );

    let tight_prob = OdeProblemBuilder::new(vdp_rhs(MU), Y0.to_vec(), T0)
        .rtol(1e-10)
        .atol(1e-10)
        .h0(1e-3)
        .build()
        .expect("tuned problem must build");
    println!(
        "  OdeProblemBuilder     nstates = {}, t0 = {}, y0 = {:?}  (rtol = atol = 1e-10, h0 = 1e-3)",
        tight_prob.nstates(),
        tight_prob.t0(),
        tight_prob.y0()
    );

    let loose = default_prob
        .solve(Method::Tsit45, T_FINAL)
        .expect("Tsit45 solves the non-stiff oscillator");
    let tight = tight_prob
        .solve(Method::Tsit45, T_FINAL)
        .expect("Tsit45 solves the non-stiff oscillator at tight tolerance");
    assert_finite("default tolerance solve", &loose);
    assert_finite("tight tolerance solve", &tight);

    let drift = loose
        .iter()
        .zip(&tight)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f64, f64::max);
    println!(
        "  y({T_FINAL}) @ 1e-6  = [{:+.6}, {:+.6}]",
        loose[0], loose[1]
    );
    println!(
        "  y({T_FINAL}) @ 1e-10 = [{:+.6}, {:+.6}]",
        tight[0], tight[1]
    );
    println!("  max component difference = {drift:.3e} (tolerance-driven phase drift)");
    assert_within("tolerance drift", drift, 0.0, 0.5);
}

/// All four methods on the same problem, compared by trajectory statistics and
/// RHS-evaluation cost.
fn method_comparison(times: &[f64]) {
    println!("\n[2] All four Method variants (dense grid dt = {DT}, cost = RHS evaluations)");
    println!(
        "  {:<10} {:>10} {:>10} {:>10} {:>10} {:>12}",
        "method", "y0(T)", "y1(T)", "max|y0|", "period", "rhs evals"
    );

    let mut reference: Option<Vec<f64>> = None;
    for method in METHODS {
        let s = survey(method, times);
        let name = format!("{method:?}");
        let period = s.period.expect("at least two crossings on the limit cycle");
        println!(
            "  {name:<10} {:>10.5} {:>10.5} {:>10.5} {:>10.5} {:>12}",
            s.final_state[0], s.final_state[1], s.amplitude, period, s.evals
        );

        // Every method must find the same attractor, whatever its phase error.
        assert_within(
            &format!("{method:?} limit-cycle amplitude"),
            s.amplitude,
            0.9 * REF_AMPLITUDE,
            1.1 * REF_AMPLITUDE,
        );
        assert_within(
            &format!("{method:?} limit-cycle period"),
            period,
            0.9 * REF_PERIOD,
            1.1 * REF_PERIOD,
        );

        match &reference {
            None => reference = Some(s.final_state),
            Some(r) => {
                let dev = r
                    .iter()
                    .zip(&s.final_state)
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0_f64, f64::max);
                println!("             (deviation from Tsit45 state at t = {T_FINAL}: {dev:.3e})");
            }
        }
    }
    println!(
        "  reference limit cycle for mu = {MU}: amplitude {REF_AMPLITUDE}, period {REF_PERIOD}"
    );
}

/// Hermite dense output: sample the settled orbit and describe it.
fn dense_output(times: &[f64]) {
    println!("\n[3] Dense output (solve_dense) and limit-cycle geometry");

    let prob = OdeProblem::new(vdp_rhs(MU), Y0.to_vec(), T0).expect("problem must build");
    let traj = prob
        .solve_dense(Method::Tsit45, times)
        .expect("dense solve must succeed");
    assert_eq!(traj.len(), times.len(), "one state per requested time");

    println!("  {} samples over t in ({}, {}]", traj.len(), T0, T_FINAL);
    println!("  {:>7}  {:>10}  {:>10}", "t", "y0", "y1");
    for (t, y) in times.iter().zip(&traj).step_by((times.len() / 8).max(1)) {
        println!("  {t:>7.2}  {:>10.5}  {:>10.5}", y[0], y[1]);
    }

    let crossings = upward_crossings(times, &traj, SETTLED_AFTER);
    let period = mean_period(&crossings).expect("settled orbit crosses zero repeatedly");
    let amplitude = settled_amplitude(times, &traj, SETTLED_AFTER);
    let peak_rate = times
        .iter()
        .zip(&traj)
        .filter(|(t, _)| **t >= SETTLED_AFTER)
        .map(|(_, y)| y[1].abs())
        .fold(0.0_f64, f64::max);

    println!(
        "  settled orbit (t >= {SETTLED_AFTER}): {} upward zero crossings of y0",
        crossings.len()
    );
    println!(
        "  measured period    = {period:.5}  (reference {REF_PERIOD}, error {:.2e})",
        (period - REF_PERIOD).abs()
    );
    println!(
        "  measured amplitude = {amplitude:.5}  (reference {REF_AMPLITUDE}, error {:.2e})",
        (amplitude - REF_AMPLITUDE).abs()
    );
    println!("  peak |y1| on the cycle = {peak_rate:.5}");
    println!("  phase plane (y1 vertical, y0 horizontal), settled orbit only:");
    println!("{}", phase_sketch(times, &traj, SETTLED_AFTER));

    assert_within("dense period", period, REF_PERIOD - 0.05, REF_PERIOD + 0.05);
    assert_within(
        "dense amplitude",
        amplitude,
        REF_AMPLITUDE - 0.05,
        REF_AMPLITUDE + 0.05,
    );
}

/// Small `mu` must reproduce the harmonic oscillator, period `2*pi`.
fn harmonic_limit() {
    println!("\n[4] Small-mu limit: mu = {MU_SMALL} should approach the harmonic period");

    let two_pi: f64 = harmonic_period();
    let evals: EvalCounter = Rc::new(Cell::new(0));
    let prob = OdeProblemBuilder::from_rhs(
        CountingVdp::new(MU_SMALL, Rc::clone(&evals)),
        Y0.to_vec(),
        T0,
    )
    .rtol(1e-9)
    .atol(1e-9)
    .h0(1e-2)
    .build()
    .expect("small-mu problem must build");

    let t_end = 6.0 * two_pi;
    let times = grid(0.01, t_end);
    let traj = prob
        .solve_dense(Method::Tsit45, &times)
        .expect("near-harmonic problem is easy for Tsit45");
    let period =
        mean_period(&upward_crossings(&times, &traj, two_pi)).expect("several periods sampled");
    let amplitude = settled_amplitude(&times, &traj, two_pi);
    let rel_err = (period - two_pi).abs() / two_pi;

    println!(
        "  integrated {t_end:.4} time units ({} samples)",
        times.len()
    );
    println!("  measured period = {period:.6}");
    println!("  2*pi (Scalar::PI) = {two_pi:.6}   relative error = {rel_err:.3e}");
    println!("  measured amplitude = {amplitude:.6} (harmonic limit keeps |y| ~ 2)");
    println!("  RHS evaluations at rtol = atol = 1e-9: {}", evals.get());

    assert_within("harmonic period relative error", rel_err, 0.0, 0.01);
    assert_within("harmonic amplitude", amplitude, 1.9, 2.1);
}

/// `respawn` re-uses the RHS and tolerances from a new state, so a long
/// integration can be done in chunks.
fn incremental_respawn() {
    println!("\n[5] Chunked integration with OdeProblem::respawn");

    let prob = OdeProblem::new(vdp_rhs(MU), Y0.to_vec(), T0).expect("problem must build");
    let one_shot = prob
        .solve(Method::Tsit45, T_FINAL)
        .expect("single-shot solve must succeed");

    let chunks = 10usize;
    let dt = (T_FINAL - T0) / chunks as f64;
    let mut state = prob.y0().to_vec();
    let mut t = prob.t0();
    for _ in 0..chunks {
        let segment = prob
            .respawn(state, t)
            .expect("respawn keeps the RHS and tolerances");
        state = segment
            .solve(Method::Tsit45, t + dt)
            .expect("segment solve must succeed");
        t += dt;
    }
    assert_finite("chunked state", &state);

    let dev = one_shot
        .iter()
        .zip(&state)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f64, f64::max);
    let span = format!("one shot, {T0} -> {T_FINAL}");
    let chunked = format!("{chunks} chunks of {dt}");
    println!("  {span:<24}: [{:+.6}, {:+.6}]", one_shot[0], one_shot[1]);
    println!("  {chunked:<24}: [{:+.6}, {:+.6}]", state[0], state[1]);
    println!("  final t = {t}, max component difference = {dev:.3e}");
    assert_within("respawn vs one-shot difference", dev, 0.0, 1e-2);
}

/// The Cranelift JIT right-hand side plugs into the same solver pipeline.
fn jit_rhs() {
    println!("\n[6] Cranelift JIT right-hand side (JitRhsBuilder / compile_rhs)");

    // One-shot helper: compile, inspect, and evaluate directly.
    let probe = compile_rhs(2, |_t, y, dydt| {
        dydt[0] = y[1];
        dydt[1] = MU * (1.0 - y[0] * y[0]) * y[1] - y[0];
    })
    .expect("JIT compilation must succeed on the host target");
    println!("  compile_rhs -> JitRhs::nstates() = {}", probe.nstates());

    let mut dydt = [0.0_f64; 2];
    probe
        .call_safe(T0, &Y0, &mut dydt)
        .expect("2-state call must be accepted");
    println!("  f({T0}, {Y0:?}) = {dydt:?} (expected [0, -2])");
    assert!(
        dydt[0].abs() < 1e-12 && (dydt[1] + 2.0).abs() < 1e-12,
        "JIT RHS disagrees with the analytic derivative: {dydt:?}"
    );

    // Wrong dimension is reported, not undefined behaviour.
    let mut short = [0.0_f64; 1];
    expect_invalid(
        "JitRhs::call_safe(1 state)",
        probe.call_safe(T0, &[1.0], &mut short),
    );

    // Builder form, then integrate through the ordinary OdeProblem pipeline.
    let builder = JitRhsBuilder::new().expect("host ISA must be available");
    let jit = builder
        .compile(2, |_t, y, dydt| {
            JIT_EVALS.fetch_add(1, Ordering::Relaxed);
            dydt[0] = y[1];
            dydt[1] = MU * (1.0 - y[0] * y[0]) * y[1] - y[0];
        })
        .expect("JIT compilation must succeed");
    let jit_prob = OdeProblem::from_rhs(jit, Y0.to_vec(), T0).expect("JIT problem must build");
    let jit_state = jit_prob
        .solve(Method::Tsit45, T_FINAL)
        .expect("JIT-backed solve must succeed");

    let closure_state = OdeProblem::new(vdp_rhs(MU), Y0.to_vec(), T0)
        .expect("problem must build")
        .solve(Method::Tsit45, T_FINAL)
        .expect("closure-backed solve must succeed");
    let dev = jit_state
        .iter()
        .zip(&closure_state)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f64, f64::max);

    println!(
        "  JIT     y({T_FINAL}) = [{:+.6}, {:+.6}]  ({} RHS evaluations)",
        jit_state[0],
        jit_state[1],
        JIT_EVALS.load(Ordering::Relaxed)
    );
    println!(
        "  closure y({T_FINAL}) = [{:+.6}, {:+.6}]",
        closure_state[0], closure_state[1]
    );
    println!("  max component difference = {dev:.3e} (same pipeline, same arithmetic)");
    assert_finite("JIT solve", &jit_state);
    assert_within("JIT vs closure difference", dev, 0.0, 1e-9);
}

/// Stiffness is what the implicit methods are for: at `mu = 1e5` the fast
/// eigenvalue is `~ -3e5`, so an explicit method is step-size limited by
/// stability rather than accuracy.
fn stiff_regime() {
    println!("\n[7] Stiff regime, mu = {MU_STIFF:e}: explicit vs implicit cost");

    let explicit_evals: EvalCounter = Rc::new(Cell::new(0));
    let explicit = OdeProblem::from_rhs(
        CountingVdp::new(MU_STIFF, Rc::clone(&explicit_evals)),
        Y0.to_vec(),
        T0,
    )
    .expect("stiff problem must build");
    let explicit_cost = match explicit.solve(Method::Tsit45, T_STIFF) {
        Ok(y) => {
            let cost = explicit_evals.get();
            println!(
                "  Tsit45   y({T_STIFF}) = [{:+.6}, {:+.3e}]  ({cost} RHS evaluations, stability-limited steps)",
                y[0], y[1]
            );
            assert_finite("stiff Tsit45 state", &y);
            cost
        }
        Err(err) => {
            let cost = explicit_evals.get();
            println!("  Tsit45   gave up after {cost} RHS evaluations: {err}");
            cost
        }
    };

    // On the slow manifold the fast variable is quasi-steady:
    //   y1 ~ y0 / (mu (1 - y0^2)) = -2 / (3 mu)  for y0 ~ 2.
    let quasi_steady = Y0[0] / (MU_STIFF * (1.0 - Y0[0] * Y0[0]));
    for method in [Method::TrBdf2, Method::Esdirk34, Method::Bdf] {
        let evals: EvalCounter = Rc::new(Cell::new(0));
        let prob = OdeProblem::from_rhs(
            CountingVdp::new(MU_STIFF, Rc::clone(&evals)),
            Y0.to_vec(),
            T0,
        )
        .expect("stiff problem must build");
        let y = prob
            .solve(method, T_STIFF)
            .unwrap_or_else(|e| panic!("{method:?} must handle the stiff regime: {e}"));
        let name = format!("{method:?}");
        let speedup = explicit_cost as f64 / evals.get().max(1) as f64;
        println!(
            "  {name:<8} y({T_STIFF}) = [{:+.6}, {:+.3e}]  ({} RHS evaluations, {speedup:.0}x cheaper)",
            y[0],
            y[1],
            evals.get()
        );
        assert_finite(&format!("stiff {method:?} state"), &y);
        assert_within(&format!("stiff {method:?} y0"), y[0], 1.99, 2.01);
        assert_within(
            &format!("stiff {method:?} y1"),
            y[1],
            10.0 * quasi_steady,
            0.0,
        );
    }
    println!("  quasi-steady slow-manifold value y1 ~ {quasi_steady:.3e} (all methods agree)");
}

/// The error surface: everything that can be rejected up front is.
fn error_surface() {
    println!("\n[8] Error handling (OdeError)");

    expect_invalid("empty y0", OdeProblem::new(vdp_rhs(MU), Vec::new(), T0));
    expect_invalid(
        "non-positive rtol",
        OdeProblemBuilder::new(vdp_rhs(MU), Y0.to_vec(), T0)
            .rtol(0.0)
            .build(),
    );
    expect_invalid(
        "negative atol",
        OdeProblemBuilder::new(vdp_rhs(MU), Y0.to_vec(), T0)
            .atol(-1e-6)
            .build(),
    );

    let prob = OdeProblem::new(vdp_rhs(MU), Y0.to_vec(), T0).expect("problem must build");
    expect_invalid(
        "t_eval before t0",
        prob.solve_dense(Method::Tsit45, &[-1.0, 1.0]),
    );
    expect_invalid(
        "t_eval equal to t0",
        prob.solve_dense(Method::Tsit45, &[T0]),
    );

    let empty = prob
        .solve_dense(Method::Tsit45, &[])
        .expect("an empty t_eval is a no-op, not an error");
    assert!(empty.is_empty(), "empty t_eval must yield no output states");
    let label = "empty t_eval";
    println!("  {label:<26} -> Ok, 0 output states");
    println!(
        "  runtime failures (non-convergent Newton, collapsed step, step-budget exhaustion) surface"
    );
    println!("  as OdeError::Newton / StepTooSmall / MaxSteps; none were triggered by this tour.");
}
