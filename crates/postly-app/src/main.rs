use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::Duration,
};

use eframe::egui::{self, Color32, RichText, TextEdit, TextStyle};
use postly_core::{
    ApiKeyLocation, Auth, CollectionFiles, EngineOptions, Environment, HeaderEntry, HistoryEntry,
    HttpEngine, HttpResponse, KeyValue, Request, RequestBody, ResponseView, VariableContext,
    Workspace,
};

const ACCENT: Color32 = Color32::from_rgb(91, 141, 239);
const MUTED: Color32 = Color32::from_rgb(145, 157, 177);
const PANEL: Color32 = Color32::from_rgb(24, 29, 39);
const SURFACE: Color32 = Color32::from_rgb(31, 37, 49);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorTab {
    Params,
    Headers,
    Body,
    Auth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseTab {
    Pretty,
    Raw,
    Headers,
    Cookies,
    Timing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyKind {
    None,
    Raw,
    Json,
    Advanced,
}

impl BodyKind {
    fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Raw => "Raw text",
            Self::Json => "JSON",
            Self::Advanced => "Advanced body",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthKind {
    None,
    Bearer,
    Basic,
    ApiKey,
}

impl AuthKind {
    fn label(self) -> &'static str {
        match self {
            Self::None => "No auth",
            Self::Bearer => "Bearer token",
            Self::Basic => "Basic auth",
            Self::ApiKey => "API key",
        }
    }
}

pub struct PostlyApp {
    workspace: Workspace,
    collections: Vec<CollectionFiles>,
    environments: Vec<(PathBuf, Environment)>,
    selected_collection: usize,
    requests: Vec<(PathBuf, Request)>,
    selected_request: Option<usize>,
    request_path: Option<PathBuf>,
    request: Request,
    editor_tab: EditorTab,
    body_kind: BodyKind,
    body_text: String,
    auth_kind: AuthKind,
    auth_primary: String,
    auth_secondary: String,
    api_key_location: ApiKeyLocation,
    response_tab: ResponseTab,
    response: Option<HttpResponse>,
    response_error: Option<String>,
    pending: Option<Receiver<Result<HttpResponse, String>>>,
    pending_request: Option<Request>,
    selected_environment: Option<String>,
    dirty: bool,
    status_message: String,
}

impl PostlyApp {
    pub fn open(root: PathBuf) -> Result<Self, String> {
        let workspace = Workspace::open_or_init(&root, "Postly workspace")
            .map_err(|error| error.to_string())?;
        let mut collections = workspace.collections().map_err(|error| error.to_string())?;
        if collections.is_empty() {
            collections.push(
                workspace
                    .create_collection(&postly_core::Collection::new("My API"))
                    .map_err(|error| error.to_string())?,
            );
        }
        let environments = workspace
            .environments()
            .map_err(|error| error.to_string())?;
        let mut app = Self {
            workspace,
            collections,
            environments,
            selected_collection: 0,
            requests: Vec::new(),
            selected_request: None,
            request_path: None,
            request: Request::new("New request", "GET", "https://example.com"),
            editor_tab: EditorTab::Params,
            body_kind: BodyKind::None,
            body_text: String::new(),
            auth_kind: AuthKind::None,
            auth_primary: String::new(),
            auth_secondary: String::new(),
            api_key_location: ApiKeyLocation::Header,
            response_tab: ResponseTab::Pretty,
            response: None,
            response_error: None,
            pending: None,
            pending_request: None,
            selected_environment: None,
            dirty: false,
            status_message: "Ready — local workspace".to_owned(),
        };
        app.refresh_requests(None)?;
        Ok(app)
    }

    fn refresh_requests(&mut self, preferred_path: Option<&Path>) -> Result<(), String> {
        let collection = self
            .collections
            .get(self.selected_collection)
            .ok_or_else(|| "no collection selected".to_owned())?;
        self.requests = self
            .workspace
            .requests(collection)
            .map_err(|error| error.to_string())?;
        let next_index = preferred_path
            .and_then(|path| {
                self.requests
                    .iter()
                    .position(|(candidate, _)| candidate == path)
            })
            .or_else(|| {
                self.selected_request
                    .filter(|index| *index < self.requests.len())
            })
            .or_else(|| (!self.requests.is_empty()).then_some(0));
        if let Some(index) = next_index {
            self.select_request(index);
        } else {
            self.new_request();
        }
        Ok(())
    }

    fn select_request(&mut self, index: usize) {
        let Some((path, request)) = self.requests.get(index).cloned() else {
            return;
        };
        self.selected_request = Some(index);
        self.request_path = Some(path);
        self.request = request;
        self.load_request_editors();
        self.clear_response();
        self.dirty = false;
        self.status_message = "Request loaded".to_owned();
    }

    fn new_request(&mut self) {
        self.selected_request = None;
        self.request_path = None;
        self.request = Request::new("New request", "GET", "https://example.com");
        self.load_request_editors();
        self.clear_response();
        self.dirty = true;
        self.status_message = "Draft request".to_owned();
    }

    fn load_request_editors(&mut self) {
        match &self.request.body {
            RequestBody::None => {
                self.body_kind = BodyKind::None;
                self.body_text.clear();
            }
            RequestBody::Raw { text, .. } => {
                self.body_kind = BodyKind::Raw;
                self.body_text.clone_from(text);
            }
            RequestBody::Json { value } => {
                self.body_kind = BodyKind::Json;
                self.body_text = serde_json::to_string_pretty(value).unwrap_or_default();
            }
            RequestBody::FormUrlEncoded { .. }
            | RequestBody::Multipart { .. }
            | RequestBody::BinaryFile { .. } => {
                self.body_kind = BodyKind::Advanced;
                self.body_text.clear();
            }
        }
        match &self.request.auth {
            Auth::None => {
                self.auth_kind = AuthKind::None;
                self.auth_primary.clear();
                self.auth_secondary.clear();
                self.api_key_location = ApiKeyLocation::Header;
            }
            Auth::Bearer { token } => {
                self.auth_kind = AuthKind::Bearer;
                self.auth_primary.clone_from(token);
                self.auth_secondary.clear();
                self.api_key_location = ApiKeyLocation::Header;
            }
            Auth::Basic { username, password } => {
                self.auth_kind = AuthKind::Basic;
                self.auth_primary.clone_from(username);
                self.auth_secondary.clone_from(password);
                self.api_key_location = ApiKeyLocation::Header;
            }
            Auth::ApiKey {
                key,
                value,
                location,
            } => {
                self.auth_kind = AuthKind::ApiKey;
                self.auth_primary.clone_from(key);
                self.auth_secondary.clone_from(value);
                self.api_key_location = location.clone();
            }
        }
    }

    fn clear_response(&mut self) {
        self.response = None;
        self.response_error = None;
    }

    fn edited_request(&self) -> Result<Request, String> {
        let mut request = self.request.clone();
        request.body = match self.body_kind {
            BodyKind::None => RequestBody::None,
            BodyKind::Raw => RequestBody::Raw {
                text: self.body_text.clone(),
                content_type: None,
            },
            BodyKind::Json => RequestBody::Json {
                value: serde_json::from_str(&self.body_text)
                    .map_err(|error| format!("JSON body is invalid: {error}"))?,
            },
            BodyKind::Advanced => request.body,
        };
        request.auth = match self.auth_kind {
            AuthKind::None => Auth::None,
            AuthKind::Bearer => Auth::Bearer {
                token: self.auth_primary.clone(),
            },
            AuthKind::Basic => Auth::Basic {
                username: self.auth_primary.clone(),
                password: self.auth_secondary.clone(),
            },
            AuthKind::ApiKey => Auth::ApiKey {
                key: self.auth_primary.clone(),
                value: self.auth_secondary.clone(),
                location: self.api_key_location.clone(),
            },
        };
        Ok(request)
    }

    fn save_current(&mut self) -> Result<(), String> {
        let request = self.edited_request()?;
        let path = if let Some(path) = &self.request_path {
            self.workspace
                .update_request(path, &request)
                .map_err(|error| error.to_string())?;
            path.clone()
        } else {
            let collection = self
                .collections
                .get(self.selected_collection)
                .ok_or_else(|| "no collection selected".to_owned())?;
            self.workspace
                .save_request(collection, &request)
                .map_err(|error| error.to_string())?
        };
        self.request = request;
        self.request_path = Some(path.clone());
        self.refresh_requests(Some(&path))?;
        self.dirty = false;
        self.status_message = format!("Saved locally — {}", path.display());
        Ok(())
    }

    fn context(&self) -> VariableContext {
        let mut context = VariableContext::default();
        if let Some(collection) = self.collections.get(self.selected_collection) {
            context.collection = collection.collection.variables.clone();
        }
        if let Some(name) = &self.selected_environment {
            if let Some((_, environment)) = self
                .environments
                .iter()
                .find(|(_, environment)| &environment.name == name)
            {
                context.environment = environment.enabled_values();
            }
        }
        context
    }

    fn send_current(&mut self) -> Result<(), String> {
        if self.pending.is_some() {
            return Ok(());
        }
        let request = self.edited_request()?;
        let context = self.context();
        let (sender, receiver) = mpsc::channel();
        let worker_request = request.clone();
        thread::spawn(move || {
            let result = (|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())?;
                let engine = HttpEngine::new(&EngineOptions::default())
                    .map_err(|error| error.to_string())?;
                runtime
                    .block_on(engine.execute(&worker_request, &context))
                    .map_err(|error| error.to_string())
            })();
            let _ = sender.send(result);
        });
        self.pending = Some(receiver);
        self.pending_request = Some(request);
        self.clear_response();
        self.status_message = "Sending request…".to_owned();
        Ok(())
    }

    fn poll_pending(&mut self) -> bool {
        let Some(receiver) = self.pending.as_ref() else {
            return false;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return true,
            Err(TryRecvError::Disconnected) => {
                Err("request worker stopped unexpectedly".to_owned())
            }
        };
        let request = self.pending_request.take();
        self.pending = None;
        match result {
            Ok(response) => {
                if let Some(request) = request {
                    let _ = self
                        .workspace
                        .record_history(&HistoryEntry::from_response(&request, &response));
                }
                self.status_message = format!(
                    "{} {} in {} ms",
                    response.status, response.status_text, response.duration_ms
                );
                self.response = Some(response);
                self.response_error = None;
            }
            Err(error) => {
                if let Some(request) = request {
                    let _ = self
                        .workspace
                        .record_history(&HistoryEntry::from_error(&request, 0));
                }
                self.status_message = "Request failed".to_owned();
                self.response_error = Some(error);
            }
        }
        false
    }

    fn draw_navigator(&mut self, ui: &mut egui::Ui) {
        let mut collection_clicked = None;
        let mut request_clicked = None;
        let mut new_clicked = false;
        let mut environment_clicked = None;
        egui::Panel::left("navigator")
            .resizable(true)
            .default_size(280.0)
            .min_size(220.0)
            .frame(egui::Frame::default().fill(PANEL))
            .show(ui, |ui| {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.heading(RichText::new("POSTLY").color(Color32::WHITE));
                    ui.label(RichText::new("LOCAL").small().color(ACCENT));
                });
                ui.label(
                    RichText::new("Rust-native API workspace")
                        .small()
                        .color(MUTED),
                );
                ui.add_space(14.0);
                if ui
                    .add_sized(
                        [ui.available_width(), 34.0],
                        egui::Button::new(RichText::new("＋  New request").color(Color32::WHITE))
                            .fill(ACCENT),
                    )
                    .clicked()
                {
                    new_clicked = true;
                }
                ui.add_space(18.0);
                ui.label(RichText::new("COLLECTIONS").small().strong().color(MUTED));
                ui.add_space(5.0);
                for (index, collection) in self.collections.iter().enumerate() {
                    let selected = index == self.selected_collection;
                    if ui
                        .selectable_label(
                            selected,
                            RichText::new(format!("▸  {}", collection.collection.name))
                                .color(if selected { Color32::WHITE } else { MUTED }),
                        )
                        .clicked()
                    {
                        collection_clicked = Some(index);
                    }
                }
                ui.add_space(14.0);
                ui.label(RichText::new("REQUESTS").small().strong().color(MUTED));
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .id_salt("request-list")
                    .max_height(ui.available_height() - 130.0)
                    .show(ui, |ui| {
                        for (index, (_, request)) in self.requests.iter().enumerate() {
                            let selected = self.selected_request == Some(index);
                            let label = format!("{}  {}", request.method, request.name);
                            if ui
                                .selectable_label(
                                    selected,
                                    RichText::new(label).color(if selected {
                                        Color32::WHITE
                                    } else {
                                        MUTED
                                    }),
                                )
                                .clicked()
                            {
                                request_clicked = Some(index);
                            }
                        }
                    });
                ui.add_space(10.0);
                ui.separator();
                ui.label(RichText::new("ENVIRONMENT").small().strong().color(MUTED));
                let selected_name = self
                    .selected_environment
                    .as_deref()
                    .unwrap_or("No environment")
                    .to_owned();
                egui::ComboBox::from_id_salt("environment")
                    .selected_text(selected_name)
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(self.selected_environment.is_none(), "No environment")
                            .clicked()
                        {
                            environment_clicked = Some(None);
                        }
                        for (_, environment) in &self.environments {
                            if ui
                                .selectable_label(
                                    self.selected_environment.as_deref()
                                        == Some(environment.name.as_str()),
                                    &environment.name,
                                )
                                .clicked()
                            {
                                environment_clicked = Some(Some(environment.name.clone()));
                            }
                        }
                    });
                ui.add_space(8.0);
                ui.label(
                    RichText::new(self.workspace.root().display().to_string())
                        .small()
                        .color(MUTED),
                );
            });
        if new_clicked {
            self.new_request();
        }
        if let Some(index) = collection_clicked {
            self.selected_collection = index;
            if let Err(error) = self.refresh_requests(None) {
                self.status_message = error;
            }
        }
        if let Some(index) = request_clicked {
            self.select_request(index);
        }
        if let Some(environment) = environment_clicked {
            self.selected_environment = environment;
        }
    }

    fn draw_request_header(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("request-header")
            .frame(egui::Frame::default().fill(SURFACE))
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("REQUEST").strong().color(MUTED));
                    ui.label(
                        RichText::new(if self.dirty { "• unsaved" } else { "saved" })
                            .small()
                            .color(if self.dirty {
                                Color32::from_rgb(235, 180, 80)
                            } else {
                                Color32::from_rgb(100, 205, 145)
                            }),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(&self.status_message).small().color(MUTED));
                    });
                });
                ui.add_space(7.0);
                let mut send_clicked = false;
                let mut save_clicked = false;
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            TextEdit::singleline(&mut self.request.name)
                                .hint_text("Request name")
                                .desired_width(170.0),
                        )
                        .changed()
                    {
                        self.dirty = true;
                    }
                    egui::ComboBox::from_id_salt("method")
                        .selected_text(&self.request.method)
                        .width(92.0)
                        .show_ui(ui, |ui| {
                            for method in
                                ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"]
                            {
                                if ui
                                    .selectable_value(
                                        &mut self.request.method,
                                        method.to_owned(),
                                        method,
                                    )
                                    .changed()
                                {
                                    self.dirty = true;
                                }
                            }
                        });
                    if ui
                        .add(
                            TextEdit::singleline(&mut self.request.url)
                                .hint_text("https://api.example.com/resource")
                                .desired_width(ui.available_width() - 170.0),
                        )
                        .changed()
                    {
                        self.dirty = true;
                    }
                    if ui
                        .add_enabled(
                            self.pending.is_none(),
                            egui::Button::new(if self.pending.is_some() {
                                "Sending…"
                            } else {
                                "Send  ⌘↵"
                            })
                            .fill(ACCENT),
                        )
                        .clicked()
                    {
                        send_clicked = true;
                    }
                    if ui.button("Save").clicked() {
                        save_clicked = true;
                    }
                    if ui.input(|input| {
                        input.key_pressed(egui::Key::Enter) && input.modifiers.command
                    }) {
                        send_clicked = true;
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    for (tab, label) in [
                        (EditorTab::Params, "Params"),
                        (EditorTab::Headers, "Headers"),
                        (EditorTab::Body, "Body"),
                        (EditorTab::Auth, "Auth"),
                    ] {
                        if tab_button(ui, self.editor_tab == tab, label).clicked() {
                            self.editor_tab = tab;
                        }
                    }
                });
                ui.add_space(3.0);
                if save_clicked {
                    if let Err(error) = self.save_current() {
                        self.status_message = format!("Save failed: {error}");
                    }
                }
                if send_clicked {
                    if let Err(error) = self.send_current() {
                        self.status_message = format!("Send failed: {error}");
                    }
                }
            });
    }

    fn draw_editor(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(Color32::from_rgb(18, 22, 30)))
            .show(ui, |ui| {
                ui.add_space(12.0);
                match self.editor_tab {
                    EditorTab::Params => {
                        ui.heading(RichText::new("Query parameters").color(Color32::WHITE));
                        ui.label(
                            RichText::new("Values are resolved only when the request is sent.")
                                .small()
                                .color(MUTED),
                        );
                        ui.add_space(10.0);
                        self.dirty |= render_key_values(ui, &mut self.request.query, "query");
                    }
                    EditorTab::Headers => {
                        ui.heading(RichText::new("Headers").color(Color32::WHITE));
                        ui.label(
                            RichText::new(
                                "Duplicate header names are preserved in the request model.",
                            )
                            .small()
                            .color(MUTED),
                        );
                        ui.add_space(10.0);
                        self.dirty |= render_headers(ui, &mut self.request.headers);
                    }
                    EditorTab::Body => self.render_body(ui),
                    EditorTab::Auth => self.render_auth(ui),
                }
            });
    }

    fn render_body(&mut self, ui: &mut egui::Ui) {
        ui.heading(RichText::new("Request body").color(Color32::WHITE));
        ui.add_space(8.0);
        let previous = self.body_kind;
        egui::ComboBox::from_id_salt("body-kind")
            .selected_text(self.body_kind.label())
            .width(180.0)
            .show_ui(ui, |ui| {
                for kind in [BodyKind::None, BodyKind::Raw, BodyKind::Json] {
                    ui.selectable_value(&mut self.body_kind, kind, kind.label());
                }
                if previous == BodyKind::Advanced {
                    ui.selectable_value(&mut self.body_kind, BodyKind::Advanced, "Advanced body");
                }
            });
        if self.body_kind != previous {
            self.dirty = true;
            if self.body_kind == BodyKind::Raw && self.body_text.is_empty() {
                self.body_text = "".to_owned();
            }
            if self.body_kind == BodyKind::Json && self.body_text.is_empty() {
                self.body_text = "{}".to_owned();
            }
        }
        ui.add_space(10.0);
        match self.body_kind {
            BodyKind::None => {
                ui.label(RichText::new("This request has no body.").color(MUTED));
            }
            BodyKind::Raw | BodyKind::Json => {
                if ui
                    .add(
                        TextEdit::multiline(&mut self.body_text)
                            .font(TextStyle::Monospace)
                            .desired_rows(15)
                            .desired_width(f32::INFINITY)
                            .hint_text(if self.body_kind == BodyKind::Json {
                                "{\n  \"key\": \"value\"\n}"
                            } else {
                                "Request body"
                            }),
                    )
                    .changed()
                {
                    self.dirty = true;
                }
            }
            BodyKind::Advanced => {
                ui.label(RichText::new("This imported body is preserved by the core model. Its dedicated editor is next.").color(MUTED));
            }
        }
    }

    fn render_auth(&mut self, ui: &mut egui::Ui) {
        ui.heading(RichText::new("Authentication").color(Color32::WHITE));
        ui.add_space(8.0);
        let previous = self.auth_kind;
        egui::ComboBox::from_id_salt("auth-kind")
            .selected_text(self.auth_kind.label())
            .width(180.0)
            .show_ui(ui, |ui| {
                for kind in [
                    AuthKind::None,
                    AuthKind::Bearer,
                    AuthKind::Basic,
                    AuthKind::ApiKey,
                ] {
                    ui.selectable_value(&mut self.auth_kind, kind, kind.label());
                }
            });
        if self.auth_kind != previous {
            self.dirty = true;
        }
        ui.add_space(10.0);
        match self.auth_kind {
            AuthKind::None => {
                ui.label(RichText::new("No authentication will be added.").color(MUTED));
            }
            AuthKind::Bearer => {
                ui.label("Token");
                if ui
                    .add(TextEdit::singleline(&mut self.auth_primary).password(true))
                    .changed()
                {
                    self.dirty = true;
                }
            }
            AuthKind::Basic => {
                ui.label("Username");
                if ui
                    .add(TextEdit::singleline(&mut self.auth_primary))
                    .changed()
                {
                    self.dirty = true;
                }
                ui.label("Password");
                if ui
                    .add(TextEdit::singleline(&mut self.auth_secondary).password(true))
                    .changed()
                {
                    self.dirty = true;
                }
            }
            AuthKind::ApiKey => {
                ui.horizontal(|ui| {
                    ui.label("Location");
                    let header_selected = self.api_key_location == ApiKeyLocation::Header;
                    if ui.selectable_label(header_selected, "Header").clicked() {
                        self.api_key_location = ApiKeyLocation::Header;
                        self.dirty = true;
                    }
                    let query_selected = self.api_key_location == ApiKeyLocation::Query;
                    if ui.selectable_label(query_selected, "Query").clicked() {
                        self.api_key_location = ApiKeyLocation::Query;
                        self.dirty = true;
                    }
                });
                ui.label("Header key");
                if ui
                    .add(TextEdit::singleline(&mut self.auth_primary))
                    .changed()
                {
                    self.dirty = true;
                }
                ui.label("Value");
                if ui
                    .add(TextEdit::singleline(&mut self.auth_secondary).password(true))
                    .changed()
                {
                    self.dirty = true;
                }
            }
        }
    }

    fn draw_response(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("response")
            .resizable(true)
            .default_size(330.0)
            .min_size(180.0)
            .frame(egui::Frame::default().fill(SURFACE))
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("RESPONSE").strong().color(MUTED));
                    if let Some(response) = &self.response {
                        ui.label(
                            RichText::new(format!(
                                "{} {}  ·  {} ms  ·  {}",
                                response.status,
                                response.status_text,
                                response.duration_ms,
                                response.protocol
                            ))
                            .color(if response.status < 400 {
                                Color32::from_rgb(100, 205, 145)
                            } else {
                                Color32::from_rgb(240, 125, 105)
                            }),
                        );
                    }
                });
                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    for (tab, label) in [
                        (ResponseTab::Pretty, "Pretty"),
                        (ResponseTab::Raw, "Raw"),
                        (ResponseTab::Headers, "Headers"),
                        (ResponseTab::Cookies, "Cookies"),
                        (ResponseTab::Timing, "Timing"),
                    ] {
                        if tab_button(ui, self.response_tab == tab, label).clicked() {
                            self.response_tab = tab;
                        }
                    }
                });
                ui.separator();
                if let Some(error) = &self.response_error {
                    ui.colored_label(Color32::from_rgb(240, 125, 105), error);
                } else if let Some(response) = &self.response {
                    self.render_response_content(ui, response);
                } else if self.pending.is_some() {
                    ui.label(RichText::new("Waiting for the local HTTP engine…").color(MUTED));
                } else {
                    ui.label(
                        RichText::new("Send a request to inspect its response here.").color(MUTED),
                    );
                }
            });
    }

    fn render_response_content(&self, ui: &mut egui::Ui, response: &HttpResponse) {
        match self.response_tab {
            ResponseTab::Pretty | ResponseTab::Raw => {
                let view = if self.response_tab == ResponseTab::Pretty {
                    ResponseView::Pretty
                } else {
                    ResponseView::Raw
                };
                let mut text = response.formatted_body(view);
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add(
                            TextEdit::multiline(&mut text)
                                .font(TextStyle::Monospace)
                                .desired_width(f32::INFINITY)
                                .desired_rows(10),
                        );
                    });
            }
            ResponseTab::Headers => {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::Grid::new("response-headers")
                        .striped(true)
                        .show(ui, |ui| {
                            for header in &response.headers {
                                ui.label(RichText::new(&header.key).strong());
                                ui.label(&header.value);
                                ui.end_row();
                            }
                        });
                });
            }
            ResponseTab::Cookies => {
                ui.label(
                    RichText::new(
                        "Response cookie parsing is reserved for the cookie jar milestone.",
                    )
                    .color(MUTED),
                );
            }
            ResponseTab::Timing => {
                ui.label(format!("Total duration: {} ms", response.duration_ms));
                ui.label(format!("Protocol: {}", response.protocol));
                ui.label(format!("Response size: {} bytes", response.body.len()));
                ui.label(format!("Final URL: {}", response.url));
            }
        }
    }
}

impl eframe::App for PostlyApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ctx.set_visuals(egui::Visuals::dark());
        let pending = self.poll_pending();
        self.draw_navigator(ui);
        self.draw_request_header(ui);
        self.draw_response(ui);
        self.draw_editor(ui);
        if pending {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
    }
}

fn tab_button(ui: &mut egui::Ui, selected: bool, label: &str) -> egui::Response {
    let fill = if selected {
        ACCENT.linear_multiply(0.24)
    } else {
        Color32::TRANSPARENT
    };
    ui.add(
        egui::Button::new(RichText::new(label).color(if selected {
            Color32::WHITE
        } else {
            MUTED
        }))
        .fill(fill),
    )
}

fn render_key_values(ui: &mut egui::Ui, values: &mut Vec<KeyValue>, id: &str) -> bool {
    let mut changed = false;
    let mut remove = None;
    egui::Grid::new(id)
        .striped(true)
        .min_col_width(120.0)
        .show(ui, |ui| {
            ui.label(RichText::new("Enabled").small().color(MUTED));
            ui.label(RichText::new("Key").small().color(MUTED));
            ui.label(RichText::new("Value").small().color(MUTED));
            ui.end_row();
            for (index, pair) in values.iter_mut().enumerate() {
                changed |= ui.checkbox(&mut pair.enabled, "").changed();
                changed |= ui.text_edit_singleline(&mut pair.key).changed();
                changed |= ui.text_edit_singleline(&mut pair.value).changed();
                if ui.small_button("×").clicked() {
                    remove = Some(index);
                }
                ui.end_row();
            }
        });
    if let Some(index) = remove {
        values.remove(index);
        changed = true;
    }
    if ui.button("＋ Add parameter").clicked() {
        values.push(KeyValue::enabled("", ""));
        changed = true;
    }
    changed
}

fn render_headers(ui: &mut egui::Ui, headers: &mut Vec<HeaderEntry>) -> bool {
    let mut changed = false;
    let mut remove = None;
    egui::Grid::new("headers")
        .striped(true)
        .min_col_width(120.0)
        .show(ui, |ui| {
            ui.label(RichText::new("Enabled").small().color(MUTED));
            ui.label(RichText::new("Name").small().color(MUTED));
            ui.label(RichText::new("Value").small().color(MUTED));
            ui.end_row();
            for (index, header) in headers.iter_mut().enumerate() {
                changed |= ui.checkbox(&mut header.enabled, "").changed();
                changed |= ui.text_edit_singleline(&mut header.key).changed();
                changed |= ui.text_edit_singleline(&mut header.value).changed();
                if ui.small_button("×").clicked() {
                    remove = Some(index);
                }
                ui.end_row();
            }
        });
    if let Some(index) = remove {
        headers.remove(index);
        changed = true;
    }
    if ui.button("＋ Add header").clicked() {
        headers.push(HeaderEntry::enabled("", ""));
        changed = true;
    }
    changed
}

fn main() -> eframe::Result {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let app = match PostlyApp::open(root) {
        Ok(app) => app,
        Err(error) => {
            eprintln!("could not open Postly workspace: {error}");
            std::process::exit(1);
        }
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 960.0])
            .with_min_inner_size([980.0, 680.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Postly — local API workspace",
        options,
        Box::new(|_creation_context| Ok(Box::new(app))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };

    #[test]
    fn editor_state_builds_a_json_bearer_request() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.request.name = "Create user".to_owned();
        app.request.method = "POST".to_owned();
        app.request.url = "{{baseUrl}}/users".to_owned();
        app.body_kind = BodyKind::Json;
        app.body_text = r#"{"name":"Ada"}"#.to_owned();
        app.auth_kind = AuthKind::Bearer;
        app.auth_primary = "{{token}}".to_owned();

        let request = app.edited_request().expect("valid editor state");
        assert_eq!(request.method, "POST");
        assert!(matches!(request.body, RequestBody::Json { .. }));
        assert!(matches!(request.auth, Auth::Bearer { .. }));
    }

    #[test]
    fn editor_rejects_invalid_json_before_network_work() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.body_kind = BodyKind::Json;
        app.body_text = "not json".to_owned();

        let error = app.edited_request().expect_err("invalid JSON must fail");
        assert!(error.contains("JSON body is invalid"));
    }

    #[test]
    fn send_worker_delivers_a_real_local_response_to_the_gui_state() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("connection");
            let mut request = [0_u8; 1024];
            let length = stream.read(&mut request).expect("read");
            assert!(String::from_utf8_lossy(&request[..length]).contains("GET /gui HTTP/1.1"));
            stream
                .write_all(
                    b"HTTP/1.1 201 Created\r\ncontent-type: application/json\r\ncontent-length: 12\r\n\r\n{\"gui\":true}",
                )
                .expect("write");
        });
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.request.url = format!("http://{address}/gui");
        app.send_current().expect("send");

        let mut finished = false;
        for _ in 0..200 {
            if !app.poll_pending() {
                finished = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        server.join().expect("server");
        assert!(finished, "GUI worker did not finish");
        assert_eq!(
            app.response.as_ref().map(|response| response.status),
            Some(201)
        );
        assert!(app.response_error.is_none());
    }
}
