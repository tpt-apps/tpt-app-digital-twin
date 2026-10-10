//! The project model behind the desktop app.
//!
//! A project is a directory holding a `TwinStore`. This type owns the store,
//! the list of twins in it, and the one twin being edited. Edits stay in
//! memory until `save`, so a session can be discarded without touching the
//! stored twin. The window in `main.rs` is a view over this type and holds no
//! state of its own, so this part is tested without a window.

use std::collections::BTreeMap;
use std::path::Path;

use thiserror::Error;
use twin_core::{Scenario, Twin, TwinError, TwinId};
use twin_geometry::{Change, CommitId, History, HistoryError};
use twin_store::{StoreError, TwinStore};

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
    #[error(transparent)]
    History(#[from] HistoryError),
    #[error("the twin has no history yet; start one first")]
    NoHistory,
    #[error("save the twin before changing its history")]
    UnsavedTwin,
    #[error("the commit message is empty")]
    EmptyMessage,
    #[error("the branch name is empty")]
    EmptyBranchName,
}

/// Name of the scenario added by `Project::add_example_model`.
pub const EXAMPLE_SCENARIO: &str = "free-vibration";
/// Samples per simulated run, evenly spaced over the scenario duration.
const RUN_SAMPLES: usize = 100;

pub struct Project {
    store: TwinStore,
    twins: Vec<(TwinId, String)>,
    current: Option<Twin>,
    /// Text typed into parameter fields, by name. It is kept as typed (for
    /// example `2.` on the way to `2.5`) so the field does not change under the
    /// user. A draft that is not a number is kept too, and blocks `save`.
    drafts: BTreeMap<String, String>,
    /// The parameter history of the open twin, if it has one.
    history: Option<History>,
    commit_message: String,
    branch_name: String,
    status: String,
}

impl Project {
    /// Open or create the project in `dir`.
    pub fn open(dir: &Path) -> Result<Self, ProjectError> {
        let mut project = Self {
            store: TwinStore::open(dir)?,
            twins: Vec::new(),
            current: None,
            drafts: BTreeMap::new(),
            history: None,
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
        self.history = None;
        self.refresh()?;
        self.status = format!("Created {name}");
        Ok(id)
    }

    /// Open a stored twin, discarding unsaved edits to the current one.
    pub fn open_twin(&mut self, id: TwinId) -> Result<(), ProjectError> {
        let twin = self.store.load(id)?.ok_or(ProjectError::NotFound)?;
        self.status = format!("Opened {}", twin.name);
        self.history = self.store.load_history(id)?;
        self.current = Some(twin);
        self.drafts.clear();
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
        self.set_parameter(name, value)
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
        self.store.run_scenario(twin, name, &times, None)?;
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
        Ok(Some(format!(
            "{} samples to {end:.2} s, final position {:.4} m, peak |position| {peak:.4} m",
            run.times.len(),
            last[0]
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
        self.status = format!("Switched to {name} (not saved)");
        Ok(())
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
        self.store.save(twin)?;
        self.drafts.clear();
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
        let _ = std::fs::remove_dir_all(dir);
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
