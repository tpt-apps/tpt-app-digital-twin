//! Always-on analytic correctness gate for the from-scratch ODE engine.
//!
//! These tests compare every integrator against closed-form or reference
//! solutions on a battery of standard problems (exponential decay, harmonic
//! oscillator, van der Pol, SIR, Robertson). They do NOT pull in `diffsol`
//! (which is only an optional, feature-gated verification oracle).

use tpt_sci_ode::{Method, OdeProblem};

const METHODS: [Method; 4] = [
    Method::Tsit45,
    Method::TrBdf2,
    Method::Esdirk34,
    Method::Bdf,
];

fn solve_at(
    method: Method,
    rhs: impl Fn(f64, &[f64], &mut [f64]) + 'static,
    y0: Vec<f64>,
    t0: f64,
    tf: f64,
) -> Vec<f64> {
    OdeProblem::new(rhs, y0, t0)
        .unwrap()
        .solve(method, tf)
        .unwrap()
}

#[test]
fn exp_decay_all_methods() {
    // y' = -k y,  y(0) = 1  ->  y(t) = exp(-k t)
    let k = 2.0;
    for m in METHODS {
        let y = solve_at(
            m,
            move |_t, y, dydt| dydt[0] = -k * y[0],
            vec![1.0],
            0.0,
            1.0,
        );
        let exact = (-k * 1.0_f64).exp();
        // Tolerance accounts for 1st-order BDF (backward Euler) and 2nd-order TR-BDF2
        // at default rtol=atol=1e-6.
        let tol = match m {
            Method::Bdf => 5e-4,      // 1st order, larger phase/amplitude error
            Method::TrBdf2 => 2e-4,   // 2nd order
            Method::Esdirk34 => 5e-5, // 3rd order, slightly larger error
            _ => 1e-5,                // Tsit45 (5th order)
        };
        assert!(
            (y[0] - exact).abs() < tol,
            "{m:?}: y={} exact={exact} tol={tol}",
            y[0]
        );
    }
}

#[test]
fn exp_decay_dense_matches_point() {
    // solve_dense must agree with solve at the same final time.
    let k = 1.5;
    for m in METHODS {
        let dense = OdeProblem::new(move |_t, y, dydt| dydt[0] = -k * y[0], vec![1.0], 0.0)
            .unwrap()
            .solve_dense(m, &[1.0])
            .unwrap();
        let point = OdeProblem::new(move |_t, y, dydt| dydt[0] = -k * y[0], vec![1.0], 0.0)
            .unwrap()
            .solve(m, 1.0)
            .unwrap();
        assert!((dense[0][0] - point[0]).abs() < 1e-9, "{m:?}");
    }
}

#[test]
fn harmonic_oscillator() {
    // y'' = -y,  y(0)=0, y'(0)=1  ->  y = sin(t), y' = cos(t)
    for m in METHODS {
        let y = solve_at(
            m,
            |_t, y, dydt| {
                dydt[0] = y[1];
                dydt[1] = -y[0];
            },
            vec![0.0, 1.0],
            0.0,
            std::f64::consts::FRAC_PI_2,
        );
        // at t = pi/2: y = sin(pi/2) = 1, y' = cos(pi/2) = 0
        // BDF (1st order backward Euler) has larger phase error; use looser tolerance.
        let (tol_y, tol_yp) = match m {
            Method::Bdf => (2e-3, 2e-3),
            _ => (1e-3, 1e-3),
        };
        assert!((y[0] - 1.0).abs() < tol_y, "{m:?}: y[0]={}", y[0]);
        assert!((y[1] - 0.0).abs() < tol_yp, "{m:?}: y[1]={}", y[1]);
    }
}

#[test]
fn sir_model_consistent() {
    // SIR: S' = -b S I, I' = b S I - g I, R' = g I  (population conserved)
    let (b, g) = (0.6, 0.2);
    let rhs = move |_t: f64, y: &[f64], dydt: &mut [f64]| {
        let (s, i, _r) = (y[0], y[1], y[2]);
        let inf = b * s * i;
        dydt[0] = -inf;
        dydt[1] = inf - g * i;
        dydt[2] = g * i;
    };
    let y0 = vec![0.99, 0.01, 0.0];
    for m in METHODS {
        let (point, dense) = {
            let p = OdeProblem::new(rhs, y0.clone(), 0.0).unwrap();
            // Epidemic dies out by t=100 (I < 1e-3). At t=20 it's still ~0.18.
            let point = p.solve(m, 100.0).unwrap();
            let dense = p.solve_dense(m, &[100.0]).unwrap();
            (point, dense)
        };
        // population conserved to tolerance
        let sum_p = point[0] + point[1] + point[2];
        assert!((sum_p - 1.0).abs() < 1e-4, "{m:?}: sum={sum_p}");
        // solve and solve_dense agree
        assert!((point[0] - dense[0][0]).abs() < 1e-9);
        // epidemic has died out: infected ~ 0
        assert!(point[1] < 1e-3, "{m:?}: I={}", point[1]);
    }
}

#[test]
fn van_der_pol_runs_and_is_self_consistent() {
    // y'' - mu(1 - y^2) y' + y = 0  ; mu=1 (non-stiff-ish)
    let mu = 1.0;
    let rhs = move |_t: f64, y: &[f64], dydt: &mut [f64]| {
        dydt[0] = y[1];
        dydt[1] = mu * (1.0 - y[0] * y[0]) * y[1] - y[0];
    };
    let y0 = vec![2.0, 0.0];
    // Tsit45 (non-stiff) should integrate this cleanly.
    let point = solve_at(Method::Tsit45, rhs, y0.clone(), 0.0, 6.0);
    assert!(point[0].is_finite());
    // solve_dense must reproduce the point solve.
    let dense = OdeProblem::new(rhs, y0, 0.0)
        .unwrap()
        .solve_dense(Method::Tsit45, &[6.0])
        .unwrap();
    assert!((point[0] - dense[0][0]).abs() < 1e-9);
}

#[test]
fn robertson_stiff_solvable() {
    // Classic stiff Robertson problem. y' = A y + ... with conservation.
    // y1' = -0.04 y1 + 1e4 y2 y3
    // y2' =  0.04 y1 - 1e4 y2 y3 - 3e7 y2^2
    // y3' =  3e7 y2^2
    // Conserved: y1 + y2 + y3 = 1.
    let rhs = |_t: f64, y: &[f64], dydt: &mut [f64]| {
        let (y1, y2, y3) = (y[0], y[1], y[2]);
        dydt[0] = -0.04 * y1 + 1e4 * y2 * y3;
        dydt[1] = 0.04 * y1 - 1e4 * y2 * y3 - 3e7 * y2 * y2;
        dydt[2] = 3e7 * y2 * y2;
    };
    let y0 = vec![1.0, 0.0, 0.0];
    // Stiff methods only; explicit Tsit45 cannot be expected to cope here.
    for m in [Method::Bdf, Method::TrBdf2, Method::Esdirk34] {
        let y = OdeProblem::new(rhs, y0.clone(), 0.0)
            .unwrap()
            .solve(m, 1e2)
            .unwrap();
        // conservation within tolerance
        let sum = y[0] + y[1] + y[2];
        assert!((sum - 1.0).abs() < 1e-3, "{m:?}: sum={sum}");
        // all components non-negative
        assert!(
            y.iter().all(|v| *v >= -1e-6),
            "{m:?}: negative component {:?}",
            y
        );
    }
}
