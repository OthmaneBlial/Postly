use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText};
use postly_core::{
    demo::{create_workspace, DemoServer},
    Workspace,
};

use crate::PostlyApp;

/// Keep onboarding separate: starting Postly must never initialize the process'
/// working directory unless a workspace path was explicitly supplied.
pub struct DesktopApp {
    workspace: Option<Box<PostlyApp>>,
    demo: Option<DemoServer>,
    path: String,
    name: String,
    error: Option<String>,
}

impl DesktopApp {
    pub fn new(explicit_path: Option<PathBuf>) -> Self {
        let default = dirs::document_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_default()
            .join("Postly projects")
            .join("my-api");
        let mut app = Self {
            workspace: None,
            demo: None,
            path: default.display().to_string(),
            name: "My API".into(),
            error: None,
        };
        if let Some(path) = explicit_path {
            app.path = path.display().to_string();
            match PostlyApp::open(path) {
                Ok(workspace) => app.workspace = Some(Box::new(workspace)),
                Err(error) => app.error = Some(error),
            }
        }
        app
    }

    fn open_existing(&mut self, path: &Path) -> Result<(), String> {
        // Validate before PostlyApp::open, whose explicit CLI behavior permits init.
        Workspace::open(path).map_err(|e| e.to_string())?;
        self.workspace = Some(Box::new(PostlyApp::open(path.to_path_buf())?));
        self.demo = None;
        Ok(())
    }

    fn create(&mut self, example: bool) -> Result<(), String> {
        let path = PathBuf::from(self.path.trim());
        if self.path.trim().is_empty() {
            return Err("Choose a folder for your project.".into());
        }
        if path.exists()
            && std::fs::read_dir(&path)
                .map_err(|e| e.to_string())?
                .next()
                .is_some()
        {
            return Err("Choose a new or empty folder. To use an existing Postly project, choose Open project.".into());
        }
        let server = if example {
            let server = DemoServer::start(0)
                .map_err(|e| format!("Could not start the example API: {e}"))?;
            create_workspace(&path, &server.url())?;
            Some(server)
        } else {
            if self.name.trim().is_empty() {
                return Err("Give your project a name.".into());
            }
            Workspace::init(&path, self.name.trim()).map_err(|e| e.to_string())?;
            None
        };
        self.workspace = Some(Box::new(PostlyApp::open(path)?));
        self.demo = server;
        Ok(())
    }
}

impl eframe::App for DesktopApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if let Some(workspace) = &mut self.workspace {
            if let Some(server) = &self.demo {
                egui::Panel::top("example-api-status").show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("EXAMPLE API RUNNING")
                                .small()
                                .color(crate::ACCENT),
                        );
                        ui.label(server.url());
                        ui.label(
                            "Fictional data · stops when you close Postly · your files stay saved",
                        );
                    });
                });
            }
            workspace.ui(ui, frame);
            return;
        }
        let mut action = None;
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space((ui.available_height() * 0.12).max(20.0));
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("POSTLY").size(18.0).strong().color(crate::ACCENT));
                ui.add_space(14.0);
                ui.label(RichText::new("Your API. Your project.").size(38.0).strong());
                ui.add_space(10.0);
                ui.label("Send requests in a native app. Keep them as files beside your code.");
                ui.label("No account needed.");
                ui.add_space(32.0);
                ui.allocate_ui_with_layout(egui::vec2(600.0, 330.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_max_width(600.0);
                    ui.label(RichText::new("Start with a project").size(22.0).strong());
                    ui.add_space(12.0);
                    ui.label("Project folder");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.path).desired_width(465.0));
                        if ui.button("Browse…").clicked() {
                            if let Some(path) = rfd::FileDialog::new().set_title("Choose a project folder").pick_folder() {
                                self.path = path.display().to_string();
                            }
                        }
                    });
                    ui.add_space(10.0);
                    ui.label("Name for a new project");
                    ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(f32::INFINITY));
                    ui.add_space(18.0);
                    ui.horizontal(|ui| {
                        if ui.add_sized([175.0, 42.0], egui::Button::new(RichText::new("Try the example").color(egui::Color32::WHITE)).fill(crate::ACCENT)).clicked() { action = Some(0); }
                        if ui.add_sized([175.0, 42.0], egui::Button::new("Create project")).clicked() { action = Some(1); }
                        if ui.add_sized([175.0, 42.0], egui::Button::new("Open project")).clicked() { action = Some(2); }
                    });
                    ui.add_space(14.0);
                    ui.label("The example creates two requests and starts a real API on your machine.");
                    ui.label("Create and Try require a new or empty folder. Existing files stay intact.");
                    if let Some(error) = &self.error {
                        ui.add_space(12.0);
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }
                });
            });
        });
        if let Some(action) = action {
            let result = match action {
                0 => self.create(true),
                1 => self.create(false),
                _ => self.open_existing(&PathBuf::from(self.path.trim())),
            };
            self.error = result.err();
        }
    }

    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        if let Some(workspace) = &mut self.workspace {
            workspace.on_exit(gl);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_without_a_path_does_not_create_a_workspace() {
        let app = DesktopApp::new(None);
        assert!(app.workspace.is_none());
        assert!(app.demo.is_none());
    }

    #[test]
    fn opening_non_project_never_initializes_it() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = DesktopApp::new(None);
        assert!(app.open_existing(directory.path()).is_err());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn create_and_example_preserve_existing_data() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("notes"), "keep").unwrap();
        let mut app = DesktopApp::new(None);
        app.path = directory.path().display().to_string();
        assert!(app.create(false).is_err());
        assert!(app.create(true).is_err());
        assert!(!directory.path().join("postly.toml").exists());
    }
}
