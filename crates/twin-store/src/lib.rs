//! Local Keystone database layer for twin state, results, sensor history and version metadata.
//!
//! Twins are stored as validated JSON documents in a single Keystone table.
//! Everything stays on disk under one directory, with no network access.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::{SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tpt_keystone::executor::execute_query;
use tpt_keystone::storage::config::{NodeRole, UdfConfig};
use tpt_keystone::storage::database::Database;
use tpt_keystone::storage::lease::LeaseManager;
use tpt_keystone::storage::objectstore::{LocalFsObjectStore, ObjectStore};
use twin_core::{Twin, TwinError, TwinId};
use twin_geometry::design::{Design, DesignError};
use twin_geometry::{History, HistoryError};
use twin_package::{Package, PackageError};
use twin_sensors::Sensor;
use twin_sim::SimError;

const TABLE: &str = "twins";
const HISTORY_TABLE: &str = "histories";
const DESIGN_TABLE: &str = "designs";
const RUN_TABLE: &str = "runs";
const SENSOR_TABLE: &str = "sensors";
const READING_TABLE: &str = "sensor_readings";

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("storage I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Database(String),
    #[error(transparent)]
    Twin(#[from] TwinError),
    #[error(transparent)]
    History(#[from] HistoryError),
    #[error(transparent)]
    Design(#[from] DesignError),
    #[error(transparent)]
    Sim(#[from] SimError),
    #[error("twin has no scenario named `{0}`")]
    NoScenario(String),
    #[error("sensor list is invalid: {0}")]
    Sensor(String),
    #[error("sensor recording is invalid: {0}")]
    Recording(String),
    #[error("no twin with id {0}")]
    NotFound(TwinId),
    #[error(transparent)]
    Package(#[from] PackageError),
    #[error("the twin's geometry does not match its design; relink the twin before simulating")]
    StaleGeometry,
}

fn db_err(e: anyhow::Error) -> StoreError {
    StoreError::Database(format!("{e:#}"))
}

/// One stored simulation run. `geometry_ref` is the design commit the run was
/// computed against, if the twin had one, so a result can be checked for
/// staleness after the geometry changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredRun {
    pub scenario: String,
    pub geometry_ref: Option<String>,
    /// The twin's parameters when the run was made. Runs stored before this
    /// field existed load with none, so they never match a twin with parameters.
    #[serde(default)]
    pub parameters: BTreeMap<String, f64>,
    pub times: Vec<f64>,
    pub states: Vec<[f64; 2]>,
}

/// Recorded output of one sensor of a twin over time, e.g. IMU readings from a
/// scenario. Each row of `samples` holds one value per channel, in the order of
/// `channels`, at the matching entry of `times_s`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SensorRecording {
    pub sensor: String,
    pub channels: Vec<String>,
    pub times_s: Vec<f64>,
    pub samples: Vec<Vec<f64>>,
}

impl SensorRecording {
    /// Check the shape and values: at least one channel with a unique non-empty
    /// name, strictly increasing finite times, and one finite value per channel
    /// in every sample.
    pub fn validated(self) -> Result<Self, StoreError> {
        let fail = |msg: &str| Err(StoreError::Recording(format!("{}: {msg}", self.sensor)));
        if self.sensor.trim().is_empty() {
            return Err(StoreError::Recording("sensor name is empty".into()));
        }
        if self.channels.is_empty() {
            return fail("no channels");
        }
        for (i, name) in self.channels.iter().enumerate() {
            if name.trim().is_empty() || self.channels[..i].contains(name) {
                return fail("channel names must be unique and non-empty");
            }
        }
        if self.samples.len() != self.times_s.len() {
            return fail("sample count does not match time count");
        }
        if self.times_s.iter().any(|t| !t.is_finite()) {
            return fail("times must be finite");
        }
        if self.times_s.windows(2).any(|w| w[1] <= w[0]) {
            return fail("times must be strictly increasing");
        }
        for row in &self.samples {
            if row.len() != self.channels.len() {
                return fail("sample width does not match channel count");
            }
            if row.iter().any(|v| !v.is_finite()) {
                return fail("sample values must be finite");
            }
        }
        Ok(self)
    }
}

const DESIGN_ENTRY: &str = "design.json";
const SENSOR_ENTRY: &str = "sensors.json";

fn run_entry(index: usize) -> String {
    format!("runs/{index}.json")
}

fn recording_entry(index: usize) -> String {
    format!("recordings/{index}.json")
}

/// True if `run` was computed against the twin's current geometry. A run
/// made before the twin had a geometry (or with none) matches only `None`.
pub fn run_is_current(twin: &Twin, run: &StoredRun) -> bool {
    run.geometry_ref == twin.geometry_ref
}

/// True if `run` was computed with the twin's current parameters.
pub fn run_matches_parameters(twin: &Twin, run: &StoredRun) -> bool {
    run.parameters == twin.parameters
}

/// A local store of twin documents.
pub struct TwinStore {
    db: Arc<Database>,
}

impl TwinStore {
    /// Open or create a store in `dir`. Keystone keeps its write-ahead log and
    /// object store under this directory.
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        let bucket = dir.join("objects");
        std::fs::create_dir_all(&bucket)?;
        let store: Arc<dyn ObjectStore> =
            Arc::new(LocalFsObjectStore::open(&bucket).map_err(db_err)?);
        let lease = Arc::new(LeaseManager::new(
            store.clone(),
            "twin",
            "local".into(),
            Duration::from_secs(30),
        ));
        lease.try_acquire().map_err(db_err)?;
        let db = Database::open(
            &dir.join("local"),
            store,
            lease.handle(),
            NodeRole::Writer,
            UdfConfig::default(),
        )
        .map_err(db_err)?;
        let db = Arc::new(db);
        execute_query(
            &format!(
                "CREATE TABLE IF NOT EXISTS {TABLE} (id TEXT PRIMARY KEY, name TEXT, doc TEXT)"
            ),
            db.clone(),
        )
        .map_err(db_err)?;
        execute_query(
            &format!("CREATE TABLE IF NOT EXISTS {HISTORY_TABLE} (id TEXT PRIMARY KEY, doc TEXT)"),
            db.clone(),
        )
        .map_err(db_err)?;
        execute_query(
            &format!("CREATE TABLE IF NOT EXISTS {DESIGN_TABLE} (id TEXT PRIMARY KEY, doc TEXT)"),
            db.clone(),
        )
        .map_err(db_err)?;
        execute_query(
            &format!(
                "CREATE TABLE IF NOT EXISTS {RUN_TABLE} (id TEXT PRIMARY KEY, twin_id TEXT, doc TEXT)"
            ),
            db.clone(),
        )
        .map_err(db_err)?;
        execute_query(
            &format!("CREATE TABLE IF NOT EXISTS {SENSOR_TABLE} (id TEXT PRIMARY KEY, doc TEXT)"),
            db.clone(),
        )
        .map_err(db_err)?;
        execute_query(
            &format!(
                "CREATE TABLE IF NOT EXISTS {READING_TABLE} (id TEXT PRIMARY KEY, twin_id TEXT, doc TEXT)"
            ),
            db.clone(),
        )
        .map_err(db_err)?;
        Ok(Self { db })
    }

    /// Insert or replace a twin. The twin is validated before it is written.
    pub fn save(&self, twin: &Twin) -> Result<(), StoreError> {
        let json = twin.to_json()?;
        let id = sql_text(&twin.id.to_string());
        let name = sql_text(&twin.name);
        let doc = sql_text(&json);
        self.run(&format!("DELETE FROM {TABLE} WHERE id = {id}"))?;
        self.run(&format!(
            "INSERT INTO {TABLE} (id, name, doc) VALUES ({id}, {name}, {doc})"
        ))?;
        Ok(())
    }

    /// Load a twin by ID, or `None` if it is not stored.
    pub fn load(&self, id: TwinId) -> Result<Option<Twin>, StoreError> {
        let rows = self.query(&format!(
            "SELECT doc FROM {TABLE} WHERE id = {}",
            sql_text(&id.to_string())
        ))?;
        match rows.first() {
            None => Ok(None),
            Some(json) => Ok(Some(Twin::from_json(json)?)),
        }
    }

    /// List the stored twins as `(id, name)` pairs, ordered by name.
    pub fn list(&self) -> Result<Vec<(TwinId, String)>, StoreError> {
        let rows = self.query_rows(&format!("SELECT id, name FROM {TABLE} ORDER BY name"))?;
        rows.into_iter()
            .map(|(id, name)| Ok((TwinId::parse(&id)?, name)))
            .collect()
    }

    /// Delete a twin and its history.
    pub fn delete(&self, id: TwinId) -> Result<(), StoreError> {
        self.run(&format!(
            "DELETE FROM {TABLE} WHERE id = {}",
            sql_text(&id.to_string())
        ))?;
        self.run(&format!(
            "DELETE FROM {HISTORY_TABLE} WHERE id = {}",
            sql_text(&id.to_string())
        ))?;
        self.run(&format!(
            "DELETE FROM {DESIGN_TABLE} WHERE id = {}",
            sql_text(&id.to_string())
        ))?;
        self.run(&format!(
            "DELETE FROM {RUN_TABLE} WHERE twin_id = {}",
            sql_text(&id.to_string())
        ))?;
        self.run(&format!(
            "DELETE FROM {SENSOR_TABLE} WHERE id = {}",
            sql_text(&id.to_string())
        ))?;
        self.run(&format!(
            "DELETE FROM {READING_TABLE} WHERE twin_id = {}",
            sql_text(&id.to_string())
        ))
    }

    /// Replace the sensors of a twin. Each sensor is validated first.
    /// Set the twin's `sensor_refs` to the sensor names to link them.
    pub fn save_sensors(&self, id: TwinId, sensors: &[Sensor]) -> Result<(), StoreError> {
        for sensor in sensors {
            sensor
                .clone()
                .validated()
                .map_err(|e| StoreError::Sensor(e.to_string()))?;
        }
        let key = sql_text(&id.to_string());
        let doc = sql_text(&serde_json::to_string(sensors).expect("sensors are serialisable"));
        self.run(&format!("DELETE FROM {SENSOR_TABLE} WHERE id = {key}"))?;
        self.run(&format!(
            "INSERT INTO {SENSOR_TABLE} (id, doc) VALUES ({key}, {doc})"
        ))
    }

    /// The sensors of a twin, empty if none are stored.
    pub fn load_sensors(&self, id: TwinId) -> Result<Vec<Sensor>, StoreError> {
        let rows = self.query(&format!(
            "SELECT doc FROM {SENSOR_TABLE} WHERE id = {}",
            sql_text(&id.to_string())
        ))?;
        match rows.first() {
            None => Ok(Vec::new()),
            Some(json) => {
                let sensors: Vec<Sensor> =
                    serde_json::from_str(json).map_err(|e| StoreError::Sensor(e.to_string()))?;
                for sensor in &sensors {
                    sensor
                        .clone()
                        .validated()
                        .map_err(|e| StoreError::Sensor(e.to_string()))?;
                }
                Ok(sensors)
            }
        }
    }

    /// Store the recording of one sensor of `twin`. A recording of the same
    /// sensor replaces the previous one. The recording is validated first.
    pub fn save_recording(
        &self,
        twin: TwinId,
        recording: &SensorRecording,
    ) -> Result<(), StoreError> {
        let recording = recording.clone().validated()?;
        let key = sql_text(&reading_key(twin, &recording.sensor));
        let twin_id = sql_text(&twin.to_string());
        let doc = sql_text(&serde_json::to_string(&recording).expect("recording is serialisable"));
        self.run(&format!("DELETE FROM {READING_TABLE} WHERE id = {key}"))?;
        self.run(&format!(
            "INSERT INTO {READING_TABLE} (id, twin_id, doc) VALUES ({key}, {twin_id}, {doc})"
        ))
    }

    /// Delete the recording of `sensor` for `twin`, if there is one.
    pub fn remove_recording(&self, twin: TwinId, sensor: &str) -> Result<(), StoreError> {
        let key = sql_text(&reading_key(twin, sensor));
        self.run(&format!("DELETE FROM {READING_TABLE} WHERE id = {key}"))
    }

    /// Every stored recording of `twin`, sorted by sensor name.
    pub fn list_recordings(&self, twin: TwinId) -> Result<Vec<SensorRecording>, StoreError> {
        let rows = self.query(&format!(
            "SELECT doc FROM {READING_TABLE} WHERE twin_id = {}",
            sql_text(&twin.to_string())
        ))?;
        let mut recordings = rows
            .iter()
            .map(|json| {
                serde_json::from_str::<SensorRecording>(json)
                    .map_err(|e| StoreError::Recording(format!("stored recording is invalid: {e}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        recordings.sort_by(|a, b| a.sensor.cmp(&b.sensor));
        Ok(recordings)
    }

    /// The recording of `sensor` for `twin`, or `None` if there is none.
    pub fn load_recording(
        &self,
        twin: TwinId,
        sensor: &str,
    ) -> Result<Option<SensorRecording>, StoreError> {
        let rows = self.query(&format!(
            "SELECT doc FROM {READING_TABLE} WHERE id = {}",
            sql_text(&reading_key(twin, sensor))
        ))?;
        match rows.first() {
            None => Ok(None),
            Some(json) => serde_json::from_str(json)
                .map(Some)
                .map_err(|e| StoreError::Recording(format!("stored recording is invalid: {e}"))),
        }
    }

    /// Store a run of `twin`. A run with the same scenario name replaces the
    /// previous one.
    pub fn save_run(&self, twin: TwinId, run: &StoredRun) -> Result<(), StoreError> {
        let key = sql_text(&run_key(twin, &run.scenario));
        let twin_id = sql_text(&twin.to_string());
        let doc = sql_text(&serde_json::to_string(run).expect("run is serialisable"));
        self.run(&format!("DELETE FROM {RUN_TABLE} WHERE id = {key}"))?;
        self.run(&format!(
            "INSERT INTO {RUN_TABLE} (id, twin_id, doc) VALUES ({key}, {twin_id}, {doc})"
        ))
    }

    /// Load the run of `scenario` for `twin`, or `None` if there is none.
    pub fn load_run(&self, twin: TwinId, scenario: &str) -> Result<Option<StoredRun>, StoreError> {
        let rows = self.query(&format!(
            "SELECT doc FROM {RUN_TABLE} WHERE id = {}",
            sql_text(&run_key(twin, scenario))
        ))?;
        match rows.first() {
            None => Ok(None),
            Some(json) => serde_json::from_str(json)
                .map(Some)
                .map_err(|e| StoreError::Database(format!("stored run is invalid: {e}"))),
        }
    }

    /// Simulate scenario `scenario` of `twin` at `times` and store the run.
    ///
    /// If `design` is given, the twin must be linked to it, so the run is never
    /// recorded against geometry the twin does not match.
    pub fn run_scenario(
        &self,
        twin: &Twin,
        scenario: &str,
        times: &[f64],
        design: Option<&Design>,
    ) -> Result<StoredRun, StoreError> {
        if design.is_some_and(|design| !design.is_linked(twin)) {
            return Err(StoreError::StaleGeometry);
        }
        let sc = twin
            .scenarios
            .iter()
            .find(|sc| sc.name == scenario)
            .ok_or_else(|| StoreError::NoScenario(scenario.to_string()))?;
        let trace = twin_sim::simulate(twin, sc, times)?;
        let run = StoredRun {
            scenario: scenario.to_string(),
            geometry_ref: twin.geometry_ref.clone(),
            parameters: twin.parameters.clone(),
            times: trace.times,
            states: trace.states,
        };
        self.save_run(twin.id, &run)?;
        self.record_imu_readings(twin, sc)?;
        Ok(run)
    }

    /// Store the noise-free IMU readings of `twin` for `scenario`, one
    /// recording per IMU sensor, replacing the previous recording. Sensors of
    /// other kinds are skipped. The readings are the recording of the latest
    /// run, not of each scenario.
    fn record_imu_readings(
        &self,
        twin: &Twin,
        scenario: &twin_core::Scenario,
    ) -> Result<(), StoreError> {
        let imus: Vec<_> = self
            .load_sensors(twin.id)?
            .into_iter()
            .filter_map(|s| match s.kind {
                twin_sensors::SensorKind::Imu { sample_rate_hz } => Some((s.name, sample_rate_hz)),
                _ => None,
            })
            .collect();
        if imus.is_empty() {
            return Ok(());
        }
        let body = twin_sim::MassSpringDamper::from_twin(twin)?;
        for (sensor, rate_hz) in imus {
            let times = twin_sensors::sample_times(rate_hz, scenario.duration_s)
                .map_err(|e| StoreError::Sensor(e.to_string()))?;
            let trace = twin_sim::simulate(twin, scenario, &times)?;
            let readings = twin_sensors::imu_from_trace(&body, &trace)
                .map_err(|e| StoreError::Sensor(e.to_string()))?;
            self.save_recording(
                twin.id,
                &SensorRecording {
                    sensor,
                    channels: vec!["accel_m_s2".into()],
                    times_s: readings.iter().map(|r| r.time_s).collect(),
                    samples: readings.iter().map(|r| vec![r.accel_m_s2]).collect(),
                },
            )?;
        }
        Ok(())
    }

    /// Export a twin with its design and stored runs as a signed `.twin`
    /// package. Encrypted if `passphrase` is given.
    pub fn export_twin(
        &self,
        id: TwinId,
        created: &str,
        signer: &SigningKey,
        passphrase: Option<&str>,
    ) -> Result<Vec<u8>, StoreError> {
        let twin = self.load(id)?.ok_or(StoreError::NotFound(id))?;
        let mut extras = Vec::new();
        if let Some(design) = self.load_design(id)? {
            extras.push((DESIGN_ENTRY.to_string(), design.to_json()?.into_bytes()));
        }
        let sensors = self.load_sensors(id)?;
        if !sensors.is_empty() {
            let json = serde_json::to_vec(&sensors).expect("sensors are serialisable");
            extras.push((SENSOR_ENTRY.to_string(), json));
        }
        for (index, scenario) in self.list_runs(id)?.iter().enumerate() {
            let run = self
                .load_run(id, scenario)?
                .ok_or(StoreError::NotFound(id))?;
            let json = serde_json::to_vec(&run).expect("run is serialisable");
            extras.push((run_entry(index), json));
        }
        for (index, recording) in self.list_recordings(id)?.iter().enumerate() {
            let json = serde_json::to_vec(recording).expect("recording is serialisable");
            extras.push((recording_entry(index), json));
        }
        let bytes = twin_package::export(&twin, &extras, created, signer)?;
        Ok(match passphrase {
            Some(p) => twin_package::encrypt(&bytes, p)?,
            None => bytes,
        })
    }

    /// Verify a `.twin` package and store its twin, design and runs. Replaces
    /// any existing twin with the same ID. Nothing is stored if verification
    /// fails.
    pub fn import_twin(
        &self,
        bytes: &[u8],
        verifier: &VerifyingKey,
        passphrase: Option<&str>,
    ) -> Result<TwinId, StoreError> {
        let plain = twin_package::decrypt(bytes, passphrase)?;
        let Package { twin, extras } = twin_package::import(&plain, verifier)?;
        let design = match extras.iter().find(|(p, _)| p == DESIGN_ENTRY) {
            Some((_, json)) => Some(Design::from_json(
                std::str::from_utf8(json)
                    .map_err(|_| StoreError::Database("design entry is not UTF-8".into()))?,
            )?),
            None => None,
        };
        let mut runs = Vec::new();
        let mut recordings = Vec::new();
        for (path, json) in &extras {
            if path.starts_with("runs/") {
                let run: StoredRun = serde_json::from_slice(json)
                    .map_err(|e| StoreError::Database(format!("run entry is invalid: {e}")))?;
                runs.push(run);
            } else if path.starts_with("recordings/") {
                let recording: SensorRecording = serde_json::from_slice(json).map_err(|e| {
                    StoreError::Recording(format!("recording entry is invalid: {e}"))
                })?;
                recordings.push(recording.validated()?);
            }
        }
        let sensors: Vec<Sensor> = match extras.iter().find(|(p, _)| p == SENSOR_ENTRY) {
            Some((_, json)) => {
                serde_json::from_slice(json).map_err(|e| StoreError::Sensor(e.to_string()))?
            }
            None => Vec::new(),
        };
        self.save(&twin)?;
        self.save_sensors(twin.id, &sensors)?;
        if let Some(design) = &design {
            self.save_design(twin.id, design)?;
        }
        for run in &runs {
            self.save_run(twin.id, run)?;
        }
        for recording in &recordings {
            self.save_recording(twin.id, recording)?;
        }
        Ok(twin.id)
    }

    /// Scenario names of the stored runs of `twin`, sorted.
    pub fn list_runs(&self, twin: TwinId) -> Result<Vec<String>, StoreError> {
        let rows = self.query(&format!(
            "SELECT doc FROM {RUN_TABLE} WHERE twin_id = {}",
            sql_text(&twin.to_string())
        ))?;
        let mut names = rows
            .iter()
            .map(|json| {
                serde_json::from_str::<StoredRun>(json)
                    .map(|r| r.scenario)
                    .map_err(|e| StoreError::Database(format!("stored run is invalid: {e}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        names.sort();
        Ok(names)
    }

    /// Insert or replace a twin's design (model spec plus parameter history,
    /// with geometry hashes in the commits).
    pub fn save_design(&self, id: TwinId, design: &Design) -> Result<(), StoreError> {
        let id = sql_text(&id.to_string());
        let doc = sql_text(&design.to_json()?);
        self.run(&format!("DELETE FROM {DESIGN_TABLE} WHERE id = {id}"))?;
        self.run(&format!(
            "INSERT INTO {DESIGN_TABLE} (id, doc) VALUES ({id}, {doc})"
        ))
    }

    /// Load a twin's design, or `None` if none is stored. A design that fails
    /// its integrity checks is an error.
    pub fn load_design(&self, id: TwinId) -> Result<Option<Design>, StoreError> {
        let rows = self.query(&format!(
            "SELECT doc FROM {DESIGN_TABLE} WHERE id = {}",
            sql_text(&id.to_string())
        ))?;
        match rows.first() {
            None => Ok(None),
            Some(json) => Ok(Some(Design::from_json(json)?)),
        }
    }

    /// Insert or replace the parameter history of a twin.
    pub fn save_history(&self, id: TwinId, history: &History) -> Result<(), StoreError> {
        let id = sql_text(&id.to_string());
        let doc = sql_text(&history.to_json()?);
        self.run(&format!("DELETE FROM {HISTORY_TABLE} WHERE id = {id}"))?;
        self.run(&format!(
            "INSERT INTO {HISTORY_TABLE} (id, doc) VALUES ({id}, {doc})"
        ))
    }

    /// Load a twin's history, or `None` if none is stored. A history that
    /// fails its integrity checks is an error.
    pub fn load_history(&self, id: TwinId) -> Result<Option<History>, StoreError> {
        let rows = self.query(&format!(
            "SELECT doc FROM {HISTORY_TABLE} WHERE id = {}",
            sql_text(&id.to_string())
        ))?;
        match rows.first() {
            None => Ok(None),
            Some(json) => Ok(Some(History::from_json(json)?)),
        }
    }

    fn run(&self, sql: &str) -> Result<(), StoreError> {
        execute_query(sql, self.db.clone()).map_err(db_err)?;
        Ok(())
    }

    /// Run a query and return the first column of each row as text.
    fn query(&self, sql: &str) -> Result<Vec<String>, StoreError> {
        Ok(self.query_rows(sql)?.into_iter().map(|row| row.0).collect())
    }

    /// Run a query and return the first two columns of each row as text.
    fn query_rows(&self, sql: &str) -> Result<Vec<(String, String)>, StoreError> {
        let result = execute_query(sql, self.db.clone()).map_err(db_err)?;
        let text = |cell: &Option<Vec<u8>>| {
            cell.as_deref()
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default()
        };
        Ok(result
            .rows
            .iter()
            .map(|row| {
                (
                    row.first().map(text).unwrap_or_default(),
                    row.get(1).map(text).unwrap_or_default(),
                )
            })
            .collect())
    }
}

fn run_key(twin: TwinId, scenario: &str) -> String {
    format!("{twin}/{scenario}")
}

fn reading_key(twin: TwinId, sensor: &str) -> String {
    format!("{twin}/{sensor}")
}

/// Quote a value as a SQL string literal, doubling embedded single quotes.
fn sql_text(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("twin-store-test-{tag}-{}", TwinId::new()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = temp_dir("roundtrip");
        let store = TwinStore::open(&dir).unwrap();
        let mut twin = Twin::new("Robot arm").unwrap();
        twin.set_parameter("mass_kg", 2.5).unwrap();
        store.save(&twin).unwrap();

        assert_eq!(store.load(twin.id).unwrap(), Some(twin));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn save_replaces_existing_twin() {
        let dir = temp_dir("replace");
        let store = TwinStore::open(&dir).unwrap();
        let mut twin = Twin::new("v1").unwrap();
        store.save(&twin).unwrap();
        twin.name = "v2".into();
        store.save(&twin).unwrap();

        let listed = store.list().unwrap();
        assert_eq!(listed, vec![(twin.id, "v2".to_string())]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_twin_loads_as_none() {
        let dir = temp_dir("missing");
        let store = TwinStore::open(&dir).unwrap();
        assert_eq!(store.load(TwinId::new()).unwrap(), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn delete_removes_twin() {
        let dir = temp_dir("delete");
        let store = TwinStore::open(&dir).unwrap();
        let twin = Twin::new("gone").unwrap();
        store.save(&twin).unwrap();
        store.delete(twin.id).unwrap();
        assert_eq!(store.load(twin.id).unwrap(), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn quotes_in_names_are_stored_safely() {
        let dir = temp_dir("quotes");
        let store = TwinStore::open(&dir).unwrap();
        let twin = Twin::new("Bob's \"arm\"; DROP TABLE twins").unwrap();
        store.save(&twin).unwrap();
        assert_eq!(store.load(twin.id).unwrap(), Some(twin));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn history_round_trips_and_is_removed_with_the_twin() {
        let dir = temp_dir("history");
        let store = TwinStore::open(&dir).unwrap();
        let twin = Twin::new("with history").unwrap();
        store.save(&twin).unwrap();
        assert_eq!(store.load_history(twin.id).unwrap(), None);

        let mut h = History::new([("width".to_string(), 1.0)].into(), "it's the start");
        h.set("width", 2.0);
        h.commit_changes("wider").unwrap();
        h.create_branch("exp").unwrap();
        store.save_history(twin.id, &h).unwrap();
        assert_eq!(store.load_history(twin.id).unwrap(), Some(h.clone()));

        h.set("width", 3.0);
        store.save_history(twin.id, &h).unwrap();
        assert_eq!(store.load_history(twin.id).unwrap(), Some(h));

        store.delete(twin.id).unwrap();
        assert_eq!(store.load_history(twin.id).unwrap(), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn history_survives_reopening_the_store() {
        let dir = temp_dir("history-reopen");
        let id = TwinId::new();
        let h = History::new([("a".to_string(), 1.0)].into(), "init");
        {
            let store = TwinStore::open(&dir).unwrap();
            store.save_history(id, &h).unwrap();
        }
        let store = TwinStore::open(&dir).unwrap();
        assert_eq!(store.load_history(id).unwrap(), Some(h));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn design_round_trips_and_is_removed_with_the_twin() {
        use twin_geometry::model::{ModelSpec, Step, Value};
        let dir = temp_dir("design");
        let store = TwinStore::open(&dir).unwrap();
        let twin = Twin::new("with design").unwrap();
        store.save(&twin).unwrap();
        assert_eq!(store.load_design(twin.id).unwrap(), None);

        let spec = ModelSpec {
            steps: vec![Step::Block {
                width: Value::Param("w".into()),
                depth: Value::Const(1.0),
                height: Value::Const(1.0),
            }],
        };
        let mut design = Design::new(spec, [("w".to_string(), 2.0)].into(), "it's new").unwrap();
        design.set("w", 3.0);
        design.commit("wider").unwrap();
        store.save_design(twin.id, &design).unwrap();
        assert_eq!(store.load_design(twin.id).unwrap(), Some(design));

        store.delete(twin.id).unwrap();
        assert_eq!(store.load_design(twin.id).unwrap(), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn linked_twin_keeps_its_geometry_ref_after_reload() {
        use twin_geometry::design::Design;
        use twin_geometry::model::{ModelSpec, Step, Value};
        let dir = temp_dir("linked");
        let id;
        let head;
        {
            let store = TwinStore::open(&dir).unwrap();
            let spec = ModelSpec {
                steps: vec![Step::Block {
                    width: Value::Param("w".into()),
                    depth: Value::Const(1.0),
                    height: Value::Const(1.0),
                }],
            };
            let mut design = Design::new(spec, [("w".to_string(), 2.0)].into(), "start").unwrap();
            design.set("w", 3.0);
            design.commit("wider").unwrap();
            let mut twin = Twin::new("linked").unwrap();
            design.link_twin(&mut twin).unwrap();
            id = twin.id;
            head = twin.geometry_ref.clone();
            store.save(&twin).unwrap();
            store.save_design(id, &design).unwrap();
        }
        let store = TwinStore::open(&dir).unwrap();
        let twin = store.load(id).unwrap().unwrap();
        assert_eq!(twin.geometry_ref, head);
        let design = store.load_design(id).unwrap().unwrap();
        assert!(design.is_linked(&twin));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn runs_are_stored_replaced_listed_and_removed_with_the_twin() {
        let dir = temp_dir("runs");
        let store = TwinStore::open(&dir).unwrap();
        let twin = Twin::new("runner").unwrap();
        store.save(&twin).unwrap();
        assert_eq!(store.load_run(twin.id, "drop").unwrap(), None);

        let mut run = StoredRun {
            scenario: "drop".into(),
            geometry_ref: Some("abc".into()),
            parameters: BTreeMap::new(),
            times: vec![0.0, 1.0],
            states: vec![[1.0, 0.0], [0.5, -0.2]],
        };
        store.save_run(twin.id, &run).unwrap();
        store
            .save_run(
                twin.id,
                &StoredRun {
                    scenario: "bump".into(),
                    geometry_ref: None,
                    parameters: BTreeMap::new(),
                    times: vec![0.0],
                    states: vec![[0.0, 1.0]],
                },
            )
            .unwrap();
        assert_eq!(store.load_run(twin.id, "drop").unwrap(), Some(run.clone()));
        assert_eq!(store.list_runs(twin.id).unwrap(), vec!["bump", "drop"]);

        run.states[1] = [0.4, -0.1];
        store.save_run(twin.id, &run).unwrap();
        assert_eq!(store.load_run(twin.id, "drop").unwrap(), Some(run));
        assert_eq!(store.list_runs(twin.id).unwrap().len(), 2);

        store.delete(twin.id).unwrap();
        assert!(store.list_runs(twin.id).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn scenario_runs_are_recorded_and_go_stale_with_geometry() {
        use twin_geometry::design::Design;
        use twin_geometry::model::{ModelSpec, Step, Value};
        let dir = temp_dir("scenario");
        let store = TwinStore::open(&dir).unwrap();

        let mut twin = Twin::new("spring").unwrap();
        twin.set_parameter(twin_sim::MASS, 1.0).unwrap();
        twin.set_parameter(twin_sim::STIFFNESS, 4.0).unwrap();
        twin.set_parameter(twin_sim::DAMPING, 0.0).unwrap();
        twin.add_scenario(twin_core::Scenario {
            name: "release".into(),
            duration_s: 2.0,
            initial_conditions: [
                (twin_sim::POSITION0.to_string(), 1.0),
                (twin_sim::VELOCITY0.to_string(), 0.0),
            ]
            .into(),
        })
        .unwrap();
        let spec = ModelSpec {
            steps: vec![Step::Block {
                width: Value::Param("w".into()),
                depth: Value::Const(1.0),
                height: Value::Const(1.0),
            }],
        };
        let mut design = Design::new(spec, [("w".to_string(), 2.0)].into(), "start").unwrap();
        design.link_twin(&mut twin).unwrap();
        store.save(&twin).unwrap();

        let times = [0.0, 1.0, 2.0];
        let run = store
            .run_scenario(&twin, "release", &times, Some(&design))
            .unwrap();
        assert_eq!(run.geometry_ref, twin.geometry_ref);
        assert!(run_is_current(&twin, &run));
        assert_eq!(
            store.load_run(twin.id, "release").unwrap(),
            Some(run.clone())
        );

        // A new commit moves the geometry on; the old run is now stale, and
        // the twin must be relinked before it can be simulated again.
        design.set("w", 3.0);
        design.commit("wider").unwrap();
        assert!(!design.is_linked(&twin));
        assert!(matches!(
            store.run_scenario(&twin, "release", &times, Some(&design)),
            Err(StoreError::StaleGeometry)
        ));
        design.link_twin(&mut twin).unwrap();
        assert!(
            !run_is_current(&twin, &run),
            "the old run was made at the old commit"
        );
        let fresh = store
            .run_scenario(&twin, "release", &times, Some(&design))
            .unwrap();
        assert!(run_is_current(&twin, &fresh));
        assert_eq!(store.load_run(twin.id, "release").unwrap(), Some(fresh));
        assert!(matches!(
            store.run_scenario(&twin, "missing", &times, None),
            Err(StoreError::NoScenario(_))
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn exported_twin_imports_with_design_and_runs() {
        use twin_geometry::design::Design;
        use twin_geometry::model::{ModelSpec, Step, Value};
        let src_dir = temp_dir("export-src");
        let dst_dir = temp_dir("export-dst");
        let key = SigningKey::from_bytes(&[5u8; 32]);
        let src = TwinStore::open(&src_dir).unwrap();

        let mut twin = Twin::new("portable").unwrap();
        twin.set_parameter(twin_sim::MASS, 1.0).unwrap();
        twin.set_parameter(twin_sim::STIFFNESS, 4.0).unwrap();
        twin.set_parameter(twin_sim::DAMPING, 0.0).unwrap();
        twin.add_scenario(twin_core::Scenario {
            name: "a/b: release".into(),
            duration_s: 1.0,
            initial_conditions: [
                (twin_sim::POSITION0.to_string(), 1.0),
                (twin_sim::VELOCITY0.to_string(), 0.0),
            ]
            .into(),
        })
        .unwrap();
        let spec = ModelSpec {
            steps: vec![Step::Block {
                width: Value::Param("w".into()),
                depth: Value::Const(1.0),
                height: Value::Const(1.0),
            }],
        };
        let design = Design::new(spec, [("w".to_string(), 2.0)].into(), "start").unwrap();
        design.link_twin(&mut twin).unwrap();
        src.save(&twin).unwrap();
        src.save_design(twin.id, &design).unwrap();
        let run = src
            .run_scenario(&twin, "a/b: release", &[0.0, 1.0], Some(&design))
            .unwrap();

        let verifier = key.verifying_key();
        let plain = src
            .export_twin(twin.id, "2026-10-10T00:00:00Z", &key, None)
            .unwrap();
        let dst = TwinStore::open(&dst_dir).unwrap();
        let id = dst.import_twin(&plain, &verifier, None).unwrap();
        assert_eq!(id, twin.id);
        let loaded = dst.load(id).unwrap().unwrap();
        assert_eq!(loaded, twin);
        assert_eq!(dst.load_design(id).unwrap(), Some(design.clone()));
        assert!(design.is_linked(&loaded));
        assert_eq!(dst.load_run(id, "a/b: release").unwrap(), Some(run));

        // Sensor recordings travel with the twin too.
        let reading = recording("imu", vec![0.0, 0.5], vec![vec![0.1], vec![0.2]]);
        src.save_recording(twin.id, &reading).unwrap();
        let with_reading = src
            .export_twin(twin.id, "2026-10-10T00:00:00Z", &key, None)
            .unwrap();
        let reader = TwinStore::open(&temp_dir("export-reading")).unwrap();
        reader.import_twin(&with_reading, &verifier, None).unwrap();
        assert_eq!(
            reader.load_recording(twin.id, "imu").unwrap(),
            Some(reading)
        );

        let sealed = src
            .export_twin(twin.id, "2026-10-10T00:00:00Z", &key, Some("pass"))
            .unwrap();
        assert!(twin_package::is_encrypted(&sealed));
        assert!(dst.import_twin(&sealed, &verifier, None).is_err());
        let other = TwinStore::open(&temp_dir("export-other")).unwrap();
        assert_eq!(
            other.import_twin(&sealed, &verifier, Some("pass")).unwrap(),
            twin.id
        );

        let wrong = SigningKey::from_bytes(&[6u8; 32]).verifying_key();
        let rejected = temp_dir("export-rejected");
        let rejected_store = TwinStore::open(&rejected).unwrap();
        assert!(rejected_store.import_twin(&plain, &wrong, None).is_err());
        assert_eq!(rejected_store.load(twin.id).unwrap(), None);
        for d in [src_dir, dst_dir, rejected] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn imu_readings_are_recorded_with_each_scenario_run() {
        use twin_sensors::{Mount, SensorKind};
        let dir = temp_dir("imu-run");
        let store = TwinStore::open(&dir).unwrap();
        let mut twin = Twin::new("imu run").unwrap();
        twin.set_parameter(twin_sim::MASS, 1.0).unwrap();
        twin.set_parameter(twin_sim::STIFFNESS, 4.0).unwrap();
        twin.set_parameter(twin_sim::DAMPING, 0.2).unwrap();
        twin.add_scenario(twin_core::Scenario {
            name: "release".into(),
            duration_s: 1.0,
            initial_conditions: [
                (twin_sim::POSITION0.to_string(), 0.1),
                (twin_sim::VELOCITY0.to_string(), 0.0),
            ]
            .into(),
        })
        .unwrap();
        store.save(&twin).unwrap();
        store
            .save_sensors(
                twin.id,
                &[
                    twin_sensors::Sensor {
                        name: "imu".into(),
                        mount: Mount::default(),
                        kind: SensorKind::Imu {
                            sample_rate_hz: 10.0,
                        },
                    },
                    twin_sensors::Sensor {
                        name: "cam".into(),
                        mount: Mount::default(),
                        kind: SensorKind::Camera {
                            fov_deg: 60.0,
                            width_px: 640,
                            height_px: 480,
                        },
                    },
                ],
            )
            .unwrap();
        assert_eq!(store.load_recording(twin.id, "imu").unwrap(), None);

        store
            .run_scenario(&twin, "release", &[0.0, 1.0], None)
            .unwrap();
        let rec = store.load_recording(twin.id, "imu").unwrap().unwrap();
        assert_eq!(rec.channels, vec!["accel_m_s2"]);
        assert_eq!(rec.times_s.len(), 11, "10 Hz over 1 s, including t = 0");
        // At rest-start the acceleration is -k x0 / m = -0.4 m/s^2.
        assert!(
            (rec.samples[0][0] + 0.4).abs() < 1e-9,
            "{:?}",
            rec.samples[0]
        );
        // The camera is not an IMU, so it gets no recording.
        assert_eq!(store.load_recording(twin.id, "cam").unwrap(), None);

        // A second run replaces the recording rather than adding one.
        store
            .run_scenario(&twin, "release", &[0.0, 1.0], None)
            .unwrap();
        assert_eq!(
            store.list_recordings(twin.id).unwrap().len(),
            1,
            "one recording per IMU sensor"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sensors_are_stored_validated_and_exported() {
        use twin_sensors::{Mount, SensorKind};
        let dir = temp_dir("sensors");
        let store = TwinStore::open(&dir).unwrap();
        let twin = Twin::new("sensed").unwrap();
        store.save(&twin).unwrap();
        assert!(store.load_sensors(twin.id).unwrap().is_empty());

        let imu = Sensor {
            name: "imu".into(),
            mount: Mount::default(),
            kind: SensorKind::Imu {
                sample_rate_hz: 50.0,
            },
        };
        store
            .save_sensors(twin.id, std::slice::from_ref(&imu))
            .unwrap();
        assert_eq!(store.load_sensors(twin.id).unwrap(), vec![imu.clone()]);

        let bad = Sensor {
            name: "bad".into(),
            mount: Mount::default(),
            kind: SensorKind::Imu {
                sample_rate_hz: -1.0,
            },
        };
        assert!(matches!(
            store.save_sensors(twin.id, &[bad]),
            Err(StoreError::Sensor(_))
        ));
        assert_eq!(store.load_sensors(twin.id).unwrap(), vec![imu.clone()]);

        // Sensors travel with the twin in a package.
        let key = SigningKey::from_bytes(&[9u8; 32]);
        let bytes = store
            .export_twin(twin.id, "2026-10-10T00:00:00Z", &key, None)
            .unwrap();
        let other = TwinStore::open(&temp_dir("sensors-other")).unwrap();
        let id = other
            .import_twin(&bytes, &key.verifying_key(), None)
            .unwrap();
        assert_eq!(other.load_sensors(id).unwrap(), vec![imu]);

        store.delete(twin.id).unwrap();
        assert!(store.load_sensors(twin.id).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn recording(sensor: &str, times: Vec<f64>, samples: Vec<Vec<f64>>) -> SensorRecording {
        SensorRecording {
            sensor: sensor.into(),
            channels: vec!["accel_m_s2".into()],
            times_s: times,
            samples,
        }
    }

    #[test]
    fn sensor_recordings_are_stored_replaced_validated_and_removed_with_the_twin() {
        let dir = temp_dir("readings");
        let store = TwinStore::open(&dir).unwrap();
        let twin = Twin::new("recorded").unwrap();
        store.save(&twin).unwrap();
        assert_eq!(store.load_recording(twin.id, "imu").unwrap(), None);

        let first = recording("imu", vec![0.0, 0.1], vec![vec![1.0], vec![2.0]]);
        store.save_recording(twin.id, &first).unwrap();
        assert_eq!(
            store.load_recording(twin.id, "imu").unwrap(),
            Some(first.clone())
        );

        // Saving the same sensor again replaces the earlier recording.
        let second = recording("imu", vec![0.0, 0.2, 0.4], vec![vec![0.5]; 3]);
        store.save_recording(twin.id, &second).unwrap();
        assert_eq!(store.load_recording(twin.id, "imu").unwrap(), Some(second));

        // Invalid shapes and values are refused and leave the stored copy alone.
        let bad = [
            recording("imu", vec![0.0, 0.0], vec![vec![1.0], vec![1.0]]),
            recording("imu", vec![0.0, 1.0], vec![vec![1.0]]),
            recording("imu", vec![0.0], vec![vec![f64::NAN]]),
            recording("imu", vec![0.0], vec![vec![1.0, 2.0]]),
            recording("", vec![0.0], vec![vec![1.0]]),
            SensorRecording {
                sensor: "imu".into(),
                channels: vec!["a".into(), "a".into()],
                times_s: vec![0.0],
                samples: vec![vec![1.0, 2.0]],
            },
        ];
        for r in &bad {
            assert!(matches!(
                store.save_recording(twin.id, r),
                Err(StoreError::Recording(_))
            ));
        }
        assert_eq!(
            store
                .load_recording(twin.id, "imu")
                .unwrap()
                .unwrap()
                .times_s
                .len(),
            3
        );

        // Recordings of other sensors are kept separate, and go with the twin.
        store
            .save_recording(twin.id, &recording("lidar", vec![0.0], vec![vec![3.0]]))
            .unwrap();
        assert!(store.load_recording(twin.id, "lidar").unwrap().is_some());
        store.delete(twin.id).unwrap();
        assert_eq!(store.load_recording(twin.id, "imu").unwrap(), None);
        assert_eq!(store.load_recording(twin.id, "lidar").unwrap(), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
