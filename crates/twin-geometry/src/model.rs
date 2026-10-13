//! Parametric models: named parameters drive a `tpt-vertex-kernel` feature tree.
//!
//! The kernel stores plain numbers inside each feature. A [`ModelSpec`] adds
//! the missing layer: each dimension is either a constant or the name of a
//! twin parameter. [`Model::build`] turns a spec and a set of parameter values
//! into a kernel `FeatureTree` and evaluates it; [`Model::rebuild`] does the
//! same after a parameter edit.
//!
//! Steps refer to earlier steps by index, so a spec is always acyclic.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tpt_vertex_kernel::feature_tree::{BooleanOp, EvalError, Feature, FeatureId, FeatureTree};
use tpt_vertex_kernel::geometry::sketch::Sketch;
use tpt_vertex_kernel::geometry::solid::Solid;
use tpt_vertex_kernel::math::{Vec2, Vec3};

use crate::Content;

/// A dimension: a fixed number or the name of a parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Value {
    Const(f64),
    Param(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Union,
    Subtract,
    Intersect,
}

/// One modelling step. `parent`, `a` and `b` are indices of earlier steps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Step {
    /// Rectangular block with one corner at the origin.
    Block {
        width: Value,
        depth: Value,
        height: Value,
    },
    /// Cylinder along Z, base centred on the origin.
    Cylinder {
        radius: Value,
        height: Value,
    },
    Boolean {
        op: Op,
        a: usize,
        b: usize,
    },
    Translate {
        parent: usize,
        x: Value,
        y: Value,
        z: Value,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelSpec {
    pub steps: Vec<Step>,
}

#[derive(Debug, Error, PartialEq)]
pub enum ModelError {
    #[error("the model has no steps")]
    Empty,
    #[error("step {step} refers to step {target}, which is not an earlier step")]
    BadReference { step: usize, target: usize },
    #[error("step {step} uses parameter `{name}`, which has no value")]
    MissingParameter { step: usize, name: String },
    #[error("step {step}: {what} must be a positive, finite number")]
    BadDimension { step: usize, what: &'static str },
    #[error("step {step}: {what} must be a finite number")]
    NotFinite { step: usize, what: &'static str },
    #[error("kernel evaluation failed: {0:?}")]
    Eval(EvalError),
}

/// A built model: the feature tree, its evaluated solid, and the solid each
/// step produced on its own (for telling which step made which face).
#[derive(Debug, Clone)]
pub struct Model {
    spec: ModelSpec,
    tree: FeatureTree,
    solid: Solid,
    step_solids: Vec<Solid>,
}

/// Segments used to approximate a circle. The kernel fixes this at 24 when it
/// turns a sketch circle into a profile.
#[cfg(test)]
const CIRCLE_SEGMENTS: usize = 24;

impl Model {
    /// Build and evaluate `spec` with the parameter values in `params`.
    pub fn build(spec: &ModelSpec, params: &Content) -> Result<Self, ModelError> {
        let (tree, ids) = build_tree(spec, params)?;
        let evaluation = tree.evaluate().map_err(ModelError::Eval)?;
        let step_solids = ids
            .iter()
            .map(|id| evaluation.features.get(id).cloned().unwrap_or_default())
            .collect();
        Ok(Self {
            spec: spec.clone(),
            tree,
            solid: evaluation.final_solid,
            step_solids,
        })
    }

    /// For each triangle of [`Model::triangles`], the index of the step that
    /// made it: the earliest step whose own solid has that triangle on its
    /// surface. A face cut by a later step belongs to the step that first had
    /// it. `None` when no step's surface contains the triangle (for example a
    /// face split by a later cut, which does not sit on any one step's face).
    pub fn face_steps(&self) -> Vec<Option<usize>> {
        let triangles = self.triangles();
        let radius = triangles
            .iter()
            .flatten()
            .map(|p| p.iter().map(|x| x * x).sum::<f64>().sqrt())
            .fold(1.0_f64, f64::max);
        let tol = 1e-7 * radius;
        triangles
            .iter()
            .map(|t| {
                self.step_solids
                    .iter()
                    .position(|s| surface_contains(s, t, tol))
            })
            .collect()
    }

    /// Re-evaluate after a parameter edit. On error the model is unchanged.
    pub fn rebuild(&mut self, params: &Content) -> Result<(), ModelError> {
        *self = Self::build(&self.spec, params)?;
        Ok(())
    }

    pub fn spec(&self) -> &ModelSpec {
        &self.spec
    }

    pub fn tree(&self) -> &FeatureTree {
        &self.tree
    }

    /// The final solid (the last step's output).
    pub fn solid(&self) -> &Solid {
        &self.solid
    }

    /// The surface as triangles, each given by its three corners in model
    /// coordinates.
    pub fn triangles(&self) -> Vec<[[f64; 3]; 3]> {
        let vertices = &self.solid.vertices;
        self.solid
            .faces
            .iter()
            .map(|face| {
                face.indices().map(|i| {
                    let v = vertices[i as usize];
                    [v.x, v.y, v.z]
                })
            })
            .collect()
    }

    /// Enclosed volume. The kernel's sign depends on winding, so this is absolute.
    pub fn volume(&self) -> f64 {
        self.solid.volume().abs()
    }

    /// SHA-256 of the mesh as lowercase hex. Equal parameters give an equal
    /// hash on every machine, so it can stand in for the geometry in a commit.
    pub fn geometry_hash(&self) -> String {
        let mut h = Sha256::new();
        for v in &self.solid.vertices {
            for c in [v.x, v.y, v.z] {
                h.update(c.to_le_bytes());
            }
        }
        for f in &self.solid.faces {
            for i in f.indices() {
                h.update(i.to_le_bytes());
            }
        }
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// The feature tree for `spec`, and the feature ID of each step in order.
fn build_tree(
    spec: &ModelSpec,
    params: &Content,
) -> Result<(FeatureTree, Vec<FeatureId>), ModelError> {
    if spec.steps.is_empty() {
        return Err(ModelError::Empty);
    }
    let mut tree = FeatureTree::new();
    let mut ids: Vec<FeatureId> = Vec::new();

    for (step, s) in spec.steps.iter().enumerate() {
        let positive = |v: &Value, what| resolve(step, v, params, what, true);
        let finite = |v: &Value, what| resolve(step, v, params, what, false);
        let earlier = |target: usize| {
            ids.get(target)
                .copied()
                .ok_or(ModelError::BadReference { step, target })
        };
        let feature = match s {
            Step::Block {
                width,
                depth,
                height,
            } => {
                let (w, d) = (positive(width, "width")?, positive(depth, "depth")?);
                Feature::Extrude {
                    sketch: rectangle(w, d),
                    height: positive(height, "height")?,
                }
            }
            Step::Cylinder { radius, height } => {
                let r = positive(radius, "radius")?;
                let mut sketch = Sketch::new();
                sketch.circle(Vec2::ZERO, r);
                Feature::Extrude {
                    sketch,
                    height: positive(height, "height")?,
                }
            }
            Step::Boolean { op, a, b } => Feature::Boolean {
                op: match op {
                    Op::Union => BooleanOp::Union,
                    Op::Subtract => BooleanOp::Subtract,
                    Op::Intersect => BooleanOp::Intersect,
                },
                a: earlier(*a)?,
                b: earlier(*b)?,
            },
            Step::Translate { parent, x, y, z } => Feature::Transform {
                parent: earlier(*parent)?,
                translation: Vec3::new(finite(x, "x")?, finite(y, "y")?, finite(z, "z")?),
                rotation: Vec3::ZERO,
            },
        };
        ids.push(tree.add(feature, None));
    }
    Ok((tree, ids))
}

fn resolve(
    step: usize,
    value: &Value,
    params: &Content,
    what: &'static str,
    must_be_positive: bool,
) -> Result<f64, ModelError> {
    let v = match value {
        Value::Const(v) => *v,
        Value::Param(name) => *params
            .get(name)
            .ok_or_else(|| ModelError::MissingParameter {
                step,
                name: name.clone(),
            })?,
    };
    if !v.is_finite() {
        return Err(ModelError::NotFinite { step, what });
    }
    if must_be_positive && v <= 0.0 {
        return Err(ModelError::BadDimension { step, what });
    }
    Ok(v)
}

fn rectangle(w: f64, d: f64) -> Sketch {
    let mut s = Sketch::new();
    let corners = [
        Vec2::new(0.0, 0.0),
        Vec2::new(w, 0.0),
        Vec2::new(w, d),
        Vec2::new(0.0, d),
    ];
    for i in 0..4 {
        s.line(corners[i], corners[(i + 1) % 4]);
    }
    s
}

/// Whether `triangle` lies on the surface of `solid`: some face of `solid` is
/// in the same plane, and the triangle's centroid is inside that face. Works
/// whichever way the faces are wound. `tol` is a distance.
fn surface_contains(solid: &Solid, triangle: &[[f64; 3]; 3], tol: f64) -> bool {
    let centroid = [0, 1, 2].map(|k| (triangle[0][k] + triangle[1][k] + triangle[2][k]) / 3.0);
    solid.faces.iter().any(|face| {
        let [a, b, c] = face.indices().map(|i| {
            let v = solid.vertices[i as usize];
            [v.x, v.y, v.z]
        });
        let n = cross(sub(b, a), sub(c, a));
        let len = dot(n, n).sqrt();
        if len == 0.0 {
            return false;
        }
        let unit = n.map(|x| x / len);
        // Every corner of the triangle must lie in the face's plane.
        if !triangle.iter().all(|p| dot(unit, sub(*p, a)).abs() <= tol) {
            return false;
        }
        // The centroid must be inside the face: on the same side of all three
        // edges, measured in the plane, whichever way the face is wound.
        let signed: Vec<f64> = [(a, b), (b, c), (c, a)]
            .iter()
            .map(|&(p, q)| {
                let e = sub(q, p);
                let across = dot(cross(e, sub(centroid, p)), unit);
                across / dot(e, e).sqrt().max(f64::MIN_POSITIVE)
            })
            .collect();
        let inside = signed.iter().all(|&d| d >= -tol);
        let outside = signed.iter().all(|&d| d <= tol);
        inside || outside
    })
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triangles_cover_the_block_surface() {
        // A 2 x 1 x 1 block has surface area 2 (2*1 + 2*1 + 1*1) = 10.
        let spec = ModelSpec {
            steps: vec![Step::Block {
                width: Value::Const(2.0),
                depth: Value::Const(1.0),
                height: Value::Const(1.0),
            }],
        };
        let model = Model::build(&spec, &Content::new()).unwrap();
        let triangles = model.triangles();
        assert!(!triangles.is_empty());
        let area: f64 = triangles
            .iter()
            .map(|[a, b, c]| {
                let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
                let x = [
                    u[1] * v[2] - u[2] * v[1],
                    u[2] * v[0] - u[0] * v[2],
                    u[0] * v[1] - u[1] * v[0],
                ];
                0.5 * (x[0] * x[0] + x[1] * x[1] + x[2] * x[2]).sqrt()
            })
            .sum();
        assert!((area - 10.0).abs() < 1e-6, "area {area}");
    }

    fn params(pairs: &[(&str, f64)]) -> Content {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    fn block_spec() -> ModelSpec {
        ModelSpec {
            steps: vec![Step::Block {
                width: Value::Param("w".into()),
                depth: Value::Param("d".into()),
                height: Value::Const(2.0),
            }],
        }
    }

    fn cube(size: f64) -> Step {
        Step::Block {
            width: Value::Const(size),
            depth: Value::Const(size),
            height: Value::Const(size),
        }
    }

    fn shift(parent: usize, x: f64, y: f64, z: f64) -> Step {
        Step::Translate {
            parent,
            x: Value::Const(x),
            y: Value::Const(y),
            z: Value::Const(z),
        }
    }

    #[test]
    fn block_volume_follows_parameters() {
        let m = Model::build(&block_spec(), &params(&[("w", 3.0), ("d", 4.0)])).unwrap();
        assert!((m.volume() - 24.0).abs() < 1e-9, "{}", m.volume());
    }

    #[test]
    fn editing_a_parameter_rebuilds() {
        let mut m = Model::build(&block_spec(), &params(&[("w", 3.0), ("d", 4.0)])).unwrap();
        let before = m.geometry_hash();
        m.rebuild(&params(&[("w", 5.0), ("d", 4.0)])).unwrap();
        assert!((m.volume() - 40.0).abs() < 1e-9);
        assert_ne!(m.geometry_hash(), before);
        // Back to the old value: identical geometry, identical hash.
        m.rebuild(&params(&[("w", 3.0), ("d", 4.0)])).unwrap();
        assert_eq!(m.geometry_hash(), before);
    }

    #[test]
    fn failed_rebuild_leaves_model_unchanged() {
        let mut m = Model::build(&block_spec(), &params(&[("w", 3.0), ("d", 4.0)])).unwrap();
        let before = m.geometry_hash();
        assert!(m.rebuild(&params(&[("w", -1.0), ("d", 4.0)])).is_err());
        assert_eq!(m.geometry_hash(), before);
    }

    #[test]
    fn builds_are_deterministic() {
        let p = params(&[("w", 1.5), ("d", 2.5)]);
        let a = Model::build(&block_spec(), &p).unwrap();
        let b = Model::build(&block_spec(), &p).unwrap();
        assert_eq!(a.geometry_hash(), b.geometry_hash());
    }

    #[test]
    fn cylinder_volume_matches_the_polygon_prism() {
        let spec = ModelSpec {
            steps: vec![Step::Cylinder {
                radius: Value::Const(2.0),
                height: Value::Const(3.0),
            }],
        };
        let m = Model::build(&spec, &Content::new()).unwrap();
        let n = CIRCLE_SEGMENTS as f64;
        let area = 0.5 * n * 4.0 * (2.0 * std::f64::consts::PI / n).sin();
        assert!((m.volume() - area * 3.0).abs() < 1e-6, "{}", m.volume());
    }

    #[test]
    fn translate_moves_the_solid() {
        let spec = ModelSpec {
            steps: vec![
                cube(1.0),
                Step::Translate {
                    parent: 0,
                    x: Value::Param("dx".into()),
                    y: Value::Const(0.0),
                    z: Value::Const(0.0),
                },
            ],
        };
        let m = Model::build(&spec, &params(&[("dx", 10.0)])).unwrap();
        let (min, max) = m.solid().bounds().unwrap();
        assert!((min.x - 10.0).abs() < 1e-9 && (max.x - 11.0).abs() < 1e-9);
    }

    #[test]
    fn subtracting_a_hole_removes_volume() {
        let spec = ModelSpec {
            steps: vec![
                cube(10.0),
                Step::Cylinder {
                    radius: Value::Const(1.0),
                    height: Value::Const(30.0),
                },
                shift(1, 5.0, 5.0, -10.0),
                Step::Boolean {
                    op: Op::Subtract,
                    a: 0,
                    b: 2,
                },
            ],
        };
        let m = Model::build(&spec, &Content::new()).unwrap();
        let n = CIRCLE_SEGMENTS as f64;
        let hole = 0.5 * n * (2.0 * std::f64::consts::PI / n).sin() * 10.0;
        assert!(
            (m.volume() - (1000.0 - hole)).abs() < 1e-3,
            "volume {}",
            m.volume()
        );
    }

    #[test]
    fn bad_specs_are_reported() {
        let p = params(&[("w", 1.0)]);
        assert_eq!(
            Model::build(&ModelSpec::default(), &p).unwrap_err(),
            ModelError::Empty
        );
        assert!(matches!(
            Model::build(&block_spec(), &p).unwrap_err(),
            ModelError::MissingParameter { name, .. } if name == "d"
        ));
        let forward = ModelSpec {
            steps: vec![shift(0, 0.0, 0.0, 0.0)],
        };
        assert!(matches!(
            Model::build(&forward, &p).unwrap_err(),
            ModelError::BadReference { step: 0, target: 0 }
        ));
        let nan = params(&[("w", f64::NAN), ("d", 1.0)]);
        assert!(matches!(
            Model::build(&block_spec(), &nan).unwrap_err(),
            ModelError::NotFinite { .. }
        ));
    }

    #[test]
    fn spec_round_trips_through_json() {
        let spec = ModelSpec {
            steps: vec![cube(2.0), shift(0, 1.0, 0.0, 0.0)],
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert_eq!(serde_json::from_str::<ModelSpec>(&json).unwrap(), spec);
    }

    #[test]
    fn a_single_block_is_all_made_by_step_zero() {
        let m = Model::build(
            &ModelSpec {
                steps: vec![cube(2.0)],
            },
            &Default::default(),
        )
        .unwrap();
        let owners = m.face_steps();
        assert_eq!(owners.len(), m.triangles().len());
        assert!(owners.iter().all(|o| *o == Some(0)));
    }

    #[test]
    fn a_hole_is_made_by_the_cutter_and_the_rest_by_the_block() {
        // Block 0, cylinder 1, moved by step 2, subtracted from the block at 3.
        let spec = ModelSpec {
            steps: vec![
                cube(2.0),
                Step::Cylinder {
                    radius: Value::Const(0.3),
                    height: Value::Const(3.0),
                },
                shift(1, 1.0, 1.0, -0.5),
                Step::Boolean {
                    op: Op::Subtract,
                    a: 0,
                    b: 2,
                },
            ],
        };
        let m = Model::build(&spec, &Default::default()).unwrap();
        let owners = m.face_steps();
        assert_eq!(owners.len(), m.triangles().len());
        assert!(owners.contains(&Some(0)), "the block keeps most faces");
        assert!(
            owners.contains(&Some(2)),
            "the hole walls come from the cutter"
        );
        assert!(
            owners.iter().all(|o| o.is_some()),
            "every face has an owner"
        );
    }
}
