use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use tpt_sci_ode::{Method, OdeProblem};

fn bench_exponential_decay(c: &mut Criterion) {
    c.bench_function("exponential_decay_to_t10", |b| {
        b.iter(|| {
            let prob = OdeProblem::new(|_t, y, dydt| dydt[0] = -y[0], vec![1.0], 0.0).unwrap();
            prob.solve(black_box(Method::Bdf), 10.0).unwrap()
        });
    });
}

criterion_group!(benches, bench_exponential_decay);
criterion_main!(benches);
