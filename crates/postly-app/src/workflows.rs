//! Desktop entry points for the same migration and runner used by the CLI.

use crate::PostlyApp;
use eframe::egui::{self, RichText};
use postly_core::{CancellationToken, RunnerOptions, RunnerSummary};
use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::Duration,
};

struct ImportOutcome {
    root: PathBuf,
    requests: usize,
    warnings: Vec<String>,
}

#[derive(Default)]
pub struct Workflows {
    comparison: crate::comparison::Comparison,
    import_open: bool,
    import_source: String,
    import_destination: String,
    openapi: bool,
    importing: Option<Receiver<Result<ImportOutcome, String>>>,
    imported: Option<ImportOutcome>,
    import_error: Option<String>,
    runner_open: bool,
    folder: String,
    scripts: bool,
    fail_fast: bool,
    running: Option<Receiver<Result<RunnerSummary, String>>>,
    cancellation: Option<CancellationToken>,
    summary: Option<RunnerSummary>,
    runner_error: Option<String>,
    run_label: String,
}

impl Drop for Workflows {
    fn drop(&mut self) {
        if let Some(token) = &self.cancellation {
            token.cancel();
        }
    }
}

impl Workflows {
    pub fn toolbar(&mut self, ui: &mut egui::Ui, app: &PostlyApp) {
        egui::Panel::top("project-workflows")
            .frame(crate::design::frame(ui, 10))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new("WORKSPACE")
                            .small()
                            .strong()
                            .color(crate::design::accent(ui)),
                    );
                    ui.label(
                        RichText::new(
                            app.workspace
                                .root()
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy(),
                        )
                        .strong(),
                    );
                    ui.separator();
                    if ui.button("Import collection…").clicked() {
                        self.import_open = true;
                        if self.import_destination.is_empty() {
                            self.import_destination = app
                                .workspace
                                .root()
                                .parent()
                                .unwrap_or_else(|| Path::new("."))
                                .join("imported-api")
                                .display()
                                .to_string();
                        }
                    }
                    if ui.button("Run collection…").clicked() {
                        self.runner_open = true;
                    }
                    self.comparison.button(ui, app);
                    if self.running.is_some() {
                        ui.spinner();
                        ui.label("Running saved requests…");
                    }
                });
            });
    }

    pub fn windows(&mut self, ctx: &egui::Context, app: &mut PostlyApp) -> Option<PathBuf> {
        self.poll(ctx);
        let open = self.import_window(ctx, app);
        self.runner_window(ctx, app);
        self.comparison.show(ctx, app);
        open
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(receiver) = &self.importing {
            match receiver.try_recv() {
                Ok(Ok(result)) => {
                    self.imported = Some(result);
                    self.importing = None;
                }
                Ok(Err(error)) => {
                    self.import_error = Some(error);
                    self.importing = None;
                }
                Err(TryRecvError::Disconnected) => {
                    self.import_error = Some(
                        "The importer stopped unexpectedly. Check the destination before retrying."
                            .into(),
                    );
                    self.importing = None;
                }
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(80));
                }
            }
        }
        if let Some(receiver) = &self.running {
            match receiver.try_recv() {
                Ok(result) => {
                    match result {
                        Ok(summary) => self.summary = Some(summary),
                        Err(error) => self.runner_error = Some(error),
                    }
                    self.running = None;
                    self.cancellation = None;
                }
                Err(TryRecvError::Disconnected) => {
                    self.runner_error = Some("The runner stopped unexpectedly.".into());
                    self.running = None;
                    self.cancellation = None;
                }
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(80));
                }
            }
        }
    }

    fn start_import(&mut self) {
        self.import_error = None;
        self.imported = None;
        let source = PathBuf::from(self.import_source.trim());
        let root = PathBuf::from(self.import_destination.trim());
        if self.import_source.trim().is_empty() || self.import_destination.trim().is_empty() {
            self.import_error = Some("Choose the source file and a new destination folder.".into());
            return;
        }
        let openapi = self.openapi;
        let (sender, receiver) = mpsc::channel();
        self.importing = Some(receiver);
        thread::spawn(move || {
            let _ = sender.send(import_collection(&source, &root, openapi));
        });
    }

    fn import_window(&mut self, ctx: &egui::Context, app: &PostlyApp) -> Option<PathBuf> {
        if !self.import_open {
            return None;
        }
        let mut visible = true;
        let mut start = false;
        let mut open = None;
        egui::Window::new("Import a collection").open(&mut visible).default_width(620.0).show(ctx, |ui| {
            ui.label("Bring a collection into a new local project. Review the migration report before sending requests.");
            ui.add_enabled_ui(self.importing.is_none(), |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.openapi, false, "Postman v2.1");
                    ui.selectable_value(&mut self.openapi, true, "OpenAPI 3.0 / 3.1");
                });
                ui.label("Source file");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.import_source).desired_width(490.0));
                    if ui.button("Browse…").clicked() {
                        if let Some(path) = rfd::FileDialog::new().add_filter("API collection", &["json", "yaml", "yml"]).pick_file() { self.import_source = path.display().to_string(); }
                    }
                });
                ui.label("Destination — a new or empty folder");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.import_destination).desired_width(490.0));
                    if ui.button("Choose…").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() { self.import_destination = path.display().to_string(); }
                    }
                });
                ui.label("Scripts are preserved as source and never run during import. Local OpenAPI references are supported; remote references remain visible warnings.");
                start = ui.button("Import and review").clicked();
            });
            if self.importing.is_some() { ui.spinner(); ui.label("Importing into your chosen folder…"); }
            if let Some(error) = &self.import_error { ui.colored_label(ui.visuals().error_fg_color, error); }
            if let Some(report) = &self.imported {
                ui.separator();
                ui.heading(format!("{} requests imported", report.requests));
                ui.label(format!("{} warning(s) to review", report.warnings.len()));
                egui::ScrollArea::vertical().id_salt("migration-report").max_height(230.0).show(ui, |ui| {
                    for warning in &report.warnings { ui.label(warning); ui.separator(); }
                });
                let can_open = !app.has_dirty_work() && self.running.is_none();
                if !can_open { ui.label("Save your drafts and let any collection run finish before switching projects."); }
                if ui.add_enabled(can_open, egui::Button::new("Open imported project")).clicked() { open = Some(report.root.clone()); }
            }
        });
        self.import_open = visible && open.is_none();
        if start {
            self.start_import();
        }
        open
    }

    fn start_run(&mut self, app: &mut PostlyApp) -> Result<(), String> {
        let collection = app
            .collections
            .get(app.selected_collection)
            .ok_or("Choose a collection first.")?;
        let requests = app
            .workspace
            .requests(collection)
            .map_err(|e| e.to_string())?;
        let folder = self
            .folder
            .trim()
            .replace('\\', "/")
            .trim_matches('/')
            .to_owned();
        let requests: Vec<_> = requests
            .into_iter()
            .filter(|(_, request)| {
                folder.is_empty()
                    || request.folder.as_ref().is_some_and(|value| {
                        let value = value.replace('\\', "/");
                        let value = value.trim_matches('/');
                        value == folder || value.starts_with(&format!("{folder}/"))
                    })
            })
            .collect();
        if requests.is_empty() {
            return Err("No saved requests match this collection and folder.".into());
        }
        self.run_label = format!(
            "{} · {} · {} saved requests",
            collection.collection.name,
            app.selected_environment
                .as_deref()
                .unwrap_or("No environment"),
            requests.len()
        );
        let context = app.context()?;
        let engine = app.configured_engine()?;
        let token = CancellationToken::default();
        let options = RunnerOptions {
            scripts: self.scripts,
            fail_fast: self.fail_fast,
            cancellation: token.clone(),
            ..Default::default()
        };
        let (sender, receiver) = mpsc::channel();
        self.running = Some(receiver);
        self.cancellation = Some(token);
        self.summary = None;
        self.runner_error = None;
        thread::spawn(move || {
            let result = tokio::runtime::Runtime::new()
                .map_err(|e| e.to_string())
                .map(|runtime| {
                    runtime.block_on(postly_core::run_requests(
                        &engine, &requests, &context, &options,
                    ))
                });
            let _ = sender.send(result);
        });
        Ok(())
    }

    fn runner_window(&mut self, ctx: &egui::Context, app: &mut PostlyApp) {
        if !self.runner_open {
            return;
        }
        let mut visible = true;
        let mut start = false;
        let mut selected = None;
        egui::Window::new("Collection runner")
            .open(&mut visible)
            .default_width(710.0)
            .show(ctx, |ui| {
                let collection = app
                    .collections
                    .get(app.selected_collection)
                    .map(|c| c.collection.name.as_str())
                    .unwrap_or("No collection");
                ui.heading(collection);
                ui.label(format!(
                    "Environment: {}",
                    app.selected_environment.as_deref().unwrap_or("None")
                ));
                ui.label(
                    "Runs saved files with the shared CLI engine. Unsaved edits are excluded.",
                );
                ui.add_enabled_ui(self.running.is_none(), |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Folder (optional)");
                        ui.text_edit_singleline(&mut self.folder);
                    });
                    ui.checkbox(&mut self.fail_fast, "Stop after the first failure");
                    ui.checkbox(&mut self.scripts, "Run imported scripts (requires Node.js)");
                    if self.scripts {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            "Only run scripts you trust. They are not a hostile-code sandbox.",
                        );
                    }
                    start = ui.button("Run saved requests").clicked();
                });
                if let Some(token) = &self.cancellation {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(&self.run_label);
                        if ui.button("Cancel run").clicked() {
                            token.cancel();
                        }
                    });
                }
                if let Some(error) = &self.runner_error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                if let Some(summary) = &self.summary {
                    ui.separator();
                    ui.label(&self.run_label);
                    ui.heading(format!(
                        "{} passed · {} failed · {} assertions",
                        summary.passed, summary.failed, summary.assertions
                    ));
                    if summary.cancelled {
                        ui.label("Run cancelled — results below cover only completed requests.");
                    }
                    if ui.button("Export JSON report…").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_file_name("postly-results.json")
                            .save_file()
                        {
                            let result = serde_json::to_vec_pretty(summary)
                                .map_err(|e| e.to_string())
                                .and_then(|bytes| {
                                    std::fs::write(path, bytes).map_err(|e| e.to_string())
                                });
                            if let Err(error) = result {
                                self.runner_error = Some(error);
                            }
                        }
                    }
                    egui::ScrollArea::vertical()
                        .id_salt("runner-results")
                        .max_height(350.0)
                        .show(ui, |ui| {
                            for result in &summary.results {
                                ui.horizontal(|ui| {
                                    ui.label(if result.passed { "PASS" } else { "FAIL" });
                                    if ui.link(&result.name).clicked() {
                                        selected = Some(result.path.clone());
                                    }
                                    ui.label(format!(
                                        "{} · {} ms",
                                        result
                                            .status
                                            .map(|s| s.to_string())
                                            .unwrap_or_else(|| "No response".into()),
                                        result.duration_ms
                                    ));
                                });
                                if let Some(error) = &result.error {
                                    ui.colored_label(ui.visuals().error_fg_color, error);
                                }
                                for failure in &result.assertion_failures {
                                    ui.label(failure);
                                }
                                for test in &result.script_tests {
                                    ui.label(format!(
                                        "{}: {}",
                                        if test.passed { "PASS" } else { "FAIL" },
                                        test.name
                                    ));
                                }
                                ui.separator();
                            }
                        });
                }
            });
        self.runner_open = visible;
        if start {
            if let Err(error) = self.start_run(app) {
                self.runner_error = Some(error);
            }
        }
        if let Some(path) = selected {
            for index in 0..app.collections.len() {
                if let Ok(requests) = app.workspace.requests(&app.collections[index]) {
                    if requests.iter().any(|(candidate, _)| candidate == &path) {
                        app.selected_collection = index;
                        if let Err(error) = app.refresh_requests(Some(&path)) {
                            self.runner_error = Some(error);
                        }
                        break;
                    }
                }
            }
        }
    }
}

fn import_collection(source: &Path, root: &Path, openapi: bool) -> Result<ImportOutcome, String> {
    if root.exists()
        && std::fs::read_dir(root)
            .map_err(|e| e.to_string())?
            .next()
            .is_some()
    {
        return Err(
            "Choose a new or empty destination. Existing workspaces are not overwritten.".into(),
        );
    }
    let metadata = std::fs::metadata(source).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > 32 * 1024 * 1024 {
        return Err("Choose a collection file up to 32 MiB.".into());
    }
    if openapi {
        let report = postly_core::import_openapi(source, root).map_err(|e| e.to_string())?;
        Ok(ImportOutcome {
            root: root.into(),
            requests: report.imported_operations,
            warnings: report.warnings,
        })
    } else {
        let report =
            postly_core::import_postman_collection(source, root).map_err(|e| e.to_string())?;
        Ok(ImportOutcome {
            root: root.into(),
            requests: report.imported_requests,
            warnings: report.warnings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_public_example_and_preserves_destination_on_retry() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("imported");
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/orders/postman.json");
        let report = import_collection(&source, &root, false).unwrap();
        assert_eq!(report.requests, 2);
        assert!(report.warnings.is_empty());
        let before = std::fs::read(root.join("postly.toml")).unwrap();
        assert!(import_collection(&source, &root, false).is_err());
        assert_eq!(before, std::fs::read(root.join("postly.toml")).unwrap());
    }

    #[test]
    fn imports_openapi_and_retains_warnings() {
        let directory = tempfile::tempdir().unwrap();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../compat/openapi/basic-openapi-3.yaml");
        let report = import_collection(&source, &directory.path().join("api"), true).unwrap();
        assert!(report.requests > 0);
        assert!(postly_core::Workspace::open(&report.root).is_ok());
    }

    #[test]
    fn runner_executes_the_native_example_and_reports_failure() {
        let directory = tempfile::tempdir().unwrap();
        let server = postly_core::demo::DemoServer::start(0).unwrap();
        let workspace =
            postly_core::demo::create_workspace(directory.path(), &server.url()).unwrap();
        let collection = &workspace.collections().unwrap()[0];
        let (path, mut request) = workspace.requests(collection).unwrap().remove(0);
        request
            .assertions
            .push(postly_core::Assertion::Status { expected: 201 });
        workspace
            .relocate_request(path, collection, &request)
            .unwrap();
        let mut app = PostlyApp::open(directory.path().to_path_buf()).unwrap();
        let mut workflows = Workflows::default();
        workflows.start_run(&mut app).unwrap();
        let summary = workflows
            .running
            .take()
            .unwrap()
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.assertion_failures, 1);
    }
}
