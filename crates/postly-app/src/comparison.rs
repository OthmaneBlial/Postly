use crate::PostlyApp;
use eframe::egui;
use postly_core::diff::{compare_json, ChangeKind, JsonComparison, MAX_COMPARISON_BYTES};

#[derive(Default)]
pub struct Comparison {
    open: bool,
    baseline: Option<Vec<u8>>,
    baseline_name: String,
    ignored: String,
    result: Option<JsonComparison>,
    result_label: String,
    error: Option<String>,
}

impl Comparison {
    pub fn button(&mut self, ui: &mut egui::Ui, app: &PostlyApp) {
        if ui
            .add_enabled(app.response.is_some(), egui::Button::new("Compare JSON…"))
            .clicked()
        {
            self.open = true;
            self.baseline = None;
            self.baseline_name.clear();
            self.result = None;
            self.error = None;
            self.ignored.clear();
            if let Some(example) = app.request.examples.iter().find(|e| e.body.is_some()) {
                self.baseline = example.body.as_ref().map(|body| body.as_bytes().to_vec());
                self.baseline_name = example.name.clone();
            }
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, app: &PostlyApp) {
        if !self.open {
            return;
        }
        let mut visible = true;
        egui::Window::new("Compare JSON responses").open(&mut visible).default_width(800.0).show(ctx, |ui| {
            ui.label("Compare the current response with a saved example or JSON file. Nothing is saved automatically.");
            ui.horizontal_wrapped(|ui| {
                ui.label("Baseline:");
                egui::ComboBox::from_id_salt("comparison-baseline").selected_text(if self.baseline_name.is_empty() { "Choose an example" } else { &self.baseline_name }).show_ui(ui, |ui| {
                    for example in &app.request.examples {
                        if let Some(body) = &example.body {
                            if ui.selectable_label(self.baseline_name == example.name, &example.name).clicked() {
                                self.baseline = Some(body.as_bytes().to_vec());
                                self.baseline_name = example.name.clone();
                                self.result = None;
                            }
                        }
                    }
                });
                if ui.button("Load JSON file…").clicked() {
                    if let Some(path) = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file() {
                        let result = std::fs::metadata(&path).map_err(|e| e.to_string()).and_then(|metadata| {
                            if metadata.len() > MAX_COMPARISON_BYTES as u64 { return Err("Choose a JSON file up to 4 MiB.".into()); }
                            std::fs::read(&path).map_err(|e| e.to_string())
                        });
                        match result {
                            Ok(bytes) => { self.baseline = Some(bytes); self.baseline_name = path.file_name().unwrap_or_default().to_string_lossy().into_owned(); self.result = None; self.error = None; }
                            Err(error) => self.error = Some(error),
                        }
                    }
                }
            });
            ui.label("Exclude volatile fields (one JSON Pointer per line, e.g. /timestamp or /meta/requestId)");
            if ui.add(egui::TextEdit::multiline(&mut self.ignored).desired_rows(2).desired_width(f32::INFINITY)).changed() { self.result = None; }
            ui.label("Exclusions apply to entire subtrees. Arrays are compared by index; this is a JSON body comparison, not a status/header comparison.");
            if ui.add_enabled(self.baseline.is_some() && app.response.is_some(), egui::Button::new("Compare current response")).clicked() {
                let ignored = self.ignored.lines().map(str::trim).filter(|p| !p.is_empty()).map(str::to_owned).collect::<Vec<_>>();
                if let (Some(baseline), Some(response)) = (&self.baseline, &app.response) {
                    self.result_label = format!("{} against {} · captured response {} {}", app.request.name, self.baseline_name, response.status, response.status_text);
                    match compare_json(baseline, &response.body, &ignored) {
                        Ok(result) => { self.result = Some(result); self.error = None; }
                        Err(error) => { self.error = Some(error); self.result = None; }
                    }
                }
            }
            if let Some(error) = &self.error { ui.colored_label(ui.visuals().error_fg_color, error); }
            if let Some(result) = &self.result {
                ui.separator();
                ui.label(&self.result_label);
                ui.heading(format!("{} difference(s) · {} excluded subtree(s)", result.changes.len(), result.ignored_subtrees));
                if result.truncated { ui.colored_label(ui.visuals().warn_fg_color, "Partial result: comparison reached its size, depth or change limit. This does not prove equivalence."); }
                if result.changes.is_empty() && !result.truncated { ui.label("JSON bodies match outside the explicit exclusions."); }
                egui::ScrollArea::both().id_salt("json-comparison").max_height(380.0).show(ui, |ui| {
                    for change in &result.changes {
                        let kind = match change.kind { ChangeKind::Added => "ADDED", ChangeKind::Removed => "REMOVED", ChangeKind::Changed => "CHANGED" };
                        ui.strong(format!("{kind}  {}", if change.pointer.is_empty() { "(root)" } else { &change.pointer }));
                        ui.monospace(format!("Before: {}", preview(change.before.as_ref())));
                        ui.monospace(format!("After:  {}", preview(change.after.as_ref())));
                        ui.separator();
                    }
                });
            }
        });
        self.open = visible;
        if !visible {
            self.baseline = None;
            self.result = None;
        }
    }
}

fn preview(value: Option<&serde_json::Value>) -> String {
    let Some(value) = value else {
        return "(missing)".into();
    };
    let value = value.to_string();
    let mut chars = value.chars();
    let mut text: String = chars.by_ref().take(240).collect();
    if chars.next().is_some() {
        text.push_str("… [preview shortened]");
    }
    text
}
