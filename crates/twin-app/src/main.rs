//! Twin desktop application: a window over a `Project`.
//!
//! Usage: `twin-app [project-dir]`. The directory defaults to `twin-project`
//! and is created if missing. The window keeps no state of its own. Each
//! button sends a message, the message changes the `Project`, and the next
//! frame is built from it.

mod theme;

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use tpt_appfront_canvas::CanvasApp;
use tpt_appfront_core::ui_tree::ContainerBuilder;
use tpt_appfront_core::UITree;
use twin_app::Project;
use twin_core::TwinId;

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
    AddExample,
    Run(String),
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "twin-project".to_string());
    let project = Rc::new(RefCell::new(Project::open(Path::new(&dir))?));

    let view = project.clone();
    let build_ui = move || -> UITree<Msg> {
        let p = view.borrow();
        UITree::container(|c| {
            c.heading(1, "Twin");
            card(c, |c| {
                c.text(p.status().to_string())
                    .class("text-sm text-gray-700");
                c.button("New twin").on_click(Msg::NewTwin);
                c.button("Save").on_click(Msg::Save);
            });

            card(c, |c| {
                c.heading(2, "Twins");
                for (id, name) in p.twins() {
                    c.button(name.clone()).on_click(Msg::Open(*id));
                }
            });

            if let Some(twin) = p.current() {
                c.heading(2, format!("Open: {}", twin.name));

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
                                }
                            }
                            c.text("New branch name").class("text-sm text-gray-700");
                            c.input(p.branch_name().to_string())
                                .on_input(Msg::BranchName);
                            c.button("Create branch").on_click(Msg::CreateBranch);

                            c.heading(3, "Log");
                            for commit in history.log() {
                                c.text(format!("{} {}", commit.id, commit.message))
                                    .class("text-sm text-gray-700");
                            }

                            c.heading(3, "Changes in head commit");
                            for change in p.head_diff().unwrap_or_default() {
                                let show =
                                    |v: Option<f64>| v.map_or("-".to_string(), |v| v.to_string());
                                c.text(format!(
                                    "{}: {} -> {}",
                                    change.key,
                                    show(change.before),
                                    show(change.after)
                                ))
                                .class("text-sm text-gray-700");
                            }
                        }
                    }
                });

                card(c, |c| {
                    c.heading(3, "Simulation");
                    c.button("Add example model").on_click(Msg::AddExample);
                    for scenario in &twin.scenarios {
                        let name = scenario.name.clone();
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
            Msg::AddExample => p.add_example_model(),
            Msg::Run(name) => p.run_scenario(&name),
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

/// Paints the page background, then the canvas content on top of it.
struct Themed<A> {
    inner: A,
}

impl eframe::App for Themed<CanvasApp<Msg>> {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        theme::paint_page(ui);
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

/// Height of the canvas region inside the scroll area. Content shorter than
/// this leaves blank space at the bottom, which is harmless.
const CONTENT_HEIGHT: f32 = 2400.0;

/// A white card with padding, grouping one section of the window.
fn card(c: &mut ContainerBuilder<Msg>, build: impl FnOnce(&mut ContainerBuilder<Msg>)) {
    c.container(build).class("bg-white p-4");
}
