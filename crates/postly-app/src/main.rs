use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use base64::Engine;
use chrono::Local;
use eframe::egui::{self, Color32, RichText, TextEdit, TextStyle};
use futures_util::{SinkExt, StreamExt};
use postly_core::{
    export_curl_command, message_from_json, message_to_json, parse_curl_command,
    parse_graphql_response, parse_graphql_schema, run_script, schema_introspection_query,
    ApiKeyLocation, Assertion, Auth, CancellationToken, CollectionFiles, EngineOptions,
    Environment, EnvironmentVariable, GraphqlSchema, GrpcRequest, GrpcSchema, HeaderEntry,
    HistoryEntry, HistoryFilter, HttpEngine, HttpResponse, KeyValue, MultipartPart, Request,
    RequestBody, RequestSearchResult, ResponseView, ScriptResult, SecretStore, SseEvent, SseParser,
    VariableContext, Workspace,
};
use prost::Message as ProstMessage;
use prost_reflect::{DynamicMessage, MessageDescriptor};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest,
        http::{header::HeaderName, HeaderValue},
        Message,
    },
};
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint, Identity};

const ACCENT: Color32 = Color32::from_rgb(91, 141, 239);
const MUTED: Color32 = Color32::from_rgb(145, 157, 177);
const PANEL: Color32 = Color32::from_rgb(24, 29, 39);
const SURFACE: Color32 = Color32::from_rgb(31, 37, 49);

#[derive(Clone)]
struct DynamicGrpcCodec {
    output: MessageDescriptor,
}

struct DynamicGrpcEncoder;

struct DynamicGrpcDecoder {
    output: MessageDescriptor,
}

impl tonic::codec::Codec for DynamicGrpcCodec {
    type Encode = DynamicMessage;
    type Decode = DynamicMessage;
    type Encoder = DynamicGrpcEncoder;
    type Decoder = DynamicGrpcDecoder;

    fn encoder(&mut self) -> Self::Encoder {
        DynamicGrpcEncoder
    }

    fn decoder(&mut self) -> Self::Decoder {
        DynamicGrpcDecoder {
            output: self.output.clone(),
        }
    }
}

impl tonic::codec::Encoder for DynamicGrpcEncoder {
    type Item = DynamicMessage;
    type Error = tonic::Status;

    fn encode(
        &mut self,
        item: Self::Item,
        dst: &mut tonic::codec::EncodeBuf<'_>,
    ) -> Result<(), Self::Error> {
        ProstMessage::encode(&item, dst).map_err(|error| {
            tonic::Status::internal(format!("could not encode protobuf message: {error}"))
        })
    }
}

impl tonic::codec::Decoder for DynamicGrpcDecoder {
    type Item = DynamicMessage;
    type Error = tonic::Status;

    fn decode(
        &mut self,
        src: &mut tonic::codec::DecodeBuf<'_>,
    ) -> Result<Option<Self::Item>, Self::Error> {
        DynamicMessage::decode(self.output.clone(), src)
            .map(Some)
            .map_err(|error| {
                tonic::Status::internal(format!("could not decode protobuf message: {error}"))
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorTab {
    Params,
    Headers,
    Cookies,
    Body,
    Grpc,
    Auth,
    Scripts,
    Assertions,
    Transport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseTab {
    Pretty,
    Raw,
    Headers,
    Cookies,
    Timing,
    SseEvents,
    WebSocket,
    GraphqlSchema,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScriptRunKind {
    PreRequest,
    Tests,
}

impl ScriptRunKind {
    fn label(self) -> &'static str {
        match self {
            Self::PreRequest => "pre-request",
            Self::Tests => "post-response tests",
        }
    }
}

#[derive(Debug)]
struct ScriptRunReport {
    kind: ScriptRunKind,
    result: ScriptResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandPaletteAction {
    NewRequest,
    NewGrpcRequest,
    SaveRequest,
    SendRequest,
    CancelOperation,
    ClearResponse,
    ToggleResponseWrap,
    ImportCurl,
}

impl CommandPaletteAction {
    fn label(self) -> &'static str {
        match self {
            Self::NewRequest => "New request",
            Self::NewGrpcRequest => "New gRPC request",
            Self::SaveRequest => "Save current request",
            Self::SendRequest => "Send current request",
            Self::CancelOperation => "Cancel active operation",
            Self::ClearResponse => "Clear response",
            Self::ToggleResponseWrap => "Toggle response wrapping",
            Self::ImportCurl => "Import cURL command",
        }
    }

    fn shortcut(self) -> &'static str {
        match self {
            Self::NewRequest => "⌘N",
            Self::NewGrpcRequest => "",
            Self::SaveRequest => "⌘S",
            Self::SendRequest => "⌘↵",
            Self::CancelOperation => "Esc",
            Self::ClearResponse | Self::ToggleResponseWrap => "",
            Self::ImportCurl => "",
        }
    }
}

const MAX_CONSOLE_ITEMS: usize = 500;

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
    Reconnecting {
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        last_event_id: Option<String>,
    },
    Event(SseEvent),
    Closed,
}

#[derive(Debug, Clone, Copy)]
enum WebSocketDirection {
    Sent,
    Received,
}

#[derive(Debug, Clone)]
struct ReceivedWebSocketMessage {
    direction: WebSocketDirection,
    kind: String,
    data: String,
    received_at: String,
}

#[derive(Debug)]
enum WebSocketStreamUpdate {
    Connected {
        url: String,
    },
    Message {
        direction: WebSocketDirection,
        kind: String,
        data: String,
    },
    Closed,
}

#[derive(Debug)]
enum WebSocketCommand {
    SendText(String),
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyKind {
    None,
    Raw,
    Json,
    Graphql,
    FormUrlEncoded,
    Multipart,
    BinaryFile,
    Advanced,
}

impl BodyKind {
    fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Raw => "Raw text",
            Self::Json => "JSON",
            Self::Graphql => "GraphQL",
            Self::FormUrlEncoded => "Form URL encoded",
            Self::Multipart => "Multipart form-data",
            Self::BinaryFile => "Binary file",
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
    OAuth2ClientCredentials,
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
            Self::OAuth2ClientCredentials => "OAuth 2.0 client credentials",
        }
    }
}

const GUI_SETTINGS_FILE: &str = ".postly/gui-settings.json";
const GUI_TABS_FILE: &str = ".postly/gui-tabs.json";
const RECOVERY_FILE: &str = ".postly/recovery.json";
const RECOVERY_VERSION: u8 = 1;
const MAX_RECOVERY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
struct TransportSettings {
    timeout_seconds: u64,
    proxy_url: String,
    ca_cert_path: String,
    client_identity_path: String,
    insecure_tls: bool,
}

impl Default for TransportSettings {
    fn default() -> Self {
        Self {
            timeout_seconds: 30,
            proxy_url: String::new(),
            ca_cert_path: String::new(),
            client_identity_path: String::new(),
            insecure_tls: false,
        }
    }
}

impl TransportSettings {
    fn load(root: &Path) -> Self {
        fs::read_to_string(root.join(GUI_SETTINGS_FILE))
            .ok()
            .and_then(|contents| serde_json::from_str(&contents).ok())
            .unwrap_or_default()
    }

    fn save(&self, root: &Path) -> Result<(), String> {
        let path = root.join(GUI_SETTINGS_FILE);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let contents = serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?;
        fs::write(path, contents).map_err(|error| error.to_string())
    }

    fn engine_options(&self, root: &Path) -> EngineOptions {
        let path = |value: &str| (!value.trim().is_empty()).then(|| PathBuf::from(value.trim()));
        EngineOptions {
            timeout: Duration::from_secs(self.timeout_seconds.max(1)),
            accept_invalid_certs: self.insecure_tls,
            proxy: (!self.proxy_url.trim().is_empty()).then(|| self.proxy_url.trim().to_owned()),
            ca_cert: path(&self.ca_cert_path),
            client_identity: path(&self.client_identity_path),
            cookie_jar: Some(root.join(".postly/cookies.json")),
            ..EngineOptions::default()
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct RecoverySnapshot {
    version: u8,
    saved_at_unix: u64,
    collection_id: String,
    collection_name: String,
    request: Request,
}

#[derive(Debug, Clone)]
struct EnvironmentVariableDraft {
    key: String,
    value: String,
    enabled: bool,
    secret: bool,
    secret_ref: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
struct TabsSettings {
    paths: Vec<PathBuf>,
    active_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct RequestTab {
    collection_index: usize,
    request_path: Option<PathBuf>,
    request: Request,
    dirty: bool,
}

fn recovery_path(root: &Path) -> PathBuf {
    root.join(RECOVERY_FILE)
}

fn write_recovery_snapshot(root: &Path, snapshot: &RecoverySnapshot) -> Result<(), String> {
    let path = recovery_path(root);
    let parent = path
        .parent()
        .ok_or_else(|| "recovery path has no parent directory".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let contents = serde_json::to_vec_pretty(snapshot).map_err(|error| error.to_string())?;
    if contents.len() > MAX_RECOVERY_BYTES {
        return Err(format!(
            "recovery snapshot exceeds the {} MiB safety limit",
            MAX_RECOVERY_BYTES / (1024 * 1024)
        ));
    }
    let temporary = path.with_file_name(format!(".recovery-{}.json.tmp", std::process::id()));
    fs::write(&temporary, contents).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&temporary)
            .map_err(|error| error.to_string())?
            .permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&temporary, permissions).map_err(|error| error.to_string())?;
    }
    fs::rename(&temporary, &path).map_err(|error| error.to_string())
}

fn read_recovery_snapshot(root: &Path) -> Result<Option<RecoverySnapshot>, String> {
    let path = recovery_path(root);
    let contents = match fs::read(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if contents.len() > MAX_RECOVERY_BYTES {
        return Err(format!(
            "recovery snapshot is larger than the {} MiB safety limit",
            MAX_RECOVERY_BYTES / (1024 * 1024)
        ));
    }
    let snapshot: RecoverySnapshot =
        serde_json::from_slice(&contents).map_err(|error| error.to_string())?;
    if snapshot.version != RECOVERY_VERSION {
        return Err(format!(
            "unsupported recovery snapshot version {}",
            snapshot.version
        ));
    }
    Ok(Some(snapshot))
}

fn remove_recovery_snapshot(root: &Path) -> Result<(), String> {
    match fs::remove_file(recovery_path(root)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

pub struct PostlyApp {
    workspace: Workspace,
    engine: HttpEngine,
    collections: Vec<CollectionFiles>,
    environments: Vec<(PathBuf, Environment)>,
    open_tabs: Vec<RequestTab>,
    active_tab: usize,
    history: Vec<HistoryEntry>,
    history_search: String,
    workspace_search: String,
    workspace_search_results: Vec<RequestSearchResult>,
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
    grpc_proto_path: String,
    grpc_reflection: bool,
    grpc_reflection_host: String,
    grpc_includes_text: String,
    grpc_method: String,
    grpc_metadata: Vec<KeyValue>,
    graphql_schema: Option<GraphqlSchema>,
    graphql_schema_search: String,
    graphql_schema_error: Option<String>,
    pre_request_script: String,
    test_script: String,
    assertion_json_text: Vec<String>,
    new_assertion_kind: AssertionKind,
    auth_kind: AuthKind,
    auth_primary: String,
    auth_secondary: String,
    auth_tertiary: String,
    auth_quaternary: String,
    api_key_location: ApiKeyLocation,
    response_tab: ResponseTab,
    response_search: String,
    response: Option<HttpResponse>,
    response_error: Option<String>,
    response_wrap: bool,
    pending: Option<Receiver<Result<HttpResponse, String>>>,
    pending_request: Option<Request>,
    pending_graphql_schema: bool,
    pending_grpc: bool,
    pending_cancellation: Option<CancellationToken>,
    script_pending: Option<Receiver<Result<ScriptRunReport, String>>>,
    script_report: Option<ScriptRunReport>,
    script_error: Option<String>,
    sse_pending: Option<Receiver<Result<SseStreamUpdate, String>>>,
    sse_cancellation: Option<CancellationToken>,
    sse_events: VecDeque<ReceivedSseEvent>,
    sse_status: Option<(u16, String)>,
    sse_content_type: Option<String>,
    sse_protocol: Option<String>,
    sse_url: Option<String>,
    sse_reconnect_limit: u32,
    sse_started: bool,
    sse_connected: bool,
    websocket_pending: Option<Receiver<Result<WebSocketStreamUpdate, String>>>,
    websocket_cancellation: Option<CancellationToken>,
    websocket_commands: Option<tokio::sync::mpsc::UnboundedSender<WebSocketCommand>>,
    websocket_messages: VecDeque<ReceivedWebSocketMessage>,
    websocket_input: String,
    websocket_url: Option<String>,
    websocket_started: bool,
    websocket_connected: bool,
    selected_environment: Option<String>,
    transport: TransportSettings,
    transport_settings_dirty: bool,
    command_palette_open: bool,
    command_palette_query: String,
    command_palette_selected: usize,
    curl_import_open: bool,
    curl_import_text: String,
    curl_import_error: Option<String>,
    environment_editor_open: bool,
    environment_editor_path: Option<PathBuf>,
    environment_editor_name: String,
    environment_editor_variables: Vec<EnvironmentVariableDraft>,
    environment_editor_error: Option<String>,
    tabs_settings_dirty: bool,
    dirty: bool,
    recovery_restored: bool,
    recovery_last_saved: Option<Instant>,
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
        let transport = TransportSettings::load(workspace.root());
        let engine =
            HttpEngine::new(&EngineOptions::default()).map_err(|error| error.to_string())?;
        let mut app = Self {
            workspace,
            engine,
            collections,
            environments,
            open_tabs: Vec::new(),
            active_tab: 0,
            history,
            history_search: String::new(),
            workspace_search: String::new(),
            workspace_search_results: Vec::new(),
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
            grpc_proto_path: String::new(),
            grpc_reflection: false,
            grpc_reflection_host: String::new(),
            grpc_includes_text: String::new(),
            grpc_method: String::new(),
            grpc_metadata: Vec::new(),
            graphql_schema: None,
            graphql_schema_search: String::new(),
            graphql_schema_error: None,
            pre_request_script: String::new(),
            test_script: String::new(),
            assertion_json_text: Vec::new(),
            new_assertion_kind: AssertionKind::Status,
            auth_kind: AuthKind::None,
            auth_primary: String::new(),
            auth_secondary: String::new(),
            auth_tertiary: String::new(),
            auth_quaternary: String::new(),
            api_key_location: ApiKeyLocation::Header,
            response_tab: ResponseTab::Pretty,
            response_search: String::new(),
            response: None,
            response_error: None,
            response_wrap: false,
            pending: None,
            pending_request: None,
            pending_graphql_schema: false,
            pending_grpc: false,
            pending_cancellation: None,
            script_pending: None,
            script_report: None,
            script_error: None,
            sse_pending: None,
            sse_cancellation: None,
            sse_events: VecDeque::new(),
            sse_status: None,
            sse_content_type: None,
            sse_protocol: None,
            sse_url: None,
            sse_reconnect_limit: 0,
            sse_started: false,
            sse_connected: false,
            websocket_pending: None,
            websocket_cancellation: None,
            websocket_commands: None,
            websocket_messages: VecDeque::new(),
            websocket_input: String::new(),
            websocket_url: None,
            websocket_started: false,
            websocket_connected: false,
            selected_environment: None,
            transport,
            transport_settings_dirty: false,
            command_palette_open: false,
            command_palette_query: String::new(),
            command_palette_selected: 0,
            curl_import_open: false,
            curl_import_text: String::new(),
            curl_import_error: None,
            environment_editor_open: false,
            environment_editor_path: None,
            environment_editor_name: String::new(),
            environment_editor_variables: Vec::new(),
            environment_editor_error: None,
            tabs_settings_dirty: false,
            dirty: false,
            recovery_restored: false,
            recovery_last_saved: None,
            status_message,
        };
        app.refresh_requests(None)?;
        app.restore_tabs();
        match read_recovery_snapshot(app.workspace.root()) {
            Ok(Some(snapshot)) => app.restore_recovery(snapshot)?,
            Ok(None) => {}
            Err(error) => {
                app.status_message = format!("Recovery snapshot ignored: {error}");
            }
        }
        Ok(app)
    }

    fn restore_recovery(&mut self, snapshot: RecoverySnapshot) -> Result<(), String> {
        if let Some(index) = self
            .collections
            .iter()
            .position(|collection| collection.collection.id.to_string() == snapshot.collection_id)
        {
            self.selected_collection = index;
            self.requests = self
                .workspace
                .requests(&self.collections[index])
                .map_err(|error| error.to_string())?;
        }
        self.selected_request = None;
        self.request_path = None;
        self.request = snapshot.request;
        self.load_request_editors();
        self.clear_response();
        self.dirty = true;
        self.recovery_restored = true;
        self.recovery_last_saved = Some(Instant::now());
        self.status_message = format!(
            "Recovered unsaved draft from {} — save it or discard recovery",
            snapshot.collection_name
        );
        Ok(())
    }

    fn persist_recovery(&mut self) -> Result<(), String> {
        let collection = self
            .collections
            .get(self.selected_collection)
            .ok_or_else(|| "no collection selected".to_owned())?;
        let request = self.edited_request()?;
        let saved_at_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_secs();
        let snapshot = RecoverySnapshot {
            version: RECOVERY_VERSION,
            saved_at_unix,
            collection_id: collection.collection.id.to_string(),
            collection_name: collection.collection.name.clone(),
            request,
        };
        write_recovery_snapshot(self.workspace.root(), &snapshot)?;
        self.recovery_last_saved = Some(Instant::now());
        Ok(())
    }

    fn persist_recovery_if_due(&mut self) {
        if !self.dirty
            || self
                .recovery_last_saved
                .is_some_and(|saved| saved.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        if let Err(error) = self.persist_recovery() {
            self.status_message = format!("Draft recovery unavailable: {error}");
        }
    }

    fn discard_recovery(&mut self) {
        if let Err(error) = remove_recovery_snapshot(self.workspace.root()) {
            self.status_message = format!("Recovery cleanup failed: {error}");
            return;
        }
        self.recovery_restored = false;
        self.recovery_last_saved = None;
        self.reset_new_request();
        let current_tab = self.current_tab();
        if self.open_tabs.is_empty() {
            self.open_tabs.push(current_tab);
            self.active_tab = 0;
        } else if let Some(tab) = self.open_tabs.get_mut(self.active_tab) {
            *tab = current_tab;
        }
        self.tabs_settings_dirty = true;
        self.status_message = "Recovered draft discarded".to_owned();
    }

    fn open_environment_editor(&mut self, index: Option<usize>) {
        self.environment_editor_error = None;
        if let Some(index) = index {
            let Some((path, environment)) = self.environments.get(index).cloned() else {
                return;
            };
            self.environment_editor_path = Some(path);
            self.environment_editor_name = environment.name;
            self.environment_editor_variables = environment
                .variables
                .into_iter()
                .map(|(key, variable)| EnvironmentVariableDraft {
                    key,
                    value: variable.value,
                    enabled: variable.enabled,
                    secret: variable.secret,
                    secret_ref: variable.secret_ref,
                })
                .collect();
        } else {
            self.environment_editor_path = None;
            self.environment_editor_name = "local".to_owned();
            self.environment_editor_variables = Vec::new();
        }
        self.environment_editor_open = true;
    }

    fn save_environment_editor(&mut self) -> Result<(), String> {
        let name = self.environment_editor_name.trim();
        if name.is_empty() {
            return Err("environment name cannot be empty".to_owned());
        }
        let mut variables = std::collections::BTreeMap::new();
        let store = SecretStore::for_workspace(self.workspace.root());
        for row in &self.environment_editor_variables {
            let key = row.key.trim();
            if key.is_empty() {
                continue;
            }
            if variables.contains_key(key) {
                return Err(format!("duplicate environment variable: {key}"));
            }
            let variable = if row.secret {
                let reference = if row.value.is_empty() {
                    row.secret_ref.clone().ok_or_else(|| {
                        format!("secret value is required for environment variable {key}")
                    })?
                } else {
                    store
                        .set_environment_secret(name, key, &row.value)
                        .map_err(|error| error.to_string())?
                        .into_string()
                };
                EnvironmentVariable {
                    value: String::new(),
                    enabled: row.enabled,
                    secret: true,
                    secret_ref: Some(reference),
                }
            } else {
                if row.secret_ref.is_some() {
                    return Err(format!(
                        "remove keychain-backed variable {key} instead of unmarking it as secret"
                    ));
                }
                EnvironmentVariable {
                    value: row.value.clone(),
                    enabled: row.enabled,
                    secret: false,
                    secret_ref: None,
                }
            };
            variables.insert(key.to_owned(), variable);
        }
        let environment = Environment {
            format: "postly-environment".to_owned(),
            version: 1,
            name: name.to_owned(),
            variables,
        };
        let path = self
            .workspace
            .save_environment(&environment)
            .map_err(|error| error.to_string())?;
        if let Some(previous_path) = self.environment_editor_path.as_ref() {
            if previous_path != &path {
                fs::remove_file(previous_path).map_err(|error| error.to_string())?;
            }
        }
        self.environments = self
            .workspace
            .environments()
            .map_err(|error| error.to_string())?;
        self.selected_environment = Some(environment.name.clone());
        self.environment_editor_open = false;
        self.environment_editor_path = Some(path.clone());
        self.status_message = format!("Environment saved locally — {}", path.display());
        Ok(())
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

    fn sync_active_tab(&mut self) -> Result<(), String> {
        if self.open_tabs.is_empty() {
            return Ok(());
        }
        let request = self.edited_request()?;
        if let Some(tab) = self.open_tabs.get_mut(self.active_tab) {
            if self
                .request_path
                .as_ref()
                .is_some_and(|path| self.requests.iter().any(|(candidate, _)| candidate == path))
            {
                tab.collection_index = self.selected_collection;
            }
            tab.request_path = self.request_path.clone();
            tab.request = request;
            tab.dirty = self.dirty;
        }
        Ok(())
    }

    fn current_tab(&self) -> RequestTab {
        RequestTab {
            collection_index: self.selected_collection,
            request_path: self.request_path.clone(),
            request: self.request.clone(),
            dirty: self.dirty,
        }
    }

    fn install_tab(&mut self, index: usize) {
        let Some(tab) = self.open_tabs.get(index).cloned() else {
            return;
        };
        self.active_tab = index;
        self.selected_collection = tab
            .collection_index
            .min(self.collections.len().saturating_sub(1));
        self.selected_request = tab.request_path.as_ref().and_then(|path| {
            self.requests
                .iter()
                .position(|(candidate, _)| candidate == path)
        });
        self.request_path = tab.request_path;
        self.request = tab.request;
        self.load_request_editors();
        self.clear_response();
        self.dirty = tab.dirty;
        self.recovery_restored = false;
    }

    fn restore_tabs(&mut self) {
        let path = self.workspace.root().join(GUI_TABS_FILE);
        let Ok(contents) = fs::read_to_string(&path) else {
            return;
        };
        let Ok(settings) = serde_json::from_str::<TabsSettings>(&contents) else {
            self.status_message = "Saved GUI tabs could not be restored".to_owned();
            return;
        };
        let root = self.workspace.root().to_path_buf();
        let mut restored = Vec::new();
        for relative in settings.paths {
            let full_path = root.join(&relative);
            if full_path.strip_prefix(&root).is_err() || !full_path.is_file() {
                continue;
            }
            let Some(collection_index) = self.collections.iter().position(|collection| {
                full_path.starts_with(collection.directory.join("requests"))
            }) else {
                continue;
            };
            let Ok(request) = self.workspace.load_request(&full_path) else {
                continue;
            };
            restored.push(RequestTab {
                collection_index,
                request_path: Some(full_path),
                request,
                dirty: false,
            });
        }
        if restored.is_empty() {
            return;
        }
        self.open_tabs = restored;
        self.active_tab = settings
            .active_path
            .and_then(|active_path| {
                let active_path = root.join(active_path);
                self.open_tabs
                    .iter()
                    .position(|tab| tab.request_path.as_ref() == Some(&active_path))
            })
            .unwrap_or(0);
        self.load_requests_for_tab(self.active_tab);
        self.install_tab(self.active_tab);
        self.tabs_settings_dirty = false;
    }

    fn load_requests_for_tab(&mut self, index: usize) {
        let Some(collection) = self
            .open_tabs
            .get(index)
            .and_then(|tab| self.collections.get(tab.collection_index))
            .cloned()
        else {
            return;
        };
        if let Ok(requests) = self.workspace.requests(&collection) {
            self.requests = requests;
        }
    }

    fn save_tabs_settings(&mut self) -> Result<(), String> {
        if !self.tabs_settings_dirty {
            return Ok(());
        }
        self.sync_active_tab()?;
        let root = self.workspace.root();
        let paths = self
            .open_tabs
            .iter()
            .filter_map(|tab| tab.request_path.as_ref())
            .filter_map(|path| path.strip_prefix(root).ok().map(PathBuf::from))
            .collect::<Vec<_>>();
        let active_path = self
            .open_tabs
            .get(self.active_tab)
            .and_then(|tab| tab.request_path.as_ref())
            .and_then(|path| path.strip_prefix(root).ok())
            .map(PathBuf::from);
        let settings = TabsSettings { paths, active_path };
        let path = root.join(GUI_TABS_FILE);
        if settings.paths.is_empty() {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
        } else {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            let contents =
                serde_json::to_vec_pretty(&settings).map_err(|error| error.to_string())?;
            fs::write(path, contents).map_err(|error| error.to_string())?;
        }
        self.tabs_settings_dirty = false;
        Ok(())
    }

    fn switch_to_tab(&mut self, index: usize) {
        if index >= self.open_tabs.len() || index == self.active_tab {
            return;
        }
        if let Err(error) = self.sync_active_tab() {
            self.status_message = format!("Cannot switch tab: {error}");
            return;
        }
        self.load_requests_for_tab(index);
        self.install_tab(index);
        self.tabs_settings_dirty = true;
        self.status_message = "Request tab activated".to_owned();
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.open_tabs.len() {
            return;
        }
        if self.open_tabs[index].dirty {
            self.status_message = "Save the request before closing its tab".to_owned();
            return;
        }
        self.open_tabs.remove(index);
        if self.open_tabs.is_empty() {
            self.active_tab = 0;
            self.reset_new_request();
            self.open_tabs.push(self.current_tab());
        } else {
            if index < self.active_tab {
                self.active_tab -= 1;
            } else if self.active_tab >= self.open_tabs.len() {
                self.active_tab = self.open_tabs.len() - 1;
            }
            self.load_requests_for_tab(self.active_tab);
            self.install_tab(self.active_tab);
        }
        self.tabs_settings_dirty = true;
    }

    fn close_other_tabs(&mut self) {
        if self
            .open_tabs
            .iter()
            .enumerate()
            .any(|(index, tab)| index != self.active_tab && tab.dirty)
        {
            self.status_message = "Save other dirty tabs before closing them".to_owned();
            return;
        }
        if self.open_tabs.len() <= 1 {
            return;
        }
        let active = self.open_tabs[self.active_tab].clone();
        self.open_tabs = vec![active];
        self.active_tab = 0;
        self.tabs_settings_dirty = true;
    }

    fn move_active_tab(&mut self, direction: isize) {
        let Some(target) = self.active_tab.checked_add_signed(direction) else {
            return;
        };
        if target >= self.open_tabs.len() {
            return;
        }
        self.open_tabs.swap(self.active_tab, target);
        self.active_tab = target;
        self.tabs_settings_dirty = true;
    }

    fn refresh_workspace_search(&mut self) {
        if self.workspace_search.trim().is_empty() {
            self.workspace_search_results.clear();
            return;
        }
        match self.workspace.search_requests(&self.workspace_search) {
            Ok(results) => self.workspace_search_results = results,
            Err(error) => {
                self.workspace_search_results.clear();
                self.status_message = format!("Workspace search failed: {error}");
            }
        }
    }

    fn open_search_result(&mut self, result: &RequestSearchResult) -> Result<(), String> {
        let collection_index = self
            .collections
            .iter()
            .position(|collection| collection.collection.id == result.collection_id)
            .ok_or_else(|| format!("collection not found: {}", result.collection))?;
        self.selected_collection = collection_index;
        let path = self.workspace.root().join(&result.path);
        self.refresh_requests(Some(&path))?;
        self.workspace_search.clear();
        self.workspace_search_results.clear();
        self.status_message = format!("Opened search result — {}", result.name);
        Ok(())
    }

    fn select_request(&mut self, index: usize) {
        let Some((path, request)) = self.requests.get(index).cloned() else {
            return;
        };
        if self.dirty {
            let _ = self.persist_recovery();
        }
        if let Err(error) = self.sync_active_tab() {
            self.status_message = format!("Cannot switch request: {error}");
            return;
        }
        let tab_index = self
            .open_tabs
            .iter()
            .position(|tab| tab.request_path.as_ref() == Some(&path));
        let tab_index = tab_index.unwrap_or_else(|| {
            self.open_tabs.push(RequestTab {
                collection_index: self.selected_collection,
                request_path: Some(path.clone()),
                request,
                dirty: false,
            });
            self.tabs_settings_dirty = true;
            self.open_tabs.len() - 1
        });
        self.load_requests_for_tab(tab_index);
        self.install_tab(tab_index);
        self.selected_request = Some(index);
        self.status_message = "Request loaded".to_owned();
    }

    fn new_request(&mut self) {
        if self.dirty {
            let _ = self.persist_recovery();
        }
        let _ = self.sync_active_tab();
        self.reset_new_request();
        self.open_tabs.push(self.current_tab());
        self.active_tab = self.open_tabs.len() - 1;
        self.tabs_settings_dirty = true;
        self.status_message = "Draft request".to_owned();
    }

    fn reset_new_request(&mut self) {
        self.selected_request = None;
        self.request_path = None;
        self.request = Request::new("New request", "GET", "https://example.com");
        self.editor_tab = EditorTab::Params;
        self.load_request_editors();
        self.clear_response();
        self.dirty = true;
        self.recovery_restored = false;
    }

    fn new_grpc_request(&mut self) {
        if self.dirty {
            let _ = self.persist_recovery();
        }
        let _ = self.sync_active_tab();
        self.selected_request = None;
        self.request_path = None;
        self.request = Request::new("New gRPC request", "POST", "http://127.0.0.1:50051");
        self.request.grpc = Some(GrpcRequest::new("api.proto", "/demo.Echo/Echo"));
        self.load_request_editors();
        self.editor_tab = EditorTab::Grpc;
        self.clear_response();
        self.dirty = true;
        self.recovery_restored = false;
        self.open_tabs.push(self.current_tab());
        self.active_tab = self.open_tabs.len() - 1;
        self.tabs_settings_dirty = true;
        self.status_message = "gRPC draft request".to_owned();
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
        if let Some(grpc) = &self.request.grpc {
            self.editor_tab = EditorTab::Grpc;
            self.grpc_proto_path.clone_from(&grpc.proto);
            self.grpc_reflection = grpc.reflection;
            self.grpc_reflection_host.clone_from(&grpc.reflection_host);
            self.grpc_includes_text = grpc.includes.join("\n");
            self.grpc_method.clone_from(&grpc.method);
            self.grpc_metadata = grpc.metadata.clone();
        } else {
            self.grpc_proto_path.clear();
            self.grpc_reflection = false;
            self.grpc_reflection_host.clear();
            self.grpc_includes_text.clear();
            self.grpc_method.clear();
            self.grpc_metadata.clear();
        }
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
            RequestBody::FormUrlEncoded { .. } => {
                self.body_kind = BodyKind::FormUrlEncoded;
                self.body_text.clear();
            }
            RequestBody::Multipart { .. } => {
                self.body_kind = BodyKind::Multipart;
                self.body_text.clear();
            }
            RequestBody::BinaryFile { .. } => {
                self.body_kind = BodyKind::BinaryFile;
                self.body_text.clear();
            }
        }
        if self.request.grpc.is_some() && matches!(self.body_kind, BodyKind::None) {
            self.body_kind = BodyKind::Json;
            self.body_text = "{}".to_owned();
        }
        self.pre_request_script = self.request.pre_request_script.clone().unwrap_or_default();
        self.test_script = self.request.test_script.clone().unwrap_or_default();
        match &self.request.auth {
            Auth::None => {
                self.auth_kind = AuthKind::None;
                self.auth_primary.clear();
                self.auth_secondary.clear();
                self.auth_tertiary.clear();
                self.auth_quaternary.clear();
                self.api_key_location = ApiKeyLocation::Header;
            }
            Auth::Bearer { token } => {
                self.auth_kind = AuthKind::Bearer;
                self.auth_primary.clone_from(token);
                self.auth_secondary.clear();
                self.auth_tertiary.clear();
                self.auth_quaternary.clear();
                self.api_key_location = ApiKeyLocation::Header;
            }
            Auth::Basic { username, password } => {
                self.auth_kind = AuthKind::Basic;
                self.auth_primary.clone_from(username);
                self.auth_secondary.clone_from(password);
                self.auth_tertiary.clear();
                self.auth_quaternary.clear();
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
                self.auth_tertiary.clear();
                self.auth_quaternary.clear();
                self.api_key_location = location.clone();
            }
            Auth::OAuth2ClientCredentials {
                token_url,
                client_id,
                client_secret,
                scope,
            } => {
                self.auth_kind = AuthKind::OAuth2ClientCredentials;
                self.auth_primary.clone_from(token_url);
                self.auth_secondary.clone_from(client_id);
                self.auth_tertiary.clone_from(client_secret);
                self.auth_quaternary = scope.clone().unwrap_or_default();
                self.api_key_location = ApiKeyLocation::Header;
            }
        }
    }

    fn clear_response(&mut self) {
        self.cancel_active();
        self.response = None;
        self.response_error = None;
        self.graphql_schema = None;
        self.graphql_schema_search.clear();
        self.graphql_schema_error = None;
        self.response_search.clear();
        self.response_tab = ResponseTab::Pretty;
        self.pending = None;
        self.pending_request = None;
        self.pending_graphql_schema = false;
        self.pending_grpc = false;
        self.pending_cancellation = None;
        self.script_pending = None;
        self.script_report = None;
        self.script_error = None;
        self.sse_pending = None;
        self.sse_cancellation = None;
        self.sse_events.clear();
        self.sse_status = None;
        self.sse_content_type = None;
        self.sse_protocol = None;
        self.sse_url = None;
        self.sse_started = false;
        self.sse_connected = false;
        self.websocket_pending = None;
        self.websocket_cancellation = None;
        self.websocket_commands = None;
        self.websocket_messages.clear();
        self.websocket_input.clear();
        self.websocket_url = None;
        self.websocket_started = false;
        self.websocket_connected = false;
    }

    fn palette_actions(&self) -> Vec<CommandPaletteAction> {
        vec![
            CommandPaletteAction::NewRequest,
            CommandPaletteAction::NewGrpcRequest,
            CommandPaletteAction::SaveRequest,
            CommandPaletteAction::SendRequest,
            CommandPaletteAction::CancelOperation,
            CommandPaletteAction::ClearResponse,
            CommandPaletteAction::ToggleResponseWrap,
            CommandPaletteAction::ImportCurl,
        ]
    }

    fn run_palette_action(&mut self, action: CommandPaletteAction) {
        self.command_palette_open = false;
        self.command_palette_query.clear();
        self.command_palette_selected = 0;
        match action {
            CommandPaletteAction::NewRequest => self.new_request(),
            CommandPaletteAction::NewGrpcRequest => self.new_grpc_request(),
            CommandPaletteAction::SaveRequest => {
                if let Err(error) = self.save_current() {
                    self.status_message = format!("Save failed: {error}");
                }
            }
            CommandPaletteAction::SendRequest => {
                if let Err(error) = self.send_current() {
                    self.status_message = format!("Send failed: {error}");
                }
            }
            CommandPaletteAction::CancelOperation => self.cancel_active(),
            CommandPaletteAction::ClearResponse => self.clear_response(),
            CommandPaletteAction::ToggleResponseWrap => self.response_wrap = !self.response_wrap,
            CommandPaletteAction::ImportCurl => {
                self.curl_import_open = true;
                self.curl_import_error = None;
            }
        }
    }

    fn apply_curl_import(&mut self) -> Result<Vec<String>, String> {
        let (request, warnings) =
            parse_curl_command(&self.curl_import_text).map_err(|error| error.to_string())?;
        self.selected_request = None;
        self.request_path = None;
        self.request = request;
        self.editor_tab = EditorTab::Params;
        self.load_request_editors();
        self.clear_response();
        self.dirty = true;
        self.recovery_restored = false;
        self.status_message = if warnings.is_empty() {
            "cURL command imported as a local draft".to_owned()
        } else {
            format!(
                "cURL imported with {} warning{} — review before sending",
                warnings.len(),
                if warnings.len() == 1 { "" } else { "s" }
            )
        };
        Ok(warnings)
    }

    fn draw_curl_import_dialog(&mut self, ctx: &egui::Context) {
        if !self.curl_import_open {
            return;
        }
        let mut import_clicked = false;
        let mut cancel_clicked = false;
        egui::Window::new("Import cURL")
            .collapsible(false)
            .resizable(true)
            .default_width(640.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "Paste a POSIX-shell cURL command. It becomes a new unsaved local draft.",
                    )
                    .small()
                    .color(MUTED),
                );
                ui.add_space(8.0);
                ui.add(
                    TextEdit::multiline(&mut self.curl_import_text)
                        .font(TextStyle::Monospace)
                        .desired_rows(9)
                        .desired_width(f32::INFINITY)
                        .hint_text(
                            "curl https://api.example.test/users -H 'Accept: application/json'",
                        ),
                );
                if let Some(error) = &self.curl_import_error {
                    ui.add_space(5.0);
                    ui.colored_label(Color32::from_rgb(240, 125, 105), error);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancel_clicked = true;
                    }
                    if ui.button("Import as draft").clicked() {
                        import_clicked = true;
                    }
                });
            });
        if cancel_clicked {
            self.curl_import_open = false;
            self.curl_import_error = None;
        }
        if import_clicked {
            match self.apply_curl_import() {
                Ok(warnings) => {
                    self.curl_import_open = false;
                    self.curl_import_error = None;
                    if !warnings.is_empty() {
                        self.status_message =
                            format!("{} · {}", self.status_message, warnings.join(" "));
                    }
                }
                Err(error) => self.curl_import_error = Some(error),
            }
        }
    }

    fn copy_current_as_curl(&mut self, ctx: &egui::Context) {
        match self.edited_request() {
            Ok(request) => {
                let exported = export_curl_command(&request);
                ctx.copy_text(exported.command);
                self.status_message = if exported.warnings.is_empty() {
                    "cURL command copied to clipboard".to_owned()
                } else {
                    format!(
                        "cURL copied with {} warning{}",
                        exported.warnings.len(),
                        if exported.warnings.len() == 1 {
                            ""
                        } else {
                            "s"
                        }
                    )
                };
            }
            Err(error) => self.status_message = format!("Cannot copy cURL: {error}"),
        }
    }

    fn handle_global_shortcuts(&mut self, ctx: &egui::Context) {
        let command_key = ctx.input(|input| input.modifiers.command || input.modifiers.ctrl);
        if command_key && ctx.input(|input| input.key_pressed(egui::Key::K)) {
            self.command_palette_open = !self.command_palette_open;
            self.command_palette_query.clear();
            self.command_palette_selected = 0;
        }
        if !self.command_palette_open && command_key {
            if ctx.input(|input| input.key_pressed(egui::Key::N)) {
                self.run_palette_action(CommandPaletteAction::NewRequest);
            } else if ctx.input(|input| input.key_pressed(egui::Key::S)) {
                self.run_palette_action(CommandPaletteAction::SaveRequest);
            }
        }
        if self.command_palette_open && ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.command_palette_open = false;
            self.command_palette_query.clear();
            self.command_palette_selected = 0;
        }
    }

    fn draw_command_palette(&mut self, ctx: &egui::Context) {
        if !self.command_palette_open {
            return;
        }
        let actions = self.palette_actions();
        let query = self.command_palette_query.trim().to_ascii_lowercase();
        let filtered = actions
            .iter()
            .copied()
            .filter(|action| action.label().to_ascii_lowercase().contains(&query))
            .collect::<Vec<_>>();
        if filtered.is_empty() {
            self.command_palette_selected = 0;
        } else {
            self.command_palette_selected = self
                .command_palette_selected
                .min(filtered.len().saturating_sub(1));
        }
        let mut chosen = None;
        egui::Window::new("Command palette")
            .collapsible(false)
            .resizable(false)
            .default_width(430.0)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 72.0])
            .show(ctx, |ui| {
                let response = ui.add(
                    TextEdit::singleline(&mut self.command_palette_query)
                        .hint_text("Type a command…")
                        .desired_width(ui.available_width()),
                );
                if response.gained_focus() {
                    self.command_palette_selected = 0;
                }
                ui.add_space(6.0);
                if filtered.is_empty() {
                    ui.label(RichText::new("No matching command").color(MUTED));
                } else {
                    for (index, action) in filtered.iter().enumerate() {
                        let selected = index == self.command_palette_selected;
                        let label = if action.shortcut().is_empty() {
                            action.label().to_owned()
                        } else {
                            format!("{}    {}", action.label(), action.shortcut())
                        };
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
                            chosen = Some(*action);
                        }
                    }
                    if ui.input(|input| input.key_pressed(egui::Key::ArrowDown)) {
                        self.command_palette_selected =
                            (self.command_palette_selected + 1) % filtered.len();
                    }
                    if ui.input(|input| input.key_pressed(egui::Key::ArrowUp)) {
                        self.command_palette_selected = self
                            .command_palette_selected
                            .checked_sub(1)
                            .unwrap_or(filtered.len() - 1);
                    }
                    if ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                        chosen = filtered.get(self.command_palette_selected).copied();
                    }
                }
                ui.add_space(5.0);
                ui.label(
                    RichText::new("↑↓ navigate  ·  Enter run  ·  Esc close")
                        .small()
                        .color(MUTED),
                );
            });
        if let Some(action) = chosen {
            self.run_palette_action(action);
        }
    }

    fn cancel_active(&mut self) {
        let mut cancelled = false;
        if let Some(token) = &self.pending_cancellation {
            token.cancel();
            cancelled = true;
        }
        if let Some(token) = &self.sse_cancellation {
            token.cancel();
            cancelled = true;
        }
        if let Some(token) = &self.websocket_cancellation {
            token.cancel();
            cancelled = true;
        }
        if let Some(sender) = &self.websocket_commands {
            let _ = sender.send(WebSocketCommand::Close);
        }
        if cancelled {
            self.status_message = "Cancelling…".to_owned();
        }
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
        request.pre_request_script =
            (!self.pre_request_script.trim().is_empty()).then(|| self.pre_request_script.clone());
        request.test_script =
            (!self.test_script.trim().is_empty()).then(|| self.test_script.clone());
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
            BodyKind::FormUrlEncoded => match &request.body {
                RequestBody::FormUrlEncoded { fields } => RequestBody::FormUrlEncoded {
                    fields: fields.clone(),
                },
                _ => RequestBody::FormUrlEncoded { fields: Vec::new() },
            },
            BodyKind::Multipart => match &request.body {
                RequestBody::Multipart { parts } => RequestBody::Multipart {
                    parts: parts.clone(),
                },
                _ => RequestBody::Multipart { parts: Vec::new() },
            },
            BodyKind::BinaryFile => match &request.body {
                RequestBody::BinaryFile { path, content_type } => RequestBody::BinaryFile {
                    path: path.clone(),
                    content_type: content_type.clone(),
                },
                _ => RequestBody::BinaryFile {
                    path: String::new(),
                    content_type: None,
                },
            },
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
            AuthKind::OAuth2ClientCredentials => Auth::OAuth2ClientCredentials {
                token_url: self.auth_primary.clone(),
                client_id: self.auth_secondary.clone(),
                client_secret: self.auth_tertiary.clone(),
                scope: (!self.auth_quaternary.trim().is_empty())
                    .then(|| self.auth_quaternary.clone()),
            },
        };
        if request.grpc.is_some() {
            let reflection_host = self.grpc_reflection_host.trim();
            let proto = self.grpc_proto_path.trim();
            if !self.grpc_reflection && proto.is_empty() {
                return Err(
                    "gRPC protobuf path is required unless server reflection is enabled".to_owned(),
                );
            }
            let method = self.grpc_method.trim();
            if method.is_empty() {
                return Err("gRPC method path is required".to_owned());
            }
            request.grpc = Some(GrpcRequest {
                proto: proto.to_owned(),
                reflection: self.grpc_reflection,
                reflection_host: reflection_host.to_owned(),
                includes: self
                    .grpc_includes_text
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(ToOwned::to_owned)
                    .collect(),
                method: method.to_owned(),
                metadata: self.grpc_metadata.clone(),
            });
        }
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
        self.dirty = false;
        self.recovery_restored = false;
        self.recovery_last_saved = None;
        self.sync_active_tab()?;
        self.tabs_settings_dirty = true;
        remove_recovery_snapshot(self.workspace.root())?;
        self.refresh_requests(Some(&path))?;
        self.refresh_workspace_search();
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
        self.refresh_workspace_search();
        self.dirty = false;
        self.recovery_restored = false;
        self.recovery_last_saved = None;
        self.sync_active_tab()?;
        self.tabs_settings_dirty = true;
        remove_recovery_snapshot(self.workspace.root())?;
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
        if let Some(index) = self
            .open_tabs
            .iter()
            .position(|tab| tab.request_path.as_ref() == Some(&path))
        {
            self.open_tabs.remove(index);
            if index < self.active_tab {
                self.active_tab -= 1;
            }
            if self.active_tab >= self.open_tabs.len() {
                self.active_tab = self.open_tabs.len().saturating_sub(1);
            }
        }
        self.selected_request = None;
        self.request_path = None;
        self.refresh_requests(None)?;
        self.refresh_workspace_search();
        self.recovery_restored = false;
        self.recovery_last_saved = None;
        self.tabs_settings_dirty = true;
        remove_recovery_snapshot(self.workspace.root())?;
        self.status_message = "Request deleted locally".to_owned();
        Ok(())
    }

    fn context(&self) -> Result<VariableContext, String> {
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
                context.environment = SecretStore::for_workspace(self.workspace.root())
                    .resolve_environment(environment)
                    .map_err(|error| format!("could not resolve environment secret: {error}"))?;
            }
        }
        Ok(context)
    }

    fn configured_engine(&mut self) -> Result<HttpEngine, String> {
        let engine = HttpEngine::new(&self.transport.engine_options(self.workspace.root()))
            .map_err(|error| format!("connection settings are invalid: {error}"))?;
        self.engine = engine.clone();
        Ok(engine)
    }

    fn save_transport_settings(&mut self) -> Result<(), String> {
        self.transport.save(self.workspace.root())?;
        self.transport_settings_dirty = false;
        self.status_message = "Connection settings saved locally".to_owned();
        Ok(())
    }

    fn send_current(&mut self) -> Result<(), String> {
        if self.pending.is_some()
            || self.sse_pending.is_some()
            || self.websocket_pending.is_some()
            || self.script_pending.is_some()
        {
            return Ok(());
        }
        let request = self.edited_request()?;
        if request.grpc.is_some() {
            return self.start_grpc_current(request);
        }
        let context = self.context()?;
        let engine = self.configured_engine()?;
        let cancellation = CancellationToken::default();
        let worker_cancellation = cancellation.clone();
        let (sender, receiver) = mpsc::channel();
        let worker_request = request.clone();
        thread::spawn(move || {
            let result = (|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())?;
                runtime.block_on(async move {
                    tokio::select! {
                        result = engine.execute(&worker_request, &context) => {
                            result.map_err(|error| error.to_string())
                        }
                        _ = worker_cancellation.cancelled() => {
                            Err("request cancelled".to_owned())
                        }
                    }
                })
            })();
            let _ = sender.send(result);
        });
        self.clear_response();
        self.pending = Some(receiver);
        self.pending_request = Some(request);
        self.pending_cancellation = Some(cancellation);
        self.status_message = "Sending request…".to_owned();
        Ok(())
    }

    fn start_grpc_current(&mut self, request: Request) -> Result<(), String> {
        if self.pending.is_some()
            || self.sse_pending.is_some()
            || self.websocket_pending.is_some()
            || self.script_pending.is_some()
        {
            return Ok(());
        }
        let context = self.context()?;
        let transport = self.transport.clone();
        let root = self.workspace.root().to_path_buf();
        let cancellation = CancellationToken::default();
        let worker_cancellation = cancellation.clone();
        let (sender, receiver) = mpsc::channel();
        let worker_request = request.clone();
        thread::spawn(move || {
            let result = (|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())?;
                runtime.block_on(async move {
                    tokio::select! {
                        result = execute_grpc_request(worker_request, context, transport, root) => result,
                        _ = worker_cancellation.cancelled() => Err("gRPC call cancelled".to_owned()),
                    }
                })
            })();
            let _ = sender.send(result);
        });
        self.clear_response();
        self.pending = Some(receiver);
        self.pending_request = Some(request);
        self.pending_grpc = true;
        self.pending_cancellation = Some(cancellation);
        self.status_message = "Calling gRPC method…".to_owned();
        Ok(())
    }

    fn start_script(&mut self, kind: ScriptRunKind) -> Result<(), String> {
        if self.script_pending.is_some()
            || self.pending.is_some()
            || self.sse_pending.is_some()
            || self.websocket_pending.is_some()
        {
            return Ok(());
        }
        let script = match kind {
            ScriptRunKind::PreRequest => self.pre_request_script.clone(),
            ScriptRunKind::Tests => self.test_script.clone(),
        };
        if script.trim().is_empty() {
            return Err(format!("{} script is empty", kind.label()));
        }
        let response =
            match kind {
                ScriptRunKind::PreRequest => None,
                ScriptRunKind::Tests => Some(self.response.clone().ok_or_else(|| {
                    "run the request before running post-response tests".to_owned()
                })?),
            };
        let request = self.edited_request()?;
        let context = self.context()?;
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = run_script(&script, &request, response.as_ref(), &context)
                .map(|result| ScriptRunReport { kind, result })
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        self.script_pending = Some(receiver);
        self.script_report = None;
        self.script_error = None;
        self.status_message = format!("Running {} script…", kind.label());
        Ok(())
    }

    fn start_graphql_schema(&mut self) -> Result<(), String> {
        if self.pending.is_some()
            || self.sse_pending.is_some()
            || self.websocket_pending.is_some()
            || self.script_pending.is_some()
        {
            return Ok(());
        }
        let mut request = self.edited_request()?;
        request.method = "POST".to_owned();
        request.body = RequestBody::Graphql {
            query: schema_introspection_query().to_owned(),
            variables: serde_json::json!({}),
            operation_name: Some("PostlySchemaIntrospection".to_owned()),
        };
        let context = self.context()?;
        let engine = self.configured_engine()?;
        let cancellation = CancellationToken::default();
        let worker_cancellation = cancellation.clone();
        let (sender, receiver) = mpsc::channel();
        let worker_request = request.clone();
        thread::spawn(move || {
            let result = (|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())?;
                runtime.block_on(async move {
                    tokio::select! {
                        result = engine.execute(&worker_request, &context) => {
                            result.map_err(|error| error.to_string())
                        }
                        _ = worker_cancellation.cancelled() => {
                            Err("schema introspection cancelled".to_owned())
                        }
                    }
                })
            })();
            let _ = sender.send(result);
        });
        self.clear_response();
        self.pending = Some(receiver);
        self.pending_request = Some(request);
        self.pending_graphql_schema = true;
        self.pending_cancellation = Some(cancellation);
        self.response_tab = ResponseTab::GraphqlSchema;
        self.status_message = "Fetching GraphQL schema…".to_owned();
        Ok(())
    }

    fn start_sse_current(&mut self) -> Result<(), String> {
        if self.pending.is_some()
            || self.sse_pending.is_some()
            || self.websocket_pending.is_some()
            || self.script_pending.is_some()
        {
            return Ok(());
        }
        let request = self.edited_request()?;
        let context = self.context()?;
        let engine = self.configured_engine()?;
        let reconnect_limit = self.sse_reconnect_limit;
        let cancellation = CancellationToken::default();
        let worker_cancellation = cancellation.clone();
        let (sender, receiver) = mpsc::channel();
        let error_sender = sender.clone();
        thread::spawn(move || {
            let result = (|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())?;
                runtime.block_on(async move {
                    let mut base_request = request;
                    if !base_request
                        .headers
                        .iter()
                        .any(|header| header.enabled && header.key.eq_ignore_ascii_case("accept"))
                    {
                        base_request
                            .headers
                            .push(HeaderEntry::enabled("accept", "text/event-stream"));
                    }
                    let mut reconnects_used = 0_u32;
                    let mut last_event_id = None;
                    loop {
                        let mut request = base_request.clone();
                        if let Some(last_event_id) = &last_event_id {
                            if let Some(header) = request.headers.iter_mut().find(|header| {
                                header.enabled && header.key.eq_ignore_ascii_case("last-event-id")
                            }) {
                                header.value.clone_from(last_event_id);
                            } else {
                                request
                                    .headers
                                    .push(HeaderEntry::enabled("last-event-id", last_event_id));
                            }
                        }
                        let response_result = tokio::select! {
                            result = engine.execute_stream(&request, &context) => {
                                result.map_err(|error| error.to_string())
                            }
                            _ = worker_cancellation.cancelled() => {
                                return Err("SSE stream cancelled".to_owned());
                            }
                        };
                        let mut response = match response_result {
                            Ok(response) => response,
                            Err(_error) if reconnects_used < reconnect_limit => {
                                reconnects_used += 1;
                                sender
                                    .send(Ok(SseStreamUpdate::Reconnecting {
                                        attempt: reconnects_used,
                                        max_attempts: reconnect_limit,
                                        delay_ms: 250,
                                        last_event_id: last_event_id.clone(),
                                    }))
                                    .map_err(|_| "SSE console was closed".to_owned())?;
                                tokio::select! {
                                    _ = tokio::time::sleep(Duration::from_millis(250)) => {}
                                    _ = worker_cancellation.cancelled() => {
                                        return Err("SSE stream cancelled".to_owned());
                                    }
                                }
                                continue;
                            }
                            Err(error) => return Err(error),
                        };
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
                        let mut retry_delay_ms = 250_u64;
                        while let Some(chunk) = tokio::select! {
                            result = response.response.chunk() => {
                                result.map_err(|error| error.to_string())?
                            }
                            _ = worker_cancellation.cancelled() => {
                                return Err("SSE stream cancelled".to_owned());
                            }
                        } {
                            for event in parser
                                .feed_bytes(&chunk)
                                .map_err(|error| error.to_string())?
                            {
                                if let Some(id) = &event.id {
                                    last_event_id = Some(id.clone());
                                }
                                if let Some(retry_ms) = event.retry_ms {
                                    retry_delay_ms = retry_ms;
                                }
                                sender
                                    .send(Ok(SseStreamUpdate::Event(event)))
                                    .map_err(|_| "SSE console was closed".to_owned())?;
                            }
                        }
                        for event in parser.finish().map_err(|error| error.to_string())? {
                            if let Some(id) = &event.id {
                                last_event_id = Some(id.clone());
                            }
                            if let Some(retry_ms) = event.retry_ms {
                                retry_delay_ms = retry_ms;
                            }
                            sender
                                .send(Ok(SseStreamUpdate::Event(event)))
                                .map_err(|_| "SSE console was closed".to_owned())?;
                        }
                        if reconnects_used >= reconnect_limit {
                            sender
                                .send(Ok(SseStreamUpdate::Closed))
                                .map_err(|_| "SSE console was closed".to_owned())?;
                            return Ok::<(), String>(());
                        }
                        reconnects_used += 1;
                        sender
                            .send(Ok(SseStreamUpdate::Reconnecting {
                                attempt: reconnects_used,
                                max_attempts: reconnect_limit,
                                delay_ms: retry_delay_ms,
                                last_event_id: last_event_id.clone(),
                            }))
                            .map_err(|_| "SSE console was closed".to_owned())?;
                        tokio::select! {
                            _ = tokio::time::sleep(Duration::from_millis(retry_delay_ms)) => {}
                            _ = worker_cancellation.cancelled() => {
                                return Err("SSE stream cancelled".to_owned());
                            }
                        }
                    }
                })
            })();
            if let Err(error) = result {
                let _ = error_sender.send(Err(error));
            }
        });
        self.clear_response();
        self.sse_pending = Some(receiver);
        self.sse_cancellation = Some(cancellation);
        self.sse_started = true;
        self.status_message = "Connecting to SSE endpoint…".to_owned();
        Ok(())
    }

    fn start_websocket_current(&mut self) -> Result<(), String> {
        if self.pending.is_some()
            || self.sse_pending.is_some()
            || self.websocket_pending.is_some()
            || self.script_pending.is_some()
        {
            return Ok(());
        }
        let request = self.edited_request()?;
        let context = self.context()?;
        let cancellation = CancellationToken::default();
        let worker_cancellation = cancellation.clone();
        let (command_sender, mut command_receiver) =
            tokio::sync::mpsc::unbounded_channel::<WebSocketCommand>();
        let (sender, receiver) = mpsc::channel();
        let error_sender = sender.clone();
        thread::spawn(move || {
            let result = (|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())?;
                runtime.block_on(async move {
                    let websocket_request = build_websocket_request(&request, &context)?;
                    let websocket_url = websocket_request.uri().to_string();
                    let connect_result = tokio::select! {
                        result = tokio::time::timeout(
                            Duration::from_secs(30),
                            connect_async(websocket_request),
                        ) => result,
                        _ = worker_cancellation.cancelled() => {
                            return Err("WebSocket connection cancelled".to_owned());
                        }
                    };
                    let (mut socket, _) = connect_result
                        .map_err(|_| "WebSocket handshake timed out".to_owned())?
                        .map_err(|error| format!("WebSocket connection failed: {error}"))?;
                    sender
                        .send(Ok(WebSocketStreamUpdate::Connected {
                            url: websocket_url,
                        }))
                        .map_err(|_| "WebSocket console was closed".to_owned())?;
                    loop {
                        tokio::select! {
                            inbound = socket.next() => {
                                match inbound {
                                    Some(Ok(Message::Text(text))) => {
                                        sender.send(Ok(WebSocketStreamUpdate::Message {
                                            direction: WebSocketDirection::Received,
                                            kind: "text".to_owned(),
                                            data: text.to_string(),
                                        })).map_err(|_| "WebSocket console was closed".to_owned())?;
                                    }
                                    Some(Ok(Message::Binary(bytes))) => {
                                        sender.send(Ok(WebSocketStreamUpdate::Message {
                                            direction: WebSocketDirection::Received,
                                            kind: "binary".to_owned(),
                                            data: format!(
                                                "{} bytes · base64 {}",
                                                bytes.len(),
                                                base64::engine::general_purpose::STANDARD.encode(&bytes)
                                            ),
                                        })).map_err(|_| "WebSocket console was closed".to_owned())?;
                                    }
                                    Some(Ok(Message::Ping(bytes))) => {
                                        socket.send(Message::Pong(bytes.clone())).await
                                            .map_err(|error| format!("could not reply to WebSocket ping: {error}"))?;
                                        sender.send(Ok(WebSocketStreamUpdate::Message {
                                            direction: WebSocketDirection::Received,
                                            kind: "ping".to_owned(),
                                            data: format!("{} bytes", bytes.len()),
                                        })).map_err(|_| "WebSocket console was closed".to_owned())?;
                                    }
                                    Some(Ok(Message::Pong(bytes))) => {
                                        sender.send(Ok(WebSocketStreamUpdate::Message {
                                            direction: WebSocketDirection::Received,
                                            kind: "pong".to_owned(),
                                            data: format!("{} bytes", bytes.len()),
                                        })).map_err(|_| "WebSocket console was closed".to_owned())?;
                                    }
                                    Some(Ok(Message::Close(frame))) => {
                                        let data = frame.map(|frame| format!("{} ({})", frame.reason, frame.code))
                                            .unwrap_or_else(|| "peer closed the connection".to_owned());
                                        sender.send(Ok(WebSocketStreamUpdate::Message {
                                            direction: WebSocketDirection::Received,
                                            kind: "close".to_owned(),
                                            data,
                                        })).map_err(|_| "WebSocket console was closed".to_owned())?;
                                        break;
                                    }
                                    Some(Ok(Message::Frame(_))) => {}
                                    Some(Err(error)) => {
                                        return Err(format!("WebSocket receive failed: {error}"));
                                    }
                                    None => break,
                                }
                            }
                            command = command_receiver.recv() => {
                                match command {
                                    Some(WebSocketCommand::SendText(text)) => {
                                        socket.send(Message::Text(text.clone().into())).await
                                            .map_err(|error| format!("WebSocket send failed: {error}"))?;
                                        sender.send(Ok(WebSocketStreamUpdate::Message {
                                            direction: WebSocketDirection::Sent,
                                            kind: "text".to_owned(),
                                            data: text,
                                        })).map_err(|_| "WebSocket console was closed".to_owned())?;
                                    }
                                    Some(WebSocketCommand::Close) | None => {
                                        let _ = socket.close(None).await;
                                        break;
                                    }
                                }
                            }
                            _ = worker_cancellation.cancelled() => {
                                let _ = socket.close(None).await;
                                break;
                            }
                        }
                    }
                    let _ = sender.send(Ok(WebSocketStreamUpdate::Closed));
                    Ok::<(), String>(())
                })
            })();
            if let Err(error) = result {
                let _ = error_sender.send(Err(error));
            }
        });
        self.clear_response();
        self.websocket_pending = Some(receiver);
        self.websocket_cancellation = Some(cancellation);
        self.websocket_commands = Some(command_sender);
        self.websocket_started = true;
        self.status_message = "Connecting to WebSocket…".to_owned();
        Ok(())
    }

    fn send_websocket_text(&mut self) -> Result<(), String> {
        if !self.websocket_connected {
            return Err("WebSocket is not connected".to_owned());
        }
        let text = std::mem::take(&mut self.websocket_input);
        if text.is_empty() {
            return Ok(());
        }
        let sender = self
            .websocket_commands
            .as_ref()
            .ok_or_else(|| "WebSocket command channel is unavailable".to_owned())?;
        sender
            .send(WebSocketCommand::SendText(text))
            .map_err(|_| "WebSocket connection is no longer available".to_owned())?;
        Ok(())
    }

    fn close_websocket(&mut self) {
        if let Some(sender) = &self.websocket_commands {
            let _ = sender.send(WebSocketCommand::Close);
        }
    }

    fn poll_script_pending(&mut self) -> bool {
        let Some(receiver) = self.script_pending.as_ref() else {
            return false;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return true,
            Err(TryRecvError::Disconnected) => Err("script worker stopped unexpectedly".to_owned()),
        };
        self.script_pending = None;
        match result {
            Ok(report) => {
                let failed = report.result.failed_tests().count();
                self.status_message = if failed == 0 {
                    format!(
                        "{} script finished · {} test(s)",
                        report.kind.label(),
                        report.result.tests.len()
                    )
                } else {
                    format!(
                        "{} script finished · {failed} failed test(s)",
                        report.kind.label()
                    )
                };
                self.script_report = Some(report);
                self.script_error = None;
            }
            Err(error) => {
                self.status_message = "Script failed".to_owned();
                self.script_error = Some(error);
                self.script_report = None;
            }
        }
        false
    }

    fn poll_pending(&mut self) -> bool {
        let script_pending = self.poll_script_pending();
        let http_pending = self.poll_http_pending();
        let sse_pending = self.poll_sse_pending();
        let websocket_pending = self.poll_websocket_pending();
        script_pending || http_pending || sse_pending || websocket_pending
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
        let cancelled = self
            .pending_cancellation
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled);
        let request = self.pending_request.take();
        let schema_pending = self.pending_graphql_schema;
        let grpc_pending = self.pending_grpc;
        self.pending = None;
        self.pending_graphql_schema = false;
        self.pending_grpc = false;
        self.pending_cancellation = None;
        match result {
            Ok(response) => {
                self.status_message = format!(
                    "{} {} in {} ms",
                    response.status, response.status_text, response.duration_ms
                );
                if schema_pending {
                    let schema = if response.status >= 400 {
                        Err(format!(
                            "GraphQL introspection endpoint returned {} {}",
                            response.status, response.status_text
                        ))
                    } else {
                        parse_graphql_response(&response.body_text())
                            .map_err(|error| error.to_string())
                            .and_then(|graphql| {
                                parse_graphql_schema(&graphql).map_err(|error| error.to_string())
                            })
                    };
                    match schema {
                        Ok(schema) => {
                            self.status_message = format!(
                                "GraphQL schema loaded · {} named types",
                                schema.types.len()
                            );
                            self.graphql_schema = Some(schema);
                            self.graphql_schema_error = None;
                            self.response_error = None;
                        }
                        Err(error) => {
                            self.status_message = "GraphQL schema introspection failed".to_owned();
                            self.graphql_schema_error = Some(error);
                            self.response_error = None;
                        }
                    }
                    self.response = Some(response);
                    self.response_tab = ResponseTab::GraphqlSchema;
                    return false;
                }
                if let Some(request) = request {
                    let _ = self
                        .workspace
                        .record_history(&HistoryEntry::from_response(&request, &response));
                }
                self.response = Some(response);
                self.response_error = None;
                self.refresh_history();
            }
            Err(error) => {
                if schema_pending {
                    if cancelled {
                        self.status_message = "GraphQL schema introspection cancelled".to_owned();
                    } else {
                        self.status_message = "GraphQL schema introspection failed".to_owned();
                        self.graphql_schema_error = Some(error);
                    }
                    self.response_error = None;
                    return false;
                }
                if !cancelled {
                    if let Some(request) = request {
                        let _ = self
                            .workspace
                            .record_history(&HistoryEntry::from_error(&request, 0));
                    }
                }
                if cancelled {
                    self.status_message = if grpc_pending {
                        "gRPC call cancelled".to_owned()
                    } else {
                        "Request cancelled".to_owned()
                    };
                    self.response_error = None;
                } else {
                    self.status_message = "Request failed".to_owned();
                    self.response_error = Some(error);
                }
                if !cancelled {
                    self.refresh_history();
                }
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
                Ok(Ok(SseStreamUpdate::Reconnecting {
                    attempt,
                    max_attempts,
                    delay_ms,
                    last_event_id,
                })) => {
                    self.sse_started = true;
                    self.sse_connected = false;
                    self.status_message = format!(
                        "SSE reconnecting · attempt {attempt}/{max_attempts} · {delay_ms} ms{}",
                        if last_event_id.is_some() {
                            " · Last-Event-ID set"
                        } else {
                            ""
                        }
                    );
                }
                Ok(Ok(SseStreamUpdate::Event(event))) => {
                    self.sse_started = true;
                    self.sse_events.push_back(ReceivedSseEvent {
                        event,
                        received_at: Local::now().format("%H:%M:%S").to_string(),
                    });
                    while self.sse_events.len() > MAX_CONSOLE_ITEMS {
                        self.sse_events.pop_front();
                    }
                    self.status_message =
                        format!("SSE event received · {} retained", self.sse_events.len());
                }
                Ok(Ok(SseStreamUpdate::Closed)) => {
                    let cancelled = self
                        .sse_cancellation
                        .as_ref()
                        .is_some_and(CancellationToken::is_cancelled);
                    self.sse_connected = false;
                    self.status_message = if cancelled {
                        "SSE stream cancelled".to_owned()
                    } else {
                        format!(
                            "SSE stream closed · {} event{}",
                            self.sse_events.len(),
                            if self.sse_events.len() == 1 { "" } else { "s" }
                        )
                    };
                    finished = true;
                    break;
                }
                Ok(Err(error)) => {
                    let cancelled = self
                        .sse_cancellation
                        .as_ref()
                        .is_some_and(CancellationToken::is_cancelled);
                    self.sse_connected = false;
                    self.sse_started = true;
                    self.status_message = if cancelled {
                        "SSE stream cancelled".to_owned()
                    } else {
                        "SSE stream failed".to_owned()
                    };
                    self.response_error = (!cancelled).then_some(error);
                    finished = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let cancelled = self
                        .sse_cancellation
                        .as_ref()
                        .is_some_and(CancellationToken::is_cancelled);
                    self.sse_connected = false;
                    self.status_message = if cancelled {
                        "SSE stream cancelled".to_owned()
                    } else {
                        "SSE stream worker stopped unexpectedly".to_owned()
                    };
                    self.response_error =
                        (!cancelled).then_some("SSE stream worker stopped unexpectedly".to_owned());
                    finished = true;
                    break;
                }
            }
        }
        if finished {
            self.sse_pending = None;
            self.sse_cancellation = None;
        }
        self.sse_pending.is_some()
    }

    fn poll_websocket_pending(&mut self) -> bool {
        let mut finished = false;
        loop {
            let result = {
                let Some(receiver) = self.websocket_pending.as_ref() else {
                    break;
                };
                receiver.try_recv()
            };
            match result {
                Ok(Ok(WebSocketStreamUpdate::Connected { url })) => {
                    self.websocket_url = Some(url);
                    self.websocket_started = true;
                    self.websocket_connected = true;
                    self.status_message = "WebSocket connected".to_owned();
                }
                Ok(Ok(WebSocketStreamUpdate::Message {
                    direction,
                    kind,
                    data,
                })) => {
                    self.websocket_started = true;
                    self.websocket_messages.push_back(ReceivedWebSocketMessage {
                        direction,
                        kind,
                        data,
                        received_at: Local::now().format("%H:%M:%S").to_string(),
                    });
                    while self.websocket_messages.len() > MAX_CONSOLE_ITEMS {
                        self.websocket_messages.pop_front();
                    }
                    self.status_message = format!(
                        "WebSocket message · {} retained",
                        self.websocket_messages.len()
                    );
                }
                Ok(Ok(WebSocketStreamUpdate::Closed)) => {
                    let cancelled = self
                        .websocket_cancellation
                        .as_ref()
                        .is_some_and(CancellationToken::is_cancelled);
                    self.websocket_connected = false;
                    self.status_message = if cancelled {
                        "WebSocket connection cancelled".to_owned()
                    } else {
                        format!(
                            "WebSocket closed · {} message{}",
                            self.websocket_messages.len(),
                            if self.websocket_messages.len() == 1 {
                                ""
                            } else {
                                "s"
                            }
                        )
                    };
                    finished = true;
                    break;
                }
                Ok(Err(error)) => {
                    let cancelled = self
                        .websocket_cancellation
                        .as_ref()
                        .is_some_and(CancellationToken::is_cancelled);
                    self.websocket_connected = false;
                    self.websocket_started = true;
                    self.status_message = if cancelled {
                        "WebSocket connection cancelled".to_owned()
                    } else {
                        "WebSocket failed".to_owned()
                    };
                    self.response_error = (!cancelled).then_some(error);
                    finished = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let cancelled = self
                        .websocket_cancellation
                        .as_ref()
                        .is_some_and(CancellationToken::is_cancelled);
                    self.websocket_connected = false;
                    self.status_message = if cancelled {
                        "WebSocket connection cancelled".to_owned()
                    } else {
                        "WebSocket worker stopped unexpectedly".to_owned()
                    };
                    self.response_error =
                        (!cancelled).then_some("WebSocket worker stopped unexpectedly".to_owned());
                    finished = true;
                    break;
                }
            }
        }
        if finished {
            self.websocket_pending = None;
            self.websocket_cancellation = None;
            self.websocket_commands = None;
        }
        self.websocket_pending.is_some()
    }

    fn draw_navigator(&mut self, ui: &mut egui::Ui) {
        let mut collection_clicked = None;
        let mut request_clicked = None;
        let mut new_clicked = false;
        let mut environment_clicked = None;
        let mut environment_edit_clicked = None;
        let mut history_clicked = None;
        let mut search_result_clicked = None;
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
                ui.add_space(14.0);
                ui.label(
                    RichText::new("WORKSPACE SEARCH")
                        .small()
                        .strong()
                        .color(MUTED),
                );
                if ui
                    .add(
                        TextEdit::singleline(&mut self.workspace_search)
                            .hint_text("Search collections, requests or URLs")
                            .desired_width(ui.available_width()),
                    )
                    .changed()
                {
                    self.refresh_workspace_search();
                }
                if self.workspace_search.trim().is_empty() {
                    ui.add_space(12.0);
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
                } else {
                    let result_count = self.workspace_search_results.len();
                    ui.label(
                        RichText::new(format!("{result_count} matching request(s)"))
                            .small()
                            .color(MUTED),
                    );
                    egui::ScrollArea::vertical()
                        .id_salt("workspace-search-results")
                        .max_height((ui.available_height() - 280.0).max(100.0))
                        .show(ui, |ui| {
                            for result in &self.workspace_search_results {
                                let location = result
                                    .folder
                                    .as_deref()
                                    .map(|folder| format!("{} / {folder}", result.collection))
                                    .unwrap_or_else(|| result.collection.clone());
                                let label = format!("{}  {}", result.method, result.name);
                                if ui
                                    .selectable_label(false, RichText::new(label).color(MUTED))
                                    .on_hover_text(format!("{location} · {}", result.url))
                                    .clicked()
                                {
                                    search_result_clicked = Some(result.clone());
                                }
                                ui.label(
                                    RichText::new(format!("{location} · {}", result.url))
                                        .small()
                                        .color(MUTED),
                                );
                            }
                        });
                }
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
                ui.horizontal(|ui| {
                    if ui.small_button("＋ New environment").clicked() {
                        environment_edit_clicked = Some(None);
                    }
                    if let Some(selected) = self.selected_environment.as_deref() {
                        if ui.small_button("Edit selected").clicked() {
                            environment_edit_clicked = Some(
                                self.environments
                                    .iter()
                                    .position(|(_, environment)| environment.name == selected),
                            );
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
        if let Some(result) = search_result_clicked {
            if let Err(error) = self.open_search_result(&result) {
                self.status_message = format!("Search result could not open: {error}");
            }
        }
        if let Some(environment) = environment_clicked {
            self.selected_environment = environment;
        }
        if let Some(index) = environment_edit_clicked {
            self.open_environment_editor(index);
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

    fn draw_environment_editor(&mut self, ctx: &egui::Context) {
        if !self.environment_editor_open {
            return;
        }
        let mut save_clicked = false;
        let mut cancel_clicked = false;
        egui::Window::new("Environment editor")
            .collapsible(false)
            .resizable(true)
            .default_width(720.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "Plain values stay in the local environment file. Secret values are stored in the OS credential store; only opaque references are written.",
                    )
                    .small()
                    .color(MUTED),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label("Environment name");
                    ui.add(
                        TextEdit::singleline(&mut self.environment_editor_name)
                            .desired_width(320.0),
                    );
                });
                ui.add_space(10.0);
                ui.label(RichText::new("Variables").strong().color(Color32::WHITE));
                ui.add_space(4.0);
                let mut remove = None;
                egui::ScrollArea::vertical()
                    .id_salt("environment-editor-variables")
                    .max_height(360.0)
                    .show(ui, |ui| {
                        egui::Grid::new("environment-editor-grid")
                            .striped(true)
                            .min_col_width(70.0)
                            .show(ui, |ui| {
                                ui.label(RichText::new("Enabled").small().color(MUTED));
                                ui.label(RichText::new("Key").small().color(MUTED));
                                ui.label(RichText::new("Secret").small().color(MUTED));
                                ui.label(RichText::new("Value").small().color(MUTED));
                                ui.end_row();
                                for (index, row) in self
                                    .environment_editor_variables
                                    .iter_mut()
                                    .enumerate()
                                {
                                    ui.checkbox(&mut row.enabled, "");
                                    ui.text_edit_singleline(&mut row.key);
                                    ui.checkbox(&mut row.secret, "");
                                    let hint = if row.secret_ref.is_some() && row.value.is_empty() {
                                        "Stored in OS credential store; leave blank to keep"
                                    } else if row.secret {
                                        "Secret value"
                                    } else {
                                        "Value"
                                    };
                                    ui.add(
                                        TextEdit::singleline(&mut row.value)
                                            .password(row.secret)
                                            .hint_text(hint)
                                            .desired_width(360.0),
                                    );
                                    if ui.small_button("×").clicked() {
                                        remove = Some(index);
                                    }
                                    ui.end_row();
                                }
                            });
                    });
                if let Some(index) = remove {
                    self.environment_editor_variables.remove(index);
                }
                if ui.button("＋ Add variable").clicked() {
                    self.environment_editor_variables.push(EnvironmentVariableDraft {
                        key: String::new(),
                        value: String::new(),
                        enabled: true,
                        secret: false,
                        secret_ref: None,
                    });
                }
                ui.add_space(8.0);
                if let Some(error) = &self.environment_editor_error {
                    ui.colored_label(Color32::from_rgb(240, 120, 110), error);
                }
                ui.horizontal(|ui| {
                    if ui.button("Save environment").clicked() {
                        save_clicked = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel_clicked = true;
                    }
                });
            });
        if cancel_clicked {
            self.environment_editor_open = false;
            self.environment_editor_error = None;
        }
        if save_clicked {
            match self.save_environment_editor() {
                Ok(()) => self.environment_editor_error = None,
                Err(error) => self.environment_editor_error = Some(error),
            }
        }
    }

    fn draw_request_tabs(&mut self, ui: &mut egui::Ui) {
        if self.open_tabs.is_empty() {
            return;
        }
        let mut switch = None;
        let mut close = None;
        let mut close_others = false;
        let mut move_left = false;
        let mut move_right = false;
        ui.horizontal_wrapped(|ui| {
            for (index, tab) in self.open_tabs.iter().enumerate() {
                let title = format!("{} {}", if tab.dirty { "•" } else { "" }, tab.request.name);
                ui.push_id(index, |ui| {
                    if ui
                        .selectable_label(index == self.active_tab, title)
                        .clicked()
                    {
                        switch = Some(index);
                    }
                    if ui
                        .add_enabled(!tab.dirty, egui::Button::new("×"))
                        .on_hover_text("Close tab after saving it")
                        .clicked()
                    {
                        close = Some(index);
                    }
                });
            }
            if self.open_tabs.len() > 1 {
                ui.separator();
                if ui.button("Close others").clicked() {
                    close_others = true;
                }
                if ui
                    .add_enabled(self.active_tab > 0, egui::Button::new("←"))
                    .on_hover_text("Move active tab left")
                    .clicked()
                {
                    move_left = true;
                }
                if ui
                    .add_enabled(
                        self.active_tab + 1 < self.open_tabs.len(),
                        egui::Button::new("→"),
                    )
                    .on_hover_text("Move active tab right")
                    .clicked()
                {
                    move_right = true;
                }
            }
        });
        if let Some(index) = switch {
            self.switch_to_tab(index);
        }
        if let Some(index) = close {
            self.close_tab(index);
        }
        if close_others {
            self.close_other_tabs();
        }
        if move_left {
            self.move_active_tab(-1);
        }
        if move_right {
            self.move_active_tab(1);
        }
    }

    fn draw_request_header(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("request-header")
            .frame(egui::Frame::default().fill(SURFACE))
            .show(ui, |ui| {
                ui.add_space(8.0);
                self.draw_request_tabs(ui);
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
                if self.recovery_restored {
                    let mut discard_recovery_clicked = false;
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(
                                "Recovered local draft — save it to keep it in the workspace.",
                            )
                            .small()
                            .color(Color32::from_rgb(235, 180, 80)),
                        );
                        if ui.small_button("Discard recovery").clicked() {
                            discard_recovery_clicked = true;
                        }
                    });
                    if discard_recovery_clicked {
                        self.discard_recovery();
                    }
                }
                ui.add_space(7.0);
                let mut send_clicked = false;
                let mut cancel_clicked = false;
                let mut stream_clicked = false;
                let mut websocket_clicked = false;
                let mut save_clicked = false;
                let mut copy_curl_clicked = false;
                let mut duplicate_clicked = false;
                let mut delete_clicked = false;
                let busy = self.pending.is_some()
                    || self.sse_pending.is_some()
                    || self.websocket_pending.is_some()
                    || self.script_pending.is_some();
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
                    if busy {
                        if ui.button("Cancel").clicked() {
                            cancel_clicked = true;
                        }
                    } else if ui.add(egui::Button::new("Send  ⌘↵").fill(ACCENT)).clicked() {
                        send_clicked = true;
                    }
                    if ui.button("Save").clicked() {
                        save_clicked = true;
                    }
                    if ui.button("Copy cURL").clicked() {
                        copy_curl_clicked = true;
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
                    ] {
                        if tab_button(ui, self.editor_tab == tab, label).clicked() {
                            self.editor_tab = tab;
                        }
                    }
                    if self.request.grpc.is_some()
                        && tab_button(ui, self.editor_tab == EditorTab::Grpc, "gRPC").clicked()
                    {
                        self.editor_tab = EditorTab::Grpc;
                    }
                    for (tab, label) in [
                        (EditorTab::Auth, "Auth"),
                        (EditorTab::Scripts, "Scripts"),
                        (EditorTab::Assertions, "Assertions"),
                        (EditorTab::Transport, "Transport"),
                    ] {
                        if tab_button(ui, self.editor_tab == tab, label).clicked() {
                            self.editor_tab = tab;
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new("SSE retries").small().color(MUTED));
                        ui.add_enabled(
                            !busy,
                            egui::DragValue::new(&mut self.sse_reconnect_limit)
                                .range(0..=10)
                                .speed(0.1),
                        )
                        .on_hover_text("Bounded reconnect attempts for SSE streams");
                        if ui
                            .add_enabled(!busy, egui::Button::new("Stream SSE"))
                            .on_hover_text("Open the current request as a progressive SSE console")
                            .clicked()
                        {
                            stream_clicked = true;
                        }
                        if ui
                            .add_enabled(!busy, egui::Button::new("Connect WS"))
                            .on_hover_text("Open a ws:// or wss:// interactive console")
                            .clicked()
                        {
                            websocket_clicked = true;
                        }
                    });
                });
                ui.add_space(3.0);
                if save_clicked {
                    if let Err(error) = self.save_current() {
                        self.status_message = format!("Save failed: {error}");
                    }
                }
                if copy_curl_clicked {
                    self.copy_current_as_curl(ui.ctx());
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
                if cancel_clicked {
                    self.cancel_active();
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
                if websocket_clicked {
                    if let Err(error) = self.start_websocket_current() {
                        self.status_message = format!("WebSocket start failed: {error}");
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
                    EditorTab::Grpc => self.render_grpc(ui),
                    EditorTab::Auth => self.render_auth(ui),
                    EditorTab::Scripts => self.render_scripts(ui),
                    EditorTab::Assertions => self.render_assertions(ui),
                    EditorTab::Transport => self.render_transport(ui),
                }
            });
    }

    fn render_grpc(&mut self, ui: &mut egui::Ui) {
        ui.heading(RichText::new("gRPC method").color(Color32::WHITE));
        ui.label(
            RichText::new(
                "Use a local .proto file or discover the schema from the server. The JSON body is converted through the method descriptor.",
            )
            .small()
            .color(MUTED),
        );
        ui.add_space(10.0);
        let mut changed = false;
        changed |= ui
            .checkbox(
                &mut self.grpc_reflection,
                "Discover schema through server reflection (v1 / v1alpha)",
            )
            .changed();
        if self.grpc_reflection {
            ui.label(
                RichText::new(
                    "Descriptors stay in memory. Leave the host empty unless the server routes reflection by virtual host.",
                )
                .small()
                .color(MUTED),
            );
            changed |= labeled_singleline(
                ui,
                "Reflection host (optional)",
                &mut self.grpc_reflection_host,
            );
        } else {
            changed |= labeled_singleline(ui, "Proto file", &mut self.grpc_proto_path);
            ui.label(
                RichText::new(
                    "Relative paths use the workspace root; one include directory per line.",
                )
                .small()
                .color(MUTED),
            );
            ui.add_space(5.0);
            ui.label(
                RichText::new("Include directories")
                    .strong()
                    .color(Color32::WHITE),
            );
            changed |= ui
                .add(
                    TextEdit::multiline(&mut self.grpc_includes_text)
                        .font(TextStyle::Monospace)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY)
                        .hint_text("proto\nthird_party/protos"),
                )
                .changed();
        }
        ui.add_space(7.0);
        changed |= labeled_singleline(ui, "Method path", &mut self.grpc_method);
        ui.label(
            RichText::new("Examples: /demo.Echo/Echo, demo.Echo/Echo or Echo/Echo")
                .small()
                .color(MUTED),
        );
        ui.add_space(9.0);
        ui.label(RichText::new("Metadata").strong().color(Color32::WHITE));
        ui.label(
            RichText::new("Enabled metadata values are resolved from the selected environment.")
                .small()
                .color(MUTED),
        );
        changed |= render_key_values(
            ui,
            &mut self.grpc_metadata,
            "grpc-metadata",
            "＋ Add metadata",
        );
        ui.add_space(8.0);
        ui.label(
            RichText::new(
                "Unary and all streaming method shapes are supported. gRPC GUI calls use verified HTTP/2 TLS when the endpoint is HTTPS; proxy and insecure TLS options are rejected explicitly.",
            )
            .small()
            .color(MUTED),
        );
        if changed {
            self.dirty = true;
        }
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
                    BodyKind::FormUrlEncoded,
                    BodyKind::Multipart,
                    BodyKind::BinaryFile,
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
            match self.body_kind {
                BodyKind::FormUrlEncoded => {
                    self.request.body = RequestBody::FormUrlEncoded { fields: Vec::new() };
                }
                BodyKind::Multipart => {
                    self.request.body = RequestBody::Multipart { parts: Vec::new() };
                }
                BodyKind::BinaryFile => {
                    self.request.body = RequestBody::BinaryFile {
                        path: String::new(),
                        content_type: None,
                    };
                }
                BodyKind::None
                | BodyKind::Raw
                | BodyKind::Json
                | BodyKind::Graphql
                | BodyKind::Advanced => {}
            }
        }
        ui.add_space(10.0);
        let mut inspect_schema_clicked = false;
        let busy = self.pending.is_some()
            || self.sse_pending.is_some()
            || self.websocket_pending.is_some()
            || self.script_pending.is_some();
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
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!busy, egui::Button::new("Inspect schema"))
                        .on_hover_text(
                            "Fetch the endpoint schema through GraphQL introspection and open the local explorer",
                        )
                        .clicked()
                    {
                        inspect_schema_clicked = true;
                    }
                    ui.label(
                        RichText::new("Uses the current endpoint, headers and auth.")
                            .small()
                            .color(MUTED),
                    );
                });
            }
            BodyKind::FormUrlEncoded => {
                ui.label(
                    RichText::new(
                        "Each enabled field is sent as application/x-www-form-urlencoded.",
                    )
                    .small()
                    .color(MUTED),
                );
                ui.add_space(8.0);
                if let RequestBody::FormUrlEncoded { fields } = &mut self.request.body {
                    self.dirty |=
                        render_key_values(ui, fields, "form-url-encoded", "＋ Add form field");
                }
            }
            BodyKind::Multipart => {
                ui.label(
                    RichText::new(
                        "Use a value for text parts or a file path for upload parts. Disabled parts are not sent.",
                    )
                    .small()
                    .color(MUTED),
                );
                ui.add_space(8.0);
                if let RequestBody::Multipart { parts } = &mut self.request.body {
                    self.dirty |= render_multipart_parts(ui, parts);
                }
            }
            BodyKind::BinaryFile => {
                ui.label(
                    RichText::new(
                        "The file is read only when the request is sent; its contents stay outside the project model.",
                    )
                    .small()
                    .color(MUTED),
                );
                ui.add_space(8.0);
                if let RequestBody::BinaryFile { path, content_type } = &mut self.request.body {
                    let mut changed = labeled_singleline(ui, "File path", path);
                    let mut content_type_value = content_type.clone().unwrap_or_default();
                    if labeled_singleline(ui, "Content type", &mut content_type_value) {
                        *content_type =
                            (!content_type_value.trim().is_empty()).then_some(content_type_value);
                        changed = true;
                    }
                    self.dirty |= changed;
                }
            }
            BodyKind::Advanced => {
                ui.label(
                    RichText::new(
                        "This body uses an older unsupported editor state; choose a specific body format above.",
                    )
                    .color(MUTED),
                );
            }
        }
        if inspect_schema_clicked {
            if let Err(error) = self.start_graphql_schema() {
                self.status_message = format!("Schema introspection failed: {error}");
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
                    AuthKind::OAuth2ClientCredentials,
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
            AuthKind::OAuth2ClientCredentials => {
                ui.label(
                    RichText::new(
                        "Client Credentials fetches a token locally and sends it as Authorization.",
                    )
                    .small()
                    .color(MUTED),
                );
                ui.label("Token URL");
                if ui
                    .add(TextEdit::singleline(&mut self.auth_primary))
                    .changed()
                {
                    self.dirty = true;
                }
                ui.label("Client ID");
                if ui
                    .add(TextEdit::singleline(&mut self.auth_secondary))
                    .changed()
                {
                    self.dirty = true;
                }
                ui.label("Client secret");
                if ui
                    .add(TextEdit::singleline(&mut self.auth_tertiary).password(true))
                    .changed()
                {
                    self.dirty = true;
                }
                ui.label("Scope (optional)");
                if ui
                    .add(TextEdit::singleline(&mut self.auth_quaternary))
                    .changed()
                {
                    self.dirty = true;
                }
            }
        }
    }

    fn render_transport(&mut self, ui: &mut egui::Ui) {
        ui.heading(RichText::new("Connection settings").color(Color32::WHITE));
        ui.label(
            RichText::new(
                "These local settings apply to HTTP requests and SSE streams. Only file paths are stored.",
            )
            .small()
            .color(MUTED),
        );
        ui.add_space(10.0);

        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label("Timeout");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut self.transport.timeout_seconds)
                        .range(1..=3_600)
                        .speed(0.2),
                )
                .changed();
            ui.label("seconds");
        });
        ui.add_space(6.0);
        ui.label(
            RichText::new("HTTP(S) proxy")
                .strong()
                .color(Color32::WHITE),
        );
        changed |= ui
            .add(
                TextEdit::singleline(&mut self.transport.proxy_url)
                    .desired_width(460.0)
                    .hint_text("http://127.0.0.1:8080 (optional)"),
            )
            .changed();
        ui.label(
            RichText::new("SOCKS and WebSocket proxying are not included in this slice.")
                .small()
                .color(MUTED),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new("Additional CA certificate (PEM)")
                .strong()
                .color(Color32::WHITE),
        );
        changed |= ui
            .add(
                TextEdit::singleline(&mut self.transport.ca_cert_path)
                    .desired_width(460.0)
                    .hint_text("/path/to/company-ca.pem (optional)"),
            )
            .changed();
        ui.add_space(6.0);
        ui.label(
            RichText::new("Client identity (combined PEM)")
                .strong()
                .color(Color32::WHITE),
        );
        changed |= ui
            .add(
                TextEdit::singleline(&mut self.transport.client_identity_path)
                    .desired_width(460.0)
                    .hint_text("/path/to/client-cert-and-key.pem (optional)"),
            )
            .changed();
        ui.label(
            RichText::new("The PEM contains the client certificate chain followed by an unencrypted private key.")
                .small()
                .color(MUTED),
        );
        ui.add_space(8.0);
        if ui
            .checkbox(
                &mut self.transport.insecure_tls,
                "Disable TLS certificate verification (unsafe)",
            )
            .changed()
        {
            changed = true;
        }
        if self.transport.insecure_tls {
            ui.colored_label(
                Color32::from_rgb(240, 165, 90),
                "Warning: certificate verification is disabled for this local GUI session.",
            );
        }
        if changed {
            self.transport_settings_dirty = true;
        }
        ui.add_space(10.0);
        let mut save_clicked = false;
        let mut validate_clicked = false;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.transport_settings_dirty,
                    egui::Button::new("Save settings"),
                )
                .clicked()
            {
                save_clicked = true;
            }
            if ui.button("Validate & apply").clicked() {
                validate_clicked = true;
            }
            if self.transport_settings_dirty {
                ui.label(RichText::new("unsaved").small().color(MUTED));
            }
        });
        if save_clicked {
            if let Err(error) = self.save_transport_settings() {
                self.status_message = format!("Settings save failed: {error}");
            }
        }
        if validate_clicked {
            match self.configured_engine() {
                Ok(_) => {
                    self.status_message =
                        "Connection settings valid and applied to the next request".to_owned();
                }
                Err(error) => self.status_message = error,
            }
        }
        ui.add_space(8.0);
        ui.label(
            RichText::new("Stored locally at .postly/gui-settings.json, which is ignored by Git.")
                .small()
                .color(MUTED),
        );
    }

    fn render_scripts(&mut self, ui: &mut egui::Ui) {
        ui.heading(RichText::new("Scripts").color(Color32::WHITE));
        ui.label(
            RichText::new(
                "Preserved Postman scripts are editable here. Execution is always explicit; GUI runs are local previews and never persist script changes automatically.",
            )
            .small()
            .color(MUTED),
        );
        let script_busy = self.script_pending.is_some()
            || self.pending.is_some()
            || self.sse_pending.is_some()
            || self.websocket_pending.is_some();
        let has_response = self.response.is_some();
        let mut run_pre_request_clicked = false;
        let mut run_tests_clicked = false;
        ui.add_space(8.0);
        ui.label(
            RichText::new("Pre-request script")
                .strong()
                .color(Color32::WHITE),
        );
        if ui
            .add(
                TextEdit::multiline(&mut self.pre_request_script)
                    .font(TextStyle::Monospace)
                    .desired_rows(9)
                    .desired_width(f32::INFINITY)
                    .hint_text("pm.variables.set('token', 'value');"),
            )
            .changed()
        {
            self.dirty = true;
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !script_busy && !self.pre_request_script.trim().is_empty(),
                    egui::Button::new("Run pre-request preview"),
                )
                .clicked()
            {
                run_pre_request_clicked = true;
            }
            ui.label(
                RichText::new("Runs with the current request and variable snapshot.")
                    .small()
                    .color(MUTED),
            );
        });
        ui.add_space(10.0);
        ui.label(
            RichText::new("Post-response / test script")
                .strong()
                .color(Color32::WHITE),
        );
        if ui
            .add(
                TextEdit::multiline(&mut self.test_script)
                    .font(TextStyle::Monospace)
                    .desired_rows(12)
                    .desired_width(f32::INFINITY)
                    .hint_text("pm.test('status is 200', function () { ... });"),
            )
            .changed()
        {
            self.dirty = true;
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !script_busy && has_response && !self.test_script.trim().is_empty(),
                    egui::Button::new("Run tests against response"),
                )
                .clicked()
            {
                run_tests_clicked = true;
            }
            if !has_response {
                ui.label(
                    RichText::new("Send the request first to provide a response.")
                        .small()
                        .color(MUTED),
                );
            }
        });
        ui.add_space(8.0);
        ui.label(
            RichText::new(
                "The current script bridge is opt-in and requires Node.js. GUI runs are previews only; this is not a sandbox for hostile code. See docs/scripting.md.",
            )
            .small()
            .color(MUTED),
        );
        if let Some(error) = &self.script_error {
            ui.add_space(8.0);
            ui.colored_label(Color32::from_rgb(240, 125, 105), error);
        }
        if let Some(report) = &self.script_report {
            ui.add_space(8.0);
            let failed = report.result.failed_tests().count();
            ui.group(|ui| {
                ui.label(
                    RichText::new(format!(
                        "Last {} preview · {} test(s), {} log entr{}",
                        report.kind.label(),
                        report.result.tests.len(),
                        report.result.logs.len(),
                        if report.result.logs.len() == 1 {
                            "y"
                        } else {
                            "ies"
                        }
                    ))
                    .strong()
                    .color(if failed == 0 {
                        Color32::from_rgb(100, 205, 145)
                    } else {
                        Color32::from_rgb(240, 125, 105)
                    }),
                );
                for test in &report.result.tests {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(if test.passed { "✓" } else { "✗" });
                        ui.label(&test.name);
                        if let Some(error) = &test.error {
                            ui.label(RichText::new(error).small().color(MUTED));
                        }
                    });
                }
                if !report.result.logs.is_empty() {
                    egui::CollapsingHeader::new("Captured logs (local only)")
                        .default_open(false)
                        .show(ui, |ui| {
                            for log in &report.result.logs {
                                ui.monospace(format!("[{}] {}", log.level, log.message));
                            }
                        });
                }
                ui.label(
                    RichText::new(
                        "Preview output is not applied to the saved request or environment.",
                    )
                    .small()
                    .color(MUTED),
                );
            });
        }
        if run_pre_request_clicked {
            if let Err(error) = self.start_script(ScriptRunKind::PreRequest) {
                self.status_message = format!("Script preview failed: {error}");
            }
        }
        if run_tests_clicked {
            if let Err(error) = self.start_script(ScriptRunKind::Tests) {
                self.status_message = format!("Script preview failed: {error}");
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
                    if self.websocket_started {
                        let state = if self.websocket_connected {
                            "connected"
                        } else {
                            "closed"
                        };
                        ui.label(
                            RichText::new(format!(
                                "WS {state} · {} message{}",
                                self.websocket_messages.len(),
                                if self.websocket_messages.len() == 1 {
                                    ""
                                } else {
                                    "s"
                                }
                            ))
                            .color(if self.websocket_connected {
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
                    if self.websocket_started
                        && tab_button(ui, self.response_tab == ResponseTab::WebSocket, "WebSocket")
                            .clicked()
                    {
                        self.response_tab = ResponseTab::WebSocket;
                    }
                    if (self.graphql_schema.is_some() || self.pending_graphql_schema)
                        && tab_button(
                            ui,
                            self.response_tab == ResponseTab::GraphqlSchema,
                            "Schema",
                        )
                        .clicked()
                    {
                        self.response_tab = ResponseTab::GraphqlSchema;
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
                    } else if self.websocket_started {
                        self.render_websocket_content(ui);
                    }
                } else if self.response_tab == ResponseTab::GraphqlSchema {
                    self.render_graphql_schema_content(ui);
                } else if let Some(response) = &self.response {
                    self.render_response_content(ui, response);
                } else if self.sse_started {
                    self.render_sse_content(ui);
                } else if self.websocket_started {
                    self.render_websocket_content(ui);
                } else if self.pending.is_some()
                    || self.sse_pending.is_some()
                    || self.websocket_pending.is_some()
                    || self.script_pending.is_some()
                {
                    ui.label(RichText::new("Waiting for the local protocol worker…").color(MUTED));
                } else {
                    ui.label(
                        RichText::new("Send a request to inspect its response here.").color(MUTED),
                    );
                }
            });
    }

    fn render_websocket_content(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(
                    self.websocket_url
                        .as_deref()
                        .unwrap_or("WebSocket endpoint"),
                )
                .small()
                .color(MUTED),
            );
        });
        ui.add_space(5.0);
        if self.websocket_messages.is_empty() {
            ui.label(RichText::new("Connected — send a text message below.").color(MUTED));
        } else {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (index, message) in self.websocket_messages.iter().enumerate() {
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                let (label, color) = match message.direction {
                                    WebSocketDirection::Sent => ("OUT", ACCENT),
                                    WebSocketDirection::Received => {
                                        ("IN", Color32::from_rgb(100, 205, 145))
                                    }
                                };
                                ui.label(
                                    RichText::new(format!("#{} {label}", index + 1))
                                        .strong()
                                        .color(color),
                                );
                                ui.label(RichText::new(&message.kind).small().color(MUTED));
                                ui.label(RichText::new(&message.received_at).small().color(MUTED));
                            });
                            ui.add_space(3.0);
                            ui.add(
                                egui::Label::new(RichText::new(&message.data).monospace()).wrap(),
                            );
                        });
                        ui.add_space(4.0);
                    }
                });
        }
        ui.separator();
        ui.horizontal(|ui| {
            let send = ui
                .add(
                    TextEdit::singleline(&mut self.websocket_input)
                        .hint_text("Text message")
                        .desired_width(ui.available_width() - 110.0),
                )
                .lost_focus()
                && ui.input(|input| input.key_pressed(egui::Key::Enter));
            if (ui.button("Send text").clicked() || send) && self.websocket_connected {
                if let Err(error) = self.send_websocket_text() {
                    self.response_error = Some(error);
                }
            }
            if ui.button("Close").clicked() {
                self.close_websocket();
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

    fn render_graphql_schema_content(&mut self, ui: &mut egui::Ui) {
        if self.pending_graphql_schema {
            ui.label(RichText::new("Fetching GraphQL schema…").color(MUTED));
            return;
        }
        if let Some(error) = &self.graphql_schema_error {
            ui.colored_label(Color32::from_rgb(240, 125, 105), error);
            ui.label(
                RichText::new(
                    "The endpoint may disable introspection or return an incomplete schema.",
                )
                .small()
                .color(MUTED),
            );
            return;
        }
        let Some(schema) = self.graphql_schema.as_ref() else {
            ui.label(
                RichText::new("Choose Inspect schema in the GraphQL body editor.").color(MUTED),
            );
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Roots").strong().color(Color32::WHITE));
            for (label, value) in [
                ("query", schema.query_type.as_deref()),
                ("mutation", schema.mutation_type.as_deref()),
                ("subscription", schema.subscription_type.as_deref()),
            ] {
                ui.label(
                    RichText::new(format!("{label}: {}", value.unwrap_or("—")))
                        .small()
                        .color(MUTED),
                );
            }
            ui.label(
                RichText::new(format!("{} named types", schema.types.len()))
                    .small()
                    .color(MUTED),
            );
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Filter").small().color(MUTED));
            ui.add(
                TextEdit::singleline(&mut self.graphql_schema_search)
                    .hint_text("type, field or description")
                    .desired_width(260.0),
            );
        });
        let query = self.graphql_schema_search.trim().to_ascii_lowercase();
        ui.add_space(6.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for graphql_type in &schema.types {
                    let type_text = format!(
                        "{} {} {}",
                        graphql_type.kind,
                        graphql_type.name,
                        graphql_type.description.as_deref().unwrap_or("")
                    )
                    .to_ascii_lowercase();
                    let matching_fields = graphql_type
                        .fields
                        .iter()
                        .filter(|field| {
                            query.is_empty()
                                || type_text.contains(&query)
                                || format!(
                                    "{} {} {}",
                                    field.name,
                                    field.type_name,
                                    field.description.as_deref().unwrap_or("")
                                )
                                .to_ascii_lowercase()
                                .contains(&query)
                        })
                        .collect::<Vec<_>>();
                    if !query.is_empty()
                        && matching_fields.is_empty()
                        && !type_text.contains(&query)
                    {
                        continue;
                    }
                    let is_root = [
                        schema.query_type.as_deref(),
                        schema.mutation_type.as_deref(),
                        schema.subscription_type.as_deref(),
                    ]
                    .into_iter()
                    .flatten()
                    .any(|name| name == graphql_type.name);
                    egui::CollapsingHeader::new(format!(
                        "{} {}",
                        graphql_type.kind, graphql_type.name
                    ))
                    .default_open(is_root || !query.is_empty())
                    .show(ui, |ui| {
                        if let Some(description) = &graphql_type.description {
                            ui.label(RichText::new(description).small().color(MUTED));
                        }
                        for field in &matching_fields {
                            let arguments = if field.arguments.is_empty() {
                                String::new()
                            } else {
                                format!(
                                    "({})",
                                    field
                                        .arguments
                                        .iter()
                                        .map(|argument| {
                                            format!("{}: {}", argument.name, argument.type_name)
                                        })
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                )
                            };
                            ui.horizontal_wrapped(|ui| {
                                ui.monospace(format!(
                                    "{}{}: {}",
                                    field.name, arguments, field.type_name
                                ));
                                if field.deprecated {
                                    ui.label(
                                        RichText::new("deprecated")
                                            .small()
                                            .color(Color32::from_rgb(235, 180, 80)),
                                    );
                                }
                            });
                            if let Some(description) = &field.description {
                                ui.label(RichText::new(description).small().color(MUTED));
                            }
                        }
                        for input in &graphql_type.input_fields {
                            ui.monospace(format!("{}: {}", input.name, input.type_name));
                        }
                        if !graphql_type.enum_values.is_empty() {
                            ui.label(RichText::new("Values").small().strong().color(MUTED));
                            ui.horizontal_wrapped(|ui| {
                                for value in &graphql_type.enum_values {
                                    ui.monospace(&value.name);
                                }
                            });
                        }
                        if !graphql_type.possible_types.is_empty() {
                            ui.label(
                                RichText::new(format!(
                                    "Possible types: {}",
                                    graphql_type.possible_types.join(", ")
                                ))
                                .small()
                                .color(MUTED),
                            );
                        }
                    });
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
                ui.label(format!("Response size: {} bytes", response.response_size));
                ui.label(format!("Final URL: {}", response.url));
            }
            ResponseTab::SseEvents => self.render_sse_content(ui),
            ResponseTab::WebSocket => {
                ui.label(RichText::new("WebSocket console is not active.").color(MUTED));
            }
            ResponseTab::GraphqlSchema => {
                ui.label(
                    RichText::new("Open the Schema tab to browse GraphQL types.").color(MUTED),
                );
            }
        }
    }
}

fn resolve_websocket_value(input: &str, context: &VariableContext) -> Result<String, String> {
    let resolved = context.resolve(input);
    if resolved.diagnostics.is_empty() {
        Ok(resolved.value)
    } else {
        Err(resolved
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>()
            .join("; "))
    }
}

fn build_websocket_request(
    request: &Request,
    context: &VariableContext,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>, String> {
    let resolved_url = resolve_websocket_value(&request.url, context)?;
    let mut url = url::Url::parse(&resolved_url).map_err(|error| error.to_string())?;
    if !matches!(url.scheme(), "ws" | "wss") {
        return Err("WebSocket endpoint must use ws:// or wss://".to_owned());
    }
    for pair in request.query.iter().filter(|pair| pair.enabled) {
        let key = resolve_websocket_value(&pair.key, context)?;
        let value = resolve_websocket_value(&pair.value, context)?;
        url.query_pairs_mut().append_pair(&key, &value);
    }
    if let Auth::ApiKey {
        key,
        value,
        location: ApiKeyLocation::Query,
    } = &request.auth
    {
        let key = resolve_websocket_value(key, context)?;
        let value = resolve_websocket_value(value, context)?;
        url.query_pairs_mut().append_pair(&key, &value);
    }

    let mut websocket_request = url
        .as_str()
        .into_client_request()
        .map_err(|error| format!("invalid WebSocket request: {error}"))?;
    for header in request.headers.iter().filter(|header| header.enabled) {
        let key = resolve_websocket_value(&header.key, context)?;
        let value = resolve_websocket_value(&header.value, context)?;
        let name = HeaderName::from_bytes(key.as_bytes())
            .map_err(|error| format!("invalid WebSocket header name {key}: {error}"))?;
        let value = HeaderValue::from_str(&value)
            .map_err(|error| format!("invalid WebSocket header value for {key}: {error}"))?;
        websocket_request.headers_mut().insert(name, value);
    }
    if !request.cookies.is_empty()
        && !request
            .headers
            .iter()
            .any(|header| header.enabled && header.key.eq_ignore_ascii_case("cookie"))
    {
        let cookie = request
            .cookies
            .iter()
            .filter(|pair| pair.enabled)
            .map(|pair| {
                Ok::<_, String>(format!(
                    "{}={}",
                    resolve_websocket_value(&pair.key, context)?,
                    resolve_websocket_value(&pair.value, context)?
                ))
            })
            .collect::<Result<Vec<_>, _>>()?
            .join("; ");
        if !cookie.is_empty() {
            websocket_request.headers_mut().insert(
                HeaderName::from_static("cookie"),
                HeaderValue::from_str(&cookie).map_err(|error| error.to_string())?,
            );
        }
    }

    match &request.auth {
        Auth::None => {}
        Auth::Bearer { token } => {
            let token = resolve_websocket_value(token, context)?;
            websocket_request.headers_mut().insert(
                HeaderName::from_static("authorization"),
                HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|error| error.to_string())?,
            );
        }
        Auth::Basic { username, password } => {
            let username = resolve_websocket_value(username, context)?;
            let password = resolve_websocket_value(password, context)?;
            let credentials =
                base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
            websocket_request.headers_mut().insert(
                HeaderName::from_static("authorization"),
                HeaderValue::from_str(&format!("Basic {credentials}"))
                    .map_err(|error| error.to_string())?,
            );
        }
        Auth::ApiKey {
            key,
            value,
            location: ApiKeyLocation::Header,
        } => {
            let key = resolve_websocket_value(key, context)?;
            let value = resolve_websocket_value(value, context)?;
            let name = HeaderName::from_bytes(key.as_bytes())
                .map_err(|error| format!("invalid WebSocket API key name {key}: {error}"))?;
            websocket_request.headers_mut().insert(
                name,
                HeaderValue::from_str(&value).map_err(|error| error.to_string())?,
            );
        }
        Auth::ApiKey {
            location: ApiKeyLocation::Query,
            ..
        } => {}
        Auth::OAuth2ClientCredentials { .. } => {
            return Err(
                "OAuth 2.0 client credentials are currently supported for HTTP requests, not WebSockets"
                    .to_owned(),
            );
        }
    }
    Ok(websocket_request)
}

fn grpc_path_from_workspace(root: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value.trim());
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

fn apply_grpc_metadata<T>(
    request: &mut tonic::Request<T>,
    config: &GrpcRequest,
    auth: &Auth,
    context: &VariableContext,
) -> Result<(), String> {
    for pair in config.metadata.iter().filter(|pair| pair.enabled) {
        let key = resolve_websocket_value(&pair.key, context)?.to_ascii_lowercase();
        if key.is_empty() {
            return Err("gRPC metadata key cannot be empty".to_owned());
        }
        let key = key
            .parse::<tonic::metadata::MetadataKey<tonic::metadata::Ascii>>()
            .map_err(|error| format!("invalid gRPC metadata key {key}: {error}"))?;
        let value = resolve_websocket_value(&pair.value, context)?
            .parse::<tonic::metadata::MetadataValue<tonic::metadata::Ascii>>()
            .map_err(|error| format!("invalid ASCII gRPC metadata value: {error}"))?;
        request.metadata_mut().insert(key, value);
    }
    match auth {
        Auth::None => {}
        Auth::Bearer { token } => {
            let token = resolve_websocket_value(token, context)?;
            let value = format!("Bearer {token}")
                .parse()
                .map_err(|error| format!("invalid bearer token: {error}"))?;
            request.metadata_mut().insert("authorization", value);
        }
        Auth::Basic { username, password } => {
            let username = resolve_websocket_value(username, context)?;
            let password = resolve_websocket_value(password, context)?;
            let credentials =
                base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
            let value = format!("Basic {credentials}")
                .parse()
                .map_err(|error| format!("invalid basic credentials: {error}"))?;
            request.metadata_mut().insert("authorization", value);
        }
        Auth::ApiKey {
            key,
            value,
            location: ApiKeyLocation::Header,
        } => {
            let key = resolve_websocket_value(key, context)?.to_ascii_lowercase();
            let key = key
                .parse::<tonic::metadata::MetadataKey<tonic::metadata::Ascii>>()
                .map_err(|error| format!("invalid gRPC API-key metadata key {key}: {error}"))?;
            let value = resolve_websocket_value(value, context)?
                .parse::<tonic::metadata::MetadataValue<tonic::metadata::Ascii>>()
                .map_err(|error| format!("invalid gRPC API-key metadata value: {error}"))?;
            request.metadata_mut().insert(key, value);
        }
        Auth::ApiKey {
            location: ApiKeyLocation::Query,
            ..
        } => {
            return Err("gRPC API keys must use metadata/header placement".to_owned());
        }
        Auth::OAuth2ClientCredentials { .. } => {
            return Err(
                "OAuth 2.0 client credentials are not yet supported for native gRPC calls"
                    .to_owned(),
            );
        }
    }
    Ok(())
}

fn build_grpc_endpoint(
    endpoint_url: &str,
    transport: &TransportSettings,
    root: &Path,
) -> Result<Endpoint, String> {
    if !transport.proxy_url.trim().is_empty() {
        return Err("gRPC GUI calls do not yet support HTTP proxy routing".to_owned());
    }
    if transport.insecure_tls {
        return Err(
            "gRPC GUI calls require verified TLS; disable insecure TLS for this request".to_owned(),
        );
    }
    let parsed = url::Url::parse(endpoint_url).map_err(|error| error.to_string())?;
    let mut endpoint = Endpoint::from_shared(endpoint_url.to_owned())
        .map_err(|error| format!("invalid gRPC endpoint: {error}"))?
        .timeout(Duration::from_secs(transport.timeout_seconds.max(1)));
    match parsed.scheme() {
        "http" => {
            if !transport.ca_cert_path.trim().is_empty()
                || !transport.client_identity_path.trim().is_empty()
            {
                return Err("gRPC CA and client identity require an https:// endpoint".to_owned());
            }
        }
        "https" => {
            let domain = parsed
                .host_str()
                .ok_or_else(|| "gRPC HTTPS endpoint has no hostname".to_owned())?;
            let mut tls = ClientTlsConfig::new()
                .domain_name(domain)
                .with_webpki_roots();
            if !transport.ca_cert_path.trim().is_empty() {
                let path = grpc_path_from_workspace(root, &transport.ca_cert_path);
                let pem = fs::read(&path).map_err(|error| {
                    format!(
                        "could not read gRPC CA certificate {}: {error}",
                        path.display()
                    )
                })?;
                if pem.is_empty() {
                    return Err(format!("gRPC CA certificate {} is empty", path.display()));
                }
                tls = tls.ca_certificate(Certificate::from_pem(pem));
            }
            if !transport.client_identity_path.trim().is_empty() {
                let path = grpc_path_from_workspace(root, &transport.client_identity_path);
                let pem = fs::read(&path).map_err(|error| {
                    format!(
                        "could not read gRPC client identity {}: {error}",
                        path.display()
                    )
                })?;
                if pem.is_empty() {
                    return Err(format!("gRPC client identity {} is empty", path.display()));
                }
                tls = tls.identity(Identity::from_pem(&pem, &pem));
            }
            endpoint = endpoint
                .tls_config(tls)
                .map_err(|error| format!("invalid gRPC TLS configuration: {error}"))?;
        }
        scheme => {
            return Err(format!(
                "gRPC endpoint must use http:// or https://, got {scheme}://"
            ));
        }
    }
    Ok(endpoint)
}

async fn execute_grpc_request(
    request: Request,
    context: VariableContext,
    transport: TransportSettings,
    root: PathBuf,
) -> Result<HttpResponse, String> {
    let config = request
        .grpc
        .clone()
        .ok_or_else(|| "gRPC configuration is missing".to_owned())?;
    let endpoint_url = resolve_websocket_value(&request.url, &context)?;
    let endpoint = build_grpc_endpoint(&endpoint_url, &transport, &root)?;
    let started = std::time::Instant::now();
    let channel = endpoint
        .connect()
        .await
        .map_err(|error| format!("could not connect to gRPC endpoint {endpoint_url}: {error}"))?;
    let schema = if config.reflection {
        let host = resolve_websocket_value(&config.reflection_host, &context)?;
        GrpcSchema::from_reflection(channel.clone(), host)
            .await
            .map_err(|error| {
                format!("could not discover gRPC schema through reflection: {error}")
            })?
    } else {
        let proto =
            grpc_path_from_workspace(&root, &resolve_websocket_value(&config.proto, &context)?);
        let includes = config
            .includes
            .iter()
            .map(|include| {
                Ok(grpc_path_from_workspace(
                    &root,
                    &resolve_websocket_value(include, &context)?,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        GrpcSchema::from_proto(&proto, &includes).map_err(|error| error.to_string())?
    };
    let method_name = resolve_websocket_value(&config.method, &context)?;
    let method = schema
        .find_method(&method_name)
        .ok_or_else(|| format!("gRPC method not found: {method_name}"))?;
    let body = match &request.body {
        RequestBody::None => serde_json::Value::Object(serde_json::Map::new()),
        RequestBody::Json { value } => value.clone(),
        _ => return Err("gRPC request body must be JSON or empty".to_owned()),
    };
    let auth = request.auth.clone();
    let message_text = serde_json::to_string(&body).map_err(|error| error.to_string())?;
    let request_message = if method.is_client_streaming() {
        None
    } else {
        Some(message_from_json(method.input(), &message_text).map_err(|error| error.to_string())?)
    };
    let stream_messages = if method.is_client_streaming() {
        let values = body
            .as_array()
            .ok_or_else(|| "client-streaming gRPC bodies must be a JSON array".to_owned())?;
        values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let raw = serde_json::to_string(value).map_err(|error| error.to_string())?;
                message_from_json(method.input(), &raw).map_err(|error| {
                    format!("invalid gRPC message at stream index {index}: {error}")
                })
            })
            .collect::<Result<Vec<_>, String>>()?
    } else {
        Vec::new()
    };
    let method_path = format!("/{}/{}", method.parent_service().full_name(), method.name());
    let path = http::uri::PathAndQuery::try_from(method_path.clone())
        .map_err(|error| format!("invalid gRPC method path: {error}"))?;
    let mut grpc = tonic::client::Grpc::new(channel);
    grpc.ready()
        .await
        .map_err(|error| format!("gRPC channel is not ready: {error}"))?;
    let body = if method.is_client_streaming() {
        let input_count = stream_messages.len();
        let request = tonic::Request::new(futures_util::stream::iter(stream_messages));
        let mut request = request;
        apply_grpc_metadata(&mut request, &config, &auth, &context)?;
        if method.is_server_streaming() {
            let response = grpc
                .streaming(
                    request,
                    path,
                    DynamicGrpcCodec {
                        output: method.output(),
                    },
                )
                .await
                .map_err(|error| format!("gRPC call failed: {error}"))?;
            let mut stream = response.into_inner();
            let mut messages = Vec::new();
            while let Some(message) = stream
                .message()
                .await
                .map_err(|error| format!("gRPC stream failed: {error}"))?
            {
                messages.push(message_to_json(&message).map_err(|error| error.to_string())?);
            }
            serde_json::json!({
                "method": method_path,
                "streaming": "bidirectional",
                "input_count": input_count,
                "messages": messages,
            })
        } else {
            let response = grpc
                .client_streaming(
                    request,
                    path,
                    DynamicGrpcCodec {
                        output: method.output(),
                    },
                )
                .await
                .map_err(|error| format!("gRPC call failed: {error}"))?;
            serde_json::json!({
                "method": method_path,
                "streaming": "client",
                "input_count": input_count,
                "response": message_to_json(&response.into_inner()).map_err(|error| error.to_string())?,
            })
        }
    } else {
        let request = tonic::Request::new(
            request_message.expect("non-client-streaming methods have a request message"),
        );
        let mut request = request;
        apply_grpc_metadata(&mut request, &config, &auth, &context)?;
        if method.is_server_streaming() {
            let response = grpc
                .server_streaming(
                    request,
                    path,
                    DynamicGrpcCodec {
                        output: method.output(),
                    },
                )
                .await
                .map_err(|error| format!("gRPC call failed: {error}"))?;
            let mut stream = response.into_inner();
            let mut messages = Vec::new();
            while let Some(message) = stream
                .message()
                .await
                .map_err(|error| format!("gRPC stream failed: {error}"))?
            {
                messages.push(message_to_json(&message).map_err(|error| error.to_string())?);
            }
            serde_json::json!({
                "method": method_path,
                "streaming": "server",
                "messages": messages,
            })
        } else {
            let response = grpc
                .unary(
                    request,
                    path,
                    DynamicGrpcCodec {
                        output: method.output(),
                    },
                )
                .await
                .map_err(|error| format!("gRPC call failed: {error}"))?;
            serde_json::json!({
                "method": method_path,
                "streaming": "unary",
                "response": message_to_json(&response.into_inner()).map_err(|error| error.to_string())?,
            })
        }
    };
    let body = serde_json::to_vec_pretty(&body).map_err(|error| error.to_string())?;
    let response_size = body.len();
    Ok(HttpResponse {
        status: 200,
        status_text: "OK".to_owned(),
        headers: Vec::new(),
        body,
        response_size,
        content_type: Some("application/json".to_owned()),
        duration_ms: started.elapsed().as_millis(),
        protocol: "gRPC".to_owned(),
        url: endpoint_url,
        cookies: Vec::new(),
    })
}

impl eframe::App for PostlyApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ctx.set_visuals(egui::Visuals::dark());
        self.handle_global_shortcuts(&ctx);
        let pending = self.poll_pending();
        self.draw_navigator(ui);
        self.draw_request_header(ui);
        self.draw_response(ui);
        self.draw_editor(ui);
        self.draw_command_palette(&ctx);
        self.draw_curl_import_dialog(&ctx);
        self.draw_environment_editor(&ctx);
        if let Err(error) = self.sync_active_tab() {
            self.status_message = format!("Draft not retained in tab state: {error}");
        }
        if let Err(error) = self.save_tabs_settings() {
            self.status_message = format!("Tab state could not be saved: {error}");
        }
        self.persist_recovery_if_due();
        if pending {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.dirty {
            let _ = self.persist_recovery();
        }
        let _ = self.save_tabs_settings();
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

fn render_multipart_parts(ui: &mut egui::Ui, parts: &mut Vec<MultipartPart>) -> bool {
    let mut changed = false;
    let mut remove = None;
    egui::Grid::new("multipart-parts")
        .striped(true)
        .min_col_width(100.0)
        .show(ui, |ui| {
            ui.label(RichText::new("Enabled").small().color(MUTED));
            ui.label(RichText::new("Name").small().color(MUTED));
            ui.label(RichText::new("Value").small().color(MUTED));
            ui.label(RichText::new("File path").small().color(MUTED));
            ui.label(RichText::new("Content type").small().color(MUTED));
            ui.end_row();
            for (index, part) in parts.iter_mut().enumerate() {
                changed |= ui.checkbox(&mut part.enabled, "").changed();
                changed |= ui.text_edit_singleline(&mut part.name).changed();
                changed |= ui.text_edit_singleline(&mut part.value).changed();
                let mut file_path = part.file_path.clone().unwrap_or_default();
                if ui.text_edit_singleline(&mut file_path).changed() {
                    part.file_path = (!file_path.trim().is_empty()).then_some(file_path);
                    changed = true;
                }
                let mut content_type = part.content_type.clone().unwrap_or_default();
                if ui.text_edit_singleline(&mut content_type).changed() {
                    part.content_type = (!content_type.trim().is_empty()).then_some(content_type);
                    changed = true;
                }
                if ui.small_button("×").clicked() {
                    remove = Some(index);
                }
                ui.end_row();
            }
        });
    if let Some(index) = remove {
        parts.remove(index);
        changed = true;
    }
    if ui.button("＋ Add multipart part").clicked() {
        parts.push(MultipartPart {
            name: String::new(),
            value: String::new(),
            file_path: None,
            content_type: None,
            enabled: true,
        });
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
        convert::Infallible,
        io::{Read, Write},
        net::TcpListener,
        task::{Context, Poll},
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
    fn script_editor_round_trips_pre_request_and_test_sources() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.request.name = "Scripted request".to_owned();
        app.pre_request_script = "pm.variables.set('token', 'local');".to_owned();
        app.test_script = "pm.test('status', function () { pm.response.to.be.ok; });".to_owned();

        app.save_current().expect("save scripted request");
        let reopened = PostlyApp::open(directory.path().to_path_buf()).expect("reopen app");
        assert_eq!(
            reopened.request.pre_request_script.as_deref(),
            Some("pm.variables.set('token', 'local');")
        );
        assert_eq!(
            reopened.request.test_script.as_deref(),
            Some("pm.test('status', function () { pm.response.to.be.ok; });")
        );
        assert_eq!(
            reopened.pre_request_script,
            reopened.request.pre_request_script.clone().unwrap()
        );
        assert_eq!(
            reopened.test_script,
            reopened.request.test_script.clone().unwrap()
        );

        let mut blanked = reopened;
        blanked.test_script = "   ".to_owned();
        assert!(blanked
            .edited_request()
            .expect("blank script is valid")
            .test_script
            .is_none());
    }

    #[test]
    fn dirty_gui_draft_round_trips_through_private_recovery_snapshot() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.request.name = "Unsaved local draft".to_owned();
        app.request.url = "https://api.example.test/draft".to_owned();
        app.body_kind = BodyKind::Json;
        app.body_text = r#"{"draft":true}"#.to_owned();
        app.dirty = true;
        app.persist_recovery().expect("persist recovery");

        let path = recovery_path(directory.path());
        assert!(path.is_file());
        let snapshot = read_recovery_snapshot(directory.path())
            .expect("read recovery")
            .expect("snapshot");
        assert_eq!(snapshot.request.name, "Unsaved local draft");
        assert_eq!(snapshot.request.url, "https://api.example.test/draft");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path)
                    .expect("metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }

        let reopened = PostlyApp::open(directory.path().to_path_buf()).expect("reopen app");
        assert!(reopened.recovery_restored);
        assert!(reopened.request_path.is_none());
        assert_eq!(reopened.request.name, "Unsaved local draft");
        assert_eq!(reopened.body_text, "{\n  \"draft\": true\n}");
    }

    #[test]
    fn recovery_can_be_discarded_without_touching_saved_requests() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.request.name = "Recovered request".to_owned();
        app.dirty = true;
        app.persist_recovery().expect("persist recovery");

        let mut reopened = PostlyApp::open(directory.path().to_path_buf()).expect("reopen app");
        reopened.discard_recovery();
        assert!(!recovery_path(directory.path()).exists());
        assert!(!reopened.recovery_restored);
        assert_eq!(reopened.request.name, "New request");
        assert!(reopened.dirty);
    }

    #[test]
    fn environment_editor_saves_plain_values_preserves_disabled_flags_and_renames() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.open_environment_editor(None);
        app.environment_editor_name = "staging".to_owned();
        app.environment_editor_variables
            .push(EnvironmentVariableDraft {
                key: "baseUrl".to_owned(),
                value: "https://staging.example.test".to_owned(),
                enabled: true,
                secret: false,
                secret_ref: None,
            });
        app.environment_editor_variables
            .push(EnvironmentVariableDraft {
                key: "disabled".to_owned(),
                value: "kept locally".to_owned(),
                enabled: false,
                secret: false,
                secret_ref: None,
            });
        app.save_environment_editor().expect("save environment");
        let original_path = app
            .environments
            .iter()
            .find(|(_, environment)| environment.name == "staging")
            .map(|(path, _)| path.clone())
            .expect("saved environment");
        let saved = app
            .environments
            .iter()
            .find(|(_, environment)| environment.name == "staging")
            .map(|(_, environment)| environment.clone())
            .expect("saved environment model");
        assert_eq!(
            saved.variables["baseUrl"].value,
            "https://staging.example.test"
        );
        assert!(!saved.variables["disabled"].enabled);

        app.open_environment_editor(Some(0));
        app.environment_editor_name = "production".to_owned();
        app.save_environment_editor().expect("rename environment");
        assert!(!original_path.exists());
        assert!(app
            .environments
            .iter()
            .any(|(_, environment)| environment.name == "production"));
    }

    #[test]
    fn saved_request_tabs_reorder_and_restore_from_local_gui_state() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.request.name = "First request".to_owned();
        app.save_current().expect("save first request");
        app.new_request();
        app.request.name = "Second request".to_owned();
        app.save_current().expect("save second request");
        assert_eq!(app.open_tabs.len(), 2);

        let first_path = app
            .open_tabs
            .iter()
            .find_map(|tab| (tab.request.name == "First request").then(|| tab.request_path.clone()))
            .flatten()
            .expect("first tab path");
        let first_index = app
            .requests
            .iter()
            .position(|(_, request)| request.name == "First request")
            .expect("first request");
        app.select_request(first_index);
        assert_eq!(app.request.name, "First request");
        app.move_active_tab(1);
        assert_eq!(app.open_tabs[app.active_tab].request.name, "First request");
        app.close_other_tabs();
        assert_eq!(app.open_tabs.len(), 1);
        assert_eq!(app.open_tabs[0].request.name, "First request");

        app.tabs_settings_dirty = true;
        app.save_tabs_settings().expect("save tab state");
        let settings =
            std::fs::read_to_string(directory.path().join(GUI_TABS_FILE)).expect("tab settings");
        let relative = first_path
            .strip_prefix(directory.path())
            .expect("relative first path");
        assert!(settings.contains(relative.to_string_lossy().as_ref()));

        let reopened = PostlyApp::open(directory.path().to_path_buf()).expect("reopen app");
        assert_eq!(reopened.open_tabs.len(), 1);
        assert_eq!(reopened.request.name, "First request");
    }

    #[test]
    fn script_preview_runs_in_a_worker_without_applying_changes() {
        if std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.pre_request_script = r#"
            pm.request.headers.add({ key: "X-Preview", value: "yes" });
            pm.test("preview ran", function () { pm.expect(true).to.be.true; });
        "#
        .to_owned();
        app.start_script(ScriptRunKind::PreRequest)
            .expect("start preview");
        for _ in 0..400 {
            if !app.poll_script_pending() && app.script_report.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let report = app.script_report.as_ref().expect("preview report");
        assert_eq!(report.kind, ScriptRunKind::PreRequest);
        assert_eq!(report.result.tests.len(), 1);
        assert!(report.result.tests[0].passed);
        assert!(!app
            .request
            .headers
            .iter()
            .any(|header| header.key == "X-Preview"));
    }

    #[test]
    fn workspace_search_opens_a_saved_request_and_returns_to_navigation() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        let collection = app.collections[0].clone();
        let request = Request::new("List payments", "GET", "https://example.com/payments");
        let path = app
            .workspace
            .save_request(&collection, &request)
            .expect("save request");

        app.workspace_search = "payments".to_owned();
        app.refresh_workspace_search();
        assert_eq!(app.workspace_search_results.len(), 1);
        let result = app.workspace_search_results[0].clone();
        assert_eq!(result.path, path.strip_prefix(directory.path()).unwrap());

        app.open_search_result(&result).expect("open result");
        assert_eq!(app.request.name, "List payments");
        assert!(app.workspace_search.is_empty());
        assert!(app.workspace_search_results.is_empty());
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
    fn advanced_body_editors_round_trip_form_multipart_and_file_bodies() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        let bodies = [
            (
                BodyKind::FormUrlEncoded,
                RequestBody::FormUrlEncoded {
                    fields: vec![KeyValue::enabled("grant_type", "client_credentials")],
                },
            ),
            (
                BodyKind::Multipart,
                RequestBody::Multipart {
                    parts: vec![MultipartPart {
                        name: "document".to_owned(),
                        value: String::new(),
                        file_path: Some("fixtures/document.json".to_owned()),
                        content_type: Some("application/json".to_owned()),
                        enabled: true,
                    }],
                },
            ),
            (
                BodyKind::BinaryFile,
                RequestBody::BinaryFile {
                    path: "fixtures/payload.bin".to_owned(),
                    content_type: Some("application/octet-stream".to_owned()),
                },
            ),
        ];
        for (kind, body) in bodies {
            app.request.body = body.clone();
            app.load_request_editors();
            assert_eq!(app.body_kind, kind);
            assert_eq!(
                app.edited_request().expect("advanced body"),
                Request {
                    body,
                    ..app.request.clone()
                }
            );
        }
    }

    #[test]
    fn oauth_client_credentials_editor_round_trips_authentication() {
        let directory = tempfile::tempdir().expect("directory");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.auth_kind = AuthKind::OAuth2ClientCredentials;
        app.auth_primary = "{{tokenUrl}}".to_owned();
        app.auth_secondary = "postly".to_owned();
        app.auth_tertiary = "{{clientSecret}}".to_owned();
        app.auth_quaternary = "read:users".to_owned();

        let request = app.edited_request().expect("OAuth editor state");
        assert_eq!(
            request.auth,
            Auth::OAuth2ClientCredentials {
                token_url: "{{tokenUrl}}".to_owned(),
                client_id: "postly".to_owned(),
                client_secret: "{{clientSecret}}".to_owned(),
                scope: Some("read:users".to_owned()),
            }
        );
        let mut reopened = app;
        reopened.request.auth = request.auth;
        reopened.load_request_editors();
        assert_eq!(reopened.auth_kind, AuthKind::OAuth2ClientCredentials);
        assert_eq!(reopened.auth_primary, "{{tokenUrl}}");
        assert_eq!(reopened.auth_tertiary, "{{clientSecret}}");
    }

    #[test]
    fn command_palette_actions_update_workspace_state() {
        let directory = tempfile::tempdir().expect("directory");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        assert!(app
            .palette_actions()
            .contains(&CommandPaletteAction::NewRequest));
        app.response_wrap = false;
        app.run_palette_action(CommandPaletteAction::ToggleResponseWrap);
        assert!(app.response_wrap);
        app.run_palette_action(CommandPaletteAction::NewRequest);
        assert!(app.request_path.is_none());
        assert!(app.dirty);
        assert!(!app.command_palette_open);
    }

    #[test]
    fn curl_import_action_creates_an_unsaved_request_draft() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        assert!(app
            .palette_actions()
            .contains(&CommandPaletteAction::ImportCurl));
        app.curl_import_text =
            "curl -X POST https://api.example.test/users -H 'Content-Type: application/json' --data-raw '{\"name\":\"Ada\"}'"
                .to_owned();
        let warnings = app.apply_curl_import().expect("import cURL");
        assert!(warnings.is_empty());
        assert_eq!(app.request.method, "POST");
        assert_eq!(app.request.url, "https://api.example.test/users");
        assert!(matches!(app.request.body, RequestBody::Json { .. }));
        assert!(app.request_path.is_none());
        assert!(app.dirty);
    }

    #[test]
    fn grpc_editor_round_trips_a_saved_dynamic_request() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.request.name = "Echo gRPC".to_owned();
        app.request.url = "http://127.0.0.1:50051".to_owned();
        app.request.grpc = Some(GrpcRequest {
            proto: "proto/echo.proto".to_owned(),
            reflection: false,
            reflection_host: String::new(),
            includes: vec!["proto".to_owned(), "third_party".to_owned()],
            method: "/demo.Echo/Echo".to_owned(),
            metadata: vec![KeyValue::enabled("x-request-id", "{{requestId}}")],
        });
        app.request.body = RequestBody::Json {
            value: serde_json::json!({"message": "hello"}),
        };
        app.load_request_editors();

        assert_eq!(app.editor_tab, EditorTab::Grpc);
        assert_eq!(app.grpc_proto_path, "proto/echo.proto");
        assert_eq!(app.grpc_method, "/demo.Echo/Echo");
        let request = app.edited_request().expect("gRPC editor state");
        assert_eq!(request.grpc, app.request.grpc);
        assert_eq!(request.body, app.request.body);

        app.save_current().expect("save gRPC request");
        let reopened = PostlyApp::open(directory.path().to_path_buf()).expect("reopen app");
        assert_eq!(reopened.request.grpc, request.grpc);
        assert_eq!(reopened.request.body, request.body);
        assert_eq!(reopened.editor_tab, EditorTab::Grpc);
    }

    #[test]
    fn grpc_reflection_editor_round_trips_without_a_proto_path() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.request.grpc = Some(GrpcRequest {
            proto: String::new(),
            reflection: true,
            reflection_host: "api.internal.example".to_owned(),
            includes: Vec::new(),
            method: "/demo.Echo/Echo".to_owned(),
            metadata: Vec::new(),
        });
        app.load_request_editors();

        assert!(app.grpc_reflection);
        assert!(app.grpc_proto_path.is_empty());
        assert_eq!(app.grpc_reflection_host, "api.internal.example");
        let request = app.edited_request().expect("reflection editor state");
        assert_eq!(request.grpc, app.request.grpc);
    }

    #[derive(Clone, PartialEq, prost::Message)]
    struct EchoRequest {
        #[prost(string, tag = "1")]
        message: String,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    struct EchoResponse {
        #[prost(string, tag = "1")]
        message: String,
    }

    #[derive(Clone, Default)]
    struct TestGrpcService;

    impl tonic::codegen::Service<tonic::Request<EchoRequest>> for TestGrpcService {
        type Response = tonic::Response<EchoResponse>;
        type Error = tonic::Status;
        type Future = tonic::codegen::BoxFuture<Self::Response, Self::Error>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: tonic::Request<EchoRequest>) -> Self::Future {
            assert_eq!(
                request
                    .metadata()
                    .get("x-test")
                    .and_then(|value| value.to_str().ok()),
                Some("local")
            );
            let message = request.into_inner().message;
            Box::pin(async move {
                Ok(tonic::Response::new(EchoResponse {
                    message: format!("echo:{message}"),
                }))
            })
        }
    }

    #[derive(Clone, Default)]
    struct TestGrpcServer;

    impl tonic::server::NamedService for TestGrpcServer {
        const NAME: &'static str = "demo.Echo";
    }

    impl tonic::codegen::Service<http::Request<tonic::body::Body>> for TestGrpcServer {
        type Response = http::Response<tonic::body::Body>;
        type Error = Infallible;
        type Future = tonic::codegen::BoxFuture<Self::Response, Self::Error>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: http::Request<tonic::body::Body>) -> Self::Future {
            if request.uri().path() == "/demo.Echo/Echo" {
                Box::pin(async move {
                    let mut grpc = tonic::server::Grpc::new(tonic::codec::ProstCodec::default());
                    Ok(grpc.unary(TestGrpcService, request).await)
                })
            } else {
                Box::pin(async move {
                    let mut response = http::Response::new(tonic::body::Body::empty());
                    *response.status_mut() = http::StatusCode::NOT_FOUND;
                    Ok(response)
                })
            }
        }
    }

    #[tokio::test]
    async fn grpc_worker_executes_a_dynamic_unary_call() {
        let directory = tempfile::tempdir().expect("tempdir");
        let proto = directory.path().join("echo.proto");
        std::fs::write(
            &proto,
            r#"
                syntax = "proto3";
                package demo;
                message EchoRequest { string message = 1; }
                message EchoResponse { string message = 1; }
                service Echo {
                    rpc Echo(EchoRequest) returns (EchoResponse);
                }
            "#,
        )
        .expect("proto");

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(TestGrpcServer)
                .serve_with_incoming_shutdown(
                    tonic::transport::server::TcpIncoming::from(listener),
                    async {
                        let _ = shutdown_rx.await;
                    },
                )
                .await
                .expect("gRPC server");
        });

        let mut request = Request::new("Echo gRPC", "POST", format!("http://{address}"));
        request.grpc = Some(GrpcRequest {
            proto: proto.to_string_lossy().into_owned(),
            reflection: false,
            reflection_host: String::new(),
            includes: Vec::new(),
            method: "/demo.Echo/Echo".to_owned(),
            metadata: vec![KeyValue::enabled("x-test", "local")],
        });
        request.body = RequestBody::Json {
            value: serde_json::json!({"message": "hello"}),
        };
        let response = execute_grpc_request(
            request,
            VariableContext::default(),
            TransportSettings::default(),
            directory.path().to_path_buf(),
        )
        .await
        .expect("gRPC call");
        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("response");
        assert_eq!(response.protocol, "gRPC");
        assert_eq!(body["response"]["message"], "echo:hello");

        let _ = shutdown_tx.send(());
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn grpc_worker_discovers_a_schema_through_server_reflection() {
        let directory = tempfile::tempdir().expect("tempdir");
        let proto = directory.path().join("echo.proto");
        std::fs::write(
            &proto,
            r#"
                syntax = "proto3";
                package demo;
                message EchoRequest { string message = 1; }
                message EchoResponse { string message = 1; }
                service Echo {
                    rpc Echo(EchoRequest) returns (EchoResponse);
                }
            "#,
        )
        .expect("proto");
        let descriptors = protox::compile([&proto], vec![directory.path().to_path_buf()])
            .expect("descriptor set");
        let mut encoded = Vec::new();
        descriptors
            .encode(&mut encoded)
            .expect("encode descriptors");

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let reflection = tonic_reflection::server::Builder::configure()
                .register_encoded_file_descriptor_set(&encoded)
                .build_v1()
                .expect("reflection service");
            tonic::transport::Server::builder()
                .add_service(reflection)
                .add_service(TestGrpcServer)
                .serve_with_incoming_shutdown(
                    tonic::transport::server::TcpIncoming::from(listener),
                    async {
                        let _ = shutdown_rx.await;
                    },
                )
                .await
                .expect("gRPC reflection server");
        });

        let mut request = Request::new("Reflected Echo", "POST", format!("http://{address}"));
        request.grpc = Some(GrpcRequest {
            proto: String::new(),
            reflection: true,
            reflection_host: String::new(),
            includes: Vec::new(),
            method: "/demo.Echo/Echo".to_owned(),
            metadata: vec![KeyValue::enabled("x-test", "local")],
        });
        request.body = RequestBody::Json {
            value: serde_json::json!({"message": "reflected"}),
        };
        let response = execute_grpc_request(
            request,
            VariableContext::default(),
            TransportSettings::default(),
            directory.path().to_path_buf(),
        )
        .await
        .expect("reflected gRPC call");
        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("response");
        assert_eq!(response.protocol, "gRPC");
        assert_eq!(body["response"]["message"], "echo:reflected");

        let _ = shutdown_tx.send(());
        server.await.expect("server task");
    }

    #[test]
    fn transport_settings_persist_paths_without_persisting_key_material() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.transport.timeout_seconds = 45;
        app.transport.proxy_url = "http://127.0.0.1:8080".to_owned();
        app.transport.ca_cert_path = "/tmp/company-ca.pem".to_owned();
        app.transport.client_identity_path = "/tmp/client-identity.pem".to_owned();
        app.transport.insecure_tls = true;
        app.transport_settings_dirty = true;
        app.save_transport_settings().expect("save settings");

        let settings = std::fs::read_to_string(directory.path().join(GUI_SETTINGS_FILE))
            .expect("settings file");
        assert!(settings.contains("company-ca.pem"));
        assert!(!settings.contains("BEGIN PRIVATE KEY"));

        let reopened = PostlyApp::open(directory.path().to_path_buf()).expect("reopen app");
        assert_eq!(reopened.transport.timeout_seconds, 45);
        assert_eq!(reopened.transport.proxy_url, "http://127.0.0.1:8080");
        assert_eq!(reopened.transport.ca_cert_path, "/tmp/company-ca.pem");
        assert_eq!(
            reopened.transport.client_identity_path,
            "/tmp/client-identity.pem"
        );
        assert!(reopened.transport.insecure_tls);
        assert!(!reopened.transport_settings_dirty);
    }

    #[test]
    fn transport_settings_surface_missing_certificate_diagnostics_before_network_work() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.transport.ca_cert_path = "/definitely-not-a-postly-ca.pem".to_owned();

        let error = app
            .configured_engine()
            .expect_err("missing CA should be diagnosed");
        assert!(error.contains("could not read CA certificate"));
        assert!(error.contains("definitely-not-a-postly-ca.pem"));
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
            response_size: 11,
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
    fn graphql_schema_worker_fetches_and_parses_a_local_schema() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        let address = listener.local_addr().expect("address");
        let schema_body = br#"{"data":{"__schema":{"queryType":{"name":"Query"},"mutationType":null,"subscriptionType":null,"types":[{"kind":"OBJECT","name":"Query","description":"Root","fields":[{"name":"health","description":"Health check","args":[],"type":{"kind":"SCALAR","name":"String","ofType":null},"isDeprecated":false,"deprecationReason":null}],"inputFields":null,"enumValues":null,"possibleTypes":null}]}}}"#;
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("connection");
            let mut request = [0_u8; 4096];
            let length = stream.read(&mut request).expect("read");
            let request = String::from_utf8_lossy(&request[..length]);
            assert!(request.contains("POST /graphql HTTP/1.1"));
            assert!(request.contains("PostlySchemaIntrospection"));
            let headers = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
                schema_body.len()
            );
            stream.write_all(headers.as_bytes()).expect("headers");
            stream.write_all(schema_body).expect("schema");
        });
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.request.url = format!("http://{address}/graphql");
        app.body_kind = BodyKind::Graphql;
        app.graphql_query = "query Example { health }".to_owned();
        app.graphql_variables = "{}".to_owned();
        app.start_graphql_schema().expect("start schema");

        let mut finished = false;
        for _ in 0..200 {
            if !app.poll_pending() {
                finished = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        server.join().expect("server");
        assert!(finished, "GraphQL schema worker did not finish");
        assert_eq!(app.response_tab, ResponseTab::GraphqlSchema);
        assert_eq!(
            app.graphql_schema
                .as_ref()
                .and_then(|schema| schema.query_type.as_deref()),
            Some("Query")
        );
        assert_eq!(
            app.graphql_schema
                .as_ref()
                .and_then(|schema| schema.named_type("Query"))
                .map(|graphql_type| graphql_type.fields[0].name.as_str()),
            Some("health")
        );
        assert!(app.graphql_schema_error.is_none());
        assert!(app.history.is_empty());
    }

    #[test]
    fn http_worker_can_be_cancelled_while_waiting_for_a_body() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        let address = listener.local_addr().expect("address");
        let (ready_sender, ready_receiver) = std::sync::mpsc::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("connection");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).expect("read");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 5\r\n\r\n",
                )
                .expect("write headers");
            ready_sender.send(()).expect("ready");
            let _ = release_receiver.recv_timeout(Duration::from_secs(2));
            let _ = stream.write_all(b"hello");
        });
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.request.url = format!("http://{address}/cancel");
        app.send_current().expect("send");
        ready_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("body wait reached");

        app.cancel_active();
        let mut finished = false;
        for _ in 0..200 {
            if !app.poll_pending() {
                finished = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        release_sender.send(()).expect("release server");
        server.join().expect("server");
        assert!(finished, "HTTP worker did not cancel");
        assert_eq!(app.status_message, "Request cancelled");
        assert!(app.response_error.is_none());
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

        for index in 0..(MAX_CONSOLE_ITEMS + 25) {
            app.sse_events.push_back(ReceivedSseEvent {
                event: SseEvent {
                    id: Some(index.to_string()),
                    event: None,
                    data: index.to_string(),
                    retry_ms: None,
                },
                received_at: "00:00:00".to_owned(),
            });
            while app.sse_events.len() > MAX_CONSOLE_ITEMS {
                app.sse_events.pop_front();
            }
        }
        assert_eq!(app.sse_events.len(), MAX_CONSOLE_ITEMS);
        assert_eq!(
            app.sse_events
                .front()
                .map(|event| event.event.id.as_deref()),
            Some(Some("25"))
        );
    }

    #[test]
    fn sse_worker_reconnects_with_the_last_event_id() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            let responses: [(Option<&str>, &[u8]); 2] = [
                (None, b"id: first\ndata: one\n\n"),
                (Some("first"), b"id: second\ndata: two\n\n"),
            ];
            for (last_event_id, body) in responses {
                let (mut stream, _) = listener.accept().expect("connection");
                let mut request = [0_u8; 4096];
                let length = stream.read(&mut request).expect("read");
                let request = String::from_utf8_lossy(&request[..length]);
                assert!(request.contains("GET /reconnect-events HTTP/1.1"));
                if let Some(last_event_id) = last_event_id {
                    assert!(request
                        .to_ascii_lowercase()
                        .contains(&format!("last-event-id: {last_event_id}")));
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n",
                    body.len()
                );
                stream.write_all(response.as_bytes()).expect("headers");
                stream.write_all(body).expect("events");
            }
        });
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.request.url = format!("http://{address}/reconnect-events");
        app.sse_reconnect_limit = 1;
        app.start_sse_current().expect("start SSE");

        let mut finished = false;
        for _ in 0..300 {
            if !app.poll_pending() {
                finished = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        server.join().expect("server");
        assert!(finished, "SSE worker did not finish after reconnect");
        assert_eq!(app.sse_events.len(), 2);
        assert_eq!(app.sse_events[0].event.data, "one");
        assert_eq!(app.sse_events[1].event.data, "two");
        assert!(!app.sse_connected);
        assert_eq!(app.status_message, "SSE stream closed · 2 events");
    }

    #[test]
    fn sse_worker_can_be_cancelled_while_waiting_for_the_next_event() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        let address = listener.local_addr().expect("address");
        let (ready_sender, ready_receiver) = std::sync::mpsc::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("connection");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).expect("read");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n")
                .expect("write headers");
            ready_sender.send(()).expect("ready");
            let _ = release_receiver.recv_timeout(Duration::from_secs(2));
        });
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.request.url = format!("http://{address}/cancel-events");
        app.start_sse_current().expect("start SSE");
        ready_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("SSE headers reached");

        let mut connected = false;
        for _ in 0..200 {
            app.poll_pending();
            if app.sse_connected {
                connected = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(connected, "SSE worker did not connect");
        app.cancel_active();
        let mut finished = false;
        for _ in 0..200 {
            if !app.poll_pending() {
                finished = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        release_sender.send(()).expect("release server");
        server.join().expect("server");
        assert!(finished, "SSE worker did not cancel");
        assert_eq!(app.status_message, "SSE stream cancelled");
        assert!(app.response_error.is_none());
    }

    #[test]
    fn websocket_request_resolves_query_api_key_auth() {
        let mut request = Request::new("Query key", "GET", "ws://example.test/socket");
        request.auth = Auth::ApiKey {
            key: "api_key".to_owned(),
            value: "{{token}}".to_owned(),
            location: ApiKeyLocation::Query,
        };
        let mut context = VariableContext::default();
        context.set_runtime("token", "secret value");

        let websocket_request = build_websocket_request(&request, &context).expect("request");
        assert_eq!(
            websocket_request.uri().to_string(),
            "ws://example.test/socket?api_key=secret+value"
        );
        assert!(websocket_request.headers().get("api_key").is_none());
    }

    #[test]
    fn websocket_worker_connects_sends_and_receives_text() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).expect("listener");
                let (stream, _) = listener.accept().await.expect("connection");
                let mut websocket = tokio_tungstenite::accept_async(stream)
                    .await
                    .expect("handshake");
                if let Some(Ok(Message::Text(text))) = websocket.next().await {
                    websocket
                        .send(Message::Text(format!("echo:{text}").into()))
                        .await
                        .expect("echo");
                    websocket.close(None).await.expect("close");
                }
            });
        });
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.request.url = format!("ws://{address}/socket");
        app.start_websocket_current().expect("start WebSocket");

        let mut sent = false;
        let mut finished = false;
        for _ in 0..200 {
            let active = app.poll_pending();
            if app.websocket_connected && !sent {
                app.websocket_input = "hello".to_owned();
                app.send_websocket_text().expect("send text");
                sent = true;
            }
            if sent
                && !active
                && app.websocket_messages.iter().any(|message| {
                    matches!(message.direction, WebSocketDirection::Received)
                        && message.data == "echo:hello"
                })
            {
                finished = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        server.join().expect("server");
        assert!(finished, "WebSocket worker did not finish");
        assert!(!app.websocket_connected);
        assert!(app.websocket_messages.iter().any(|message| {
            matches!(message.direction, WebSocketDirection::Sent)
                && message.kind == "text"
                && message.data == "hello"
        }));
        assert!(app.websocket_messages.iter().any(|message| {
            matches!(message.direction, WebSocketDirection::Received)
                && message.kind == "text"
                && message.data == "echo:hello"
        }));
    }

    #[test]
    fn websocket_worker_can_be_cancelled_after_connecting() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let address = listener.local_addr().expect("address");
        let (ready_sender, ready_receiver) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).expect("listener");
                let (stream, _) = listener.accept().await.expect("connection");
                let mut websocket = tokio_tungstenite::accept_async(stream)
                    .await
                    .expect("handshake");
                ready_sender.send(()).expect("ready");
                let _ = websocket.next().await;
            });
        });
        let directory = tempfile::tempdir().expect("tempdir");
        let mut app = PostlyApp::open(directory.path().to_path_buf()).expect("open app");
        app.request.url = format!("ws://{address}/cancel-socket");
        app.start_websocket_current().expect("start WebSocket");
        ready_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("WebSocket handshake reached");

        let mut connected = false;
        for _ in 0..200 {
            app.poll_pending();
            if app.websocket_connected {
                connected = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(connected, "WebSocket worker did not connect");
        app.cancel_active();
        let mut finished = false;
        for _ in 0..200 {
            if !app.poll_pending() {
                finished = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        server.join().expect("server");
        assert!(finished, "WebSocket worker did not cancel");
        assert_eq!(app.status_message, "WebSocket connection cancelled");
        assert!(app.response_error.is_none());
    }
}
