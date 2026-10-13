//! Twin desktop application: a window over a `Project`.
//!
//! Usage: `twin-app [project-dir]`. The directory defaults to `twin-project`
//! and is created if missing. The window keeps no state of its own. Each
//! button sends a message, the message changes the `Project`, and the next
//! frame is built from it.

mod theme;

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;

use tpt_appfront_canvas::CanvasApp;
use tpt_appfront_core::UITree;
use tpt_appfront_core::ui_tree::ContainerBuilder;
use twin_app::{
    CollabField, NewSensor, PlotChoice, PlotData, Project, ScenarioField, SensorField,
    load_or_create_export_key, new_session_code, sensor_fields, user_config_dir,
};
use twin_core::TwinId;
use twin_geometry::{Change, CommitId};
use twin_sensors::SensorKind;

#[derive(Debug, Clone)]
enum Msg {
    NewTwin,
    Open(TwinId),
    Save,
    /// Text typed into the field of the named parameter.
    Param(String, String),
    StartHistory,
    CommitMessage(String),
    Commit,
    BranchName(String),
    CreateBranch,
    Switch(String),
    Merge(String),
    Compare(String),
    CompareFrom(CommitId),
    CompareTo(CommitId),
    /// Text typed into a field of the named scenario.
    Scenario(String, ScenarioField, String),
    AddExample,
    Run(String),
    Plot(String),
    PackagePath(String),
    PackagePassphrase(String),
    /// Text typed into a session field.
    CollabText(CollabField, String),
    NewCode,
    HostSession,
    JoinSession,
    StopSession,
    SetShared,
    ApplyShared,
    /// Render what a camera sees of the design.
    RenderCamera(String),
    /// Save the in-range points of a lidar's scan of the design.
    SaveLidarScan(String),
    /// Draw a lidar's saved scan from above.
    PlotScan(String),
    /// Open a save dialog for the export path.
    BrowseExport,
    /// Open a file dialog for the import path.
    BrowseImport,
    ExportTwin,
    ImportTwin,
    StartDesign,
    /// Text typed into a geometry parameter.
    DesignParam(String, String),
    CommitDesign,
    DesignBranchName(String),
    CreateDesignBranch,
    SwitchDesign(String),
    MergeDesign(String),
    LinkDesign,
    AddSensor(NewSensor),
    PlotSensor(String),
    /// Text typed into a setting of the named sensor.
    SensorSetting(String, SensorField, String),
    RemoveSensor(String),
}

/// The sections of the window, chosen from the navigation panel. Each shows
/// the cards that belong to it, plus the toolbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Project,
    Model,
    Geometry,
    Sensors,
    Share,
    Settings,
}

impl Section {
    const ALL: [Section; 6] = [
        Section::Project,
        Section::Model,
        Section::Geometry,
        Section::Sensors,
        Section::Share,
        Section::Settings,
    ];

    fn label(self) -> &'static str {
        match self {
            Section::Project => "Project",
            Section::Model => "Model",
            Section::Geometry => "Geometry",
            Section::Sensors => "Sensors",
            Section::Share => "Share",
            Section::Settings => "Settings",
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "twin-project".to_string());
    let project = Rc::new(RefCell::new(Project::open(Path::new(&dir))?));

    let section = Rc::new(Cell::new(Section::Project));
    let view = project.clone();
    let shown = section.clone();
    let build_ui = move || -> UITree<Msg> {
        let p = view.borrow();
        let current = shown.get();
        UITree::container(|c| {
            if let Some(banner) = p.licence_banner() {
                c.text(banner).class("text-sm text-gray-700");
            }
            card(c, |c| {
                c.text(p.status().to_string())
                    .class("text-sm text-gray-700");
                c.button("New twin").on_click(Msg::NewTwin);
                c.button("Save").on_click(Msg::Save);
            });

            if current == Section::Settings {
                card(c, |c| {
                    c.heading(2, "Data location");
                    c.text(p.dir().display().to_string())
                        .class("text-sm text-gray-700");
                    c.heading(2, "Network");
                    c.text("No telemetry, no accounts, no phone-home. Nothing leaves this machine unless you host or join a session.")
                        .class("text-sm text-gray-700");
                    c.text("Sessions bind to this machine (loopback) by default. A LAN address is used only when you type one in the Share section.")
                        .class("text-sm text-gray-700");
                });
            }

            if current != Section::Project
                && current != Section::Settings
                && p.current().is_none()
            {
                card(c, |c| {
                    c.text("Create or open a twin in the Project section first.")
                        .class("text-sm text-gray-700");
                });
            }
            if current == Section::Project {
                card(c, |c| {
                    c.heading(2, "Twins");
                    for (id, name) in p.twins() {
                        c.button(name.clone()).on_click(Msg::Open(*id));
                    }
                });
            }

            if let Some(twin) = p.current() {
                if current == Section::Model {
                    card(c, |c| {
                        c.heading(3, "Parameters");
                        for name in twin.parameters.keys() {
                            let text = p.parameter_text(name).unwrap_or_default();
                            let key = name.clone();
                            c.text(name.clone()).class("text-sm text-gray-700");
                            c.input(text)
                                .on_input(move |typed| Msg::Param(key.clone(), typed));
                        }
                    });
                }

                if current == Section::Model {
                    card(c, |c| {
                        c.heading(3, "History");
                        match p.history() {
                            None => {
                                c.button("Start history").on_click(Msg::StartHistory);
                            }
                            Some(history) => {
                                c.text(format!(
                                    "On branch {} at {}",
                                    history.head_branch(),
                                    history.head_commit()
                                ))
                                .class("text-sm text-gray-700");
                                c.text("Commit message").class("text-sm text-gray-700");
                                c.input(p.commit_message().to_string())
                                    .on_input(Msg::CommitMessage);
                                c.button("Commit").on_click(Msg::Commit);

                                for (name, _) in history.branches() {
                                    if name == history.head_branch() {
                                        c.text(format!("{name} (current)"));
                                    } else {
                                        c.button(format!("Switch to {name}"))
                                            .on_click(Msg::Switch(name.to_string()));
                                        c.button(format!(
                                            "Merge {name} into {}",
                                            history.head_branch()
                                        ))
                                        .on_click(Msg::Merge(name.to_string()));
                                        c.button(format!("Compare {name} with head"))
                                            .on_click(Msg::Compare(name.to_string()));
                                    }
                                }
                                c.text("New branch name").class("text-sm text-gray-700");
                                c.input(p.branch_name().to_string())
                                    .on_input(Msg::BranchName);
                                c.button("Create branch").on_click(Msg::CreateBranch);

                                c.heading(3, "Log");
                                c.text("Pick an older and a newer commit to compare (the newer defaults to the head)")
                                .class("text-sm text-gray-700");
                                for commit in history.log() {
                                    c.text(format!("{} {}", short(&commit.id), commit.message))
                                        .class("text-sm text-gray-700");
                                    c.button("Compare from here")
                                        .on_click(Msg::CompareFrom(commit.id.clone()));
                                    c.button("Compare to here")
                                        .on_click(Msg::CompareTo(commit.id.clone()));
                                }

                                if let Ok(Some(cmp)) = p.comparison() {
                                    c.heading(
                                        3,
                                        format!(
                                            "Changes from {} to {}",
                                            short(&cmp.from),
                                            short(&cmp.to)
                                        ),
                                    );
                                    for change in cmp.changes {
                                        c.text(format_change(&change))
                                            .class("text-sm text-gray-700");
                                    }
                                }

                                c.heading(3, "Changes in head commit");
                                for change in p.head_diff().unwrap_or_default() {
                                    c.text(format_change(&change))
                                        .class("text-sm text-gray-700");
                                }
                            }
                        }
                    });
                }

                if current == Section::Model {
                    card(c, |c| {
                        c.heading(3, "Simulation");
                        c.button("Add example model").on_click(Msg::AddExample);
                        for scenario in &twin.scenarios {
                            let name = scenario.name.clone();
                            c.text(name.clone()).class("text-sm text-gray-700");
                            let mut fields = vec![ScenarioField::Duration];
                            fields.extend(
                                scenario
                                    .initial_conditions
                                    .keys()
                                    .map(|k| ScenarioField::Initial(k.clone())),
                            );
                            for field in fields {
                                let label = match &field {
                                    ScenarioField::Duration => "duration_s".to_string(),
                                    ScenarioField::Initial(k) => k.clone(),
                                };
                                let text = p.scenario_text(&name, &field).unwrap_or_default();
                                let scenario_name = name.clone();
                                c.text(label).class("text-sm text-gray-700");
                                c.input(text).on_input(move |typed| {
                                    Msg::Scenario(scenario_name.clone(), field.clone(), typed)
                                });
                            }
                            c.button(format!("Plot {name}"))
                                .on_click(Msg::Plot(name.clone()));
                            c.button(format!("Run {name}"))
                                .on_click(Msg::Run(name.clone()));
                            let summary = p
                                .run_summary(&name)
                                .ok()
                                .flatten()
                                .unwrap_or_else(|| "not run yet".to_string());
                            c.text(format!("{name}: {summary}"))
                                .class("text-sm text-gray-700");
                        }
                    });
                }

                if current == Section::Share {
                    card(c, |c| {
                        c.heading(3, "Package");
                        c.text("File path (.twin)").class("text-sm text-gray-700");
                        c.input(p.package_path().to_string())
                            .on_input(Msg::PackagePath);
                        c.text("Passphrase (blank for none; shown as typed)")
                            .class("text-sm text-gray-700");
                        c.input(p.package_passphrase().to_string())
                            .on_input(Msg::PackagePassphrase);
                        c.button("Choose export file...")
                            .on_click(Msg::BrowseExport);
                        c.button("Choose file to import...")
                            .on_click(Msg::BrowseImport);
                        c.button("Export twin").on_click(Msg::ExportTwin);
                        c.button("Import twin").on_click(Msg::ImportTwin);
                    });
                }

                if current == Section::Share {
                    card(c, |c| {
                        c.heading(3, "Shared session");
                        c.text("Address: 127.0.0.1:7000 for this machine, or the host's LAN address (LAN needs the host to type it, and is plain TLS with a pinned certificate)")
                        .class("text-sm text-gray-700");
                        let address = p.collab_text(CollabField::Address).to_string();
                        c.input(address)
                            .on_input(|typed| Msg::CollabText(CollabField::Address, typed));
                        c.text("Session code (the joining side types the host's code)")
                            .class("text-sm text-gray-700");
                        let code = p.collab_text(CollabField::Code).to_string();
                        c.input(code)
                            .on_input(|typed| Msg::CollabText(CollabField::Code, typed));
                        c.text("View-only code (peers with this code can watch, not change)")
                            .class("text-sm text-gray-700");
                        let view_code = p.collab_text(CollabField::ViewerCode).to_string();
                        c.input(view_code)
                            .on_input(|typed| Msg::CollabText(CollabField::ViewerCode, typed));
                        c.text(format!(
                            "Peers connected to this host: {}",
                            p.session_peers()
                        ))
                        .class("text-sm text-gray-700");
                        let names = p.session_peer_names();
                        if !names.is_empty() {
                            c.text(format!("Names: {}", names.join(", ")))
                                .class("text-sm text-gray-700");
                        }
                        c.text("Your name (shown to the host when you join)")
                            .class("text-sm text-gray-700");
                        let peer_name = p.collab_text(CollabField::PeerName).to_string();
                        c.input(peer_name)
                            .on_input(|typed| Msg::CollabText(CollabField::PeerName, typed));
                        c.button("New session code").on_click(Msg::NewCode);
                        c.button("Host session").on_click(Msg::HostSession);
                        c.button("Join session").on_click(Msg::JoinSession);
                        c.button("Stop session").on_click(Msg::StopSession);
                        if let Some(shared) = p.shared_parameters() {
                            c.text("Shared parameters:").class("text-sm text-gray-700");
                            for (name, value) in shared {
                                c.text(format!("{name} = {value}"))
                                    .class("text-sm text-gray-700");
                            }
                            c.text("Parameter name").class("text-sm text-gray-700");
                            let name = p.collab_text(CollabField::Name).to_string();
                            c.input(name)
                                .on_input(|typed| Msg::CollabText(CollabField::Name, typed));
                            c.text("Value").class("text-sm text-gray-700");
                            let value = p.collab_text(CollabField::Value).to_string();
                            c.input(value)
                                .on_input(|typed| Msg::CollabText(CollabField::Value, typed));
                            c.button("Set shared parameter").on_click(Msg::SetShared);
                            c.button("Apply shared parameters to twin")
                                .on_click(Msg::ApplyShared);
                        }
                    });
                }

                if current == Section::Geometry {
                    card(c, |c| {
                        c.heading(3, "Geometry");
                        match p.design() {
                            None => {
                                c.button("Start geometry").on_click(Msg::StartDesign);
                            }
                            Some(design) => {
                                if let Some(status) = p.geometry_status() {
                                    c.text(status).class("text-sm text-gray-700");
                                }
                                c.heading(3, "Feature tree");
                                for line in p.feature_tree() {
                                    c.text(line).class("text-sm text-gray-700");
                                }
                                for name in design.history().working().keys() {
                                    let owner = name.clone();
                                    c.text(name.clone()).class("text-sm text-gray-700");
                                    c.input(p.design_text(name).unwrap_or_default()).on_input(
                                        move |typed| Msg::DesignParam(owner.clone(), typed),
                                    );
                                }
                                c.heading(3, "Geometry branches");
                                let head = design.history().head_branch().to_string();
                                for (name, _) in design.history().branches() {
                                    if name == head {
                                        c.text(format!("{name} (current)"));
                                        continue;
                                    }
                                    c.button(format!("Switch geometry to {name}"))
                                        .on_click(Msg::SwitchDesign(name.to_string()));
                                    c.button(format!("Merge geometry {name} into {head}"))
                                        .on_click(Msg::MergeDesign(name.to_string()));
                                    for change in p.design_changes(name).unwrap_or_default() {
                                        c.text(format!("  {}", format_change(&change)))
                                            .class("text-sm text-gray-700");
                                    }
                                }
                                c.text("New geometry branch name")
                                    .class("text-sm text-gray-700");
                                c.input(p.design_branch_name().to_string())
                                    .on_input(Msg::DesignBranchName);
                                c.button("Create geometry branch")
                                    .on_click(Msg::CreateDesignBranch);

                                c.button("Commit geometry").on_click(Msg::CommitDesign);
                                c.button("Link twin to geometry").on_click(Msg::LinkDesign);
                            }
                        }
                    });
                }

                if current == Section::Sensors {
                    card(c, |c| {
                        c.heading(3, "Sensors");
                        c.button("Add IMU").on_click(Msg::AddSensor(NewSensor::Imu));
                        c.button("Add camera")
                            .on_click(Msg::AddSensor(NewSensor::Camera));
                        c.button("Add lidar")
                            .on_click(Msg::AddSensor(NewSensor::Lidar));
                        for sensor in p.sensors().unwrap_or_default() {
                            let name = sensor.name.clone();
                            let kind = match sensor.kind {
                                SensorKind::Imu { sample_rate_hz } => {
                                    format!("IMU at {sample_rate_hz} Hz")
                                }
                                SensorKind::Camera { .. } => "camera".to_string(),
                                SensorKind::Lidar { .. } => "lidar".to_string(),
                            };
                            let summary =
                                match sensor.kind {
                                    SensorKind::Lidar { .. } => p
                                        .lidar_summary(&name)
                                        .ok()
                                        .flatten()
                                        .unwrap_or_else(|| "start geometry to scan".to_string()),
                                    SensorKind::Camera { .. } => {
                                        "render to see what it sees of the design".to_string()
                                    }
                                    _ => p.recording_summary(&name).ok().flatten().unwrap_or_else(
                                        || "no recording yet (run a scenario)".to_string(),
                                    ),
                                };
                            c.text(if kind == name {
                                format!("{name}: {summary}")
                            } else {
                                format!("{name} ({kind}): {summary}")
                            })
                            .class("text-sm text-gray-700");
                            for (label, field) in sensor_fields(&sensor.kind) {
                                let owner = name.clone();
                                let text = p.sensor_text(&name, field).unwrap_or_default();
                                c.text(label).class("text-sm text-gray-700");
                                c.input(text).on_input(move |typed| {
                                    Msg::SensorSetting(owner.clone(), field, typed)
                                });
                            }
                            if matches!(sensor.kind, SensorKind::Lidar { .. }) {
                                c.button(format!("Save scan {name}"))
                                    .on_click(Msg::SaveLidarScan(name.clone()));
                                c.button(format!("Plot scan {name}"))
                                    .on_click(Msg::PlotScan(name.clone()));
                            }
                            if matches!(sensor.kind, SensorKind::Camera { .. }) {
                                c.button(format!("Render {name}"))
                                    .on_click(Msg::RenderCamera(name.clone()));
                            }
                            if matches!(sensor.kind, SensorKind::Imu { .. }) {
                                c.button(format!("Plot {name}"))
                                    .on_click(Msg::PlotSensor(name.clone()));
                            }
                            c.button(format!("Remove {name}"))
                                .on_click(Msg::RemoveSensor(name));
                        }
                    });
                }
            }
        })
    };

    let update = project.clone();
    let dispatch = move |msg: Msg| {
        let mut p = update.borrow_mut();
        let result = match msg {
            Msg::NewTwin => p.new_twin("New twin").map(|_| ()),
            Msg::Open(id) => p.open_twin(id),
            Msg::Save => p.save(),
            Msg::Param(name, text) => p.edit_parameter(&name, &text),
            Msg::StartHistory => p.start_history(),
            Msg::CommitMessage(text) => {
                p.set_commit_message(text);
                Ok(())
            }
            Msg::Commit => p.commit().map(|_| ()),
            Msg::BranchName(text) => {
                p.set_branch_name(text);
                Ok(())
            }
            Msg::CreateBranch => p.create_branch(),
            Msg::Switch(name) => p.switch_branch(&name),
            Msg::Merge(name) => p.merge_branch(&name),
            Msg::Compare(name) => p.set_comparison(&name),
            Msg::CompareFrom(id) => p.compare_from_commit(id),
            Msg::CompareTo(id) => p.compare_to_commit(id),
            Msg::Scenario(name, field, text) => p.edit_scenario(&name, &field, &text),
            Msg::AddExample => p.add_example_model(),
            Msg::Run(name) => p.run_scenario(&name),
            Msg::BrowseExport => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Twin package", &["twin"])
                    .set_file_name("project.twin")
                    .save_file()
                {
                    p.set_package_path(path.display().to_string());
                }
                Ok(())
            }
            Msg::BrowseImport => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Twin package", &["twin"])
                    .pick_file()
                {
                    p.set_package_path(path.display().to_string());
                }
                Ok(())
            }
            Msg::SaveLidarScan(name) => match p.save_lidar_scan(&name) {
                Ok(summary) => {
                    p.set_status(summary.unwrap_or_else(|| "no geometry to scan yet".to_string()));
                    Ok(())
                }
                Err(e) => Err(e),
            },
            Msg::RenderCamera(name) => match p.render_camera(&name) {
                Ok(summary) => {
                    p.set_status(
                        summary.unwrap_or_else(|| "no geometry to render yet".to_string()),
                    );
                    Ok(())
                }
                Err(e) => Err(e),
            },
            Msg::PackagePassphrase(text) => {
                p.set_package_passphrase(text);
                Ok(())
            }
            Msg::PackagePath(text) => {
                p.set_package_path(text);
                Ok(())
            }
            Msg::ExportTwin => {
                let path = p.package_path().to_string();
                let passphrase = p.passphrase().map(str::to_string);
                user_config_dir()
                    .ok_or_else(|| twin_app::ProjectError::Key("no user settings folder".into()))
                    .and_then(|dir| load_or_create_export_key(&dir))
                    .and_then(|key| p.export_twin(Path::new(&path), &key, passphrase.as_deref()))
            }
            Msg::ImportTwin => {
                let path = p.package_path().to_string();
                let passphrase = p.passphrase().map(str::to_string);
                p.import_twin(Path::new(&path), passphrase.as_deref())
                    .map(|_| ())
            }
            Msg::CollabText(field, text) => {
                p.set_collab_text(field, text);
                Ok(())
            }
            Msg::NewCode => match (new_session_code(), new_session_code()) {
                (Ok(edit), Ok(view)) => {
                    p.set_collab_text(CollabField::Code, edit);
                    p.set_collab_text(CollabField::ViewerCode, view);
                    Ok(())
                }
                (Err(e), _) | (_, Err(e)) => Err(e),
            },
            Msg::HostSession => {
                let address = p.collab_text(CollabField::Address).to_string();
                let code = p.collab_text(CollabField::Code).to_string();
                let view_code = p.collab_text(CollabField::ViewerCode).to_string();
                // Any address other than loopback is a LAN address: typing it is the opt-in.
                let allow_lan = address
                    .parse::<std::net::SocketAddr>()
                    .is_ok_and(|a| !a.ip().is_loopback());
                match user_config_dir() {
                    Some(dir) => p.host_session(&address, allow_lan, &dir, &code, &view_code),
                    None => Err(twin_app::ProjectError::Session(
                        "no user settings folder".into(),
                    )),
                }
            }
            Msg::JoinSession => {
                let address = p.collab_text(CollabField::Address).to_string();
                let code = p.collab_text(CollabField::Code).to_string();
                p.join_session(&address, &code)
            }
            Msg::StopSession => {
                p.stop_session();
                Ok(())
            }
            Msg::SetShared => p.set_shared_parameter(),
            Msg::ApplyShared => p.apply_shared_parameters(),
            Msg::StartDesign => p.start_design(),
            Msg::DesignParam(name, text) => p.edit_design_parameter(&name, &text),
            Msg::CommitDesign => p.commit_design().map(|_| ()),
            Msg::DesignBranchName(text) => {
                p.set_design_branch_name(text);
                Ok(())
            }
            Msg::CreateDesignBranch => p.create_design_branch(),
            Msg::SwitchDesign(name) => p.switch_design_branch(&name),
            Msg::MergeDesign(name) => p.merge_design_branch(&name),
            Msg::LinkDesign => p.link_design(),
            Msg::AddSensor(kind) => p.add_sensor(kind),
            Msg::SensorSetting(name, field, text) => p.edit_sensor(&name, field, &text),
            Msg::PlotSensor(name) => {
                p.set_plot(PlotChoice::Sensor(name));
                Ok(())
            }
            Msg::RemoveSensor(name) => p.remove_sensor(&name),
            Msg::Plot(name) => {
                p.set_plot(PlotChoice::Run(name));
                Ok(())
            }
            Msg::PlotScan(name) => {
                p.set_plot(PlotChoice::Lidar(name));
                Ok(())
            }
        };
        if let Err(e) = result {
            p.set_status(format!("Error: {e}"));
        }
    };

    // Launch eframe directly (not through `run_native`) so the theme can be
    // applied to the egui context when the window is created.
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 720.0])
            .with_min_inner_size([640.0, 480.0]),
        ..Default::default()
    };
    let app = Themed {
        inner: CanvasApp::new(build_ui, dispatch),
        project: project.clone(),
        section,
    };
    eframe::run_native(
        "Twin",
        options,
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )?;
    Ok(())
}

/// Paints the page background, then the canvas content on top of it. The
/// plot panel is painted directly with egui, because the canvas tree has no
/// element for custom drawing.
struct Themed {
    inner: CanvasApp<Msg>,
    project: Rc<RefCell<Project>>,
    section: Rc<Cell<Section>>,
}

impl eframe::App for Themed {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        theme::paint_page(ui);
        self.header(ui);
        self.navigation(ui);
        {
            // Apply what a joined session has sent, and keep the window refreshing
            // while a session is open, since changes arrive without input.
            let mut p = self.project.borrow_mut();
            if let Err(e) = p.collab_poll() {
                p.set_status(e.to_string());
            }
            if p.collab_active() {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        let (plot, scan) = {
            let p = self.project.borrow();
            (
                p.plot_data().ok().flatten(),
                p.lidar_scan_xy().ok().flatten(),
            )
        };
        if let Some((name, points)) = scan {
            egui::Panel::right("plot")
                .resizable(true)
                .default_size(380.0)
                .show(ui, |ui| scan_card(ui, &name, &points));
        } else if let Some(plot) = plot {
            egui::Panel::right("plot")
                .resizable(true)
                .default_size(380.0)
                .show(ui, |ui| plot_card(ui, &plot));
        }
        // The canvas has no scrolling of its own and lays out to the height it
        // is given, so it gets a fixed-height region inside a scroll area.
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let size = egui::vec2(ui.available_width(), CONTENT_HEIGHT);
                ui.allocate_ui(size, |ui| self.inner.ui(ui, frame));
            });
    }
}

/// A lidar scan seen from above: each point at its x and y in the sensor frame,
/// with the sensor at the centre. The axes are drawn through the sensor, and
/// one metre is the same length on both axes.
fn scan_card(ui: &mut egui::Ui, name: &str, points: &[[f64; 2]]) {
    use egui::{Align2, Color32, FontId, Stroke};

    ui.heading(format!("Lidar scan: {name}"));
    ui.label("Seen from above: x to the right, y up, metres from the sensor");
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 300.0),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    let grid = Color32::from_gray(190);
    painter.rect_filled(rect, 4.0, Color32::WHITE);
    painter.rect_stroke(rect, 4.0, Stroke::new(1.0, grid), egui::StrokeKind::Inside);

    // The largest extent from the sensor on either axis sets the scale, so the
    // sensor stays in the middle and the scan fits.
    let reach = points
        .iter()
        .flat_map(|p| [p[0].abs(), p[1].abs()])
        .fold(0.0_f64, f64::max)
        .max(1e-6);
    let scale = (rect.width().min(rect.height()) as f64 / 2.0 - 8.0) / reach;
    let centre = rect.center();
    let to_screen = |p: [f64; 2]| {
        egui::pos2(
            centre.x + (p[0] * scale) as f32,
            centre.y - (p[1] * scale) as f32,
        )
    };
    painter.line_segment(
        [
            egui::pos2(rect.left(), centre.y),
            egui::pos2(rect.right(), centre.y),
        ],
        Stroke::new(1.0, grid),
    );
    painter.line_segment(
        [
            egui::pos2(centre.x, rect.top()),
            egui::pos2(centre.x, rect.bottom()),
        ],
        Stroke::new(1.0, grid),
    );
    let dot = Color32::from_rgb(31, 119, 180);
    for p in points {
        painter.circle_filled(to_screen(*p), 1.5, dot);
    }
    painter.text(
        rect.right_bottom() + egui::vec2(-6.0, -4.0),
        Align2::RIGHT_BOTTOM,
        format!("{} points, reach {reach:.2} m", points.len()),
        FontId::proportional(12.0),
        Color32::from_gray(90),
    );
}

impl Themed {
    /// The bar across the top: the product name, the project folder, and the
    /// open twin.
    fn header(&self, ui: &mut egui::Ui) {
        let (folder, twin) = {
            let p = self.project.borrow();
            let folder = p
                .dir()
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            (folder, p.current().map(|t| t.name.clone()))
        };
        egui::Panel::top("header")
            .frame(theme::chrome_frame())
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Twin")
                            .size(18.0)
                            .strong()
                            .color(theme::ACCENT),
                    );
                    ui.label(egui::RichText::new(folder).weak());
                    if let Some(name) = twin {
                        ui.separator();
                        ui.label(egui::RichText::new(name).strong());
                    }
                });
            });
    }

    /// The left-hand navigation: one entry per section.
    fn navigation(&self, ui: &mut egui::Ui) {
        egui::Panel::left("navigation")
            .resizable(false)
            .exact_size(150.0)
            .frame(theme::chrome_frame())
            .show(ui, |ui| {
                ui.add_space(4.0);
                for section in Section::ALL {
                    let selected = self.section.get() == section;
                    if ui
                        .add_sized(
                            [ui.available_width(), 30.0],
                            egui::Button::selectable(selected, section.label()),
                        )
                        .clicked()
                    {
                        self.section.set(section);
                    }
                }
            });
    }
}

/// Draws each series of `plot` against its time axis, one colour per series,
/// with a legend, the value range and the end time.
fn plot_card(ui: &mut egui::Ui, plot: &PlotData) {
    use egui::{Align2, Color32, FontId, Stroke};
    // Colours for the series, in order; they repeat past the end of the list.
    const COLOURS: [Color32; 3] = [
        Color32::from_rgb(31, 119, 180),
        Color32::from_rgb(255, 127, 14),
        Color32::from_rgb(44, 160, 44),
    ];

    ui.heading(&plot.title);
    ui.horizontal_wrapped(|ui| {
        for (i, (name, _)) in plot.series.iter().enumerate() {
            ui.colored_label(COLOURS[i % COLOURS.len()], name);
        }
    });
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 260.0),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    let grid = Color32::from_gray(190);
    painter.rect_filled(rect, 4.0, Color32::WHITE);
    painter.rect_stroke(rect, 4.0, Stroke::new(1.0, grid), egui::StrokeKind::Inside);
    if plot.times.len() < 2 {
        return;
    }

    // Fit the value axis to the data. It always includes zero, so the rest
    // line is visible, and a flat plot still gets a non-empty range.
    let t_max = plot.times.last().copied().unwrap_or(0.0).max(1e-12);
    let values = plot.series.iter().flat_map(|(_, v)| v.iter().copied());
    let (lo, mut hi) = values.fold((0.0_f64, 0.0_f64), |(lo, hi), v| (lo.min(v), hi.max(v)));
    if hi - lo < 1e-12 {
        hi = lo + 1.0;
    }
    let to_pos = |t: f64, y: f64| {
        egui::pos2(
            rect.left() + (t / t_max) as f32 * rect.width(),
            rect.bottom() - ((y - lo) / (hi - lo)) as f32 * rect.height(),
        )
    };
    let zero = to_pos(0.0, 0.0).y;
    painter.line_segment(
        [
            egui::pos2(rect.left(), zero),
            egui::pos2(rect.right(), zero),
        ],
        Stroke::new(1.0, grid),
    );
    for (i, (_, values)) in plot.series.iter().enumerate() {
        let points: Vec<egui::Pos2> = plot
            .times
            .iter()
            .zip(values)
            .map(|(&t, &y)| to_pos(t, y))
            .collect();
        painter.line(points, Stroke::new(2.0, COLOURS[i % COLOURS.len()]));
    }
    painter.text(
        rect.right_bottom() + egui::vec2(-6.0, -4.0),
        Align2::RIGHT_BOTTOM,
        format!("{t_max:.2} s, {lo:.3} to {hi:.3}"),
        FontId::proportional(12.0),
        Color32::from_gray(90),
    );
}

/// Height of the canvas region inside the scroll area. Content shorter than
/// this leaves blank space at the bottom, which is harmless.
const CONTENT_HEIGHT: f32 = 2400.0;

/// The first 12 characters of a commit ID, enough to tell commits apart here.
fn short(id: &CommitId) -> &str {
    &id.as_str()[..12.min(id.as_str().len())]
}

/// One parameter change as a line of text, such as `mass_kg: 1 -> 2`.
fn format_change(change: &Change) -> String {
    let show = |v: Option<f64>| v.map_or("-".to_string(), |v| v.to_string());
    format!(
        "{}: {} -> {}",
        change.key,
        show(change.before),
        show(change.after)
    )
}

/// A white card with padding, grouping one section of the window.
fn card(c: &mut ContainerBuilder<Msg>, build: impl FnOnce(&mut ContainerBuilder<Msg>)) {
    c.container(build).class("bg-white p-4");
}
