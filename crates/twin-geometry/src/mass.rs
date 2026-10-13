//! Mass properties of a closed triangle mesh of uniform density: volume, mass,
//! centroid and the inertia tensor about the centroid.
//!
//! The solid is integrated from its surface with the divergence theorem: the
//! surface is split into tetrahedra with the origin, and each tetrahedron
//! contributes its exact volume and second moments. No volume grid is needed.

/// Mass properties of a solid. Lengths are in model units; mass is density
/// times volume, in the same units as the density.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MassProperties {
    pub volume: f64,
    pub mass: f64,
    pub centroid: [f64; 3],
    /// Inertia tensor about the centroid, in the model's axes (symmetric).
    pub inertia: [[f64; 3]; 3],
}

/// Mass properties of the solid enclosed by `triangles` at `density`. The
/// winding of the faces does not matter. `None` for an empty or flat mesh, or
/// a density that is not a positive finite number.
pub fn mass_properties(triangles: &[[[f64; 3]; 3]], density: f64) -> Option<MassProperties> {
    if !(density.is_finite() && density > 0.0) || triangles.is_empty() {
        return None;
    }
    // Integrals over the solid: volume, first moments, and second moments
    // about the origin. Each tetrahedron (origin, a, b, c) has signed volume
    // v = a·(b×c)/6, and its exact integrals are v, v·(a+b+c)/4 and
    // v/20·(Σ corners + sums·sums).
    let mut volume = 0.0;
    let mut first = [0.0; 3];
    let mut second = [[0.0; 3]; 3];
    for [a, b, c] in triangles {
        let v = dot(a, cross(b, c)) / 6.0;
        let sum = [a[0] + b[0] + c[0], a[1] + b[1] + c[1], a[2] + b[2] + c[2]];
        volume += v;
        for i in 0..3 {
            first[i] += v * sum[i] / 4.0;
            for j in 0..3 {
                let corners = a[i] * a[j] + b[i] * b[j] + c[i] * c[j];
                second[i][j] += v / 20.0 * (corners + sum[i] * sum[j]);
            }
        }
    }
    // Faces that point inwards make the signed total negative. Flip every
    // term so the solid is taken as the enclosed region either way.
    let sign = if volume < 0.0 { -1.0 } else { 1.0 };
    let volume = sign * volume;
    if !(volume.is_finite() && volume > 0.0) {
        return None;
    }
    let first = first.map(|x| sign * x);
    let second = second.map(|row| row.map(|x| sign * x));

    let centroid = first.map(|x| x / volume);
    // Second moments about the centroid: ∫(x_i - c_i)(x_j - c_j) dV.
    let mut cov = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            cov[i][j] = second[i][j] - volume * centroid[i] * centroid[j];
        }
    }
    let trace = cov[0][0] + cov[1][1] + cov[2][2];
    let mut inertia = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            inertia[i][j] = if i == j {
                density * (trace - cov[i][i])
            } else {
                -density * cov[i][j]
            };
        }
    }
    Some(MassProperties {
        volume,
        mass: density * volume,
        centroid,
        inertia,
    })
}

fn dot(a: &[f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: &[f64; 3], b: &[f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Model, ModelSpec, Step, Value};

    fn block_triangles(w: f64, d: f64, h: f64) -> Vec<[[f64; 3]; 3]> {
        let p = |x: f64, y: f64, z: f64| [x, y, z];
        let quads = [
            [p(0., 0., 0.), p(0., d, 0.), p(w, d, 0.), p(w, 0., 0.)],
            [p(0., 0., h), p(w, 0., h), p(w, d, h), p(0., d, h)],
            [p(0., 0., 0.), p(w, 0., 0.), p(w, 0., h), p(0., 0., h)],
            [p(0., d, 0.), p(0., d, h), p(w, d, h), p(w, d, 0.)],
            [p(0., 0., 0.), p(0., 0., h), p(0., d, h), p(0., d, 0.)],
            [p(w, 0., 0.), p(w, d, 0.), p(w, d, h), p(w, 0., h)],
        ];
        quads
            .iter()
            .flat_map(|q| [[q[0], q[1], q[2]], [q[0], q[2], q[3]]])
            .collect()
    }

    fn assert_close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    #[test]
    fn unit_cube_has_the_textbook_values() {
        let props = mass_properties(&block_triangles(1.0, 1.0, 1.0), 1.0).unwrap();
        assert_close(props.volume, 1.0);
        assert_close(props.mass, 1.0);
        for c in props.centroid {
            assert_close(c, 0.5);
        }
        // A cube of side s: I = m s² / 6 about every axis through its centre.
        for i in 0..3 {
            assert_close(props.inertia[i][i], 1.0 / 6.0);
        }
        assert_close(props.inertia[0][1], 0.0);
    }

    #[test]
    fn box_inertia_and_mass_scale_with_density_and_size() {
        // A 2 x 1 x 1 block: m = 2ρ, I_xx = m (d² + h²) / 12.
        let props = mass_properties(&block_triangles(2.0, 1.0, 1.0), 1000.0).unwrap();
        assert_close(props.mass, 2000.0);
        assert_close(props.inertia[0][0], 2000.0 * 2.0 / 12.0);
        assert_close(props.inertia[1][1], 2000.0 * 5.0 / 12.0);
        assert_close(props.inertia[2][2], 2000.0 * 5.0 / 12.0);
    }

    #[test]
    fn winding_does_not_change_the_result() {
        let outward = block_triangles(2.0, 1.0, 3.0);
        let inward: Vec<_> = outward.iter().map(|[a, b, c]| [*a, *c, *b]).collect();
        assert_eq!(
            mass_properties(&outward, 1.0),
            mass_properties(&inward, 1.0)
        );
    }

    #[test]
    fn a_kernel_block_gives_the_same_properties() {
        let spec = ModelSpec {
            steps: vec![Step::Block {
                width: Value::Const(2.0),
                depth: Value::Const(1.0),
                height: Value::Const(1.0),
            }],
        };
        let model = Model::build(&spec, &Default::default()).unwrap();
        let props = mass_properties(&model.triangles(), 1.0).unwrap();
        assert_close(props.volume, 2.0);
        assert_close(props.inertia[0][0], 2.0 * 2.0 / 12.0);
    }

    #[test]
    fn empty_or_bad_input_gives_none() {
        assert_eq!(mass_properties(&[], 1.0), None);
        assert_eq!(mass_properties(&block_triangles(1.0, 1.0, 1.0), 0.0), None);
        assert_eq!(
            mass_properties(&block_triangles(1.0, 1.0, 1.0), f64::NAN),
            None
        );
    }
}
