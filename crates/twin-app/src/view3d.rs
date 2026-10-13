//! Orbit view of a model's surface: the projection, the draw order and the
//! flat shading used by the 3D viewport. This is plain maths with no window,
//! so it is tested on its own.
//!
//! World up is +z. The camera orbits around the model's centre: `yaw` turns it
//! about the vertical axis and `pitch` tilts it from the horizon (positive looks
//! down). In view space x points right, y points up on screen, and z points
//! towards the viewer.

/// A point or vector in model coordinates.
pub type Point3 = [f64; 3];
/// One triangle of the surface, by its three corners.
pub type Triangle = [Point3; 3];

/// The camera. Zoom multiplies the fitted size of the model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Orbit {
    pub yaw: f64,
    pub pitch: f64,
    pub zoom: f64,
}

impl Default for Orbit {
    fn default() -> Self {
        Self {
            yaw: 0.6,
            pitch: 0.5,
            zoom: 1.0,
        }
    }
}

/// Pitch is kept short of straight up or down, so the view never flips.
pub const MAX_PITCH: f64 = 1.5;
/// Zoom is kept within this range.
pub const ZOOM_RANGE: (f64, f64) = (0.2, 20.0);

impl Orbit {
    /// Rotate a model-space vector into view space. Rotation only, so it is the
    /// same for directions (normals) and for points relative to the centre.
    pub fn to_view(&self, p: Point3) -> Point3 {
        let (s, c) = self.yaw.sin_cos();
        let x1 = c * p[0] - s * p[1];
        let y1 = s * p[0] + c * p[1];
        let z1 = p[2];
        let (s, c) = self.pitch.sin_cos();
        [x1, c * z1 + s * y1, c * y1 - s * z1]
    }

    /// Screen offset in pixels from the view centre of `point`, with the model's
    /// `centre` at the origin and `scale` pixels per model unit. Screen y grows
    /// downwards, so world up becomes negative y.
    pub fn screen(&self, point: Point3, centre: Point3, scale: f64) -> [f64; 2] {
        let rel = [
            point[0] - centre[0],
            point[1] - centre[1],
            point[2] - centre[2],
        ];
        let v = self.to_view(rel);
        [v[0] * scale, -v[1] * scale]
    }

    /// Brightness of a triangle in `[0.35, 1]`. Faces that point at the viewer
    /// are brightest; the absolute value makes both sides of a face read alike.
    pub fn shade(&self, triangle: &Triangle) -> f64 {
        let normal = face_normal(triangle);
        let n = self.to_view(normal);
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if len == 0.0 {
            return 0.35;
        }
        0.35 + 0.65 * (n[2] / len).abs()
    }
}

/// Unit normal of a triangle, or zero for a degenerate one.
pub fn face_normal(t: &Triangle) -> Point3 {
    let a = sub(t[1], t[0]);
    let b = sub(t[2], t[0]);
    let n = [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len == 0.0 {
        [0.0; 3]
    } else {
        [n[0] / len, n[1] / len, n[2] / len]
    }
}

fn sub(a: Point3, b: Point3) -> Point3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// The centre of the bounding box and the radius of the bounding sphere (half
/// the box diagonal). `None` when there are no triangles.
pub fn bounds(triangles: &[Triangle]) -> Option<(Point3, f64)> {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for v in triangles.iter().flatten() {
        for k in 0..3 {
            lo[k] = lo[k].min(v[k]);
            hi[k] = hi[k].max(v[k]);
        }
    }
    if triangles.is_empty() || !lo[0].is_finite() {
        return None;
    }
    let centre = [
        (lo[0] + hi[0]) / 2.0,
        (lo[1] + hi[1]) / 2.0,
        (lo[2] + hi[2]) / 2.0,
    ];
    let d = sub(hi, lo);
    let radius = ((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() / 2.0).max(1e-9);
    Some((centre, radius))
}

/// Indices of the triangles ordered from the farthest to the nearest, by the
/// depth of each triangle's centroid. Drawing in this order hides what is
/// behind (painter's algorithm, which is exact for non-overlapping faces and
/// good enough for a single solid).
pub fn draw_order(triangles: &[Triangle], orbit: &Orbit) -> Vec<usize> {
    let mut keyed: Vec<(usize, f64)> = triangles
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let c = [
                (t[0][0] + t[1][0] + t[2][0]) / 3.0,
                (t[0][1] + t[1][1] + t[2][1]) / 3.0,
                (t[0][2] + t[1][2] + t[2][2]) / 3.0,
            ];
            (i, orbit.to_view(c)[2])
        })
        .collect();
    keyed.sort_by(|a, b| a.1.total_cmp(&b.1));
    keyed.into_iter().map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unit cube's six faces as twelve triangles, wound outwards.
    fn cube() -> Vec<Triangle> {
        let p = |x: f64, y: f64, z: f64| [x, y, z];
        let quads = [
            [p(0., 0., 0.), p(0., 1., 0.), p(1., 1., 0.), p(1., 0., 0.)],
            [p(0., 0., 1.), p(1., 0., 1.), p(1., 1., 1.), p(0., 1., 1.)],
            [p(0., 0., 0.), p(1., 0., 0.), p(1., 0., 1.), p(0., 0., 1.)],
            [p(0., 1., 0.), p(0., 1., 1.), p(1., 1., 1.), p(1., 1., 0.)],
            [p(0., 0., 0.), p(0., 0., 1.), p(0., 1., 1.), p(0., 1., 0.)],
            [p(1., 0., 0.), p(1., 1., 0.), p(1., 1., 1.), p(1., 0., 1.)],
        ];
        quads
            .iter()
            .flat_map(|q| [[q[0], q[1], q[2]], [q[0], q[2], q[3]]])
            .collect()
    }

    #[test]
    fn bounds_of_a_unit_cube() {
        let (centre, radius) = bounds(&cube()).unwrap();
        assert!(centre.iter().all(|c| (c - 0.5).abs() < 1e-12));
        assert!((radius - 3f64.sqrt() / 2.0).abs() < 1e-12);
        assert_eq!(bounds(&[]), None);
    }

    #[test]
    fn rotation_keeps_lengths() {
        let orbit = Orbit {
            yaw: 1.1,
            pitch: -0.7,
            zoom: 1.0,
        };
        let v = orbit.to_view([1.0, 2.0, 3.0]);
        let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        assert!((len - 14f64.sqrt()).abs() < 1e-12);
    }

    #[test]
    fn level_view_shows_up_as_up_and_the_top_face_is_seen_from_above() {
        let level = Orbit {
            yaw: 0.0,
            pitch: 0.0,
            zoom: 1.0,
        };
        // World +z is up, so it moves to negative screen y.
        let s = level.screen([0.0, 0.0, 1.0], [0.0; 3], 10.0);
        assert!(s[0].abs() < 1e-12 && (s[1] + 10.0).abs() < 1e-12);
        // Viewed level, a horizontal face is edge-on: no brightening from it.
        let top = [[0., 0., 1.], [1., 0., 1.], [0., 1., 1.]];
        assert!((level.shade(&top) - 0.35).abs() < 1e-12);
        // Looking down, the top face faces the viewer and is fully lit.
        let down = Orbit {
            yaw: 0.0,
            pitch: std::f64::consts::FRAC_PI_2,
            zoom: 1.0,
        };
        assert!((down.shade(&top) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn nearer_triangles_are_drawn_last() {
        let orbit = Orbit {
            yaw: 0.0,
            pitch: 0.0,
            zoom: 1.0,
        };
        // Two squares, at y = -5 (triangles 0 and 1) and y = 5 (triangles 2
        // and 3). Unrotated, depth is y, and the viewer sits at +y, so the
        // square at y = 5 is nearest and must be drawn last.
        let square = |y: f64| -> Vec<Triangle> {
            vec![
                [[0., y, 0.], [1., y, 0.], [1., y, 1.]],
                [[0., y, 0.], [1., y, 1.], [0., y, 1.]],
            ]
        };
        let mut tris = square(-5.0);
        tris.extend(square(5.0));
        let order = draw_order(&tris, &orbit);
        assert_eq!(order.len(), 4);
        assert!(order[..2].iter().all(|&i| i < 2));
        assert!(order[2..].iter().all(|&i| i >= 2));
    }

    #[test]
    fn degenerate_triangle_has_zero_normal_and_base_shade() {
        let line = [[0., 0., 0.], [1., 0., 0.], [2., 0., 0.]];
        assert_eq!(face_normal(&line), [0.0; 3]);
        let orbit = Orbit::default();
        assert!((orbit.shade(&line) - 0.35).abs() < 1e-12);
    }
}
