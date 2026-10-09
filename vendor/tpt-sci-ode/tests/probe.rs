use tpt_sci_ode::{Method, OdeProblemBuilder};

fn sir_rhs(b: f64, g: f64) -> impl Fn(f64, &[f64], &mut [f64]) + 'static {
    move |_t: f64, y: &[f64], dydt: &mut [f64]| {
        let (s, i) = (y[0], y[1]);
        let inf = b * s * i;
        dydt[0] = -inf;
        dydt[1] = inf - g * i;
        dydt[2] = g * i;
    }
}

#[test]
fn probe_sir() {
    // Probe SIR model at different tolerances (1e-12 needs too many steps, skip).
    for &tol in &[1e-6_f64, 1e-9] {
        for &m in &[
            Method::Tsit45,
            Method::Bdf,
            Method::TrBdf2,
            Method::Esdirk34,
        ] {
            let p = OdeProblemBuilder::new(sir_rhs(0.6, 0.2), vec![0.99, 0.01, 0.0], 0.0)
                .rtol(tol)
                .atol(tol);
            let y = p.build().unwrap().solve(m, 20.0).unwrap();
            eprintln!(
                "tol={:e} m={:?} -> S={:.5} I={:.5} R={:.5}",
                tol, m, y[0], y[1], y[2]
            );
        }
    }
}
