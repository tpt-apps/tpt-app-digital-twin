//! Twin document model: identity, parameters and scenarios shared by all crates.
//!
//! This crate depends on no foundation crate. The document format therefore
//! stays stable whatever the simulation or geometry engines do. Geometry,
//! sensors and results are referenced by opaque ID, and their data lives in the
//! crates that own them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// The schema version this build reads and writes.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error, PartialEq)]
pub enum TwinError {
    #[error("twin name must not be empty")]
    EmptyName,
    #[error("invalid twin id: {0}")]
    InvalidId(String),
    #[error("{field} must be finite, got {value}")]
    NonFinite { field: String, value: f64 },
    #[error("scenario `{name}` duration must be positive, got {duration_s}")]
    InvalidDuration { name: String, duration_s: f64 },
    #[error("no scenario named `{0}`")]
    NoScenario(String),
    #[error("unsupported schema version {found} (this build supports up to {supported})")]
    UnsupportedSchema { found: u32, supported: u32 },
    #[error("invalid twin document: {0}")]
    Json(String),
}

/// Stable identity of a twin, kept across edits, saves and package moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TwinId(Uuid);

impl TwinId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn parse(s: &str) -> Result<Self, TwinError> {
        Uuid::parse_str(s)
            .map(Self)
            .map_err(|_| TwinError::InvalidId(s.to_owned()))
    }
}

impl Default for TwinId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TwinId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A simulation run: initial conditions and how long to run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scenario {
    pub name: String,
    pub duration_s: f64,
    pub initial_conditions: BTreeMap<String, f64>,
}

/// The Twin document. Edit it, then save it with [`Twin::to_json`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Twin {
    pub schema_version: u32,
    pub id: TwinId,
    pub name: String,
    /// Physics and design parameters, by name.
    pub parameters: BTreeMap<String, f64>,
    pub scenarios: Vec<Scenario>,
    /// Opaque ID of the geometry snapshot in `twin-geometry`, if any.
    pub geometry_ref: Option<String>,
    /// Opaque IDs of sensor models in `twin-sensors`.
    pub sensor_refs: Vec<String>,
}

impl Twin {
    pub fn new(name: impl Into<String>) -> Result<Self, TwinError> {
        let twin = Self {
            schema_version: SCHEMA_VERSION,
            id: TwinId::new(),
            name: name.into(),
            parameters: BTreeMap::new(),
            scenarios: Vec::new(),
            geometry_ref: None,
            sensor_refs: Vec::new(),
        };
        twin.validate()?;
        Ok(twin)
    }

    /// Set a parameter. Rejects NaN and infinities, which JSON cannot store.
    pub fn set_parameter(&mut self, name: impl Into<String>, value: f64) -> Result<(), TwinError> {
        let name = name.into();
        check_finite(&name, value)?;
        self.parameters.insert(name, value);
        Ok(())
    }

    pub fn add_scenario(&mut self, scenario: Scenario) -> Result<(), TwinError> {
        validate_scenario(&scenario)?;
        self.scenarios.push(scenario);
        Ok(())
    }

    /// Set the duration of scenario `scenario`. Refused, with the twin unchanged,
    /// if the scenario does not exist or the duration is not positive.
    pub fn set_scenario_duration(
        &mut self,
        scenario: &str,
        duration_s: f64,
    ) -> Result<(), TwinError> {
        self.edit_scenario(scenario, |sc| sc.duration_s = duration_s)
    }

    /// Set initial condition `key` of scenario `scenario`. Refused, with the
    /// twin unchanged, if the scenario does not exist or the value is not finite.
    pub fn set_scenario_initial(
        &mut self,
        scenario: &str,
        key: impl Into<String>,
        value: f64,
    ) -> Result<(), TwinError> {
        let key = key.into();
        self.edit_scenario(scenario, |sc| {
            sc.initial_conditions.insert(key, value);
        })
    }

    /// Apply `edit` to a copy of scenario `name`, then store the copy if it is
    /// valid.
    fn edit_scenario(
        &mut self,
        name: &str,
        edit: impl FnOnce(&mut Scenario),
    ) -> Result<(), TwinError> {
        let index = self
            .scenarios
            .iter()
            .position(|sc| sc.name == name)
            .ok_or_else(|| TwinError::NoScenario(name.to_owned()))?;
        let mut scenario = self.scenarios[index].clone();
        edit(&mut scenario);
        validate_scenario(&scenario)?;
        self.scenarios[index] = scenario;
        Ok(())
    }

    /// Check every invariant. Called on construction, on load and before save.
    pub fn validate(&self) -> Result<(), TwinError> {
        if self.name.trim().is_empty() {
            return Err(TwinError::EmptyName);
        }
        if self.schema_version > SCHEMA_VERSION {
            return Err(TwinError::UnsupportedSchema {
                found: self.schema_version,
                supported: SCHEMA_VERSION,
            });
        }
        for (name, value) in &self.parameters {
            check_finite(name, *value)?;
        }
        for scenario in &self.scenarios {
            validate_scenario(scenario)?;
        }
        Ok(())
    }

    pub fn to_json(&self) -> Result<String, TwinError> {
        self.validate()?;
        serde_json::to_string_pretty(self).map_err(|e| TwinError::Json(e.to_string()))
    }

    /// Load a document. Older schemas go through [`migrate`]. Newer ones are
    /// refused rather than read with missing or ignored fields.
    pub fn from_json(json: &str) -> Result<Self, TwinError> {
        let value: serde_json::Value =
            serde_json::from_str(json).map_err(|e| TwinError::Json(e.to_string()))?;
        let version = value
            .get("schema_version")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| TwinError::Json("missing schema_version".into()))?;
        let value = migrate(value, version as u32)?;
        let twin: Twin =
            serde_json::from_value(value).map_err(|e| TwinError::Json(e.to_string()))?;
        twin.validate()?;
        Ok(twin)
    }
}

/// Upgrade a document from `from_version` to [`SCHEMA_VERSION`].
///
/// Version 1 is the first schema, so there is nothing to upgrade yet. Future
/// versions add one arm per step, and each arm is a pure JSON transform.
fn migrate(value: serde_json::Value, from_version: u32) -> Result<serde_json::Value, TwinError> {
    match from_version {
        SCHEMA_VERSION => Ok(value),
        found => Err(TwinError::UnsupportedSchema {
            found,
            supported: SCHEMA_VERSION,
        }),
    }
}

fn check_finite(field: &str, value: f64) -> Result<(), TwinError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(TwinError::NonFinite {
            field: field.to_owned(),
            value,
        })
    }
}

fn validate_scenario(scenario: &Scenario) -> Result<(), TwinError> {
    if !(scenario.duration_s.is_finite() && scenario.duration_s > 0.0) {
        return Err(TwinError::InvalidDuration {
            name: scenario.name.clone(),
            duration_s: scenario.duration_s,
        });
    }
    for (name, value) in &scenario.initial_conditions {
        check_finite(name, *value)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenario_setters_change_valid_values_and_refuse_the_rest() {
        let mut twin = sample();
        twin.add_scenario(Scenario {
            name: "drop".into(),
            duration_s: 1.0,
            initial_conditions: [("position_m".to_string(), 0.1)].into(),
        })
        .unwrap();

        twin.set_scenario_duration("drop", 5.0).unwrap();
        twin.set_scenario_initial("drop", "position_m", 0.3)
            .unwrap();
        twin.set_scenario_initial("drop", "velocity_m_s", -0.2)
            .unwrap();
        let sc = twin.scenarios.iter().find(|sc| sc.name == "drop").unwrap();
        assert_eq!(sc.duration_s, 5.0);
        assert_eq!(sc.initial_conditions["position_m"], 0.3);
        assert_eq!(sc.initial_conditions["velocity_m_s"], -0.2);

        // Refused values and unknown scenarios leave the twin as it was.
        let before = twin.clone();
        assert!(matches!(
            twin.set_scenario_duration("drop", 0.0),
            Err(TwinError::InvalidDuration { .. })
        ));
        assert!(matches!(
            twin.set_scenario_initial("drop", "position_m", f64::NAN),
            Err(TwinError::NonFinite { .. })
        ));
        assert!(matches!(
            twin.set_scenario_duration("missing", 1.0),
            Err(TwinError::NoScenario(_))
        ));
        assert_eq!(twin, before);
    }

    fn sample() -> Twin {
        let mut twin = Twin::new("Robot arm").unwrap();
        twin.set_parameter("mass_kg", 2.5).unwrap();
        twin.add_scenario(Scenario {
            name: "drop test".into(),
            duration_s: 3.0,
            initial_conditions: BTreeMap::from([("height_m".to_string(), 1.2)]),
        })
        .unwrap();
        twin.geometry_ref = Some("geom-1".into());
        twin
    }

    #[test]
    fn round_trip_preserves_document() {
        let twin = sample();
        let json = twin.to_json().unwrap();
        assert_eq!(Twin::from_json(&json).unwrap(), twin);
    }

    #[test]
    fn new_twin_gets_a_unique_id() {
        assert_ne!(Twin::new("a").unwrap().id, Twin::new("b").unwrap().id);
    }

    #[test]
    fn rejects_empty_or_blank_name() {
        assert_eq!(Twin::new("").unwrap_err(), TwinError::EmptyName);
        assert_eq!(Twin::new("   ").unwrap_err(), TwinError::EmptyName);
    }

    #[test]
    fn rejects_non_finite_parameters() {
        let mut twin = sample();
        assert!(matches!(
            twin.set_parameter("stiffness", f64::NAN),
            Err(TwinError::NonFinite { .. })
        ));
        assert!(matches!(
            twin.set_parameter("stiffness", f64::INFINITY),
            Err(TwinError::NonFinite { .. })
        ));
        assert!(!twin.parameters.contains_key("stiffness"));
    }

    #[test]
    fn rejects_non_positive_duration() {
        let mut twin = sample();
        let bad = Scenario {
            name: "instant".into(),
            duration_s: 0.0,
            initial_conditions: BTreeMap::new(),
        };
        assert!(matches!(
            twin.add_scenario(bad),
            Err(TwinError::InvalidDuration { .. })
        ));
    }

    #[test]
    fn refuses_newer_schema() {
        let mut json: serde_json::Value =
            serde_json::from_str(&sample().to_json().unwrap()).unwrap();
        json["schema_version"] = serde_json::json!(SCHEMA_VERSION + 1);
        let err = Twin::from_json(&json.to_string()).unwrap_err();
        assert_eq!(
            err,
            TwinError::UnsupportedSchema {
                found: SCHEMA_VERSION + 1,
                supported: SCHEMA_VERSION
            }
        );
    }

    #[test]
    fn rejects_missing_schema_version() {
        let err = Twin::from_json(r#"{"name":"x"}"#).unwrap_err();
        assert!(matches!(err, TwinError::Json(_)));
    }

    #[test]
    fn parses_and_rejects_ids() {
        let id = TwinId::new();
        assert_eq!(TwinId::parse(&id.to_string()).unwrap(), id);
        assert!(matches!(
            TwinId::parse("not-a-uuid"),
            Err(TwinError::InvalidId(_))
        ));
    }
}
