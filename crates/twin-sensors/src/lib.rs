//! Sensor models and basic fusion for visualisation.
//!
//! This slice covers the sensor model types, which are attached to a twin by
//! ID; synthetic IMU data generated from the mass-spring-damper simulation in
//! `twin-sim`; and point-cloud registration (ICP from `tpt-perception`).
//! Camera and lidar models describe the sensor but generate no data yet, and
//! fusion of sensors is not started.
//!
//! Every sensor has a mount pose in the twin's frame: a position in metres and
//! XYZ Euler angles in radians.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use twin_sim::{MassSpringDamper, Trace};

#[derive(Debug, Error, PartialEq)]
pub enum SensorError {
    #[error("sensor name must not be empty")]
    EmptyName,
    #[error("sensor `{name}`: {what} must be a positive, finite number")]
    BadValue { name: String, what: &'static str },
    #[error("sensor `{name}`: {what} must be a whole number of at least 1")]
    BadCount { name: String, what: &'static str },
    #[error("sensor `{name}`: field of view must be between 0 and 180 degrees")]
    BadFieldOfView { name: String },
    #[error("sensor pose must be finite")]
    NotFinite,
    #[error("sensor has no matching trace: {0}")]
    BadTrace(&'static str),
    #[error("point-cloud registration failed: {0}")]
    Registration(String),
}

/// Where a sensor sits on the body, in the twin's frame.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Mount {
    pub position_m: [f64; 3],
    pub euler_rad: [f64; 3],
}

impl Default for Mount {
    fn default() -> Self {
        Self {
            position_m: [0.0; 3],
            euler_rad: [0.0; 3],
        }
    }
}

/// What kind of sensor, with its kind-specific settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SensorKind {
    /// Accelerometer: samples at `sample_rate_hz`.
    Imu { sample_rate_hz: f64 },
    /// Pinhole camera.
    Camera {
        fov_deg: f64,
        width_px: u32,
        height_px: u32,
    },
    /// Spinning or solid-state lidar with `beams` returns per sweep.
    Lidar { range_m: f64, beams: u32 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sensor {
    pub name: String,
    pub mount: Mount,
    pub kind: SensorKind,
}

impl Sensor {
    /// Check the sensor and return it, so construction and loading share one path.
    pub fn validated(self) -> Result<Self, SensorError> {
        if self.name.trim().is_empty() {
            return Err(SensorError::EmptyName);
        }
        let name = self.name.clone();
        let positive = |value: f64, what: &'static str| {
            if value.is_finite() && value > 0.0 {
                Ok(())
            } else {
                Err(SensorError::BadValue {
                    name: name.clone(),
                    what,
                })
            }
        };
        let finite = self.mount.position_m.iter().chain(&self.mount.euler_rad);
        if finite.into_iter().any(|v| !v.is_finite()) {
            return Err(SensorError::NotFinite);
        }
        match &self.kind {
            SensorKind::Imu { sample_rate_hz } => positive(*sample_rate_hz, "sample rate")?,
            SensorKind::Camera {
                fov_deg,
                width_px,
                height_px,
            } => {
                if !(fov_deg.is_finite() && *fov_deg > 0.0 && *fov_deg < 180.0) {
                    return Err(SensorError::BadFieldOfView { name });
                }
                if *width_px == 0 || *height_px == 0 {
                    return Err(SensorError::BadCount {
                        name,
                        what: "image size",
                    });
                }
            }
            SensorKind::Lidar { range_m, beams } => {
                positive(*range_m, "range")?;
                if *beams == 0 {
                    return Err(SensorError::BadCount {
                        name,
                        what: "beam count",
                    });
                }
            }
        }
        Ok(self)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("sensor is serialisable")
    }

    /// Parse and validate a sensor. Invalid JSON and invalid settings both fail.
    pub fn from_json(json: &str) -> Result<Self, String> {
        let sensor: Self = serde_json::from_str(json).map_err(|e| e.to_string())?;
        sensor.validated().map_err(|e| e.to_string())
    }
}

/// One synthetic IMU reading: time and the acceleration along the spring axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImuSample {
    pub time_s: f64,
    pub accel_m_s2: f64,
}

/// Motion along the IMU axis, from integrating the samples (trapezoid rule).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImuMotion {
    pub time_s: f64,
    pub velocity_m_s: f64,
    /// Displacement from the start, in metres.
    pub displacement_m: f64,
}

/// Integrate IMU acceleration twice, starting from `velocity0_m_s` at the
/// first sample. Position error grows with time, because there is no drift
/// correction; this is the kind of prior that visual or lidar registration
/// corrects, not a pose estimate on its own.
pub fn integrate_imu(
    samples: &[ImuSample],
    velocity0_m_s: f64,
) -> Result<Vec<ImuMotion>, SensorError> {
    if samples.is_empty() {
        return Err(SensorError::BadTrace("no IMU samples"));
    }
    if samples.windows(2).any(|w| w[1].time_s <= w[0].time_s) {
        return Err(SensorError::BadTrace(
            "IMU times must be strictly increasing",
        ));
    }
    let mut v = velocity0_m_s;
    let mut x = 0.0;
    let mut out = Vec::with_capacity(samples.len());
    out.push(ImuMotion {
        time_s: samples[0].time_s,
        velocity_m_s: v,
        displacement_m: x,
    });
    for w in samples.windows(2) {
        let dt = w[1].time_s - w[0].time_s;
        let v_next = v + 0.5 * (w[0].accel_m_s2 + w[1].accel_m_s2) * dt;
        x += 0.5 * (v + v_next) * dt;
        v = v_next;
        out.push(ImuMotion {
            time_s: w[1].time_s,
            velocity_m_s: v,
            displacement_m: x,
        });
    }
    Ok(out)
}

/// Sample times for a sensor at `rate_hz` over `[0, duration_s]`, starting at 0.
pub fn sample_times(rate_hz: f64, duration_s: f64) -> Result<Vec<f64>, SensorError> {
    if !(rate_hz.is_finite() && rate_hz > 0.0) {
        return Err(SensorError::BadValue {
            name: String::new(),
            what: "sample rate",
        });
    }
    let dt = 1.0 / rate_hz;
    let count = (duration_s / dt).floor() as usize;
    Ok((0..=count).map(|i| i as f64 * dt).collect())
}

/// Synthetic accelerometer data for a mass-spring-damper, from its trace.
///
/// The acceleration is `x'' = -(c x' + k x) / m`, the exact value the
/// simulation integrates, so the IMU reading is noise-free. The trace must be
/// sampled at the sensor's own times, so the caller passes the same times to
/// `twin_sim::simulate`.
pub fn imu_from_trace(
    body: &MassSpringDamper,
    trace: &Trace,
) -> Result<Vec<ImuSample>, SensorError> {
    if trace.times.len() != trace.states.len() {
        return Err(SensorError::BadTrace("times and states differ in length"));
    }
    Ok(trace
        .times
        .iter()
        .zip(&trace.states)
        .map(|(&time_s, [x, v])| ImuSample {
            time_s,
            accel_m_s2: -(body.damping * v + body.stiffness * x) / body.mass,
        })
        .collect())
}

/// Depth image of a triangle mesh seen by a pinhole camera, row by row, from
/// the top-left pixel. The camera looks along the sensor's +Z axis; `fov_deg`
/// is the horizontal field of view. Each entry is the distance in metres along
/// +Z of the nearest surface at that pixel, or infinity where nothing is seen.
///
/// No occlusion beyond the depth test, no lighting, and triangles with any
/// corner behind the camera are skipped (they are not clipped).
pub fn camera_depth(
    triangles: &[[[f64; 3]; 3]],
    mount: &Mount,
    fov_deg: f64,
    width: u32,
    height: u32,
) -> Vec<f32> {
    let (w, h) = (width as usize, height as usize);
    let mut depth = vec![f32::INFINITY; w * h];
    let r = rotation_from_euler(mount.euler_rad);
    let t = mount.position_m;
    let focal = (w as f64 / 2.0) / (fov_deg.to_radians() / 2.0).tan();
    let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
    let to_sensor = |p: [f64; 3]| {
        let d = [p[0] - t[0], p[1] - t[1], p[2] - t[2]];
        [0, 1, 2].map(|i| r[0][i] * d[0] + r[1][i] * d[1] + r[2][i] * d[2])
    };
    // Twice the signed area of (a, b, c) in pixel space.
    let edge = |a: [f64; 2], b: [f64; 2], c: [f64; 2]| {
        (c[0] - a[0]) * (b[1] - a[1]) - (c[1] - a[1]) * (b[0] - a[0])
    };
    for tri in triangles {
        let p = tri.map(to_sensor);
        if p.iter().any(|v| v[2] <= 1e-9) {
            continue;
        }
        let q = p.map(|v| [cx + focal * v[0] / v[2], cy - focal * v[1] / v[2]]);
        let area = edge(q[0], q[1], q[2]);
        if area.abs() < 1e-12 {
            continue;
        }
        let min_x = q
            .iter()
            .map(|v| v[0])
            .fold(f64::INFINITY, f64::min)
            .floor()
            .max(0.0);
        let max_x = q
            .iter()
            .map(|v| v[0])
            .fold(f64::NEG_INFINITY, f64::max)
            .ceil()
            .min(w as f64 - 1.0);
        let min_y = q
            .iter()
            .map(|v| v[1])
            .fold(f64::INFINITY, f64::min)
            .floor()
            .max(0.0);
        let max_y = q
            .iter()
            .map(|v| v[1])
            .fold(f64::NEG_INFINITY, f64::max)
            .ceil()
            .min(h as f64 - 1.0);
        if min_x > max_x || min_y > max_y {
            continue;
        }
        for py in min_y as usize..=max_y as usize {
            for px in min_x as usize..=max_x as usize {
                let s = [px as f64 + 0.5, py as f64 + 0.5];
                let w0 = edge(q[1], q[2], s) / area;
                let w1 = edge(q[2], q[0], s) / area;
                let w2 = edge(q[0], q[1], s) / area;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                // Depth is linear in 1/z on the screen, not in z.
                let inv_z = w0 / p[0][2] + w1 / p[1][2] + w2 / p[2][2];
                let z = (1.0 / inv_z) as f32;
                let cell = &mut depth[py * w + px];
                if z < *cell {
                    *cell = z;
                }
            }
        }
    }
    depth
}

/// Points on a triangle mesh, `per_edge` steps along each edge of every
/// triangle. Deterministic: the same mesh and `per_edge` give the same points.
/// Vertices are included. Spacing is roughly `edge length / per_edge`.
pub fn sample_surface(triangles: &[[[f64; 3]; 3]], per_edge: usize) -> Vec<[f64; 3]> {
    let n = per_edge.max(1);
    let mut out = Vec::new();
    for [a, b, c] in triangles {
        for i in 0..=n {
            for j in 0..=(n - i) {
                let (u, v) = (i as f64 / n as f64, j as f64 / n as f64);
                let w = 1.0 - u - v;
                out.push([0, 1, 2].map(|k| w * a[k] + u * b[k] + v * c[k]));
            }
        }
    }
    out
}

/// What a lidar sees: world-frame surface points, expressed in the sensor
/// frame, within the sensor's range.
///
/// The sensor pose is `mount` in the world frame, with Euler angles applied
/// as `R = Rz(yaw) · Ry(pitch) · Rx(roll)` to `[roll, pitch, yaw]`.
///
/// Simplification: every in-range point is returned. There is no occlusion or
/// beam pattern, so the scan contains back faces too. Good enough for
/// registration tests; not a model of a real lidar.
pub fn lidar_returns(world_points: &[[f64; 3]], mount: &Mount, range_m: f64) -> Vec<[f64; 3]> {
    let r = rotation_from_euler(mount.euler_rad);
    let t = mount.position_m;
    world_points
        .iter()
        .map(|p| {
            let d = [p[0] - t[0], p[1] - t[1], p[2] - t[2]];
            // Sensor frame = R^T · (p − t).
            [0, 1, 2].map(|i| r[0][i] * d[0] + r[1][i] * d[1] + r[2][i] * d[2])
        })
        .filter(|q| (q[0] * q[0] + q[1] * q[1] + q[2] * q[2]).sqrt() <= range_m)
        .collect()
}

/// Random error of a lidar's range. Each return is pushed along its own ray by
/// a zero-mean Gaussian error of standard deviation `sigma_m`. The generator is
/// seeded, so the same seed always gives the same scan.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RangeNoise {
    pub sigma_m: f64,
    pub seed: u64,
}

/// [`lidar_returns`] with range noise. With `sigma_m` of zero the result is the
/// noise-free scan.
pub fn lidar_returns_with_noise(
    world_points: &[[f64; 3]],
    mount: &Mount,
    range_m: f64,
    noise: RangeNoise,
) -> Result<Vec<[f64; 3]>, SensorError> {
    add_range_noise(lidar_returns(world_points, mount, range_m), noise)
}

/// Where a lidar's beams point, in the sensor frame: `azimuth_count` beams
/// spread evenly around the full circle, at `elevation_count` elevations spread
/// evenly from `elevation_min_rad` to `elevation_max_rad` (both inclusive).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BeamPattern {
    pub azimuth_count: usize,
    pub elevation_count: usize,
    pub elevation_min_rad: f64,
    pub elevation_max_rad: f64,
}

impl BeamPattern {
    /// The pattern for a lidar with `beams` returns per sweep: that many
    /// azimuths, in 8 rows spread over ±15 degrees of elevation.
    pub fn for_beams(beams: u32) -> Self {
        let limit = 15f64.to_radians();
        Self {
            azimuth_count: beams as usize,
            elevation_count: 8,
            elevation_min_rad: -limit,
            elevation_max_rad: limit,
        }
    }

    fn validate(&self) -> Result<(), SensorError> {
        let bad = |what| SensorError::BadValue {
            name: "lidar beam pattern".into(),
            what,
        };
        if self.azimuth_count == 0 {
            return Err(bad("azimuth count"));
        }
        if self.elevation_count == 0 {
            return Err(bad("elevation count"));
        }
        if !(self.elevation_min_rad.is_finite()
            && self.elevation_max_rad.is_finite()
            && self.elevation_min_rad <= self.elevation_max_rad)
        {
            return Err(bad("elevation range"));
        }
        Ok(())
    }
}

/// A beam scan of a mesh: each beam from the sensor returns the nearest surface
/// it hits within `range_m`, so surfaces behind others are occluded and beams
/// that meet nothing return nothing. Returns are in the sensor frame. With
/// `noise`, each return also gets the range error of [`RangeNoise`].
///
/// Beams are cast into the triangles one by one (no acceleration structure),
/// which is fine for the small designs the app builds.
pub fn lidar_beam_scan(
    triangles: &[[[f64; 3]; 3]],
    mount: &Mount,
    range_m: f64,
    pattern: &BeamPattern,
    noise: Option<RangeNoise>,
) -> Result<Vec<[f64; 3]>, SensorError> {
    if !(range_m.is_finite() && range_m > 0.0) {
        return Err(SensorError::BadValue {
            name: "lidar".into(),
            what: "range",
        });
    }
    pattern.validate()?;
    let r = rotation_from_euler(mount.euler_rad);
    let origin = mount.position_m;
    let mut returns = Vec::new();
    for row in 0..pattern.elevation_count {
        let el = if pattern.elevation_count == 1 {
            pattern.elevation_min_rad
        } else {
            let f = row as f64 / (pattern.elevation_count - 1) as f64;
            pattern.elevation_min_rad + f * (pattern.elevation_max_rad - pattern.elevation_min_rad)
        };
        for col in 0..pattern.azimuth_count {
            let az = std::f64::consts::TAU * col as f64 / pattern.azimuth_count as f64;
            // Beam direction in the sensor frame, then in the world frame.
            let local = [el.cos() * az.cos(), el.cos() * az.sin(), el.sin()];
            let world =
                [0, 1, 2].map(|i| r[i][0] * local[0] + r[i][1] * local[1] + r[i][2] * local[2]);
            let nearest = triangles
                .iter()
                .filter_map(|t| ray_triangle(origin, world, t))
                .filter(|&t| t <= range_m)
                .fold(None, |best: Option<f64>, t| {
                    Some(best.map_or(t, |b| b.min(t)))
                });
            if let Some(t) = nearest {
                returns.push(local.map(|d| d * t));
            }
        }
    }
    Ok(match noise {
        Some(noise) => add_range_noise(returns, noise)?,
        None => returns,
    })
}

/// Distance along a ray from `origin` in direction `dir` to triangle `tri`
/// (Möller–Trumbore), or `None` if the ray misses it or it is behind the origin.
/// Both sides of a triangle count.
fn ray_triangle(origin: [f64; 3], dir: [f64; 3], tri: &[[f64; 3]; 3]) -> Option<f64> {
    const EPS: f64 = 1e-12;
    let e1 = sub(tri[1], tri[0]);
    let e2 = sub(tri[2], tri[0]);
    let p = cross(dir, e2);
    let det = dot(e1, p);
    if det.abs() < EPS {
        return None;
    }
    let inv = 1.0 / det;
    let s = sub(origin, tri[0]);
    let u = dot(s, p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = cross(s, e1);
    let v = dot(dir, q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = dot(e2, q) * inv;
    (t > EPS).then_some(t)
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

/// Acceleration noise of an IMU: each sample gets a zero-mean Gaussian error of
/// standard deviation `sigma_m_s2`, from a seeded generator (repeatable).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AccelNoise {
    pub sigma_m_s2: f64,
    pub seed: u64,
}

/// [`imu_from_trace`] with acceleration noise. Zero sigma gives the noise-free
/// readings.
pub fn imu_from_trace_with_noise(
    body: &MassSpringDamper,
    trace: &Trace,
    noise: AccelNoise,
) -> Result<Vec<ImuSample>, SensorError> {
    check_sigma(noise.sigma_m_s2, "IMU acceleration noise")?;
    let mut rng = GaussianRng::new(noise.seed);
    Ok(imu_from_trace(body, trace)?
        .into_iter()
        .map(|s| ImuSample {
            accel_m_s2: s.accel_m_s2 + noise.sigma_m_s2 * rng.next_normal(),
            ..s
        })
        .collect())
}

fn check_sigma(sigma: f64, name: &str) -> Result<(), SensorError> {
    if sigma.is_finite() && sigma >= 0.0 {
        Ok(())
    } else {
        Err(SensorError::BadValue {
            name: name.into(),
            what: "standard deviation",
        })
    }
}

/// Push each point along its ray by a seeded Gaussian range error.
fn add_range_noise(points: Vec<[f64; 3]>, noise: RangeNoise) -> Result<Vec<[f64; 3]>, SensorError> {
    check_sigma(noise.sigma_m, "lidar range noise")?;
    let mut rng = GaussianRng::new(noise.seed);
    Ok(points
        .into_iter()
        .map(|q| {
            let r = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2]).sqrt();
            if r == 0.0 {
                return q;
            }
            let scale = (r + noise.sigma_m * rng.next_normal()) / r;
            q.map(|x| x * scale)
        })
        .collect())
}

/// A small seeded generator (splitmix64) with Gaussian samples by Box–Muller.
/// Not for security: it only makes simulated noise repeatable.
struct GaussianRng {
    state: u64,
}

impl GaussianRng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in the open interval (0, 1).
    fn next_uniform(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 0.5) / 9_007_199_254_740_992.0
    }

    /// A standard normal sample.
    fn next_normal(&mut self) -> f64 {
        let u1 = self.next_uniform();
        let u2 = self.next_uniform();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

fn rotation_from_euler([roll, pitch, yaw]: [f64; 3]) -> [[f64; 3]; 3] {
    let (sr, cr) = roll.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    let (sy, cy) = yaw.sin_cos();
    [
        [cy * cp, cy * sp * sr - sy * cr, cy * sp * cr + sy * sr],
        [sy * cp, sy * sp * sr + cy * cr, sy * sp * cr - cy * sr],
        [-sp, cp * sr, cp * cr],
    ]
}

/// A rigid pose `target ≈ rotation · source + translation`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Alignment {
    pub rotation: [[f64; 3]; 3],
    pub translation: [f64; 3],
    /// Fraction of source points with a close target point.
    pub fitness: f64,
    /// RMSE over those points, in metres.
    pub rmse_m: f64,
}

/// Align `source` onto `target` with point-to-point ICP, starting from the
/// pose `initial` (rotation, translation).
///
/// ICP only finds the true alignment from a nearby start. Pass a prior from
/// odometry or the IMU. Starting from the identity is enough for small motions
/// only: on a dense cube scan, a 2° yaw with 4 cm of shift stalls in a local
/// minimum with an RMSE near 2 cm. `max_distance_m` rejects correspondences
/// farther than that; it should be a little larger than the expected motion
/// left after the prior.
pub fn register_point_clouds(
    source: &[[f64; 3]],
    target: &[[f64; 3]],
    initial: ([[f64; 3]; 3], [f64; 3]),
    max_distance_m: f64,
) -> Result<Alignment, SensorError> {
    use tpt_percept_cloud::cloud::PointCloud;
    use tpt_percept_register::icp::{IcpParams, IcpVariant, icp};

    let params = IcpParams {
        max_correspondence_distance: max_distance_m,
        ..IcpParams::default()
    };
    let (init_rotation, init_translation) = initial;
    let result = icp(
        &PointCloud::from_points(source.to_vec()),
        &PointCloud::from_points(target.to_vec()),
        None,
        IcpVariant::PointToPoint,
        init_rotation,
        init_translation,
        &params,
    )
    .map_err(|e| SensorError::Registration(format!("{e:?}")))?;
    Ok(Alignment {
        rotation: result.rotation,
        translation: result.translation,
        fitness: result.fitness,
        rmse_m: result.rmse,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use twin_core::{Scenario, Twin};

    fn imu() -> Sensor {
        Sensor {
            name: "imu-1".into(),
            mount: Mount::default(),
            kind: SensorKind::Imu {
                sample_rate_hz: 100.0,
            },
        }
    }

    #[test]
    fn valid_sensors_pass_and_round_trip_through_json() {
        let cam = Sensor {
            name: "front".into(),
            mount: Mount {
                position_m: [0.1, 0.0, 0.2],
                euler_rad: [0.0, 0.3, 0.0],
            },
            kind: SensorKind::Camera {
                fov_deg: 60.0,
                width_px: 640,
                height_px: 480,
            },
        };
        for s in [imu(), cam] {
            let back = Sensor::from_json(&s.to_json()).unwrap();
            assert_eq!(back, s);
        }
    }

    #[test]
    fn bad_settings_are_rejected() {
        let mut s = imu();
        s.name = "  ".into();
        assert_eq!(s.validated(), Err(SensorError::EmptyName));

        let mut s = imu();
        s.kind = SensorKind::Imu {
            sample_rate_hz: 0.0,
        };
        assert!(matches!(s.validated(), Err(SensorError::BadValue { .. })));

        let mut s = imu();
        s.kind = SensorKind::Camera {
            fov_deg: 180.0,
            width_px: 1,
            height_px: 1,
        };
        assert!(matches!(
            s.validated(),
            Err(SensorError::BadFieldOfView { .. })
        ));

        let mut s = imu();
        s.kind = SensorKind::Lidar {
            range_m: 10.0,
            beams: 0,
        };
        assert!(matches!(s.validated(), Err(SensorError::BadCount { .. })));

        let mut s = imu();
        s.mount.position_m[1] = f64::NAN;
        assert_eq!(s.validated(), Err(SensorError::NotFinite));

        assert!(Sensor::from_json("{\"name\":\"x\"}").is_err());
    }

    #[test]
    fn sample_times_cover_the_duration_at_the_rate() {
        let t = sample_times(4.0, 1.0).unwrap();
        assert_eq!(t, vec![0.0, 0.25, 0.5, 0.75, 1.0]);
        assert!(sample_times(0.0, 1.0).is_err());
    }

    #[test]
    fn imu_matches_the_spring_acceleration() {
        // Undamped, k = 4, m = 1, x0 = 1: a(0) = -4, and a = -4 x at all times.
        let mut twin = Twin::new("spring").unwrap();
        twin.set_parameter(twin_sim::MASS, 1.0).unwrap();
        twin.set_parameter(twin_sim::STIFFNESS, 4.0).unwrap();
        twin.set_parameter(twin_sim::DAMPING, 0.5).unwrap();
        let scenario = Scenario {
            name: "release".into(),
            duration_s: 2.0,
            initial_conditions: BTreeMap::from([
                (twin_sim::POSITION0.to_string(), 1.0),
                (twin_sim::VELOCITY0.to_string(), 0.0),
            ]),
            controller: None,
        };
        let times = sample_times(10.0, 2.0).unwrap();
        let trace = twin_sim::simulate(&twin, &scenario, &times).unwrap();
        let body = MassSpringDamper::from_twin(&twin).unwrap();
        let samples = imu_from_trace(&body, &trace).unwrap();

        assert_eq!(samples.len(), times.len());
        assert!((samples[0].accel_m_s2 + 4.0).abs() < 1e-12);
        // Check the reading against the state it came from, at every sample.
        for (s, [x, v]) in samples.iter().zip(&trace.states) {
            assert!((s.accel_m_s2 + (0.5 * v + 4.0 * x)).abs() < 1e-12);
        }
    }

    #[test]
    fn mismatched_trace_is_rejected() {
        let body = MassSpringDamper {
            mass: 1.0,
            stiffness: 1.0,
            damping: 0.0,
        };
        let trace = Trace {
            times: vec![0.0, 1.0],
            states: vec![[0.0, 0.0]],
        };
        assert!(imu_from_trace(&body, &trace).is_err());
    }

    const IDENTITY: ([[f64; 3]; 3], [f64; 3]) = (
        [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        [0.0; 3],
    );

    fn spring_twin(x0: f64, v0: f64, duration: f64) -> (Twin, twin_core::Scenario) {
        let mut twin = Twin::new("spring").unwrap();
        twin.set_parameter(twin_sim::MASS, 1.0).unwrap();
        twin.set_parameter(twin_sim::STIFFNESS, 4.0).unwrap();
        twin.set_parameter(twin_sim::DAMPING, 0.0).unwrap();
        let scenario = Scenario {
            name: "move".into(),
            duration_s: duration,
            initial_conditions: BTreeMap::from([
                (twin_sim::POSITION0.to_string(), x0),
                (twin_sim::VELOCITY0.to_string(), v0),
            ]),
            controller: None,
        };
        (twin, scenario)
    }

    #[test]
    fn integrated_imu_matches_the_simulated_motion() {
        let (twin, scenario) = spring_twin(0.0, 0.2, 1.0);
        let times = sample_times(100.0, 1.0).unwrap();
        let trace = twin_sim::simulate(&twin, &scenario, &times).unwrap();
        let body = MassSpringDamper::from_twin(&twin).unwrap();
        let samples = imu_from_trace(&body, &trace).unwrap();
        let motion = integrate_imu(&samples, 0.2).unwrap();
        for (m, [x, v]) in motion.iter().zip(&trace.states) {
            assert!(
                (m.displacement_m - x).abs() < 1e-3,
                "x {} vs {}",
                m.displacement_m,
                x
            );
            assert!(
                (m.velocity_m_s - v).abs() < 1e-3,
                "v {} vs {}",
                m.velocity_m_s,
                v
            );
        }
    }

    #[test]
    fn imu_integration_rejects_bad_input() {
        assert!(integrate_imu(&[], 0.0).is_err());
        let dup = [
            ImuSample {
                time_s: 0.0,
                accel_m_s2: 0.0,
            },
            ImuSample {
                time_s: 0.0,
                accel_m_s2: 0.0,
            },
        ];
        assert!(integrate_imu(&dup, 0.0).is_err());
    }

    /// An L-shaped set of points on two planes. It is not symmetric, so the
    /// alignment is unique.
    fn l_shape() -> Vec<[f64; 3]> {
        let mut pts = Vec::new();
        for i in 0..12 {
            for j in 0..12 {
                let (u, v) = (i as f64 * 0.1, j as f64 * 0.1);
                pts.push([u, v, 0.0]);
                if i < 4 {
                    pts.push([u, 0.0, v + 0.1]);
                }
            }
        }
        pts
    }

    #[test]
    fn registration_recovers_a_known_rigid_motion() {
        let target = l_shape();
        // Rotate 5 degrees about Z and shift by a few centimetres.
        let (s, c) = (5f64.to_radians().sin(), 5f64.to_radians().cos());
        let t = [0.03, -0.02, 0.01];
        let source: Vec<[f64; 3]> = target
            .iter()
            .map(|p| {
                // Invert the motion: source = R^T (target - t).
                let (x, y) = (p[0] - t[0], p[1] - t[1]);
                [c * x + s * y, -s * x + c * y, p[2] - t[2]]
            })
            .collect();

        let a = register_point_clouds(&source, &target, IDENTITY, 0.5).unwrap();
        assert!(a.fitness > 0.99, "fitness {}", a.fitness);
        assert!(a.rmse_m < 1e-4, "rmse {}", a.rmse_m);
        let applied = |p: [f64; 3]| {
            let r = a.rotation;
            [
                r[0][0] * p[0] + r[0][1] * p[1] + r[0][2] * p[2] + a.translation[0],
                r[1][0] * p[0] + r[1][1] * p[1] + r[1][2] * p[2] + a.translation[1],
                r[2][0] * p[0] + r[2][1] * p[1] + r[2][2] * p[2] + a.translation[2],
            ]
        };
        for (moved, want) in source.iter().zip(&target).step_by(17) {
            let got = applied(*moved);
            for k in 0..3 {
                assert!(
                    (got[k] - want[k]).abs() < 1e-3,
                    "axis {k}: {} vs {}",
                    got[k],
                    want[k]
                );
            }
        }
    }

    #[test]
    fn identity_start_converges_for_small_motion() {
        let world = sample_surface(&cube([0.0; 3], 0.5), 24);
        let first = Mount {
            position_m: [-2.0, 0.0, 0.0],
            euler_rad: [0.0; 3],
        };
        let second = Mount {
            position_m: [-2.0, 0.01, 0.0],
            euler_rad: [0.0; 3],
        };
        let a = register_point_clouds(
            &lidar_returns(&world, &second, 10.0),
            &lidar_returns(&world, &first, 10.0),
            IDENTITY,
            0.3,
        )
        .unwrap();
        assert!(
            (a.translation[1] - 0.01).abs() < 1e-3,
            "dy {}",
            a.translation[1]
        );
    }

    #[test]
    fn imu_displacement_is_a_usable_registration_prior() {
        // The sensor moves along Y by the displacement the IMU reports over
        // one second. No rotation prior is available (no gyro model), and the
        // motion has no yaw, so the translation prior alone should suffice.
        let (twin, scenario) = spring_twin(0.0, 0.2, 1.0);
        let times = sample_times(100.0, 1.0).unwrap();
        let trace = twin_sim::simulate(&twin, &scenario, &times).unwrap();
        let body = MassSpringDamper::from_twin(&twin).unwrap();
        let motion = integrate_imu(&imu_from_trace(&body, &trace).unwrap(), 0.2).unwrap();
        let dy = motion.last().unwrap().displacement_m;

        let world = sample_surface(&cube([0.0; 3], 0.5), 24);
        let first = Mount {
            position_m: [-2.0, 0.0, 0.0],
            euler_rad: [0.0; 3],
        };
        let second = Mount {
            position_m: [-2.0, dy, 0.0],
            euler_rad: [0.0; 3],
        };
        let a = register_point_clouds(
            &lidar_returns(&world, &second, 10.0),
            &lidar_returns(&world, &first, 10.0),
            (IDENTITY.0, [0.0, dy - 0.002, 0.0]),
            0.3,
        )
        .unwrap();
        assert!(a.rmse_m < 2e-3, "rmse {}", a.rmse_m);
        assert!(
            (a.translation[1] - dy).abs() < 3e-3,
            "dy {} vs {dy}",
            a.translation[1]
        );
    }

    #[test]
    fn registration_rejects_empty_clouds() {
        assert!(register_point_clouds(&[], &l_shape(), IDENTITY, 0.5).is_err());
    }

    /// Six faces of an axis-aligned cube with corners at ±half.
    fn cube(center: [f64; 3], half: f64) -> Vec<[[f64; 3]; 3]> {
        let c = |x: f64, y: f64, z: f64| {
            [
                center[0] + x * half,
                center[1] + y * half,
                center[2] + z * half,
            ]
        };
        let quads = [
            [
                c(-1., -1., -1.),
                c(1., -1., -1.),
                c(1., 1., -1.),
                c(-1., 1., -1.),
            ],
            [
                c(-1., -1., 1.),
                c(1., -1., 1.),
                c(1., 1., 1.),
                c(-1., 1., 1.),
            ],
            [
                c(-1., -1., -1.),
                c(1., -1., -1.),
                c(1., -1., 1.),
                c(-1., -1., 1.),
            ],
            [
                c(-1., 1., -1.),
                c(1., 1., -1.),
                c(1., 1., 1.),
                c(-1., 1., 1.),
            ],
            [
                c(-1., -1., -1.),
                c(-1., 1., -1.),
                c(-1., 1., 1.),
                c(-1., -1., 1.),
            ],
            [
                c(1., -1., -1.),
                c(1., 1., -1.),
                c(1., 1., 1.),
                c(1., -1., 1.),
            ],
        ];
        quads
            .iter()
            .flat_map(|q| [[q[0], q[1], q[2]], [q[0], q[2], q[3]]])
            .collect()
    }

    #[test]
    fn surface_samples_lie_on_the_mesh_and_include_vertices() {
        let tri = [[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]];
        let pts = sample_surface(&tri, 4);
        assert_eq!(pts.len(), 15); // (n+1)(n+2)/2
        assert!(
            pts.iter()
                .all(|p| p[2] == 0.0 && p[0] >= 0.0 && p[1] >= 0.0 && p[0] + p[1] <= 1.0 + 1e-12)
        );
        assert!(pts.contains(&[1.0, 0.0, 0.0]) && pts.contains(&[0.0, 1.0, 0.0]));
    }

    #[test]
    fn a_camera_sees_a_square_at_its_distance_and_misses_the_rest() {
        // A unit square 2 m in front of the camera, facing it.
        let square = [
            [[-1.0, -1.0, 2.0], [1.0, -1.0, 2.0], [1.0, 1.0, 2.0]],
            [[-1.0, -1.0, 2.0], [1.0, 1.0, 2.0], [-1.0, 1.0, 2.0]],
        ];
        let mount = Mount::default();
        let depth = camera_depth(&square, &mount, 60.0, 16, 16);
        let centre = depth[8 * 16 + 8];
        assert!((centre - 2.0).abs() < 1e-4, "{centre}");
        let hit = depth.iter().filter(|d| d.is_finite()).count();
        assert!(hit > 0 && hit < depth.len(), "hit {hit} of {}", depth.len());
        // Corners of the image are outside the square.
        assert!(depth[0].is_infinite());

        // Moving the camera closer to the square shortens the depth.
        let closer = Mount {
            position_m: [0.0, 0.0, 1.0],
            ..Mount::default()
        };
        let depth = camera_depth(&square, &closer, 60.0, 16, 16);
        assert!((depth[8 * 16 + 8] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn returns_are_in_the_sensor_frame_and_range_limited() {
        let mount = Mount {
            position_m: [1.0, 0.0, 0.0],
            euler_rad: [0.0, 0.0, std::f64::consts::FRAC_PI_2],
        };
        // A point 2 m in front of the sensor's +X axis in world terms.
        let pts = [[3.0, 0.0, 0.0], [100.0, 0.0, 0.0]];
        let back = lidar_returns(&pts, &mount, 10.0);
        assert_eq!(back.len(), 1);
        // Yaw of 90 degrees: world +X is sensor −Y.
        assert!(
            (back[0][0]).abs() < 1e-12 && (back[0][1] + 2.0).abs() < 1e-12,
            "{:?}",
            back[0]
        );
    }

    #[test]
    fn registering_two_scans_recovers_the_sensor_motion() {
        let block = cube([0.0, 0.0, 0.0], 0.5);
        let world = sample_surface(&block, 24);
        let range = 10.0;
        let first = Mount {
            position_m: [-2.0, 0.0, 0.0],
            euler_rad: [0.0; 3],
        };
        // The sensor moves 4 cm along +Y and yaws by 2 degrees.
        let second = Mount {
            position_m: [-2.0, 0.04, 0.0],
            euler_rad: [0.0, 0.0, 2f64.to_radians()],
        };
        let scan_a = lidar_returns(&world, &first, range);
        let scan_b = lidar_returns(&world, &second, range);
        assert!(scan_a.len() > 100);

        // Registering B onto A gives the pose change of the sensor, expressed
        // in the first sensor frame. Check it against the pose change itself.
        // A prior within about 1 cm and 0.3 degrees, as odometry would give.
        let prior_yaw = 1.7f64.to_radians();
        let prior_r = [
            [prior_yaw.cos(), -prior_yaw.sin(), 0.0],
            [prior_yaw.sin(), prior_yaw.cos(), 0.0],
            [0.0, 0.0, 1.0],
        ];
        let a = register_point_clouds(&scan_b, &scan_a, (prior_r, [0.0, 0.03, 0.0]), 0.3).unwrap();
        assert!(a.fitness > 0.95, "fitness {}", a.fitness);
        assert!(a.rmse_m < 2e-3, "rmse {}", a.rmse_m);
        // With the first sensor frame as reference, the target is the second
        // pose: b-frame points map back by R_B and the offset t_B − t_A.
        let yaw = a.rotation[1][0].atan2(a.rotation[0][0]).to_degrees();
        assert!((yaw - 2.0).abs() < 0.1, "yaw {yaw}");
        assert!(
            (a.translation[1] - 0.04).abs() < 2e-3,
            "dy {}",
            a.translation[1]
        );
    }

    #[test]
    fn range_noise_is_seeded_and_zero_sigma_is_noise_free() {
        let world = sample_surface(&cube([0.0, 0.0, 5.0], 1.0), 12);
        let mount = Mount::default();
        let clean = lidar_returns(&world, &mount, 10.0);
        let zero = RangeNoise {
            sigma_m: 0.0,
            seed: 7,
        };
        assert_eq!(
            lidar_returns_with_noise(&world, &mount, 10.0, zero).unwrap(),
            clean
        );
        let noisy = RangeNoise {
            sigma_m: 0.02,
            seed: 7,
        };
        let a = lidar_returns_with_noise(&world, &mount, 10.0, noisy).unwrap();
        let b = lidar_returns_with_noise(&world, &mount, 10.0, noisy).unwrap();
        assert_eq!(a, b, "same seed, same scan");
        let other = RangeNoise {
            sigma_m: 0.02,
            seed: 8,
        };
        assert_ne!(
            lidar_returns_with_noise(&world, &mount, 10.0, other).unwrap(),
            a
        );
        assert!(
            lidar_returns_with_noise(
                &world,
                &mount,
                10.0,
                RangeNoise {
                    sigma_m: -1.0,
                    seed: 1
                }
            )
            .is_err()
        );
    }

    #[test]
    fn range_error_has_the_requested_spread_and_no_bias() {
        let world = sample_surface(&cube([0.0, 0.0, 5.0], 1.0), 40);
        let mount = Mount::default();
        let clean = lidar_returns(&world, &mount, 10.0);
        let sigma = 0.05;
        let noisy = lidar_returns_with_noise(
            &world,
            &mount,
            10.0,
            RangeNoise {
                sigma_m: sigma,
                seed: 42,
            },
        )
        .unwrap();
        assert_eq!(clean.len(), noisy.len());
        let errors: Vec<f64> = clean
            .iter()
            .zip(&noisy)
            .map(|(c, n)| {
                let rc = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt();
                let rn = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                rn - rc
            })
            .collect();
        let n = errors.len() as f64;
        let mean = errors.iter().sum::<f64>() / n;
        let var = errors.iter().map(|e| (e - mean).powi(2)).sum::<f64>() / n;
        // The mean of n samples has spread sigma / sqrt(n); allow four of those.
        assert!(mean.abs() < 4.0 * sigma / n.sqrt(), "bias {mean}");
        assert!(
            (var.sqrt() - sigma).abs() < 0.05 * sigma,
            "spread {}",
            var.sqrt()
        );
    }

    fn one_beam_forward() -> BeamPattern {
        BeamPattern {
            azimuth_count: 4,
            elevation_count: 1,
            elevation_min_rad: 0.0,
            elevation_max_rad: 0.0,
        }
    }

    #[test]
    fn a_beam_returns_the_nearest_surface_and_nothing_behind_it_shows() {
        let near = cube([5.0, 0.0, 0.0], 1.0);
        let mut both = near.clone();
        both.extend(cube([7.0, 0.0, 0.0], 1.0));
        let mount = Mount::default();
        let pattern = one_beam_forward();
        let alone = lidar_beam_scan(&near, &mount, 10.0, &pattern, None).unwrap();
        let behind = lidar_beam_scan(&both, &mount, 10.0, &pattern, None).unwrap();
        assert_eq!(alone.len(), 1, "only the forward beam meets the cube");
        // The near face is at x = 4. The cube behind it at x = 6..8 is hidden.
        assert!((alone[0][0] - 4.0).abs() < 1e-9 && alone[0][1].abs() < 1e-9);
        assert_eq!(behind, alone, "the far cube is occluded");
    }

    #[test]
    fn beams_stop_at_the_range_limit() {
        let triangles = cube([5.0, 0.0, 0.0], 1.0);
        let mount = Mount::default();
        let pattern = one_beam_forward();
        assert!(
            lidar_beam_scan(&triangles, &mount, 3.0, &pattern, None)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            lidar_beam_scan(&triangles, &mount, 4.5, &pattern, None)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn the_mount_rotates_the_beams_into_the_world() {
        // Yaw of pi turns the sensor's forward beam to world -x, where the cube is.
        let triangles = cube([-5.0, 0.0, 0.0], 1.0);
        let mount = Mount {
            position_m: [0.0; 3],
            euler_rad: [0.0, 0.0, std::f64::consts::PI],
        };
        let hits = lidar_beam_scan(&triangles, &mount, 10.0, &one_beam_forward(), None).unwrap();
        assert_eq!(hits.len(), 1);
        assert!((hits[0][0] - 4.0).abs() < 1e-9, "{hits:?}");
    }

    #[test]
    fn beam_scan_rejects_bad_patterns_and_ranges() {
        let triangles = cube([5.0, 0.0, 0.0], 1.0);
        let mount = Mount::default();
        let mut bad = one_beam_forward();
        bad.azimuth_count = 0;
        assert!(lidar_beam_scan(&triangles, &mount, 10.0, &bad, None).is_err());
        let mut inverted = one_beam_forward();
        inverted.elevation_min_rad = 0.5;
        inverted.elevation_max_rad = -0.5;
        assert!(lidar_beam_scan(&triangles, &mount, 10.0, &inverted, None).is_err());
        assert!(lidar_beam_scan(&triangles, &mount, 0.0, &one_beam_forward(), None).is_err());
    }

    #[test]
    fn the_default_pattern_has_one_azimuth_per_beam() {
        // Inside a closed unit-half cube every beam meets a wall, at a distance
        // between 1 (the side walls) and sqrt(3) (the corners). The count is
        // one return per beam, azimuths times elevations.
        let pattern = BeamPattern::for_beams(360);
        assert_eq!(pattern.azimuth_count, 360);
        assert_eq!(pattern.elevation_count, 8);
        let scan = lidar_beam_scan(
            &cube([0.0; 3], 1.0),
            &Mount::default(),
            10.0,
            &pattern,
            None,
        )
        .unwrap();
        assert_eq!(scan.len(), 360 * 8);
        for p in &scan {
            let r = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
            assert!(r >= 1.0 - 1e-9 && r <= 3f64.sqrt() + 1e-9, "{r}");
        }
    }

    #[test]
    fn imu_noise_is_seeded_centred_and_zero_sigma_is_noise_free() {
        let body = MassSpringDamper {
            mass: 2.0,
            stiffness: 8.0,
            damping: 0.0,
        };
        let n = 4000;
        let trace = Trace {
            times: (0..n).map(|i| i as f64 * 0.01).collect(),
            states: vec![[1.0, 0.0]; n],
        };
        let clean = imu_from_trace(&body, &trace).unwrap();
        let zero = AccelNoise {
            sigma_m_s2: 0.0,
            seed: 3,
        };
        assert_eq!(
            imu_from_trace_with_noise(&body, &trace, zero).unwrap(),
            clean
        );
        let noisy = AccelNoise {
            sigma_m_s2: 0.1,
            seed: 3,
        };
        let a = imu_from_trace_with_noise(&body, &trace, noisy).unwrap();
        assert_eq!(a, imu_from_trace_with_noise(&body, &trace, noisy).unwrap());
        let errors: Vec<f64> = a
            .iter()
            .zip(&clean)
            .map(|(x, c)| x.accel_m_s2 - c.accel_m_s2)
            .collect();
        let mean = errors.iter().sum::<f64>() / n as f64;
        let sd = (errors.iter().map(|e| (e - mean).powi(2)).sum::<f64>() / n as f64).sqrt();
        assert!(mean.abs() < 4.0 * 0.1 / (n as f64).sqrt(), "bias {mean}");
        assert!((sd - 0.1).abs() < 0.005, "spread {sd}");
        assert!(
            imu_from_trace_with_noise(
                &body,
                &trace,
                AccelNoise {
                    sigma_m_s2: -1.0,
                    seed: 1
                }
            )
            .is_err()
        );
    }
}
