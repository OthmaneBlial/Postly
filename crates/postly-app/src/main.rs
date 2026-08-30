use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::Duration,
};

use chrono::Local;
use eframe::egui::{self, Color32, RichText, TextEdit, TextStyle};
use postly_core::{
    ApiKeyLocation, Assertion, Auth, CollectionFiles, EngineOptions, Environment, HeaderEntry,
    HistoryEntry, HistoryFilter, HttpEngine, HttpResponse, KeyValue, Request, RequestBody,
    ResponseView, SseEvent, SseParser, VariableContext, Workspace,
};

const ACCENT: Color32 = Color32::from_rgb(91, 141, 239);
const MUTED: Color32 = Color32::from_rgb(145, 157, 177);
const PANEL: Color32 = Color32::from_rgb(24, 29, 39);
const SURFACE: Color32 = Color32::from_rgb(31, 37, 49);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorTab {
    Params,
    Headers,
    Cookies,
    Body,
    Auth,
    Assertions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseTab {
    Pretty,
    Raw,
    Headers,
    Cookies,
    Timing,
    SseEvents,
}

const MAX_SSE_EVENTS: usize = 500;

#[derive(Debug, Clone)]
struct ReceivedSseEvent {
    event: SseEvent,
    received_at: String,
}

#[derive(Debug)]
enum SseStreamUpdate {
    Connected {
        status: u16,
        status_text: String,
        content_type: Option<String>,
        protocol: String,
        url: String,
    },
    Event(SseEvent),
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyKind {
    None,
    Raw,
    Json,
    Graphql,
    Advanced,
}

impl BodyKind {
    fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Raw => "Raw text",
            Self::Json => "JSON",
            Self::Graphql => "GraphQL",
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssertionKind {
    Status,
    HeaderPresent,
    HeaderEquals,
    BodyContains,
    JsonPointerEquals,
}

impl AssertionKind {
    fn label(self) -> &'static str {
        match self {
            Self::Status => "Status equals",
            Self::HeaderPresent => "Header exists",
            Self::HeaderEquals => "Header equals",
            Self::BodyContains => "Body contains",
            Self::JsonPointerEquals => "JSON Pointer equals",
        }
    }

    fn default_assertion(self) -> Assertion {
        match self {
            Self::Status => Assertion::Status { expected: 200 },
            Self::HeaderPresent => Assertion::HeaderPresent {
                name: "content-type".to_owned(),
            },
            Self::HeaderEquals => Assertion::HeaderEquals {
                name: "content-type".to_owned(),
                expected: "application/json".to_owned(),
            },
            Self::BodyContains => Assertion::BodyContains {
                value: String::new(),
            },
            Self::JsonPointerEquals => Assertion::JsonPointerEquals {
                pointer: "/status".to_owned(),
                expected: serde_json::Value::Null,
            },
        }
    }
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
    engine: HttpEngine,
    collections: Vec<CollectionFiles>,
    environments: Vec<(PathBuf, Environment)>,
    history: Vec<HistoryEntry>,
    history_search: String,
    selected_collection: usize,
    requests: Vec<(PathBuf, Request)>,
    selected_request: Option<usize>,
    request_path: Option<PathBuf>,
    request: Request,
    editor_tab: EditorTab,
    body_kind: BodyKind,
    body_text: String,
    graphql_query: String,
    graphql_variables: String,
    graphql_operation_name: String,
    assertion_json_text: Vec<String>,
    new_assertion_kind: AssertionKind,
    auth_kind: AuthKind,
    auth_primary: String,
    auth_secondary: String,
    api_key_location: ApiKeyLocation,
    response_tab: ResponseTab,
    response_search: String,
    response: Option<HttpResponse>,
    response_error: Option<String>,
    response_wrap: bool,
    pending: Option<Receiver<Result<HttpResponse, String>>>,
    pending_request: Option<Request>,
    sse_pending: Option<Receiver<Result<SseStreamUpdate, String>>>,
    sse_events: VecDeque<ReceivedSseEvent>,
    sse_status: Option<(u16, String)>,
    sse_content_type: Option<String>,
    sse_protocol: Option<String>,
    sse_url: Option<String>,
    sse_started: bool,
    sse_connected: bool,
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
        let (history, status_message) = match workspace.history(100) {
            Ok(history) => (history, "Ready — local workspace".to_owned()),
            Err(error) => (Vec::new(), format!("Ready — history unavailable: {error}")),
        };
        let engine =
            HttpEngine::new(&EngineOptions::default()).map_err(|error| error.to_string())?;
        let mut app = Self {
            workspace,
            engine,
            collections,
            environments,
            history,
            history_search: String::new(),
            selected_collection: 0,
            requests: Vec::new(),
            selected_request: None,
            request_path: None,
            request: Request::new("New request", "GET", "https://example.com"),
            editor_tab: EditorTab::Params,
            body_kind: BodyKind::None,
            body_text: String::new(),
            graphql_query: String::new(),
            graphql_variables: String::new(),
            graphql_operation_name: String::new(),
            assertion_json_text: Vec::new(),
            new_assertion_kind: AssertionKind::Status,
            auth_kind: AuthKind::None,
            auth_primary: String::new(),
            auth_secondary: String::new(),
            api_key_location: ApiKeyLocation::Header,
            response_tab: ResponseTab::Pretty,
            response_search: String::new(),
            response: None,
            response_error: None,
            response_wrap: false,
            pending: None,
            pending_request: None,
            sse_pending: None,
            sse_events: VecDeque::new(),
            sse_status: None,
            sse_content_type: None,
            sse_protocol: None,
            sse_url: None,
            sse_started: false,
            sse_connected: false,
            selected_environment: None,
            dirty: false,
            status_message,
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

    fn refresh_history(&mut self) {
        if let Ok(history) = self.workspace.history(100) {
            self.history = history;
        }
    }

    fn reopen_history(&mut self, entry: &HistoryEntry) -> Result<(), String> {
        let request_id = entry
            .request_id
            .ok_or_else(|| "This history entry predates request identity tracking.".to_owned())?;
        let mut found = None;
        for (collection_index, collection) in self.collections.iter().enumerate() {
            let requests = self
                .workspace
                .requests(collection)
                .map_err(|error| error.to_string())?;
            if let Some(request_index) = requests
                .iter()
                .position(|(_, request)| request.id == request_id)
            {
                found = Some((collection_index, requests, request_index));
                break;
            }
        }
        let Some((collection_index, requests, request_index)) = found else {
            return Err(format!(
                "Saved request for history entry not found: {}",
                entry.request_name
            ));
        };
        self.selected_collection = collection_index;
        self.requests = requests;
        self.select_request(request_index);
        self.status_message = "Reopened from local history".to_owned();
        Ok(())
    }

    fn load_request_editors(&mut self) {
        self.assertion_json_text = self
            .request
            .assertions
            .iter()
            .map(|assertion| match assertion {
                Assertion::JsonPointerEquals { expected, .. } => {
                    serde_json::to_string_pretty(expected).unwrap_or_else(|_| "null".to_owned())
                }
                _ => String::new(),
            })
            .collect();
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
            RequestBody::Graphql {
                query,
                variables,
                operation_name,
            } => {
                self.body_kind = BodyKind::Graphql;
                self.graphql_query.clone_from(query);
                self.graphql_variables =
                    serde_json::to_string_pretty(variables).unwrap_or_else(|_| "{}".to_owned());
                self.graphql_operation_name = operation_name.clone().unwrap_or_default();
                self.body_text.clear();
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
        self.response_search.clear();
        self.response_tab = ResponseTab::Pretty;
        self.sse_pending = None;
        self.sse_events.clear();
        self.sse_status = None;
        self.sse_content_type = None;
        self.sse_protocol = None;
        self.sse_url = None;
        self.sse_started = false;
        self.sse_connected = false;
    }

    fn save_current_response(&mut self) -> Result<(), String> {
        let response = self
            .response
            .as_ref()
            .ok_or_else(|| "no response to save".to_owned())?;
        let directory = self.workspace.root().join(".postly").join("responses");
        std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        let extension = if response
            .content_type
            .as_deref()
            .is_some_and(|content_type| content_type.contains("json"))
        {
            "json"
        } else {
            "txt"
        };
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_secs();
        let slug = response_file_slug(&self.request.name);
        let mut path = directory.join(format!("{slug}-{timestamp}.{extension}"));
        let mut suffix = 2_u32;
        while path.exists() {
            path = directory.join(format!("{slug}-{timestamp}-{suffix}.{extension}"));
            suffix = suffix.saturating_add(1);
        }
        std::fs::write(&path, response.body_text()).map_err(|error| error.to_string())?;
        self.status_message = format!("Response saved locally — {}", path.display());
        Ok(())
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
            BodyKind::Graphql => {
                postly_core::validate_graphql_query(&self.graphql_query)
                    .map_err(|error| format!("GraphQL query is invalid: {error}"))?;
                let variables = postly_core::parse_variables_json(&self.graphql_variables)
                    .map_err(|error| format!("GraphQL variables are invalid: {error}"))?;
                RequestBody::Graphql {
                    query: self.graphql_query.clone(),
                    variables,
                    operation_name: (!self.graphql_operation_name.trim().is_empty())
                        .then(|| self.graphql_operation_name.clone()),
                }
            }
            BodyKind::Advanced => request.body,
        };
        for (index, assertion) in request.assertions.iter_mut().enumerate() {
            if let Assertion::JsonPointerEquals { expected, .. } = assertion {
                let text = self
                    .assertion_json_text
                    .get(index)
                    .map(String::as_str)
                    .unwrap_or("null");
                *expected = serde_json::from_str(text)
                    .map_err(|error| format!("assertion JSON value is invalid: {error}"))?;
            }
        }
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
        let path = if let Some(path) = self.request_path.clone() {
            let collection = self
                .collections
                .get(self.selected_collection)
                .ok_or_else(|| "no collection selected".to_owned())?;
            self.workspace
                .relocate_request(&path, collection, &request)
                .map_err(|error| error.to_string())?
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

    fn duplicate_current(&mut self) -> Result<(), String> {
        let request = self.edited_request()?;
        let collection = self
            .collections
            .get(self.selected_collection)
            .ok_or_else(|| "no collection selected".to_owned())?;
        let path = self
            .workspace
            .duplicate_request(collection, &request)
            .map_err(|error| error.to_string())?;
        self.refresh_requests(Some(&path))?;
        self.dirty = false;
        self.status_message = format!("Duplicated locally — {}", path.display());
        Ok(())
    }

    fn delete_current(&mut self) -> Result<(), String> {
        let path = self
            .request_path
            .clone()
            .ok_or_else(|| "draft requests cannot be deleted".to_owned())?;
        self.workspace
            .delete_request(&path)
            .map_err(|error| error.to_string())?;
        self.selected_request = None;
        self.request_path = None;
        self.refresh_requests(None)?;
        self.status_message = "Request deleted locally".to_owned();
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
        if self.pending.is_some() || self.sse_pending.is_some() {
            return Ok(());
        }
        let request = self.edited_request()?;
        let context = self.context();
        let engine = self.engine.clone();
        let (sender, receiver) = mpsc::channel();
        let worker_request = request.clone();
        thread::spawn(move || {
            let result = (|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
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

    fn start_sse_current(&mut self) -> Result<(), String> {
        if self.pending.is_some() || self.sse_pending.is_some() {
            return Ok(());
        }
        let request = self.edited_request()?;
        let context = self.context();
        let engine = self.engine.clone();
        let (sender, receiver) = mpsc::channel();
        let error_sender = sender.clone();
        thread::spawn(move || {
            let result =
                (|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|error| error.to_string())?;
                    runtime.block_on(async move {
                        let mut request = request;
                        if !request.headers.iter().any(|header| {
                            header.enabled && header.key.eq_ignore_ascii_case("accept")
                        }) {
                            request
                                .headers
                                .push(HeaderEntry::enabled("accept", "text/event-stream"));
                        }
                        let mut response = engine
                            .execute_stream(&request, &context)
                            .await
                            .map_err(|error| error.to_string())?;
                        if response.status >= 400 {
                            let body = response.response.text().await.unwrap_or_default();
                            return Err(format!(
                                "SSE endpoint returned {} {}{}",
                                response.status,
                                response.status_text,
                                if body.trim().is_empty() {
                                    String::new()
                                } else {
                                    format!(": {}", body.trim())
                                }
                            ));
                        }
                        sender
                            .send(Ok(SseStreamUpdate::Connected {
                                status: response.status,
                                status_text: response.status_text.clone(),
                                content_type: response.content_type.clone(),
                                protocol: response.protocol.clone(),
                                url: response.url.clone(),
                            }))
                            .map_err(|_| "SSE console was closed".to_owned())?;
                        let mut parser = SseParser::default();
                        while let Some(chunk) = response
                            .response
                            .chunk()
                            .await
                            .map_err(|error| error.to_string())?
                        {
                            for event in parser
                                .feed_bytes(&chunk)
                                .map_err(|error| error.to_string())?
                            {
                                sender
                                    .send(Ok(SseStreamUpdate::Event(event)))
                                    .map_err(|_| "SSE console was closed".to_owned())?;
                            }
                        }
                        for event in parser.finish().map_err(|error| error.to_string())? {
                            sender
                                .send(Ok(SseStreamUpdate::Event(event)))
                                .map_err(|_| "SSE console was closed".to_owned())?;
                        }
                        let _ = sender.send(Ok(SseStreamUpdate::Closed));
                        Ok::<(), String>(())
                    })
                })();
            if let Err(error) = result {
                let _ = error_sender.send(Err(error));
            }
        });
        self.clear_response();
        self.sse_pending = Some(receiver);
        self.sse_started = true;
        self.status_message = "Connecting to SSE endpoint…".to_owned();
        Ok(())
    }

    fn poll_pending(&mut self) -> bool {
        let http_pending = self.poll_http_pending();
        let sse_pending = self.poll_sse_pending();
        http_pending || sse_pending
    }

    fn poll_http_pending(&mut self) -> bool {
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
                self.refresh_history();
            }
            Err(error) => {
                if let Some(request) = request {
                    let _ = self
                        .workspace
                        .record_history(&HistoryEntry::from_error(&request, 0));
                }
                self.status_message = "Request failed".to_owned();
                self.response_error = Some(error);
                self.refresh_history();
            }
        }
        false
    }

    fn poll_sse_pending(&mut self) -> bool {
        let mut finished = false;
        loop {
            let result = {
                let Some(receiver) = self.sse_pending.as_ref() else {
                    break;
                };
                receiver.try_recv()
            };
            match result {
                Ok(Ok(SseStreamUpdate::Connected {
                    status,
                    status_text,
                    content_type,
                    protocol,
                    url,
                })) => {
                    self.sse_status = Some((status, status_text.clone()));
                    self.sse_content_type = content_type;
                    self.sse_protocol = Some(protocol);
                    self.sse_url = Some(url);
                    self.sse_started = true;
                    self.sse_connected = true;
                    self.status_message = format!("SSE connected · {status} {status_text}");
                }
                Ok(Ok(SseStreamUpdate::Event(event))) => {
                    self.sse_started = true;
                    self.sse_events.push_back(ReceivedSseEvent {
                        event,
                        received_at: Local::now().format("%H:%M:%S").to_string(),
                    });
                    while self.sse_events.len() > MAX_SSE_EVENTS {
                        self.sse_events.pop_front();
                    }
                    self.status_message =
                        format!("SSE event received · {} retained", self.sse_events.len());
                }
                Ok(Ok(SseStreamUpdate::Closed)) => {
                    self.sse_connected = false;
                    self.status_message = format!(
                        "SSE stream closed · {} event{}",
                        self.sse_events.len(),
                        if self.sse_events.len() == 1 { "" } else { "s" }
                    );
                    finished = true;
                    break;
                }
                Ok(Err(error)) => {
                    self.sse_connected = false;
                    self.sse_started = true;
                    self.status_message = "SSE stream failed".to_owned();
                    self.response_error = Some(error);
                    finished = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.sse_connected = false;
                    self.status_message = "SSE stream worker stopped unexpectedly".to_owned();
                    self.response_error = Some("SSE stream worker stopped unexpectedly".to_owned());
                    finished = true;
                    break;
                }
            }
        }
        if finished {
            self.sse_pending = None;
        }
        self.sse_pending.is_some()
    }

    fn draw_navigator(&mut self, ui: &mut egui::Ui) {
        let mut collection_clicked = None;
        let mut request_clicked = None;
        let mut new_clicked = false;
        let mut environment_clicked = None;
        let mut history_clicked = None;
        let mut clear_history_clicked = false;
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
                    .max_height((ui.available_height() - 280.0).max(100.0))
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
                ui.horizontal(|ui| {
                    ui.label(RichText::new("HISTORY").small().strong().color(MUTED));
                    if ui.small_button("Clear").clicked() {
                        clear_history_clicked = true;
                    }
                });
                ui.add(
                    TextEdit::singleline(&mut self.history_search)
                        .hint_text("Search recent requests")
                        .desired_width(ui.available_width()),
                );
                let history_filter = HistoryFilter {
                    search: Some(self.history_search.clone()),
                    ..HistoryFilter::default()
                };
                egui::ScrollArea::vertical()
                    .id_salt("history-list")
                    .max_height(130.0)
                    .show(ui, |ui| {
                        for entry in self
                            .history
                            .iter()
                            .filter(|entry| history_filter.matches(entry))
                        {
                            let status = entry
                                .status
                                .map(|status| status.to_string())
                                .unwrap_or_else(|| "error".to_owned());
                            let label =
                                format!("{} {} · {}", entry.method, entry.request_name, status);
                            if ui
                                .selectable_label(false, RichText::new(label).color(MUTED))
                                .on_hover_text(&entry.url)
                                .clicked()
                            {
                                history_clicked = Some(entry.clone());
                            }
                        }
                    });
                ui.add_space(8.0);
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
        if clear_history_clicked {
            match self.workspace.clear_history() {
                Ok(()) => {
                    self.history.clear();
                    self.status_message = "Local history cleared".to_owned();
                }
                Err(error) => self.status_message = format!("History clear failed: {error}"),
            }
        }
        if let Some(entry) = history_clicked {
            if let Err(error) = self.reopen_history(&entry) {
                self.status_message = format!("History reopen failed: {error}");
            }
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
                let mut stream_clicked = false;
                let mut save_clicked = false;
                let mut duplicate_clicked = false;
                let mut delete_clicked = false;
                let busy = self.pending.is_some() || self.sse_pending.is_some();
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
                            !busy,
                            egui::Button::new(if busy { "Sending…" } else { "Send  ⌘↵" })
                                .fill(ACCENT),
                        )
                        .clicked()
                    {
                        send_clicked = true;
                    }
                    if ui.button("Save").clicked() {
                        save_clicked = true;
                    }
                    if ui
                        .add_enabled(self.request_path.is_some(), egui::Button::new("Duplicate"))
                        .clicked()
                    {
                        duplicate_clicked = true;
                    }
                    if ui
                        .add_enabled(self.request_path.is_some(), egui::Button::new("Delete"))
                        .clicked()
                    {
                        delete_clicked = true;
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
                        (EditorTab::Cookies, "Cookies"),
                        (EditorTab::Body, "Body"),
                        (EditorTab::Auth, "Auth"),
                        (EditorTab::Assertions, "Assertions"),
                    ] {
                        if tab_button(ui, self.editor_tab == tab, label).clicked() {
                            self.editor_tab = tab;
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(!busy, egui::Button::new("Stream SSE"))
                            .on_hover_text("Open the current request as a progressive SSE console")
                            .clicked()
                        {
                            stream_clicked = true;
                        }
                    });
                });
                ui.add_space(3.0);
                if save_clicked {
                    if let Err(error) = self.save_current() {
                        self.status_message = format!("Save failed: {error}");
                    }
                }
                if duplicate_clicked {
                    if let Err(error) = self.duplicate_current() {
                        self.status_message = format!("Duplicate failed: {error}");
                    }
                }
                if delete_clicked {
                    if let Err(error) = self.delete_current() {
                        self.status_message = format!("Delete failed: {error}");
                    }
                }
                if send_clicked {
                    if let Err(error) = self.send_current() {
                        self.status_message = format!("Send failed: {error}");
                    }
                }
                if stream_clicked {
                    if let Err(error) = self.start_sse_current() {
                        self.status_message = format!("SSE start failed: {error}");
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
                        self.dirty |= render_key_values(
                            ui,
                            &mut self.request.query,
                            "query",
                            "＋ Add parameter",
                        );
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
                    EditorTab::Cookies => {
                        ui.heading(RichText::new("Request cookies").color(Color32::WHITE));
                        ui.label(
                            RichText::new(
                                "Explicit cookies are sent for this request and override the automatic jar header.",
                            )
                            .small()
                            .color(MUTED),
                        );
                        ui.add_space(10.0);
                        self.dirty |= render_key_values(
                            ui,
                            &mut self.request.cookies,
                            "request-cookies",
                            "＋ Add cookie",
                        );
                    }
                    EditorTab::Body => self.render_body(ui),
                    EditorTab::Auth => self.render_auth(ui),
                    EditorTab::Assertions => self.render_assertions(ui),
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
                for kind in [
                    BodyKind::None,
                    BodyKind::Raw,
                    BodyKind::Json,
                    BodyKind::Graphql,
                ] {
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
            if self.body_kind == BodyKind::Graphql {
                if self.graphql_query.is_empty() {
                    self.graphql_query = "query Example {\n  field\n}".to_owned();
                }
                if self.graphql_variables.is_empty() {
                    self.graphql_variables = "{}".to_owned();
                }
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
            BodyKind::Graphql => {
                ui.label(
                    RichText::new(
                        "The query is sent as a standard JSON GraphQL envelope. Variables must be an object.",
                    )
                    .small()
                    .color(MUTED),
                );
                ui.add_space(8.0);
                ui.label(RichText::new("Query").strong().color(Color32::WHITE));
                if ui
                    .add(
                        TextEdit::multiline(&mut self.graphql_query)
                            .font(TextStyle::Monospace)
                            .desired_rows(10)
                            .desired_width(f32::INFINITY)
                            .hint_text("query Example {\n  field\n}"),
                    )
                    .changed()
                {
                    self.dirty = true;
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Operation name (optional)")
                        .strong()
                        .color(Color32::WHITE),
                );
                if ui
                    .add(
                        TextEdit::singleline(&mut self.graphql_operation_name).desired_width(320.0),
                    )
                    .changed()
                {
                    self.dirty = true;
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Variables (JSON object)")
                        .strong()
                        .color(Color32::WHITE),
                );
                if ui
                    .add(
                        TextEdit::multiline(&mut self.graphql_variables)
                            .font(TextStyle::Monospace)
                            .desired_rows(7)
                            .desired_width(f32::INFINITY)
                            .hint_text("{\n  \"id\": \"42\"\n}"),
                    )
                    .changed()
                {
                    self.dirty = true;
                }
            }
            BodyKind::Advanced => {
                ui.label(
                    RichText::new(
                        "This imported body is preserved by the core model. Its dedicated editor is next.",
                    )
                    .color(MUTED),
                );
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

    fn render_assertions(&mut self, ui: &mut egui::Ui) {
        ui.heading(RichText::new("Response assertions").color(Color32::WHITE));
        ui.label(
            RichText::new(
                "These local checks run in the collection runner and do not require Node.js.",
            )
            .small()
            .color(MUTED),
        );
        ui.add_space(8.0);
        let mut changed = false;
        let mut remove = None;
        for (index, assertion) in self.request.assertions.iter_mut().enumerate() {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("Assertion {}", index + 1))
                            .strong()
                            .color(Color32::WHITE),
                    );
                    if ui.small_button("Remove").clicked() {
                        remove = Some(index);
                    }
                });
                match assertion {
                    Assertion::Status { expected } => {
                        ui.horizontal(|ui| {
                            ui.label("Expected status");
                            changed |= ui.add(egui::DragValue::new(expected)).changed();
                        });
                    }
                    Assertion::HeaderPresent { name } => {
                        changed |= labeled_singleline(ui, "Header name", name);
                    }
                    Assertion::HeaderEquals { name, expected } => {
                        changed |= labeled_singleline(ui, "Header name", name);
                        changed |= labeled_singleline(ui, "Expected value", expected);
                    }
                    Assertion::BodyContains { value } => {
                        changed |= labeled_singleline(ui, "Text", value);
                    }
                    Assertion::JsonPointerEquals { pointer, .. } => {
                        changed |= labeled_singleline(ui, "JSON Pointer", pointer);
                        if let Some(value) = self.assertion_json_text.get_mut(index) {
                            ui.label("Expected JSON value");
                            if ui
                                .add(
                                    TextEdit::multiline(value)
                                        .font(TextStyle::Monospace)
                                        .desired_rows(3)
                                        .desired_width(f32::INFINITY),
                                )
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    }
                }
            });
            ui.add_space(5.0);
        }
        if let Some(index) = remove {
            self.request.assertions.remove(index);
            if index < self.assertion_json_text.len() {
                self.assertion_json_text.remove(index);
            }
            changed = true;
        }
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("new-assertion-kind")
                .selected_text(self.new_assertion_kind.label())
                .show_ui(ui, |ui| {
                    for kind in [
                        AssertionKind::Status,
                        AssertionKind::HeaderPresent,
                        AssertionKind::HeaderEquals,
                        AssertionKind::BodyContains,
                        AssertionKind::JsonPointerEquals,
                    ] {
                        ui.selectable_value(&mut self.new_assertion_kind, kind, kind.label());
                    }
                });
            if ui.button("＋ Add assertion").clicked() {
                let assertion = self.new_assertion_kind.default_assertion();
                self.assertion_json_text.push(match &assertion {
                    Assertion::JsonPointerEquals { expected, .. } => {
                        serde_json::to_string_pretty(expected).unwrap_or_else(|_| "null".to_owned())
                    }
                    _ => String::new(),
                });
                self.request.assertions.push(assertion);
                changed = true;
            }
        });
        if changed {
            self.dirty = true;
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
                    if self.sse_started {
                        let state = if self.sse_connected {
                            "connected"
                        } else {
                            "closed"
                        };
                        let status = self
                            .sse_status
                            .as_ref()
                            .map(|(code, text)| format!("{code} {text} · "))
                            .unwrap_or_default();
                        ui.label(
                            RichText::new(format!(
                                "SSE {state} · {status}{} event{}",
                                self.sse_events.len(),
                                if self.sse_events.len() == 1 { "" } else { "s" }
                            ))
                            .color(if self.sse_connected {
                                Color32::from_rgb(100, 205, 145)
                            } else {
                                MUTED
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
                    if self.sse_started
                        && tab_button(ui, self.response_tab == ResponseTab::SseEvents, "Events")
                            .clicked()
                    {
                        self.response_tab = ResponseTab::SseEvents;
                    }
                });
                if self.response.is_some()
                    && matches!(self.response_tab, ResponseTab::Pretty | ResponseTab::Raw)
                {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Search").small().color(MUTED));
                        ui.add(
                            TextEdit::singleline(&mut self.response_search)
                                .hint_text("find in response")
                                .desired_width(220.0),
                        );
                        if ui.button("Copy").clicked() {
                            if let Some(response) = &self.response {
                                let view = if self.response_tab == ResponseTab::Pretty {
                                    ResponseView::Pretty
                                } else {
                                    ResponseView::Raw
                                };
                                ui.ctx().copy_text(response.formatted_body(view));
                                self.status_message = "Response copied to clipboard".to_owned();
                            }
                        }
                        if ui.button("Save response").clicked() {
                            if let Err(error) = self.save_current_response() {
                                self.status_message = format!("Save failed: {error}");
                            }
                        }
                        ui.checkbox(&mut self.response_wrap, "Wrap");
                    });
                }
                ui.separator();
                if let Some(error) = &self.response_error {
                    ui.colored_label(Color32::from_rgb(240, 125, 105), error);
                    if self.sse_started {
                        self.render_sse_content(ui);
                    }
                } else if let Some(response) = &self.response {
                    self.render_response_content(ui, response);
                } else if self.sse_started {
                    self.render_sse_content(ui);
                } else if self.pending.is_some() {
                    ui.label(RichText::new("Waiting for the local HTTP engine…").color(MUTED));
                } else {
                    ui.label(
                        RichText::new("Send a request to inspect its response here.").color(MUTED),
                    );
                }
            });
    }

    fn render_sse_content(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(content_type) = &self.sse_content_type {
                ui.label(RichText::new(content_type).small().color(MUTED));
            }
            if let Some(protocol) = &self.sse_protocol {
                ui.label(RichText::new(protocol).small().color(MUTED));
            }
            if let Some(url) = &self.sse_url {
                ui.label(RichText::new(url).small().color(MUTED));
            }
        });
        if self.sse_events.is_empty() {
            ui.label(RichText::new("Waiting for SSE events…").color(MUTED));
            return;
        }
        ui.add_space(5.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (index, received) in self.sse_events.iter().enumerate() {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("#{}", index + 1))
                                    .strong()
                                    .color(ACCENT),
                            );
                            ui.label(RichText::new(&received.received_at).small().color(MUTED));
                            if let Some(event) = &received.event.event {
                                ui.label(RichText::new(format!("event: {event}")));
                            } else {
                                ui.label(RichText::new("message").color(MUTED));
                            }
                            if let Some(id) = &received.event.id {
                                ui.label(RichText::new(format!("id: {id}")));
                            }
                            if let Some(retry_ms) = received.event.retry_ms {
                                ui.label(RichText::new(format!("retry: {retry_ms} ms")));
                            }
                        });
                        ui.add_space(3.0);
                        ui.add(
                            egui::Label::new(RichText::new(&received.event.data).monospace())
                                .wrap(),
                        );
                    });
                    ui.add_space(4.0);
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
                let text = response.formatted_body(view);
                if !self.response_search.trim().is_empty() {
                    let total = response_search_matches(&text, &self.response_search);
                    let lines = response_search_lines(&text, &self.response_search);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!(
                                "{} match{}",
                                total,
                                if total == 1 { "" } else { "es" }
                            ))
                            .small()
                            .color(MUTED),
                        );
                        if lines.len() == 50 {
                            ui.label(
                                RichText::new("showing the first 50 matching lines")
                                    .small()
                                    .color(MUTED),
                            );
                        }
                    });
                    egui::ScrollArea::vertical()
                        .max_height(96.0)
                        .show(ui, |ui| {
                            for (line_number, line) in lines {
                                ui.monospace(format!("{line_number}: {line}"));
                            }
                        });
                }
                let lines = text.lines().collect::<Vec<_>>();
                let line_count = lines.len().max(1);
                let line_height = ui.text_style_height(&TextStyle::Monospace);
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .show_rows(ui, line_height, line_count, |ui, row_range| {
                        for index in row_range {
                            ui.horizontal(|ui| {
                                ui.add_sized(
                                    [52.0, line_height],
                                    egui::Label::new(
                                        RichText::new(format!("{:>5}", index + 1))
                                            .monospace()
                                            .color(MUTED),
                                    ),
                                );
                                let line = lines.get(index).copied().unwrap_or_default();
                                let label = egui::Label::new(RichText::new(line).monospace());
                                if self.response_wrap {
                                    ui.add(label.wrap());
                                } else {
                                    ui.add(label);
                                }
                            });
                        }
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
                if response.cookies.is_empty() {
                    ui.label(RichText::new("No Set-Cookie headers in this response.").color(MUTED));
                } else {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        egui::Grid::new("response-cookies")
                            .striped(true)
                            .show(ui, |ui| {
                                ui.label(RichText::new("Cookie").strong());
                                ui.label(RichText::new("Attributes").strong());
                                ui.end_row();
                                for cookie in &response.cookies {
                                    ui.label(format!("{} = {}", cookie.name, cookie.value));
                                    let mut attributes = Vec::new();
                                    if let Some(domain) = &cookie.domain {
                                        attributes.push(format!("Domain={domain}"));
                                    }
                                    if let Some(path) = &cookie.path {
                                        attributes.push(format!("Path={path}"));
                                    }
                                    if cookie.secure {
                                        attributes.push("Secure".to_owned());
                                    }
                                    if cookie.http_only {
                                        attributes.push("HttpOnly".to_owned());
                                    }
                                    if let Some(same_site) = &cookie.same_site {
                                        attributes.push(format!("SameSite={same_site}"));
                                    }
                                    if let Some(expires) = &cookie.expires {
                                        attributes.push(format!("Expires={expires}"));
                                    }
                                    if let Some(max_age) = cookie.max_age_seconds {
                                        attributes.push(format!("Max-Age={max_age}"));
                                    }
                                    ui.label(if attributes.is_empty() {
                                        "session cookie".to_owned()
                                    } else {
                                        attributes.join("; ")
                                    });
                                    ui.end_row();
                                }
                            });
                    });
                }
            }
            ResponseTab::Timing => {
                ui.label(format!("Total duration: {} ms", response.duration_ms));
                ui.label(format!("Protocol: {}", response.protocol));
                ui.label(format!("Response size: {} bytes", response.body.len()));
                ui.label(format!("Final URL: {}", response.url));
            }
            ResponseTab::SseEvents => self.render_sse_content(ui),
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

fn response_search_matches(text: &str, query: &str) -> usize {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return 0;
    }
    text.lines()
        .map(|line| line.to_lowercase().match_indices(&query).count())
        .sum()
}

fn response_search_lines(text: &str, query: &str) -> Vec<(usize, String)> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    text.lines()
        .enumerate()
        .filter(|(_, line)| line.to_lowercase().contains(&query))
        .take(50)
        .map(|(line_number, line)| (line_number + 1, line.to_owned()))
        .collect()
}

fn labeled_singleline(ui: &mut egui::Ui, label: &str, value: &mut String) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        changed = ui
            .add(TextEdit::singleline(value).desired_width(360.0))
            .changed();
    });
    changed
}

fn response_file_slug(value: &str) -> String {
    let slug = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(48)
        .collect::<String>();
    if slug.is_empty() {
        "response".to_owned()
    } else {
        slug
    }
}

fn render_key_values(
    ui: &mut egui::Ui,
    values: &mut Vec<KeyValue>,
    id: &str,
    add_label: &str,
) -> bool {
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
    if ui.button(add_label).clicked() {
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
    fn graphql_editor_round_trips_query_variables_and_operation_name() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.request.body = RequestBody::Graphql {
            query: "query User($id: ID!) { user(id: $id) { id } }".to_owned(),
            variables: serde_json::json!({"id": "42"}),
            operation_name: Some("User".to_owned()),
        };
        app.load_request_editors();

        assert_eq!(app.body_kind, BodyKind::Graphql);
        app.graphql_query = "query User($id: ID!) { user(id: $id) { name } }".to_owned();
        app.graphql_variables = r#"{"id":"43"}"#.to_owned();
        let request = app.edited_request().expect("valid GraphQL editor state");

        assert_eq!(
            request.body,
            RequestBody::Graphql {
                query: "query User($id: ID!) { user(id: $id) { name } }".to_owned(),
                variables: serde_json::json!({"id": "43"}),
                operation_name: Some("User".to_owned()),
            }
        );
    }

    #[test]
    fn graphql_editor_rejects_non_object_variables_before_network_work() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.body_kind = BodyKind::Graphql;
        app.graphql_query = "query Example { field }".to_owned();
        app.graphql_variables = "[1, 2]".to_owned();

        let error = app
            .edited_request()
            .expect_err("GraphQL variables must be an object");
        assert!(error.contains("GraphQL variables are invalid"));
    }

    #[test]
    fn response_can_be_saved_to_ignored_local_artifacts() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.request.name = "Saved users / response".to_owned();
        app.response = Some(HttpResponse {
            status: 200,
            status_text: "OK".to_owned(),
            headers: vec![HeaderEntry::enabled("content-type", "application/json")],
            body: br#"{"ok":true}"#.to_vec(),
            content_type: Some("application/json".to_owned()),
            duration_ms: 4,
            protocol: "HTTP/1.1".to_owned(),
            url: "https://example.test/users".to_owned(),
            cookies: Vec::new(),
        });

        app.save_current_response().expect("save response");
        let response_directory = directory.path().join(".postly/responses");
        let entries = std::fs::read_dir(response_directory)
            .expect("response directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("response entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(
            std::fs::read_to_string(entries[0].path()).expect("saved response"),
            r#"{"ok":true}"#
        );
    }

    #[test]
    fn saved_requests_can_be_duplicated_and_deleted_from_the_workspace() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.request.name = "List users".to_owned();
        app.save_current().expect("save request");
        app.duplicate_current().expect("duplicate request");
        assert_eq!(app.request.name, "List users copy");
        assert_eq!(app.requests.len(), 2);

        app.delete_current().expect("delete duplicate");
        assert_eq!(app.requests.len(), 1);
        assert_eq!(app.requests[0].1.name, "List users");
    }

    #[test]
    fn saving_a_request_name_or_folder_relocates_its_file() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.request.name = "List users".to_owned();
        app.save_current().expect("save request");
        let original_path = app.request_path.clone().expect("original path");

        app.request.name = "List all users".to_owned();
        app.request.folder = Some("Users / Read".to_owned());
        app.save_current().expect("relocate request");

        let relocated_path = app.request_path.clone().expect("relocated path");
        assert_ne!(relocated_path, original_path);
        assert!(!original_path.exists());
        assert!(relocated_path.to_string_lossy().contains("users/read"));
        assert_eq!(app.requests.len(), 1);
        assert_eq!(app.requests[0].1.name, "List all users");
    }

    #[test]
    fn response_search_is_case_insensitive_and_reports_occurrences() {
        let body = "{\n  \"Name\": \"Ada\",\n  \"name\": \"Grace\"\n}";
        assert_eq!(response_search_matches(body, "name"), 2);
        assert_eq!(
            response_search_lines(body, "ADA"),
            vec![(2, "  \"Name\": \"Ada\",".to_owned())]
        );
        assert!(response_search_lines(body, "missing").is_empty());
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
        app.save_current().expect("save request");
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
        let history_entry = app.history.first().cloned().expect("history entry");
        app.new_request();
        app.reopen_history(&history_entry).expect("reopen history");
        assert!(app.request_path.is_some());
        assert_eq!(
            app.request.id,
            history_entry.request_id.expect("request id")
        );
    }

    #[test]
    fn sse_worker_delivers_events_and_bounds_console_history() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("connection");
            let mut request = [0_u8; 1024];
            let length = stream.read(&mut request).expect("read");
            assert!(String::from_utf8_lossy(&request[..length]).contains("GET /events HTTP/1.1"));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\nid: 1\nevent: update\ndata: first\nretry: 1500\n\nid: 2\ndata: second\n\n",
                )
                .expect("write");
        });
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.request.url = format!("http://{address}/events");
        app.start_sse_current().expect("start SSE");

        let mut finished = false;
        for _ in 0..200 {
            if !app.poll_pending() {
                finished = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        server.join().expect("server");
        assert!(finished, "SSE worker did not finish");
        assert_eq!(app.sse_events.len(), 2);
        assert_eq!(app.sse_events[0].event.id.as_deref(), Some("1"));
        assert_eq!(app.sse_events[0].event.event.as_deref(), Some("update"));
        assert_eq!(app.sse_events[0].event.data, "first");
        assert_eq!(app.sse_events[0].event.retry_ms, Some(1500));
        assert!(!app.sse_events[0].received_at.is_empty());
        assert_eq!(
            app.sse_status.as_ref().map(|(status, _)| *status),
            Some(200)
        );
        assert!(!app.sse_connected);

        for index in 0..(MAX_SSE_EVENTS + 25) {
            app.sse_events.push_back(ReceivedSseEvent {
                event: SseEvent {
                    id: Some(index.to_string()),
                    event: None,
                    data: index.to_string(),
                    retry_ms: None,
                },
                received_at: "00:00:00".to_owned(),
            });
            while app.sse_events.len() > MAX_SSE_EVENTS {
                app.sse_events.pop_front();
            }
        }
        assert_eq!(app.sse_events.len(), MAX_SSE_EVENTS);
        assert_eq!(
            app.sse_events
                .front()
                .map(|event| event.event.id.as_deref()),
            Some(Some("25"))
        );
    }
}
