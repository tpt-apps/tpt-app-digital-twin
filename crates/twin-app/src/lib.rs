//! The project model behind the desktop app.
//!
//! A project is a directory holding a `TwinStore`. This type owns the store,
//! the list of twins in it, and the one twin being edited. Edits stay in
//! memory until `save`, so a session can be discarded without touching the
//! stored twin. The window in `main.rs` is a view over this type and holds no
//! state of its own, so this part is tested without a window.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ed25519_dalek::SigningKey;
use thiserror::Error;
use twin_collab::TwinReplica;
use twin_collab::sync::{SyncClient, SyncHost};
use twin_core::{Scenario, Twin, TwinError, TwinId};
use twin_geometry::Content;
use twin_geometry::design::{Design, DesignError};
use twin_geometry::model::{ModelSpec, Step, Value};
use twin_geometry::{Change, CommitId, History, HistoryError};
use twin_license::{License, LicenseVerifier};
use twin_package::PackageError;
use twin_sensors::{Mount, Sensor, SensorKind};
use twin_store::{StoreError, TwinStore, run_is_current, run_matches_parameters};

#[derive(Debug, Error)]
pub enum ProjectError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Twin(#[from] TwinError),
    #[error("no twin with that id in this project")]
    NotFound,
    #[error("no twin is open")]
    NoTwinOpen,
    #[error("`{0}` is not a number")]
    NotANumber(String),
    #[error("`{0}` has a value the twin refuses; fix it before saving")]
    RejectedValue(String),
    #[error(transparent)]
    History(#[from] HistoryError),
    #[error("the twin has no history yet; start one first")]
    NoHistory,
    #[error("save the twin before changing its history")]
    UnsavedTwin,
    #[error("the commit message is empty")]
    EmptyMessage,
    #[error(transparent)]
    Sensor(#[from] twin_sensors::SensorError),
    #[error("no sensor named `{0}`")]
    NoSensor(String),
    #[error("sensor `{0}` has no such setting")]
    NoSensorField(String),
    #[error(transparent)]
    Design(#[from] DesignError),
    #[error("the twin has no geometry yet; start geometry first")]
    NoDesign,
    #[error("the twin already has geometry")]
    DesignExists,
    #[error("commit the geometry before linking the twin to it")]
    UncommittedDesign,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Session(String),
    #[error(transparent)]
    Package(#[from] PackageError),
    #[error("export key: {0}")]
    Key(String),
    #[error("twin {0} was signed by a different key than before, so it was not imported")]
    SignerChanged(String),
    #[error("the branch name is empty")]
    EmptyBranchName,
}
/// A field of a scenario that the window can edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScenarioField {
    /// How long the run lasts, in seconds.
    Duration,
    /// An initial condition, by name (for example `position_m`).
    Initial(String),
}

/// Named time series to draw against a shared time axis.
#[derive(Debug, Clone, PartialEq)]
pub struct PlotData {
    pub title: String,
    pub times: Vec<f64>,
    /// Each series has one value per entry of `times`.
    pub series: Vec<(String, Vec<f64>)>,
}

/// A lidar scan's sensor name and its top-down points.
pub type LidarScanXy = (String, Vec<[f64; 2]>);

/// What the window plots: a stored run, or the recording of a sensor.
#[derive(Debug, Clone, PartialEq)]
pub enum PlotChoice {
    Run(String),
    Sensor(String),
    /// A lidar's saved scan, drawn top-down (see `Project::lidar_scan_xy`).
    Lidar(String),
}

/// A kind of sensor the window can add, with default settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewSensor {
    /// 50 Hz accelerometer.
    Imu,
    /// 60 degree field of view, 640 x 480 pixels.
    Camera,
    /// 30 m range, 360 beams.
    Lidar,
}

/// A setting of a sensor that the window can edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensorField {
    /// IMU sample rate, in Hz.
    SampleRate,
    /// Camera field of view, in degrees.
    FieldOfView,
    /// Camera image width, in pixels (a whole number).
    WidthPx,
    /// Camera image height, in pixels (a whole number).
    HeightPx,
    /// Lidar range, in metres.
    RangeM,
    /// Lidar beam count (a whole number).
    Beams,
    /// Mount position along axis 0, 1 or 2 (x, y, z), in metres.
    Position(usize),
}

/// The settings a sensor of this kind has, with the label each one shows.
pub fn sensor_fields(kind: &SensorKind) -> Vec<(&'static str, SensorField)> {
    let mut fields = match kind {
        SensorKind::Imu { .. } => vec![("sample rate (Hz)", SensorField::SampleRate)],
        SensorKind::Camera { .. } => vec![
            ("field of view (deg)", SensorField::FieldOfView),
            ("width (px)", SensorField::WidthPx),
            ("height (px)", SensorField::HeightPx),
        ],
        SensorKind::Lidar { .. } => vec![
            ("range (m)", SensorField::RangeM),
            ("beams", SensorField::Beams),
        ],
    };
    fields.extend([
        ("mount x (m)", SensorField::Position(0)),
        ("mount y (m)", SensorField::Position(1)),
        ("mount z (m)", SensorField::Position(2)),
    ]);
    fields
}

/// The value of `field` on `sensor`, as a number, or `None` if the sensor has no such setting.
fn sensor_value(sensor: &Sensor, field: SensorField) -> Option<f64> {
    match (field, &sensor.kind) {
        (SensorField::SampleRate, SensorKind::Imu { sample_rate_hz }) => Some(*sample_rate_hz),
        (SensorField::FieldOfView, SensorKind::Camera { fov_deg, .. }) => Some(*fov_deg),
        (SensorField::WidthPx, SensorKind::Camera { width_px, .. }) => Some(f64::from(*width_px)),
        (SensorField::HeightPx, SensorKind::Camera { height_px, .. }) => {
            Some(f64::from(*height_px))
        }
        (SensorField::RangeM, SensorKind::Lidar { range_m, .. }) => Some(*range_m),
        (SensorField::Beams, SensorKind::Lidar { beams, .. }) => Some(f64::from(*beams)),
        (SensorField::Position(axis), _) => sensor.mount.position_m.get(axis).copied(),
        _ => None,
    }
}

/// A copy of `sensor` with `field` set to `value`, checked by the sensor's own
/// rules. Whole-number fields refuse fractions and negative numbers.
fn with_sensor_value(
    sensor: &Sensor,
    field: SensorField,
    value: f64,
) -> Result<Sensor, ProjectError> {
    let mut updated = sensor.clone();
    let whole = |what: &'static str| -> Result<u32, ProjectError> {
        if value.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(&value) {
            Ok(value as u32)
        } else {
            Err(twin_sensors::SensorError::BadCount {
                name: sensor.name.clone(),
                what,
            }
            .into())
        }
    };
    let applied = match (field, &mut updated.kind) {
        (SensorField::SampleRate, SensorKind::Imu { sample_rate_hz }) => {
            *sample_rate_hz = value;
            true
        }
        (SensorField::FieldOfView, SensorKind::Camera { fov_deg, .. }) => {
            *fov_deg = value;
            true
        }
        (SensorField::WidthPx, SensorKind::Camera { width_px, .. }) => {
            *width_px = whole("image width")?;
            true
        }
        (SensorField::HeightPx, SensorKind::Camera { height_px, .. }) => {
            *height_px = whole("image height")?;
            true
        }
        (SensorField::RangeM, SensorKind::Lidar { range_m, .. }) => {
            *range_m = value;
            true
        }
        (SensorField::Beams, SensorKind::Lidar { beams, .. }) => {
            *beams = whole("beam count")?;
            true
        }
        (SensorField::Position(axis), _) if axis < 3 => {
            updated.mount.position_m[axis] = value;
            true
        }
        _ => false,
    };
    if !applied {
        return Err(ProjectError::NoSensorField(sensor.name.clone()));
    }
    Ok(updated.validated()?)
}

fn design_draft_key(name: &str) -> String {
    format!("design:{name}")
}

fn sensor_draft_key(name: &str, field: SensorField) -> String {
    format!("sensor:{name}:{field:?}")
}

/// Parameter changes between two commits of the history, `from` being older.
#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    pub from: CommitId,
    pub to: CommitId,
    pub changes: Vec<Change>,
}

fn scenario_draft_key(scenario: &str, field: &ScenarioField) -> String {
    match field {
        ScenarioField::Duration => format!("scenario:{scenario}:duration_s"),
        ScenarioField::Initial(name) => format!("scenario:{scenario}:initial:{name}"),
    }
}

/// A random replica number for a joining peer. Replica IDs only need to differ
/// from each other within a session.
fn random_replica_id() -> Result<u64, ProjectError> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).map_err(|e| ProjectError::Key(e.to_string()))?;
    Ok(u64::from_le_bytes(bytes).max(2))
}

/// A random session code, 24 hex digits, for the host to show.
pub fn new_session_code() -> Result<String, ProjectError> {
    let mut bytes = [0u8; 12];
    getrandom::fill(&mut bytes).map_err(|e| ProjectError::Key(e.to_string()))?;
    Ok(to_hex(&bytes))
}

/// The folder for this user's settings, such as the export signing key:
/// `%APPDATA%\twin` where APPDATA is set, otherwise `~/.config/twin`.
pub fn user_config_dir() -> Option<PathBuf> {
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return Some(PathBuf::from(appdata).join("twin"));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config").join("twin"))
}

/// This host's TLS identity for LAN sessions, read from `dir` (the user config
/// folder). Created on first use, so clients that pinned it keep recognising
/// the host. The private key stays in that folder.
pub fn load_or_create_server_identity(
    dir: &Path,
) -> Result<twin_collab::tls::ServerIdentity, ProjectError> {
    let cert_path = dir.join(SERVER_CERT_FILE);
    let key_path = dir.join(SERVER_KEY_FILE);
    if let (Ok(cert), Ok(key)) = (
        std::fs::read_to_string(&cert_path),
        std::fs::read_to_string(&key_path),
    ) {
        let cert = from_hex(cert.trim())
            .ok_or_else(|| ProjectError::Key("server certificate is not hex".into()))?;
        let key = from_hex(key.trim())
            .ok_or_else(|| ProjectError::Key("server key is not hex".into()))?;
        return Ok(twin_collab::tls::ServerIdentity::from_der(cert, key));
    }
    let identity = twin_collab::tls::ServerIdentity::generate()?;
    std::fs::create_dir_all(dir)?;
    std::fs::write(&cert_path, to_hex(identity.cert_der()))?;
    std::fs::write(&key_path, to_hex(identity.key_der()))?;
    Ok(identity)
}

/// This user's key for signing exports, read from `dir`. It is created on
/// first use, from the operating system's random source.
pub fn load_or_create_export_key(dir: &Path) -> Result<SigningKey, ProjectError> {
    let path = dir.join(EXPORT_KEY_FILE);
    if let Ok(text) = std::fs::read_to_string(&path) {
        let seed: [u8; 32] = from_hex(text.trim())
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| ProjectError::Key(format!("{} is not a 32-byte key", path.display())))?;
        return Ok(SigningKey::from_bytes(&seed));
    }
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| ProjectError::Key(e.to_string()))?;
    std::fs::create_dir_all(dir)?;
    std::fs::write(&path, to_hex(&seed))?;
    Ok(SigningKey::from_bytes(&seed))
}

/// A Unix time as an ISO 8601 UTC timestamp, such as `2023-11-14T22:13:20Z`.
fn iso8601_utc(unix_seconds: u64) -> String {
    // Civil date from day count (Howard Hinnant's algorithm), days since 1970-01-01.
    let days = (unix_seconds / 86_400) as i64;
    let rem = unix_seconds % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    text.as_bytes()
        .chunks(2)
        .map(|pair| {
            let digits = std::str::from_utf8(pair).ok()?;
            if digits.len() != 2 {
                return None;
            }
            u8::from_str_radix(digits, 16).ok()
        })
        .collect()
}

/// One feature-tree line for a modelling step, in words: what it makes and
/// which parameters it uses.
pub fn describe_step(step: &Step) -> String {
    let value = |v: &Value| match v {
        Value::Const(c) => c.to_string(),
        Value::Param(name) => name.clone(),
    };
    match step {
        Step::Block {
            width,
            depth,
            height,
        } => format!(
            "block {} x {} x {}",
            value(width),
            value(depth),
            value(height)
        ),
        Step::Cylinder { radius, height } => {
            format!(
                "cylinder, radius {}, height {}",
                value(radius),
                value(height)
            )
        }
        Step::Boolean { op, a, b } => format!("{op:?} of steps {} and {}", a + 1, b + 1),
        Step::Translate { parent, x, y, z } => format!(
            "step {} moved by ({}, {}, {})",
            parent + 1,
            value(x),
            value(y),
            value(z)
        ),
    }
}

/// A depth image as a binary greyscale PGM: the nearest finite depth is white,
/// the farthest is dark, and pixels with nothing in view are black.
fn depth_to_pgm(width: u32, height: u32, depth: &[f32]) -> Vec<u8> {
    let finite = depth.iter().copied().filter(|d| d.is_finite());
    let (near, far) = finite.fold((f32::INFINITY, f32::NEG_INFINITY), |(n, f), d| {
        (n.min(d), f.max(d))
    });
    let mut out = format!("P5\n{width} {height}\n255\n").into_bytes();
    out.extend(depth.iter().map(|&d| {
        if !d.is_finite() {
            0
        } else if far - near < 1e-9 {
            255
        } else {
            (255.0 * (far - d) / (far - near)).round() as u8
        }
    }));
    out
}

/// The vendor's public key for licence files, as committed in `keys/`.
const VENDOR_PUBLIC_KEY: &str = include_str!("../../../keys/twin-vendor.pub");
/// File name of this user's export signing key, in the user config folder.
const EXPORT_KEY_FILE: &str = "export-key.seed";
/// File names of this host's TLS certificate and private key, in the user config folder.
const SERVER_CERT_FILE: &str = "server-cert.hex";
const SERVER_KEY_FILE: &str = "server-key.hex";
/// File name, in the project folder, of the signer pinned to each imported twin.
const PINS_FILE: &str = "trusted-signers.txt";
/// File name, in the project folder, of the host each session address was pinned to.
const COLLAB_PINS_FILE: &str = "collab-pins.txt";

/// The text typed into the session fields of the window.
#[derive(Debug, Clone, Default)]
pub struct CollabFields {
    pub address: String,
    /// The code for peers who may edit.
    pub code: String,
    /// The code for peers who may only watch.
    pub viewer_code: String,
    /// The name this window shows a host, when it joins a session.
    pub peer_name: String,
    pub name: String,
    pub value: String,
}

/// One of the session fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollabField {
    Address,
    Code,
    ViewerCode,
    PeerName,
    Name,
    Value,
}

/// Name of the licence file, looked for in the project folder.
pub const LICENCE_FILE: &str = "twin.license";

/// The licence in `dir`, if it has a valid one. A missing or invalid file gives
/// `None`; it never stops the project from opening.
fn load_licence(dir: &Path) -> Option<License> {
    let verifier = LicenseVerifier::from_base64(VENDOR_PUBLIC_KEY).ok()?;
    let text = std::fs::read_to_string(dir.join(LICENCE_FILE)).ok()?;
    verifier.verify(&text).ok()
}

/// Name of the scenario added by `Project::add_example_model`.
pub const EXAMPLE_SCENARIO: &str = "free-vibration";
/// One lidar scan of the design, before it is summarised or saved.
struct LidarScan {
    range_m: f64,
    sampled: usize,
    returns: Vec<[f64; 3]>,
}

/// Points sampled along each edge of a face when a lidar scans the geometry.
const LIDAR_SAMPLES_PER_EDGE: usize = 8;
/// Samples per simulated run, evenly spaced over the scenario duration.
const RUN_SAMPLES: usize = 100;

pub struct Project {
    dir: PathBuf,
    store: TwinStore,
    twins: Vec<(TwinId, String)>,
    current: Option<Twin>,
    /// Text typed into parameter fields, by name. It is kept as typed (for
    /// example `2.` on the way to `2.5`) so the field does not change under the
    /// user. A draft that is not a number is kept too, and blocks `save`.
    drafts: BTreeMap<String, String>,
    /// Drafts that are numbers but were refused by the twin (for example a
    /// negative duration). `save` is refused while any is left.
    rejected: BTreeSet<String>,
    /// The parameter history of the open twin, if it has one.
    history: Option<History>,
    /// The branch the head is compared with in the history panel.
    compare_from: Option<CommitId>,
    compare_to: Option<CommitId>,
    /// What the window plots.
    plot: Option<PlotChoice>,
    /// The geometry design of the open twin, if it has one.
    design: Option<Design>,
    /// Text typed for a new geometry branch.
    design_branch_name: String,
    /// The file path typed for exporting and importing a `.twin` package.
    package_path: String,
    /// Passphrase for exporting and importing encrypted packages. Kept only for
    /// this session, never written to disk.
    package_passphrase: String,
    /// The shared session this window hosts, if any.
    collab_host: Option<SyncHost>,
    /// The shared session this window has joined, if any.
    collab_client: Option<SyncClient>,
    /// The text typed into the session fields.
    collab_fields: CollabFields,
    /// The licence found in the project folder, if any.
    licence: Option<License>,
    commit_message: String,
    branch_name: String,
    status: String,
}

impl Project {
    /// Open or create the project in `dir`.
    pub fn open(dir: &Path) -> Result<Self, ProjectError> {
        let mut project = Self {
            dir: dir.to_path_buf(),
            store: TwinStore::open(dir)?,
            twins: Vec::new(),
            current: None,
            drafts: BTreeMap::new(),
            rejected: BTreeSet::new(),
            history: None,
            compare_from: None,
            compare_to: None,
            plot: None,
            design: None,
            design_branch_name: String::new(),
            package_path: String::new(),
            package_passphrase: String::new(),
            collab_host: None,
            collab_client: None,
            collab_fields: CollabFields::default(),
            licence: load_licence(dir),
            commit_message: String::new(),
            branch_name: String::new(),
            status: "Project ready".to_string(),
        };
        project.refresh()?;
        Ok(project)
    }

    /// Create a twin with `name`, save it, and open it.
    pub fn new_twin(&mut self, name: &str) -> Result<TwinId, ProjectError> {
        let twin = Twin::new(name)?;
        self.store.save(&twin)?;
        let id = twin.id;
        self.current = Some(twin);
        self.drafts.clear();
        self.rejected.clear();
        self.history = None;
        self.design = None;
        self.compare_from = None;
        self.compare_to = None;
        self.refresh()?;
        self.status = format!("Created {name}");
        Ok(id)
    }

    /// Open a stored twin, discarding unsaved edits to the current one.
    pub fn open_twin(&mut self, id: TwinId) -> Result<(), ProjectError> {
        let twin = self.store.load(id)?.ok_or(ProjectError::NotFound)?;
        self.status = format!("Opened {}", twin.name);
        self.history = self.store.load_history(id)?;
        self.design = self.store.load_design(id)?;
        self.compare_from = None;
        self.compare_to = None;
        self.current = Some(twin);
        self.drafts.clear();
        self.rejected.clear();
        Ok(())
    }

    /// Set a parameter on the open twin. Not saved until `save`.
    pub fn set_parameter(&mut self, name: &str, value: f64) -> Result<(), ProjectError> {
        let twin = self.current.as_mut().ok_or(ProjectError::NoTwinOpen)?;
        twin.set_parameter(name, value)?;
        self.status = format!("Edited {name} (not saved)");
        Ok(())
    }

    /// Type `text` into the field of parameter `name`. A number is applied to
    /// the open twin at once (not saved until `save`). Other text is kept as a
    /// draft and reported, and the twin keeps its last good value.
    pub fn edit_parameter(&mut self, name: &str, text: &str) -> Result<(), ProjectError> {
        if self.current.is_none() {
            return Err(ProjectError::NoTwinOpen);
        }
        self.drafts.insert(name.to_string(), text.to_string());
        let value: f64 = text
            .trim()
            .parse()
            .map_err(|_| ProjectError::NotANumber(name.to_string()))?;
        let result = self.set_parameter(name, value);
        self.reject_or_accept(name, result)
    }

    /// Record whether the twin accepted the value typed into field `key`.
    fn reject_or_accept(
        &mut self,
        key: &str,
        result: Result<(), ProjectError>,
    ) -> Result<(), ProjectError> {
        match result {
            Ok(()) => {
                self.rejected.remove(key);
                Ok(())
            }
            Err(e) => {
                self.rejected.insert(key.to_string());
                Err(e)
            }
        }
    }

    /// The text a parameter field should show: the draft if one is being typed,
    /// otherwise the stored value. `None` if the twin has no such parameter.
    pub fn parameter_text(&self, name: &str) -> Option<String> {
        if let Some(draft) = self.drafts.get(name) {
            return Some(draft.clone());
        }
        let twin = self.current.as_ref()?;
        twin.parameters.get(name).map(|v| v.to_string())
    }

    /// Type `text` into a field of scenario `scenario`. Works like
    /// `edit_parameter`: a number is applied at once (not saved until `save`),
    /// other text is kept as a draft and blocks `save`.
    pub fn edit_scenario(
        &mut self,
        scenario: &str,
        field: &ScenarioField,
        text: &str,
    ) -> Result<(), ProjectError> {
        if self.current.is_none() {
            return Err(ProjectError::NoTwinOpen);
        }
        let key = scenario_draft_key(scenario, field);
        self.drafts.insert(key.clone(), text.to_string());
        let value: f64 = text
            .trim()
            .parse()
            .map_err(|_| ProjectError::NotANumber(key.clone()))?;
        let twin = self.current.as_mut().ok_or(ProjectError::NoTwinOpen)?;
        let result = match field {
            ScenarioField::Duration => twin.set_scenario_duration(scenario, value),
            ScenarioField::Initial(name) => twin.set_scenario_initial(scenario, name, value),
        };
        self.reject_or_accept(&key, result.map_err(ProjectError::from))?;
        self.status = format!("Edited {scenario} (not saved)");
        Ok(())
    }

    /// The text a scenario field should show: the draft if one is being typed,
    /// otherwise the stored value. `None` if the scenario has no such field.
    pub fn scenario_text(&self, scenario: &str, field: &ScenarioField) -> Option<String> {
        let key = scenario_draft_key(scenario, field);
        if let Some(draft) = self.drafts.get(&key) {
            return Some(draft.clone());
        }
        let sc = self
            .current
            .as_ref()?
            .scenarios
            .iter()
            .find(|sc| sc.name == scenario)?;
        match field {
            ScenarioField::Duration => Some(sc.duration_s.to_string()),
            ScenarioField::Initial(name) => sc.initial_conditions.get(name).map(|v| v.to_string()),
        }
    }

    /// Fill in a mass-spring-damper example: the three physical parameters and
    /// a free-vibration scenario, each only where the twin does not have one.
    /// Changes are unsaved until `save`.
    pub fn add_example_model(&mut self) -> Result<(), ProjectError> {
        let twin = self.current.as_mut().ok_or(ProjectError::NoTwinOpen)?;
        for (name, value) in [
            ("mass_kg", 1.0),
            ("stiffness_n_per_m", 4.0),
            ("damping_n_s_per_m", 0.2),
        ] {
            if !twin.parameters.contains_key(name) {
                twin.set_parameter(name, value)?;
            }
        }
        if !twin.scenarios.iter().any(|sc| sc.name == EXAMPLE_SCENARIO) {
            twin.add_scenario(Scenario {
                name: EXAMPLE_SCENARIO.to_string(),
                duration_s: 10.0,
                initial_conditions: [
                    ("position_m".to_string(), 0.1),
                    ("velocity_m_s".to_string(), 0.0),
                ]
                .into(),
            })?;
        }
        self.status = "Added example model (not saved)".to_string();
        Ok(())
    }

    /// Simulate scenario `name` of the open twin and store the run, replacing
    /// the previous run of that scenario. The twin must be saved first.
    pub fn run_scenario(&mut self, name: &str) -> Result<(), ProjectError> {
        if self.is_dirty()? {
            return Err(ProjectError::UnsavedTwin);
        }
        let twin = self.current.as_ref().ok_or(ProjectError::NoTwinOpen)?;
        let duration = twin
            .scenarios
            .iter()
            .find(|sc| sc.name == name)
            .map(|sc| sc.duration_s)
            .ok_or_else(|| StoreError::NoScenario(name.to_string()))?;
        let times: Vec<f64> = (0..=RUN_SAMPLES)
            .map(|i| duration * i as f64 / RUN_SAMPLES as f64)
            .collect();
        self.store
            .run_scenario(twin, name, &times, self.design.as_ref())?;
        self.status = format!("Ran {name}");
        Ok(())
    }

    /// A one-line summary of the stored run of `scenario`, or `None` if it has
    /// not been run.
    pub fn run_summary(&self, scenario: &str) -> Result<Option<String>, ProjectError> {
        let Some(twin) = &self.current else {
            return Err(ProjectError::NoTwinOpen);
        };
        let Some(run) = self.store.load_run(twin.id, scenario)? else {
            return Ok(None);
        };
        let (Some(&end), Some(last)) = (run.times.last(), run.states.last()) else {
            return Ok(None);
        };
        let peak = run
            .states
            .iter()
            .map(|s| s[0].abs())
            .fold(0.0_f64, f64::max);
        let mut summary = format!(
            "{} samples to {end:.2} s, final position {:.4} m, peak |position| {peak:.4} m",
            run.times.len(),
            last[0]
        );
        if !run_matches_parameters(twin, &run) || !run_is_current(twin, &run) {
            summary.push_str(" (stale: the twin changed after this run)");
        }
        Ok(Some(summary))
    }

    /// Choose what the window plots.
    pub fn set_plot(&mut self, choice: PlotChoice) {
        self.plot = Some(choice);
    }

    /// The saved lidar scan chosen for plotting, as top-down `(x, y)` points in
    /// the sensor frame, with the sensor's name. `None` unless a lidar scan has
    /// been chosen and saved.
    pub fn lidar_scan_xy(&self) -> Result<Option<LidarScanXy>, ProjectError> {
        let Some(PlotChoice::Lidar(name)) = &self.plot else {
            return Ok(None);
        };
        let text = match std::fs::read_to_string(self.dir.join(format!("lidar-{name}.xyz"))) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let points = text
            .lines()
            .filter_map(|line| {
                let mut parts = line.split_whitespace().map(str::parse::<f64>);
                match (parts.next(), parts.next()) {
                    (Some(Ok(x)), Some(Ok(y))) => Some([x, y]),
                    _ => None,
                }
            })
            .collect();
        Ok(Some((name.clone(), points)))
    }

    /// The data for the chosen plot. `None` if nothing is chosen, or the chosen
    /// run or recording is not stored.
    pub fn plot_data(&self) -> Result<Option<PlotData>, ProjectError> {
        let (Some(choice), Some(twin)) = (&self.plot, &self.current) else {
            return Ok(None);
        };
        match choice {
            // A scan is not a time series; it is drawn by `lidar_scan_xy`.
            PlotChoice::Lidar(_) => Ok(None),
            PlotChoice::Run(name) => {
                let Some(run) = self.store.load_run(twin.id, name)? else {
                    return Ok(None);
                };
                Ok(Some(PlotData {
                    title: format!("Run: {name}"),
                    times: run.times,
                    series: vec![
                        (
                            "position (m)".into(),
                            run.states.iter().map(|s| s[0]).collect(),
                        ),
                        (
                            "velocity (m/s)".into(),
                            run.states.iter().map(|s| s[1]).collect(),
                        ),
                    ],
                }))
            }
            PlotChoice::Sensor(name) => {
                let Some(rec) = self.store.load_recording(twin.id, name)? else {
                    return Ok(None);
                };
                let series = rec
                    .channels
                    .iter()
                    .enumerate()
                    .map(|(i, channel)| {
                        (
                            channel.clone(),
                            rec.samples.iter().map(|row| row[i]).collect(),
                        )
                    })
                    .collect();
                Ok(Some(PlotData {
                    title: format!("Sensor: {name}"),
                    times: rec.times_s,
                    series,
                }))
            }
        }
    }

    /// The geometry design of the open twin, if it has one.
    pub fn design(&self) -> Option<&Design> {
        self.design.as_ref()
    }

    /// Start geometry for the open twin: a block whose width, depth and height
    /// are design parameters. The first commit is made at once, and the twin
    /// must be saved first.
    pub fn start_design(&mut self) -> Result<(), ProjectError> {
        let twin = self.current.as_ref().ok_or(ProjectError::NoTwinOpen)?;
        if self.design.is_some() {
            return Err(ProjectError::DesignExists);
        }
        if self.is_dirty()? {
            return Err(ProjectError::UnsavedTwin);
        }
        let spec = ModelSpec {
            steps: vec![Step::Block {
                width: Value::Param("width_m".into()),
                depth: Value::Param("depth_m".into()),
                height: Value::Param("height_m".into()),
            }],
        };
        let initial: Content = [("width_m", 2.0), ("depth_m", 1.0), ("height_m", 1.0)]
            .map(|(k, v)| (k.to_string(), v))
            .into();
        let design = Design::new(spec, initial, "Initial geometry")?;
        self.store.save_design(twin.id, &design)?;
        self.design = Some(design);
        self.status = "Started geometry".to_string();
        Ok(())
    }

    /// Type `text` into design parameter `name`. A finite number is applied to
    /// the design's working copy, and is committed by `commit_design`.
    pub fn edit_design_parameter(&mut self, name: &str, text: &str) -> Result<(), ProjectError> {
        if self.design.is_none() {
            return Err(ProjectError::NoDesign);
        }
        let key = design_draft_key(name);
        self.drafts.insert(key.clone(), text.to_string());
        let value: f64 = text
            .trim()
            .parse()
            .map_err(|_| ProjectError::NotANumber(key.clone()))?;
        let result = match self.design.as_mut() {
            Some(design) if value.is_finite() => {
                design.set(name, value);
                Ok(())
            }
            _ => Err(ProjectError::RejectedValue(key.clone())),
        };
        self.reject_or_accept(&key, result)
    }

    /// The text a design parameter field should show: the draft if one is being
    /// typed, otherwise the working value. `None` if there is no such parameter.
    pub fn design_text(&self, name: &str) -> Option<String> {
        if let Some(draft) = self.drafts.get(&design_draft_key(name)) {
            return Some(draft.clone());
        }
        let design = self.design.as_ref()?;
        design.history().working().get(name).map(|v| v.to_string())
    }

    /// Commit the design's working parameters as a new geometry commit. Refused
    /// if they do not build a model.
    pub fn commit_design(&mut self) -> Result<CommitId, ProjectError> {
        let id = self.current.as_ref().ok_or(ProjectError::NoTwinOpen)?.id;
        let design = self.design.as_mut().ok_or(ProjectError::NoDesign)?;
        let commit = design.commit("Update geometry")?;
        self.store.save_design(id, design)?;
        self.status = format!("Committed geometry {}", &commit.as_str()[..12]);
        Ok(commit)
    }

    /// Link the open twin to the design's head commit. The committed parameters
    /// are copied into the twin as unsaved edits. Refused while the design has
    /// uncommitted parameter changes, or while the twin has unsaved edits.
    pub fn link_design(&mut self) -> Result<(), ProjectError> {
        if self.is_dirty()? {
            return Err(ProjectError::UnsavedTwin);
        }
        self.require_committed_design()?;
        let design = self.design.as_ref().ok_or(ProjectError::NoDesign)?;
        let twin = self.current.as_mut().ok_or(ProjectError::NoTwinOpen)?;
        design.link_twin(twin)?;
        self.drafts.clear();
        self.status = "Linked the twin to its geometry (not saved)".to_string();
        Ok(())
    }

    /// Refused while the design has parameter changes that are not committed.
    fn require_committed_design(&self) -> Result<(), ProjectError> {
        let design = self.design.as_ref().ok_or(ProjectError::NoDesign)?;
        let history = design.history();
        if history.working() != &history.commit(history.head_commit())?.content {
            return Err(ProjectError::UncommittedDesign);
        }
        Ok(())
    }

    /// Store the design of the open twin.
    fn save_design(&self) -> Result<(), ProjectError> {
        let id = self.current.as_ref().ok_or(ProjectError::NoTwinOpen)?.id;
        let design = self.design.as_ref().ok_or(ProjectError::NoDesign)?;
        self.store.save_design(id, design)?;
        Ok(())
    }

    pub fn design_branch_name(&self) -> &str {
        &self.design_branch_name
    }

    pub fn set_design_branch_name(&mut self, text: impl Into<String>) {
        self.design_branch_name = text.into();
    }

    /// Create a geometry branch named by the branch name field, at the head.
    pub fn create_design_branch(&mut self) -> Result<(), ProjectError> {
        let name = self.design_branch_name.trim().to_string();
        if name.is_empty() {
            return Err(ProjectError::EmptyBranchName);
        }
        let design = self.design.as_mut().ok_or(ProjectError::NoDesign)?;
        design.create_branch(&name)?;
        self.save_design()?;
        self.design_branch_name.clear();
        self.status = format!("Created geometry branch {name}");
        Ok(())
    }

    /// Switch the design to branch `name`. Refused while there are uncommitted
    /// parameter changes.
    pub fn switch_design_branch(&mut self, name: &str) -> Result<(), ProjectError> {
        self.require_committed_design()?;
        let design = self.design.as_mut().ok_or(ProjectError::NoDesign)?;
        design.switch(name)?;
        self.save_design()?;
        self.status = format!("Switched geometry to {name} (link the twin to use it)");
        Ok(())
    }

    /// Merge geometry branch `from` into the current one. Refused while there
    /// are uncommitted changes, or if a parameter conflicts (nothing changes).
    pub fn merge_design_branch(&mut self, from: &str) -> Result<(), ProjectError> {
        self.require_committed_design()?;
        let design = self.design.as_mut().ok_or(ProjectError::NoDesign)?;
        design.merge(from, &format!("Merge {from}"))?;
        self.save_design()?;
        self.status = format!("Merged geometry {from} (link the twin again)");
        Ok(())
    }

    /// Parameter changes from the tip of geometry branch `branch` to the head.
    pub fn design_changes(&self, branch: &str) -> Result<Vec<Change>, ProjectError> {
        let design = self.design.as_ref().ok_or(ProjectError::NoDesign)?;
        let history = design.history();
        let tip = history
            .branches()
            .find(|(name, _)| *name == branch)
            .map(|(_, id)| id.clone())
            .ok_or_else(|| HistoryError::NoSuchBranch(branch.to_string()))?;
        Ok(history.diff(&tip, history.head_commit())?)
    }

    /// The design's modelling steps, one line each, in order. Empty if there
    /// is no design.
    pub fn feature_tree(&self) -> Vec<String> {
        self.design.as_ref().map_or_else(Vec::new, |design| {
            design
                .spec()
                .steps
                .iter()
                .enumerate()
                .map(|(i, step)| format!("{}. {}", i + 1, describe_step(step)))
                .collect()
        })
    }

    /// One line on the design: its head commit, and whether the open twin is
    /// linked to it. `None` if there is no design.
    pub fn geometry_status(&self) -> Option<String> {
        let design = self.design.as_ref()?;
        let head = design.history().head_commit();
        let linked = self.current.as_ref().is_some_and(|t| design.is_linked(t));
        Some(format!(
            "Geometry at {}; the twin is {}",
            &head.as_str()[..12],
            if linked {
                "linked to it"
            } else {
                "not linked to it"
            }
        ))
    }

    /// The licence found in the project folder, if any.
    pub fn licence(&self) -> Option<&License> {
        self.licence.as_ref()
    }

    /// A banner for official builds without a valid licence. Source builds show
    /// none, and work the same either way.
    pub fn licence_banner(&self) -> Option<String> {
        if !cfg!(feature = "official") || self.licence.is_some() {
            return None;
        }
        Some(format!(
            "Unlicensed copy. Put a licence file named {LICENCE_FILE} in the project folder to remove this banner."
        ))
    }

    pub fn package_passphrase(&self) -> &str {
        &self.package_passphrase
    }

    pub fn set_package_passphrase(&mut self, text: impl Into<String>) {
        self.package_passphrase = text.into();
    }

    /// The session passphrase, or `None` if it is empty.
    pub fn passphrase(&self) -> Option<&str> {
        Some(self.package_passphrase.as_str()).filter(|p| !p.is_empty())
    }

    pub fn package_path(&self) -> &str {
        &self.package_path
    }

    pub fn set_package_path(&mut self, text: impl Into<String>) {
        self.package_path = text.into();
    }

    /// Export the open twin, with its design and runs, as a signed `.twin`
    /// package at `path`. Signed with `key` (see `load_or_create_export_key`),
    /// and encrypted with `passphrase` if one is given. The twin must be saved.
    pub fn export_twin(
        &mut self,
        path: &Path,
        key: &SigningKey,
        passphrase: Option<&str>,
    ) -> Result<(), ProjectError> {
        let twin = self.current.as_ref().ok_or(ProjectError::NoTwinOpen)?;
        if self.is_dirty()? {
            return Err(ProjectError::UnsavedTwin);
        }
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let bytes = self
            .store
            .export_twin(twin.id, &iso8601_utc(seconds), key, passphrase)?;
        std::fs::write(path, bytes)?;
        self.status = format!("Exported {} to {}", twin.name, path.display());
        Ok(())
    }

    /// Import a `.twin` package from `path`, verifying it fully before anything
    /// is stored. Trust on first use: the signer of a twin is pinned the first
    /// time it is imported, and a later package for the same twin that is
    /// signed by a different key is refused.
    pub fn import_twin(
        &mut self,
        path: &Path,
        passphrase: Option<&str>,
    ) -> Result<TwinId, ProjectError> {
        let bytes = std::fs::read(path)?;
        let plain = twin_package::decrypt(&bytes, passphrase)?;
        let signer = twin_package::signer_key(&plain)?;
        let verified = twin_package::import(&plain, &signer)?;
        let id = verified.twin.id;
        let signer_hex = to_hex(signer.as_bytes());
        let mut pins = self.load_pins()?;
        if pins
            .get(&id.to_string())
            .is_some_and(|known| *known != signer_hex)
        {
            return Err(ProjectError::SignerChanged(id.to_string()));
        }
        self.store.import_twin(&plain, &signer, None)?;
        pins.insert(id.to_string(), signer_hex);
        self.save_pins(&pins)?;
        self.refresh()?;
        self.status = format!("Imported {}", verified.twin.name);
        Ok(id)
    }

    /// The pinned signers, by twin ID.
    fn load_pins(&self) -> Result<BTreeMap<String, String>, ProjectError> {
        let text = match std::fs::read_to_string(self.dir.join(PINS_FILE)) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(e.into()),
        };
        Ok(text
            .lines()
            .filter_map(|line| line.split_once(' '))
            .map(|(id, key)| (id.to_string(), key.to_string()))
            .collect())
    }

    fn save_pins(&self, pins: &BTreeMap<String, String>) -> Result<(), ProjectError> {
        let text: String = pins
            .iter()
            .map(|(id, key)| format!("{id} {key}\n"))
            .collect();
        std::fs::write(self.dir.join(PINS_FILE), text)?;
        Ok(())
    }

    /// Render what camera `sensor` sees of the design's surface. The image is
    /// written as a greyscale PGM (nearer is brighter, background is black) to
    /// `camera-<sensor>.pgm` in the project folder. Returns a summary, or `None`
    /// if the sensor is not a camera or there is no geometry.
    pub fn render_camera(&self, sensor: &str) -> Result<Option<String>, ProjectError> {
        let Some(design) = &self.design else {
            return Ok(None);
        };
        let sensors = self.sensors()?;
        let found = sensors
            .iter()
            .find(|s| s.name == sensor)
            .ok_or_else(|| ProjectError::NoSensor(sensor.to_string()))?;
        let SensorKind::Camera {
            fov_deg,
            width_px,
            height_px,
        } = found.kind
        else {
            return Ok(None);
        };
        let model = design.preview().map_err(DesignError::from)?;
        let depth = twin_sensors::camera_depth(
            &model.triangles(),
            &found.mount,
            fov_deg,
            width_px,
            height_px,
        );
        let hits: Vec<f32> = depth.iter().copied().filter(|d| d.is_finite()).collect();
        let nearest = hits.iter().copied().fold(f32::INFINITY, f32::min);
        let path = self.dir.join(format!("camera-{sensor}.pgm"));
        std::fs::write(&path, depth_to_pgm(width_px, height_px, &depth))?;
        let summary = if hits.is_empty() {
            format!("no surface in view; image saved to {}", path.display())
        } else {
            format!(
                "{} of {} pixels hit, nearest {nearest:.2} m; image saved to {}",
                hits.len(),
                depth.len(),
                path.display()
            )
        };
        Ok(Some(summary))
    }

    /// The session fields, as typed.
    pub fn collab_text(&self, field: CollabField) -> &str {
        match field {
            CollabField::Address => &self.collab_fields.address,
            CollabField::Code => &self.collab_fields.code,
            CollabField::ViewerCode => &self.collab_fields.viewer_code,
            CollabField::PeerName => &self.collab_fields.peer_name,
            CollabField::Name => &self.collab_fields.name,
            CollabField::Value => &self.collab_fields.value,
        }
    }

    pub fn set_collab_text(&mut self, field: CollabField, text: impl Into<String>) {
        let text = text.into();
        match field {
            CollabField::Address => self.collab_fields.address = text,
            CollabField::Code => self.collab_fields.code = text,
            CollabField::ViewerCode => self.collab_fields.viewer_code = text,
            CollabField::PeerName => self.collab_fields.peer_name = text,
            CollabField::Name => self.collab_fields.name = text,
            CollabField::Value => self.collab_fields.value = text,
        }
    }

    /// The address the host session listens on, if this window hosts one.
    pub fn host_address(&self) -> Option<SocketAddr> {
        self.collab_host.as_ref().map(SyncHost::addr)
    }

    /// The names of the peers connected to this window's hosted session.
    pub fn session_peer_names(&self) -> Vec<String> {
        self.collab_host
            .as_ref()
            .map_or_else(Vec::new, SyncHost::peer_names)
    }

    /// How many peers are connected to this window's hosted session.
    pub fn session_peers(&self) -> usize {
        self.collab_host.as_ref().map_or(0, SyncHost::peer_count)
    }

    /// The project folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// True while this window hosts or has joined a session.
    pub fn collab_active(&self) -> bool {
        self.collab_host.is_some() || self.collab_client.is_some()
    }

    /// Host the open twin's parameters on `addr`, for peers that give `code`.
    /// Non-loopback addresses need `allow_lan`. The host's TLS identity lives in
    /// `config_dir`. Replaces any session this window already has.
    pub fn host_session(
        &mut self,
        addr: &str,
        allow_lan: bool,
        config_dir: &Path,
        editor_code: &str,
        viewer_code: &str,
    ) -> Result<(), ProjectError> {
        let twin = self.current.as_ref().ok_or(ProjectError::NoTwinOpen)?;
        if editor_code.trim().is_empty() || viewer_code.trim().is_empty() {
            return Err(ProjectError::Session(
                "type an edit code and a view code first".into(),
            ));
        }
        if editor_code == viewer_code {
            return Err(ProjectError::Session(
                "the edit code and the view code must differ".into(),
            ));
        }
        let identity = load_or_create_server_identity(config_dir)?;
        let host = SyncHost::start(
            TwinReplica::host(1, twin),
            &identity,
            addr,
            allow_lan,
            editor_code,
            viewer_code,
        )
        .map_err(|e| ProjectError::Session(e.to_string()))?;
        self.status = format!(
            "Hosting at {} (fingerprint {})",
            host.addr(),
            identity.fingerprint()
        );
        self.collab_client = None;
        self.collab_host = Some(host);
        Ok(())
    }

    /// Join the session hosted at `addr` with `code`. The host's fingerprint is
    /// pinned for that address the first time, and a later change is refused.
    pub fn join_session(&mut self, addr: &str, code: &str) -> Result<(), ProjectError> {
        let key = addr.trim().to_string();
        let socket: SocketAddr = key.parse().map_err(|_| {
            ProjectError::Session(format!("`{key}` is not an address such as 127.0.0.1:7000"))
        })?;
        let mut pins = self.load_collab_pins()?;
        let pinned = pins.get(&key).cloned();
        let replica_id = random_replica_id()?;
        let name = match self.collab_fields.peer_name.trim() {
            "" => "peer",
            name => name,
        }
        .to_string();
        let (client, fingerprint) =
            SyncClient::connect_as(socket, pinned.as_deref(), code, &name, replica_id)
                .map_err(|e| ProjectError::Session(e.to_string()))?;
        if pinned.is_none() {
            pins.insert(key, fingerprint.clone());
            self.save_collab_pins(&pins)?;
        }
        let access = match client.access() {
            twin_collab::sync::Access::Editor => "can edit",
            twin_collab::sync::Access::Viewer => "view only",
        };
        self.status = format!("Joined {socket} ({access}; fingerprint {fingerprint})");
        self.collab_host = None;
        self.collab_client = Some(client);
        Ok(())
    }

    /// Stop hosting or leave the session. A host stops listening at once.
    pub fn stop_session(&mut self) {
        if self.collab_active() {
            self.status = "Session stopped".to_string();
        }
        self.collab_host = None;
        self.collab_client = None;
    }

    /// The parameters of the session, as this window sees them.
    pub fn shared_parameters(&self) -> Option<BTreeMap<String, f64>> {
        match (&self.collab_host, &self.collab_client) {
            (Some(host), _) => Some(host.parameters()),
            (None, Some(client)) => Some(client.parameters()),
            (None, None) => None,
        }
    }

    /// Set a shared parameter from the session fields. Refused unless the value
    /// is a finite number.
    pub fn set_shared_parameter(&mut self) -> Result<(), ProjectError> {
        let name = self.collab_fields.name.trim().to_string();
        if name.is_empty() {
            return Err(ProjectError::Session("type a parameter name first".into()));
        }
        let value: f64 = self
            .collab_fields
            .value
            .trim()
            .parse()
            .map_err(|_| ProjectError::NotANumber(name.clone()))?;
        if !value.is_finite() {
            return Err(ProjectError::RejectedValue(name));
        }
        if let Some(host) = &self.collab_host {
            host.set_parameter(&name, value);
        } else if let Some(client) = self.collab_client.as_mut() {
            client
                .set_parameter(&name, value)
                .map_err(|e| ProjectError::Session(e.to_string()))?;
        } else {
            return Err(ProjectError::Session(
                "start or join a session first".into(),
            ));
        }
        self.status = format!("Shared {name} = {value}");
        Ok(())
    }

    /// Copy the session's parameters into the open twin, as unsaved edits.
    pub fn apply_shared_parameters(&mut self) -> Result<(), ProjectError> {
        let shared = self
            .shared_parameters()
            .ok_or_else(|| ProjectError::Session("start or join a session first".into()))?;
        let twin = self.current.as_mut().ok_or(ProjectError::NoTwinOpen)?;
        for (name, value) in shared {
            twin.set_parameter(name, value)?;
        }
        self.status = "Applied the shared parameters (not saved)".to_string();
        Ok(())
    }

    /// Apply whatever a joined session has sent. Call this regularly while a
    /// session is open. If the host goes away, the session ends.
    pub fn collab_poll(&mut self) -> Result<(), ProjectError> {
        let Some(client) = self.collab_client.as_mut() else {
            return Ok(());
        };
        if let Err(e) = client.poll(Duration::from_millis(1)) {
            self.collab_client = None;
            return Err(ProjectError::Session(format!("the session ended: {e}")));
        }
        Ok(())
    }

    fn load_collab_pins(&self) -> Result<BTreeMap<String, String>, ProjectError> {
        let text = match std::fs::read_to_string(self.dir.join(COLLAB_PINS_FILE)) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(e.into()),
        };
        Ok(text
            .lines()
            .filter_map(|line| line.split_once(' '))
            .map(|(addr, fp)| (addr.to_string(), fp.to_string()))
            .collect())
    }

    fn save_collab_pins(&self, pins: &BTreeMap<String, String>) -> Result<(), ProjectError> {
        let text: String = pins
            .iter()
            .map(|(addr, fp)| format!("{addr} {fp}\n"))
            .collect();
        std::fs::write(self.dir.join(COLLAB_PINS_FILE), text)?;
        Ok(())
    }

    /// The sensors of the open twin, empty if none is open.
    pub fn sensors(&self) -> Result<Vec<Sensor>, ProjectError> {
        match &self.current {
            Some(twin) => Ok(self.store.load_sensors(twin.id)?),
            None => Ok(Vec::new()),
        }
    }

    /// Add a sensor of `kind` to the open twin, named after its kind with a
    /// number when needed (`imu`, `imu-2`, and so on). Sensors are stored
    /// straight away, separately from the twin document.
    pub fn add_sensor(&mut self, kind: NewSensor) -> Result<(), ProjectError> {
        let twin = self.current.as_ref().ok_or(ProjectError::NoTwinOpen)?;
        let mut sensors = self.store.load_sensors(twin.id)?;
        let (base, kind) = match kind {
            NewSensor::Imu => (
                "imu",
                SensorKind::Imu {
                    sample_rate_hz: 50.0,
                },
            ),
            NewSensor::Camera => (
                "camera",
                SensorKind::Camera {
                    fov_deg: 60.0,
                    width_px: 640,
                    height_px: 480,
                },
            ),
            NewSensor::Lidar => (
                "lidar",
                SensorKind::Lidar {
                    range_m: 30.0,
                    beams: 360,
                },
            ),
        };
        let name = (1..)
            .map(|n| {
                if n == 1 {
                    base.to_string()
                } else {
                    format!("{base}-{n}")
                }
            })
            .find(|name| !sensors.iter().any(|s| &s.name == name))
            .expect("some name is free");
        sensors.push(Sensor {
            name: name.clone(),
            mount: Mount::default(),
            kind,
        });
        self.store.save_sensors(twin.id, &sensors)?;
        self.status = format!("Added sensor {name}");
        Ok(())
    }

    /// Type `text` into a setting of sensor `name`. A valid number is stored at
    /// once (sensors are stored separately from the twin). Other text is kept as
    /// a draft, and a value the sensor refuses is kept too and blocks `save`
    /// until it is fixed.
    pub fn edit_sensor(
        &mut self,
        name: &str,
        field: SensorField,
        text: &str,
    ) -> Result<(), ProjectError> {
        let id = self.current.as_ref().ok_or(ProjectError::NoTwinOpen)?.id;
        let key = sensor_draft_key(name, field);
        self.drafts.insert(key.clone(), text.to_string());
        let value: f64 = text
            .trim()
            .parse()
            .map_err(|_| ProjectError::NotANumber(key.clone()))?;
        let mut sensors = self.store.load_sensors(id)?;
        let index = sensors
            .iter()
            .position(|s| s.name == name)
            .ok_or_else(|| ProjectError::NoSensor(name.to_string()))?;
        let result = with_sensor_value(&sensors[index], field, value);
        match result {
            Ok(updated) => {
                sensors[index] = updated;
                self.store.save_sensors(id, &sensors)?;
                self.rejected.remove(&key);
                self.status = format!("Edited sensor {name}");
                Ok(())
            }
            Err(e) => {
                // A setting the sensor does not have is not something to type
                // into, so no draft is kept for it.
                if matches!(e, ProjectError::NoSensorField(_)) {
                    self.drafts.remove(&key);
                } else {
                    self.rejected.insert(key);
                }
                Err(e)
            }
        }
    }

    /// The text a sensor setting field should show: the draft if one is being
    /// typed, otherwise the stored value. `None` if there is no such setting.
    pub fn sensor_text(&self, name: &str, field: SensorField) -> Option<String> {
        let key = sensor_draft_key(name, field);
        if let Some(draft) = self.drafts.get(&key) {
            return Some(draft.clone());
        }
        let twin = self.current.as_ref()?;
        let sensors = self.store.load_sensors(twin.id).ok()?;
        let sensor = sensors.iter().find(|s| s.name == name)?;
        sensor_value(sensor, field).map(|v| v.to_string())
    }

    /// Remove sensor `name` and its stored recording, if any.
    pub fn remove_sensor(&mut self, name: &str) -> Result<(), ProjectError> {
        let twin = self.current.as_ref().ok_or(ProjectError::NoTwinOpen)?;
        let sensors: Vec<Sensor> = self
            .store
            .load_sensors(twin.id)?
            .into_iter()
            .filter(|s| s.name != name)
            .collect();
        self.store.save_sensors(twin.id, &sensors)?;
        self.store.remove_recording(twin.id, name)?;
        self.status = format!("Removed sensor {name}");
        Ok(())
    }

    /// For a lidar on the open twin: how many sampled points of the design's
    /// surface fall within its range. `None` for other sensors, or if there is
    /// no geometry yet. The count ignores occlusion, so back faces count too.
    pub fn lidar_summary(&self, sensor: &str) -> Result<Option<String>, ProjectError> {
        Ok(self.scan_lidar(sensor)?.map(|scan| {
            format!(
                "{} of {} sampled surface points within {} m (no occlusion)",
                scan.returns.len(),
                scan.sampled,
                scan.range_m
            )
        }))
    }

    /// Write the in-range points of lidar `sensor`'s scan of the design to
    /// `lidar-<sensor>.xyz` in the project folder, one `x y z` line per point in
    /// the sensor frame. Returns a summary, or `None` if the sensor is not a
    /// lidar or there is no geometry.
    pub fn save_lidar_scan(&self, sensor: &str) -> Result<Option<String>, ProjectError> {
        let Some(scan) = self.scan_lidar(sensor)? else {
            return Ok(None);
        };
        let path = self.dir.join(format!("lidar-{sensor}.xyz"));
        let text: String = scan
            .returns
            .iter()
            .map(|p| {
                format!(
                    "{} {} {}
",
                    p[0], p[1], p[2]
                )
            })
            .collect();
        std::fs::write(&path, text)?;
        Ok(Some(format!(
            "{} points saved to {}",
            scan.returns.len(),
            path.display()
        )))
    }

    /// The in-range points of lidar `sensor`, with the sampling it used.
    fn scan_lidar(&self, sensor: &str) -> Result<Option<LidarScan>, ProjectError> {
        let Some(design) = &self.design else {
            return Ok(None);
        };
        let sensors = self.sensors()?;
        let found = sensors
            .iter()
            .find(|s| s.name == sensor)
            .ok_or_else(|| ProjectError::NoSensor(sensor.to_string()))?;
        let SensorKind::Lidar { range_m, .. } = found.kind else {
            return Ok(None);
        };
        let model = design.preview().map_err(DesignError::from)?;
        let points = twin_sensors::sample_surface(&model.triangles(), LIDAR_SAMPLES_PER_EDGE);
        let returns = twin_sensors::lidar_returns(&points, &found.mount, range_m);
        Ok(Some(LidarScan {
            range_m,
            sampled: points.len(),
            returns,
        }))
    }

    /// One line describing the stored recording of `sensor`, or `None` if it
    /// has none yet (a recording is made by running a scenario).
    pub fn recording_summary(&self, sensor: &str) -> Result<Option<String>, ProjectError> {
        let twin = self.current.as_ref().ok_or(ProjectError::NoTwinOpen)?;
        let Some(rec) = self.store.load_recording(twin.id, sensor)? else {
            return Ok(None);
        };
        let (Some(&end), Some(&start)) = (rec.times_s.last(), rec.times_s.first()) else {
            return Ok(None);
        };
        let peak = rec
            .samples
            .iter()
            .flatten()
            .map(|v| v.abs())
            .fold(0.0_f64, f64::max);
        Ok(Some(format!(
            "{} samples from {start:.2} s to {end:.2} s, peak |{}| {peak:.3}",
            rec.times_s.len(),
            rec.channels.first().map_or("", String::as_str),
        )))
    }

    pub fn history(&self) -> Option<&History> {
        self.history.as_ref()
    }

    pub fn commit_message(&self) -> &str {
        &self.commit_message
    }

    pub fn set_commit_message(&mut self, text: impl Into<String>) {
        self.commit_message = text.into();
    }

    pub fn branch_name(&self) -> &str {
        &self.branch_name
    }

    pub fn set_branch_name(&mut self, text: impl Into<String>) {
        self.branch_name = text.into();
    }

    /// Start a parameter history for the open twin, with its current parameters
    /// as the first commit.
    pub fn start_history(&mut self) -> Result<(), ProjectError> {
        let twin = self.current.as_ref().ok_or(ProjectError::NoTwinOpen)?;
        let history = History::new(twin.parameters.clone(), "Initial parameters");
        self.store.save_history(twin.id, &history)?;
        self.status = format!("Started history for {}", twin.name);
        self.history = Some(history);
        Ok(())
    }

    /// Record the open twin's parameters as a commit on the current branch.
    /// The twin must be saved first, so the history matches the stored twin.
    pub fn commit(&mut self) -> Result<CommitId, ProjectError> {
        let twin = self.current.as_ref().ok_or(ProjectError::NoTwinOpen)?;
        if self.history.is_none() {
            return Err(ProjectError::NoHistory);
        }
        if self.is_dirty()? {
            return Err(ProjectError::UnsavedTwin);
        }
        if self.commit_message.trim().is_empty() {
            return Err(ProjectError::EmptyMessage);
        }
        let history = self.history.as_mut().ok_or(ProjectError::NoHistory)?;
        for (name, value) in &twin.parameters {
            history.set(name.clone(), *value);
        }
        let id = history.commit_changes(self.commit_message.trim())?;
        self.commit_message.clear();
        self.save_history()?;
        self.status = format!("Committed {id}");
        Ok(id)
    }

    /// Create a branch at the current commit, named by the branch name field.
    pub fn create_branch(&mut self) -> Result<(), ProjectError> {
        let name = self.branch_name.trim().to_string();
        if name.is_empty() {
            return Err(ProjectError::EmptyBranchName);
        }
        let history = self.history.as_mut().ok_or(ProjectError::NoHistory)?;
        history.create_branch(&name)?;
        self.branch_name.clear();
        self.save_history()?;
        self.status = format!("Created branch {name}");
        Ok(())
    }

    /// Switch to branch `name`. The twin's parameters become that branch's, as
    /// unsaved edits. The twin must be saved first.
    pub fn switch_branch(&mut self, name: &str) -> Result<(), ProjectError> {
        if self.is_dirty()? {
            return Err(ProjectError::UnsavedTwin);
        }
        let history = self.history.as_mut().ok_or(ProjectError::NoHistory)?;
        history.switch(name)?;
        let parameters = history.working().clone();
        self.save_history()?;
        let twin = self.current.as_mut().ok_or(ProjectError::NoTwinOpen)?;
        twin.parameters = parameters;
        self.drafts.clear();
        self.rejected.clear();
        self.status = format!("Switched to {name} (not saved)");
        Ok(())
    }

    /// Merge branch `from` into the current branch and load the merged
    /// parameters into the twin, as unsaved edits. Refused, with nothing
    /// changed, if a parameter was changed differently on both sides. The twin
    /// must be saved first.
    pub fn merge_branch(&mut self, from: &str) -> Result<(), ProjectError> {
        if self.is_dirty()? {
            return Err(ProjectError::UnsavedTwin);
        }
        let history = self.history.as_mut().ok_or(ProjectError::NoHistory)?;
        history.merge(from, &format!("Merge {from}"))?;
        let parameters = history.working().clone();
        self.save_history()?;
        let twin = self.current.as_mut().ok_or(ProjectError::NoTwinOpen)?;
        twin.parameters = parameters;
        self.drafts.clear();
        self.rejected.clear();
        self.status = format!("Merged {from} (not saved)");
        Ok(())
    }

    /// Compare the tip of `branch` with the head, or with another commit
    /// chosen by `compare_to_commit`.
    pub fn set_comparison(&mut self, branch: &str) -> Result<(), ProjectError> {
        let history = self.history.as_ref().ok_or(ProjectError::NoHistory)?;
        let tip = history
            .branches()
            .find(|(name, _)| *name == branch)
            .map(|(_, id)| id.clone())
            .ok_or_else(|| HistoryError::NoSuchBranch(branch.to_string()))?;
        self.compare_from = Some(tip);
        self.compare_to = None;
        Ok(())
    }

    /// Use commit `id` as the older side of the comparison.
    pub fn compare_from_commit(&mut self, id: CommitId) -> Result<(), ProjectError> {
        let history = self.history.as_ref().ok_or(ProjectError::NoHistory)?;
        history.commit(&id)?;
        self.compare_from = Some(id);
        Ok(())
    }

    /// Use commit `id` as the newer side of the comparison (default: the head).
    pub fn compare_to_commit(&mut self, id: CommitId) -> Result<(), ProjectError> {
        let history = self.history.as_ref().ok_or(ProjectError::NoHistory)?;
        history.commit(&id)?;
        self.compare_to = Some(id);
        Ok(())
    }

    /// The comparison chosen in the history panel, with the parameter changes
    /// from its older commit to its newer one. `None` until an older commit is
    /// chosen.
    pub fn comparison(&self) -> Result<Option<Comparison>, ProjectError> {
        let (Some(from), Some(history)) = (&self.compare_from, &self.history) else {
            return Ok(None);
        };
        let to = self
            .compare_to
            .clone()
            .unwrap_or_else(|| history.head_commit().clone());
        let changes = history.diff(from, &to)?;
        Ok(Some(Comparison {
            from: from.clone(),
            to,
            changes,
        }))
    }

    /// Parameter changes made by the head commit, against its first parent.
    /// Empty if there is no history or the head has no parent.
    pub fn head_diff(&self) -> Result<Vec<Change>, ProjectError> {
        let Some(history) = &self.history else {
            return Ok(Vec::new());
        };
        let head = history.head_commit().clone();
        match history.commit(&head)?.parents.first() {
            None => Ok(Vec::new()),
            Some(parent) => Ok(history.diff(parent, &head)?),
        }
    }

    fn save_history(&self) -> Result<(), ProjectError> {
        let (Some(twin), Some(history)) = (&self.current, &self.history) else {
            return Err(ProjectError::NoHistory);
        };
        self.store.save_history(twin.id, history)?;
        Ok(())
    }

    /// Save the open twin. Refused while a parameter field holds text that is
    /// not a number, so the stored twin never silently loses a typed edit.
    pub fn save(&mut self) -> Result<(), ProjectError> {
        let twin = self.current.as_ref().ok_or(ProjectError::NoTwinOpen)?;
        for (name, text) in &self.drafts {
            if text.trim().parse::<f64>().is_err() {
                return Err(ProjectError::NotANumber(name.clone()));
            }
        }
        if let Some(key) = self.rejected.first() {
            return Err(ProjectError::RejectedValue(key.clone()));
        }
        self.store.save(twin)?;
        self.drafts.clear();
        self.rejected.clear();
        self.status = format!("Saved {}", twin.name);
        Ok(())
    }

    /// Reload the twin list from the store.
    pub fn refresh(&mut self) -> Result<(), ProjectError> {
        self.twins = self.store.list()?;
        Ok(())
    }

    pub fn twins(&self) -> &[(TwinId, String)] {
        &self.twins
    }

    pub fn current(&self) -> Option<&Twin> {
        self.current.as_ref()
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    /// Show a message in the status line, for example an error from an action.
    pub fn set_status(&mut self, message: impl Into<String>) {
        self.status = message.into();
    }

    /// True if the open twin differs from what is stored.
    pub fn is_dirty(&self) -> Result<bool, ProjectError> {
        let Some(twin) = &self.current else {
            return Ok(false);
        };
        Ok(self.store.load(twin.id)?.as_ref() != Some(twin))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("twin-app-test-{tag}-{}", TwinId::new()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn new_twin_is_saved_and_listed() {
        let dir = temp_dir("new");
        let mut p = Project::open(&dir).unwrap();
        let id = p.new_twin("Robot arm").unwrap();
        assert_eq!(p.twins(), [(id, "Robot arm".to_string())]);
        assert_eq!(p.current().unwrap().id, id);
        assert!(!p.is_dirty().unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn edits_survive_a_save_and_a_reopen() {
        let dir = temp_dir("save");
        let id = {
            let mut p = Project::open(&dir).unwrap();
            let id = p.new_twin("spring").unwrap();
            p.set_parameter("mass_kg", 2.5).unwrap();
            assert!(p.is_dirty().unwrap());
            p.save().unwrap();
            assert!(!p.is_dirty().unwrap());
            id
        };
        let mut p = Project::open(&dir).unwrap();
        p.open_twin(id).unwrap();
        assert_eq!(p.current().unwrap().parameters["mass_kg"], 2.5);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unsaved_edits_are_discarded_when_another_twin_opens() {
        let dir = temp_dir("discard");
        let mut p = Project::open(&dir).unwrap();
        let first = p.new_twin("first").unwrap();
        p.set_parameter("mass_kg", 9.0).unwrap();
        let second = p.new_twin("second").unwrap();
        p.open_twin(first).unwrap();
        assert!(!p.current().unwrap().parameters.contains_key("mass_kg"));
        assert_eq!(p.current().unwrap().id, first);
        assert_ne!(first, second);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn bad_values_are_refused_and_leave_the_twin_alone() {
        let dir = temp_dir("bad");
        let mut p = Project::open(&dir).unwrap();
        p.new_twin("spring").unwrap();
        p.set_parameter("mass_kg", 1.0).unwrap();
        assert!(matches!(
            p.set_parameter("mass_kg", f64::NAN),
            Err(ProjectError::Twin(_))
        ));
        assert_eq!(p.current().unwrap().parameters["mass_kg"], 1.0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn parameter_fields_keep_what_is_typed_and_block_save_when_invalid() {
        let dir = temp_dir("drafts");
        let mut p = Project::open(&dir).unwrap();
        p.new_twin("spring").unwrap();
        p.set_parameter("mass_kg", 1.0).unwrap();
        assert_eq!(p.parameter_text("mass_kg").as_deref(), Some("1"));

        // A partial number is applied, but the field still shows what was typed.
        p.edit_parameter("mass_kg", "2.").unwrap();
        assert_eq!(p.current().unwrap().parameters["mass_kg"], 2.0);
        assert_eq!(p.parameter_text("mass_kg").as_deref(), Some("2."));

        // Text that is not a number is refused: the twin keeps 2.0, the draft
        // stays visible, and save is refused.
        assert!(matches!(
            p.edit_parameter("mass_kg", "2.x"),
            Err(ProjectError::NotANumber(_))
        ));
        assert_eq!(p.current().unwrap().parameters["mass_kg"], 2.0);
        assert_eq!(p.parameter_text("mass_kg").as_deref(), Some("2.x"));
        assert!(matches!(p.save(), Err(ProjectError::NotANumber(_))));

        // Fixing the text clears the problem, and save stores the value.
        p.edit_parameter("mass_kg", "3.5").unwrap();
        p.save().unwrap();
        assert_eq!(p.parameter_text("mass_kg").as_deref(), Some("3.5"));
        assert!(p.parameter_text("no_such_parameter").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn history_commits_branches_and_switches_with_the_twin_parameters() {
        let dir = temp_dir("history");
        let id = {
            let mut p = Project::open(&dir).unwrap();
            let id = p.new_twin("spring").unwrap();
            p.set_parameter("mass_kg", 1.0).unwrap();
            p.save().unwrap();
            assert!(p.history().is_none());
            assert!(matches!(p.commit(), Err(ProjectError::NoHistory)));
            p.start_history().unwrap();

            // Unsaved edits block commits and branch switches.
            p.set_parameter("mass_kg", 2.0).unwrap();
            p.set_commit_message("heavier");
            assert!(matches!(p.commit(), Err(ProjectError::UnsavedTwin)));
            p.save().unwrap();
            p.commit().unwrap();
            assert_eq!(p.history().unwrap().log().len(), 2);
            assert!(p.commit_message().is_empty());
            assert!(matches!(p.commit(), Err(ProjectError::EmptyMessage)));

            // A branch from here, then a change on main only.
            p.set_branch_name("  stiff  ");
            p.create_branch().unwrap();
            p.set_parameter("mass_kg", 5.0).unwrap();
            p.save().unwrap();
            p.set_commit_message("much heavier");
            p.commit().unwrap();
            let diff = p.head_diff().unwrap();
            assert_eq!(diff.len(), 1);
            assert_eq!(diff[0].key, "mass_kg");
            assert_eq!((diff[0].before, diff[0].after), (Some(2.0), Some(5.0)));

            // Switching back loads the branch's parameters into the twin.
            p.switch_branch("stiff").unwrap();
            assert_eq!(p.current().unwrap().parameters["mass_kg"], 2.0);
            assert!(p.is_dirty().unwrap());
            p.save().unwrap();
            assert_eq!(p.history().unwrap().head_branch(), "stiff");
            id
        };

        // History survives a reopen, and the head branch with it.
        let mut p = Project::open(&dir).unwrap();
        p.open_twin(id).unwrap();
        let history = p.history().unwrap();
        assert_eq!(history.head_branch(), "stiff");
        assert_eq!(history.branches().count(), 2);
        assert_eq!(history.log().len(), 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn branches_merge_when_they_do_not_conflict_and_refuse_when_they_do() {
        let dir = temp_dir("merge");
        let mut p = Project::open(&dir).unwrap();
        p.new_twin("spring").unwrap();
        p.set_parameter("mass_kg", 1.0).unwrap();
        p.set_parameter("damping_n_s_per_m", 0.5).unwrap();
        p.save().unwrap();
        p.start_history().unwrap();

        // `side` changes the damping; main changes the mass. No conflict.
        p.set_branch_name("side");
        p.create_branch().unwrap();
        // Creating a branch does not move to it, so switch before committing.
        p.switch_branch("side").unwrap();
        p.set_parameter("damping_n_s_per_m", 0.9).unwrap();
        p.save().unwrap();
        p.set_commit_message("more damping");
        p.commit().unwrap();
        p.switch_branch("main").unwrap();
        p.save().unwrap();
        p.set_parameter("mass_kg", 2.0).unwrap();
        p.save().unwrap();
        p.set_commit_message("heavier");
        p.commit().unwrap();

        // The comparison lists what differs between `side` and the head.
        assert!(p.comparison().unwrap().is_none());
        p.set_comparison("side").unwrap();
        let cmp = p.comparison().unwrap().unwrap();
        assert_eq!(cmp.changes.len(), 2);
        assert!(matches!(
            p.set_comparison("nowhere"),
            Err(ProjectError::History(HistoryError::NoSuchBranch(_)))
        ));

        // Any two commits can be compared. The first commit of the history is
        // the oldest in the log (main: mass 1, damping 0.5; head: mass 2).
        let history = p.history().unwrap();
        let first = history.log().last().unwrap().id.clone();
        let head = history.head_commit().clone();
        p.compare_from_commit(first.clone()).unwrap();
        let cmp = p.comparison().unwrap().unwrap();
        assert_eq!((cmp.from.clone(), cmp.to.clone()), (first.clone(), head));
        assert_eq!(cmp.changes.len(), 1);
        assert_eq!(cmp.changes[0].key, "mass_kg");
        p.compare_to_commit(first.clone()).unwrap();
        assert!(p.comparison().unwrap().unwrap().changes.is_empty());

        p.merge_branch("side").unwrap();
        assert_eq!(p.current().unwrap().parameters["mass_kg"], 2.0);
        assert_eq!(p.current().unwrap().parameters["damping_n_s_per_m"], 0.9);
        assert!(p.is_dirty().unwrap());
        p.save().unwrap();

        // Both sides now change the mass differently: the merge is refused and
        // the twin keeps its parameters.
        p.switch_branch("side").unwrap();
        p.save().unwrap();
        p.set_parameter("mass_kg", 7.0).unwrap();
        p.save().unwrap();
        p.set_commit_message("side mass");
        p.commit().unwrap();
        p.switch_branch("main").unwrap();
        p.save().unwrap();
        p.set_parameter("mass_kg", 3.0).unwrap();
        p.save().unwrap();
        p.set_commit_message("main mass");
        p.commit().unwrap();
        assert!(matches!(
            p.merge_branch("side"),
            Err(ProjectError::History(HistoryError::Conflict(_)))
        ));
        assert_eq!(p.current().unwrap().parameters["mass_kg"], 3.0);
        assert!(!p.is_dirty().unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_example_model_runs_only_when_saved_and_is_summarised() {
        let dir = temp_dir("sim");
        let mut p = Project::open(&dir).unwrap();
        p.new_twin("spring").unwrap();
        assert!(matches!(
            p.run_scenario(EXAMPLE_SCENARIO),
            Err(ProjectError::Store(StoreError::NoScenario(_)))
        ));
        assert_eq!(p.run_summary(EXAMPLE_SCENARIO).unwrap(), None);

        p.add_example_model().unwrap();
        // Unsaved edits block a run, so a result always matches the stored twin.
        assert!(matches!(
            p.run_scenario(EXAMPLE_SCENARIO),
            Err(ProjectError::UnsavedTwin)
        ));
        p.save().unwrap();
        // A second call keeps the parameters the user may have changed.
        p.set_parameter("mass_kg", 2.0).unwrap();
        p.add_example_model().unwrap();
        assert_eq!(p.current().unwrap().parameters["mass_kg"], 2.0);
        p.set_parameter("mass_kg", 1.0).unwrap();
        p.save().unwrap();

        p.run_scenario(EXAMPLE_SCENARIO).unwrap();
        let summary = p.run_summary(EXAMPLE_SCENARIO).unwrap().unwrap();
        assert!(summary.starts_with("101 samples to 10.00 s"), "{summary}");
        assert!(!summary.contains("stale"), "{summary}");

        // Changing a parameter after the run marks it stale, until it is run again.
        p.set_parameter("mass_kg", 3.0).unwrap();
        p.save().unwrap();
        let summary = p.run_summary(EXAMPLE_SCENARIO).unwrap().unwrap();
        assert!(summary.contains("(stale"), "{summary}");
        p.run_scenario(EXAMPLE_SCENARIO).unwrap();
        let summary = p.run_summary(EXAMPLE_SCENARIO).unwrap().unwrap();
        assert!(!summary.contains("stale"), "{summary}");

        // The plot shows the chosen scenario's stored run, starting from the
        // initial conditions (position 0.1 m, at rest).
        assert_eq!(p.plot_data().unwrap(), None);
        p.set_plot(PlotChoice::Run(EXAMPLE_SCENARIO.into()));
        let plot = p.plot_data().unwrap().unwrap();
        assert_eq!(plot.times.len(), 101);
        assert_eq!(plot.series[0].0, "position (m)");
        assert_eq!(plot.series[1].0, "velocity (m/s)");
        assert!((plot.series[0].1[0] - 0.1).abs() < 1e-12);
        assert_eq!(plot.series[1].1[0], 0.0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn scenario_fields_edit_like_parameters_and_block_save_when_invalid() {
        let dir = temp_dir("scenario-edit");
        let mut p = Project::open(&dir).unwrap();
        p.new_twin("spring").unwrap();
        p.add_example_model().unwrap();
        p.save().unwrap();

        let duration = ScenarioField::Duration;
        let position = ScenarioField::Initial("position_m".into());
        assert_eq!(
            p.scenario_text(EXAMPLE_SCENARIO, &duration).as_deref(),
            Some("10")
        );

        // A partial number is applied and the field keeps what was typed.
        p.edit_scenario(EXAMPLE_SCENARIO, &duration, "2.").unwrap();
        assert_eq!(p.current().unwrap().scenarios[0].duration_s, 2.0);
        assert_eq!(
            p.scenario_text(EXAMPLE_SCENARIO, &duration).as_deref(),
            Some("2.")
        );

        // Invalid values leave the twin alone, and save is refused while the
        // draft is not a number.
        assert!(matches!(
            p.edit_scenario(EXAMPLE_SCENARIO, &duration, "-1"),
            Err(ProjectError::Twin(_))
        ));
        assert_eq!(p.current().unwrap().scenarios[0].duration_s, 2.0);
        assert!(matches!(p.save(), Err(ProjectError::RejectedValue(_))));
        assert!(matches!(
            p.edit_scenario(EXAMPLE_SCENARIO, &position, "abc"),
            Err(ProjectError::NotANumber(_))
        ));
        assert!(matches!(p.save(), Err(ProjectError::NotANumber(_))));

        // Fixing both fields lets save store the new values.
        p.edit_scenario(EXAMPLE_SCENARIO, &duration, "2").unwrap();
        p.edit_scenario(EXAMPLE_SCENARIO, &position, "0.25")
            .unwrap();
        p.save().unwrap();
        let sc = &p.current().unwrap().scenarios[0];
        assert_eq!(sc.initial_conditions["position_m"], 0.25);
        assert_eq!(sc.duration_s, 2.0);

        // The duration can be shortened and run again.
        p.edit_scenario(EXAMPLE_SCENARIO, &duration, "10").unwrap();
        p.save().unwrap();
        p.run_scenario(EXAMPLE_SCENARIO).unwrap();
        let summary = p.run_summary(EXAMPLE_SCENARIO).unwrap().unwrap();
        assert!(summary.starts_with("101 samples to 10.00 s"), "{summary}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn imu_sensors_are_added_summarised_after_a_run_and_removed() {
        let dir = temp_dir("sensors-panel");
        let mut p = Project::open(&dir).unwrap();
        p.new_twin("spring").unwrap();
        p.add_example_model().unwrap();
        p.save().unwrap();
        assert!(p.sensors().unwrap().is_empty());

        p.add_sensor(NewSensor::Imu).unwrap();
        p.add_sensor(NewSensor::Imu).unwrap();
        let names: Vec<String> = p.sensors().unwrap().into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["imu", "imu-2"]);

        // No recording until a scenario runs.
        assert_eq!(p.recording_summary("imu").unwrap(), None);
        p.run_scenario(EXAMPLE_SCENARIO).unwrap();
        let summary = p.recording_summary("imu").unwrap().unwrap();
        assert!(
            summary.starts_with("501 samples from 0.00 s to 10.00 s"),
            "{summary}"
        );

        // Removing a sensor removes its recording, and leaves the others.
        p.remove_sensor("imu").unwrap();
        assert_eq!(p.recording_summary("imu").unwrap(), None);
        assert_eq!(p.sensors().unwrap().len(), 1);
        assert!(p.recording_summary("imu-2").unwrap().is_some());

        // The IMU recording can be plotted: one acceleration channel over time.
        p.set_plot(PlotChoice::Sensor("imu-2".into()));
        let plot = p.plot_data().unwrap().unwrap();
        assert_eq!(plot.times.len(), 501);
        assert_eq!(plot.series.len(), 1);
        assert_eq!(plot.series[0].0, "accel_m_s2");

        // Cameras and lidars can be added too, each with a distinct name.
        p.add_sensor(NewSensor::Camera).unwrap();
        p.add_sensor(NewSensor::Lidar).unwrap();
        p.add_sensor(NewSensor::Lidar).unwrap();
        let names: Vec<String> = p.sensors().unwrap().into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["imu-2", "camera", "lidar", "lidar-2"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sensor_settings_are_stored_when_valid_and_refused_otherwise() {
        let dir = temp_dir("sensor-settings");
        let mut p = Project::open(&dir).unwrap();
        p.new_twin("spring").unwrap();
        p.add_sensor(NewSensor::Imu).unwrap();
        p.add_sensor(NewSensor::Camera).unwrap();
        p.add_sensor(NewSensor::Lidar).unwrap();
        let stored = |p: &Project, name: &str| {
            p.sensors()
                .unwrap()
                .into_iter()
                .find(|s| s.name == name)
                .unwrap()
        };

        p.edit_sensor("imu", SensorField::SampleRate, "100")
            .unwrap();
        assert_eq!(
            p.sensor_text("imu", SensorField::SampleRate).as_deref(),
            Some("100")
        );
        assert!(matches!(
            stored(&p, "imu").kind,
            SensorKind::Imu { sample_rate_hz } if sample_rate_hz == 100.0
        ));

        // A refused value keeps the stored sensor and blocks saving the twin.
        assert!(matches!(
            p.edit_sensor("imu", SensorField::SampleRate, "-5"),
            Err(ProjectError::Sensor(_))
        ));
        assert!(matches!(
            stored(&p, "imu").kind,
            SensorKind::Imu { sample_rate_hz } if sample_rate_hz == 100.0
        ));
        assert!(matches!(p.save(), Err(ProjectError::RejectedValue(_))));
        p.edit_sensor("imu", SensorField::SampleRate, "50").unwrap();
        p.save().unwrap();

        // Whole-number fields refuse fractions; fields a kind lacks are refused.
        assert!(matches!(
            p.edit_sensor("camera", SensorField::WidthPx, "800.5"),
            Err(ProjectError::Sensor(_))
        ));
        p.edit_sensor("camera", SensorField::WidthPx, "800")
            .unwrap();
        assert!(matches!(
            stored(&p, "camera").kind,
            SensorKind::Camera { width_px: 800, .. }
        ));
        assert!(matches!(
            p.edit_sensor("camera", SensorField::Beams, "10"),
            Err(ProjectError::NoSensorField(_))
        ));
        assert!(matches!(
            p.edit_sensor("camera", SensorField::FieldOfView, "200"),
            Err(ProjectError::Sensor(_))
        ));
        assert!(matches!(p.save(), Err(ProjectError::RejectedValue(_))));
        p.edit_sensor("camera", SensorField::FieldOfView, "60")
            .unwrap();
        p.save().unwrap();

        // Mount positions apply to any sensor, and the lidar's range can change.
        p.edit_sensor("lidar", SensorField::Position(2), "0.25")
            .unwrap();
        p.edit_sensor("lidar", SensorField::RangeM, "12").unwrap();
        assert_eq!(stored(&p, "lidar").mount.position_m, [0.0, 0.0, 0.25]);
        assert!(matches!(
            stored(&p, "lidar").kind,
            SensorKind::Lidar { range_m, .. } if range_m == 12.0
        ));
        assert!(matches!(
            p.edit_sensor("missing", SensorField::RangeM, "1"),
            Err(ProjectError::NoSensor(_))
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn geometry_is_committed_linked_and_gates_runs() {
        let dir = temp_dir("geometry");
        let id = {
            let mut p = Project::open(&dir).unwrap();
            let id = p.new_twin("block").unwrap();
            p.add_example_model().unwrap();
            p.save().unwrap();
            assert!(p.design().is_none());
            assert!(matches!(p.commit_design(), Err(ProjectError::NoDesign)));

            p.start_design().unwrap();
            assert!(matches!(p.start_design(), Err(ProjectError::DesignExists)));
            assert_eq!(p.design_text("width_m").as_deref(), Some("2"));
            assert!(p.geometry_status().unwrap().contains("not linked"));
            // The feature tree lists the modelling steps in words.
            assert_eq!(p.feature_tree(), ["1. block width_m x depth_m x height_m"]);

            // Once geometry exists, a run is refused until the twin is linked.
            assert!(matches!(
                p.run_scenario(EXAMPLE_SCENARIO),
                Err(ProjectError::Store(StoreError::StaleGeometry))
            ));

            // Uncommitted parameter changes cannot be linked.
            p.edit_design_parameter("width_m", "3").unwrap();
            assert_eq!(p.design_text("width_m").as_deref(), Some("3"));
            assert!(matches!(
                p.link_design(),
                Err(ProjectError::UncommittedDesign)
            ));
            assert!(matches!(
                p.edit_design_parameter("width_m", "inf"),
                Err(ProjectError::RejectedValue(_))
            ));
            // A refused value is not applied, and it blocks saving the twin.
            assert_eq!(p.design().unwrap().history().working()["width_m"], 3.0);
            assert!(matches!(p.save(), Err(ProjectError::RejectedValue(_))));
            p.edit_design_parameter("width_m", "3").unwrap();

            p.commit_design().unwrap();
            p.link_design().unwrap();
            assert!(p.current().unwrap().parameters.contains_key("width_m"));
            p.save().unwrap();
            assert!(p.geometry_status().unwrap().contains("linked to it"));
            p.run_scenario(EXAMPLE_SCENARIO).unwrap();
            assert!(
                !p.run_summary(EXAMPLE_SCENARIO)
                    .unwrap()
                    .unwrap()
                    .contains("stale")
            );
            // A lidar scans the linked geometry: a shorter range sees fewer points.
            let count = |summary: String| -> usize {
                summary.split_whitespace().next().unwrap().parse().unwrap()
            };
            p.add_sensor(NewSensor::Lidar).unwrap();
            p.add_sensor(NewSensor::Imu).unwrap();
            let far = count(p.lidar_summary("lidar").unwrap().unwrap());
            p.edit_sensor("lidar", SensorField::RangeM, "1.5").unwrap();
            let near = count(p.lidar_summary("lidar").unwrap().unwrap());
            assert!(far > 0 && near > 0 && near < far, "{near} vs {far}");
            assert_eq!(p.lidar_summary("imu").unwrap(), None);
            id
        };

        // The design survives a reopen, and the link with it.
        let mut p = Project::open(&dir).unwrap();
        p.open_twin(id).unwrap();
        assert!(p.geometry_status().unwrap().contains("linked to it"));
        assert_eq!(p.design_text("depth_m").as_deref(), Some("1"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn geometry_branches_compare_switch_and_merge() {
        let dir = temp_dir("geometry-branches");
        let mut p = Project::open(&dir).unwrap();
        p.new_twin("block").unwrap();
        p.save().unwrap();
        p.start_design().unwrap();

        // `wide` widens the block; main deepens it. Both start from the same commit.
        p.set_design_branch_name("wide");
        p.create_design_branch().unwrap();
        assert!(p.design_branch_name().is_empty());
        p.switch_design_branch("wide").unwrap();
        p.edit_design_parameter("width_m", "4").unwrap();
        p.commit_design().unwrap();
        p.switch_design_branch("main").unwrap();
        p.edit_design_parameter("depth_m", "3").unwrap();
        p.commit_design().unwrap();

        // Comparing lists what `wide` has that the head does not.
        let changes = p.design_changes("wide").unwrap();
        assert_eq!(changes.len(), 2, "{changes:?}");

        // Uncommitted edits block switching and merging.
        p.edit_design_parameter("depth_m", "7").unwrap();
        assert!(matches!(
            p.switch_design_branch("wide"),
            Err(ProjectError::UncommittedDesign)
        ));
        assert!(matches!(
            p.merge_design_branch("wide"),
            Err(ProjectError::UncommittedDesign)
        ));
        p.edit_design_parameter("depth_m", "3").unwrap();

        // The two sides changed different parameters, so the merge succeeds and
        // keeps both changes.
        p.merge_design_branch("wide").unwrap();
        assert_eq!(p.design_text("width_m").as_deref(), Some("4"));
        assert_eq!(p.design_text("depth_m").as_deref(), Some("3"));
        // `wide` still lacks the depth change that main made.
        let left = p.design_changes("wide").unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].key, "depth_m");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_folder_without_a_licence_has_no_licence_and_the_banner_follows_the_build() {
        let dir = temp_dir("licence-none");
        let p = Project::open(&dir).unwrap();
        assert!(p.licence().is_none());
        // Only official builds show the banner; source builds stay quiet.
        assert_eq!(p.licence_banner().is_some(), cfg!(feature = "official"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn twins_export_and_import_with_the_signer_pinned_on_first_use() {
        let src_dir = temp_dir("export-src");
        let dst_dir = temp_dir("export-dst");
        let keys_dir = temp_dir("export-keys");
        let key = load_or_create_export_key(&keys_dir).unwrap();
        // Creating the key again reads it back rather than replacing it.
        assert_eq!(
            load_or_create_export_key(&keys_dir).unwrap().to_bytes(),
            key.to_bytes()
        );

        let mut src = Project::open(&src_dir).unwrap();
        let id = src.new_twin("portable").unwrap();
        src.add_example_model().unwrap();
        src.save().unwrap();
        let package = src_dir.join("portable.twin");
        src.export_twin(&package, &key, None).unwrap();

        // Imports into another project, pinning the signer. Importing again is
        // fine because the signer is the same.
        let mut dst = Project::open(&dst_dir).unwrap();
        assert_eq!(dst.import_twin(&package, None).unwrap(), id);
        assert_eq!(dst.import_twin(&package, None).unwrap(), id);
        dst.open_twin(id).unwrap();
        assert!(
            dst.current()
                .unwrap()
                .scenarios
                .iter()
                .any(|s| s.name == EXAMPLE_SCENARIO)
        );

        // A package for the same twin from a different key is refused.
        let other = SigningKey::from_bytes(&[9u8; 32]);
        let forged = src_dir.join("forged.twin");
        src.export_twin(&forged, &other, None).unwrap();
        assert!(matches!(
            dst.import_twin(&forged, None),
            Err(ProjectError::SignerChanged(_))
        ));

        // Encrypted packages need the passphrase.
        let sealed = src_dir.join("sealed.twin");
        src.export_twin(&sealed, &key, Some("pass")).unwrap();
        assert!(dst.import_twin(&sealed, None).is_err());
        assert_eq!(dst.import_twin(&sealed, Some("pass")).unwrap(), id);

        for d in [src_dir, dst_dir, keys_dir] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn unix_times_format_as_iso_8601_utc() {
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601_utc(1_700_000_000), "2023-11-14T22:13:20Z");
        // A leap day.
        assert_eq!(iso8601_utc(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn a_camera_renders_the_design_and_writes_an_image() {
        let dir = temp_dir("camera-render");
        let mut p = Project::open(&dir).unwrap();
        p.new_twin("block").unwrap();
        p.save().unwrap();
        p.start_design().unwrap();
        p.add_sensor(NewSensor::Camera).unwrap();

        // Put the camera 5 m in front of the block's front face (z = 0), in line with it.
        p.edit_sensor("camera", SensorField::Position(0), "1")
            .unwrap();
        p.edit_sensor("camera", SensorField::Position(1), "0.5")
            .unwrap();
        p.edit_sensor("camera", SensorField::Position(2), "-5")
            .unwrap();
        let summary = p.render_camera("camera").unwrap().unwrap();
        assert!(summary.contains("nearest 5.00 m"), "{summary}");
        let image = std::fs::read(dir.join("camera-camera.pgm")).unwrap();
        assert!(image.starts_with(b"P5\n640 480\n255\n"));
        // A lidar scan is saved one point per line, matching its summary count.
        p.add_sensor(NewSensor::Lidar).unwrap();
        let summary = p.lidar_summary("lidar").unwrap().unwrap();
        let count: usize = summary.split_whitespace().next().unwrap().parse().unwrap();
        assert!(count > 0);
        assert!(p.save_lidar_scan("lidar").unwrap().is_some());
        // The saved scan can be drawn: one top-down point per saved line.
        p.set_plot(PlotChoice::Lidar("lidar".into()));
        let (name, points) = p.lidar_scan_xy().unwrap().unwrap();
        assert_eq!(name, "lidar");
        assert_eq!(points.len(), count);
        assert_eq!(p.plot_data().unwrap(), None);
        let saved = std::fs::read_to_string(dir.join("lidar-lidar.xyz")).unwrap();
        assert_eq!(saved.lines().count(), count);
        // An IMU is not a camera, so it has no render.
        p.add_sensor(NewSensor::Imu).unwrap();
        assert_eq!(p.render_camera("imu").unwrap(), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_server_identity_is_created_once_and_read_back() {
        let dir = temp_dir("server-identity");
        let first = load_or_create_server_identity(&dir).unwrap();
        let again = load_or_create_server_identity(&dir).unwrap();
        assert_eq!(first.fingerprint(), again.fingerprint());
        assert_eq!(first.key_der(), again.key_der());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Poll `check` until it holds, or fail after a few seconds.
    fn until(mut check: impl FnMut() -> bool) {
        for _ in 0..500 {
            if check() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("condition not reached in time");
    }

    #[test]
    fn two_projects_share_parameters_over_a_session() {
        let config = temp_dir("session-config");
        let host_dir = temp_dir("session-host");
        let join_dir = temp_dir("session-join");
        let mut host = Project::open(&host_dir).unwrap();
        host.new_twin("arm").unwrap();
        host.set_parameter("mass_kg", 1.0).unwrap();
        host.save().unwrap();
        host.host_session("127.0.0.1:0", false, &config, "code-1", "watch-1")
            .unwrap();
        let addr = host.host_address().unwrap().to_string();

        let mut joiner = Project::open(&join_dir).unwrap();
        joiner.new_twin("other").unwrap();

        // A wrong code is refused and nothing is pinned for the address.
        assert!(matches!(
            joiner.join_session(&addr, "wrong"),
            Err(ProjectError::Session(_))
        ));
        assert!(!joiner.collab_active());

        joiner.join_session(&addr, "code-1").unwrap();
        assert_eq!(joiner.shared_parameters().unwrap()["mass_kg"], 1.0);
        // The first join pinned the host's fingerprint for this address.
        let pins = std::fs::read_to_string(join_dir.join(COLLAB_PINS_FILE)).unwrap();
        assert!(pins.starts_with(&format!("{addr} ")), "{pins}");

        // A joiner's edit reaches the host.
        joiner.set_collab_text(CollabField::Name, "mass_kg");
        joiner.set_collab_text(CollabField::Value, "2.5");
        joiner.set_shared_parameter().unwrap();
        until(|| host.shared_parameters().unwrap()["mass_kg"] == 2.5);

        // The host's edit reaches the joiner, which can apply it to its twin.
        host.set_collab_text(CollabField::Name, "stiffness_n_per_m");
        host.set_collab_text(CollabField::Value, "9");
        host.set_shared_parameter().unwrap();
        until(|| {
            joiner.collab_poll().unwrap();
            joiner.shared_parameters().unwrap().get("stiffness_n_per_m") == Some(&9.0)
        });
        joiner.apply_shared_parameters().unwrap();
        assert_eq!(
            joiner.current().unwrap().parameters["stiffness_n_per_m"],
            9.0
        );

        // Refused values never leave the window.
        joiner.set_collab_text(CollabField::Value, "inf");
        assert!(matches!(
            joiner.set_shared_parameter(),
            Err(ProjectError::RejectedValue(_))
        ));

        // A view-only joiner sees the parameters, but its edits go nowhere.
        let viewer_dir = temp_dir("session-viewer");
        let mut viewer = Project::open(&viewer_dir).unwrap();
        viewer.new_twin("watcher").unwrap();
        viewer.join_session(&addr, "watch-1").unwrap();
        assert!(viewer.status().contains("view only"), "{}", viewer.status());
        assert_eq!(host.session_peers(), 2);
        assert_eq!(host.session_peer_names().len(), 2);
        viewer.set_collab_text(CollabField::Name, "mass_kg");
        viewer.set_collab_text(CollabField::Value, "7");
        assert!(viewer.set_shared_parameter().is_err());
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(host.shared_parameters().unwrap()["mass_kg"], 2.5);
        let _ = std::fs::remove_dir_all(viewer_dir);

        // Stopping the host ends the joiner's session.
        host.stop_session();
        until(|| {
            let _ = joiner.collab_poll();
            !joiner.collab_active()
        });
        for d in [config, host_dir, join_dir] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    /// Manual check of a real licence file: set `TWIN_LICENCE_CHECK_DIR` to a
    /// folder holding a `twin.license` signed with the vendor key, then run
    /// `cargo test -p twin-app -- --ignored licence_file_is_accepted`.
    #[test]
    #[ignore]
    fn licence_file_is_accepted() {
        let dir = std::env::var("TWIN_LICENCE_CHECK_DIR").expect("set TWIN_LICENCE_CHECK_DIR");
        let p = Project::open(Path::new(&dir)).unwrap();
        let licence = p.licence().expect("licence accepted");
        assert_eq!(licence.payload.license_id, "TEST-0001");
        assert!(p.licence_banner().is_none());
    }

    #[test]
    fn actions_without_an_open_twin_are_refused() {
        let dir = temp_dir("none");
        let mut p = Project::open(&dir).unwrap();
        assert!(matches!(
            p.set_parameter("mass_kg", 1.0),
            Err(ProjectError::NoTwinOpen)
        ));
        assert!(matches!(p.save(), Err(ProjectError::NoTwinOpen)));
        assert!(matches!(
            p.open_twin(TwinId::new()),
            Err(ProjectError::NotFound)
        ));
        let _ = std::fs::remove_dir_all(dir);
    }
}
