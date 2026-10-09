use tpt_sci_ode::{Method, OdeProblem};

#[test]
fn debug_exp_decay_bdf() {
    let k = 2.0;
    let p = OdeProblem::new(move |_t, y, dydt| dydt[0] = -k * y[0], vec![1.0], 0.0).unwrap();
    let y = p.solve(Method::Bdf, 1.0).unwrap();
    eprintln!(
        "BDF exp decay y(1) = {:?}, exact = {:.6}",
        y,
        (-k * 1.0_f64).exp()
    );
    let p2 = OdeProblem::new(
        |_t, y, dydt| {
            dydt[0] = y[1];
            dydt[1] = -y[0];
        },
        vec![0.0, 1.0],
        0.0,
    )
    .unwrap();
    let y2 = p2.solve(Method::Bdf, std::f64::consts::FRAC_PI_2).unwrap();
    eprintln!("BDF harmonic y = {:?}, exact = [1.0, 0.0]", y2);
}
