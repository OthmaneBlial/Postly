use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type Variables = BTreeMap<String, String>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectManifest {
    pub format: String,
    pub version: u32,
    pub name: String,
}

impl ProjectManifest {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            format: "postly".to_owned(),
            version: 1,
            name: name.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Collection {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub variables: Variables,
    #[serde(default)]
    pub auth: Auth,
    #[serde(default)]
    pub pre_request_script: Option<String>,
    #[serde(default)]
    pub test_script: Option<String>,
}

impl Collection {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            description: None,
            variables: Variables::new(),
            auth: Auth::None,
            pre_request_script: None,
            test_script: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Environment {
    pub format: String,
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub variables: BTreeMap<String, EnvironmentVariable>,
}

impl Environment {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            format: "postly-environment".to_owned(),
            version: 1,
            name: name.into(),
            variables: BTreeMap::new(),
        }
    }

    pub fn enabled_values(&self) -> Variables {
        self.variables
            .iter()
            .filter(|(_, variable)| variable.enabled)
            .map(|(key, variable)| (key.clone(), variable.value.clone()))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvironmentVariable {
    pub value: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub secret: bool,
    /// Opaque OS-keychain reference. When present, `value` is intentionally
    /// empty so the secret never enters Git-native environment files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<String>,
}

impl EnvironmentVariable {
    pub fn plain(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            enabled: true,
            secret: false,
            secret_ref: None,
        }
    }

    pub fn keychain(reference: impl Into<String>) -> Self {
        Self {
            value: String::new(),
            enabled: true,
            secret: true,
            secret_ref: Some(reference.into()),
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Request {
    pub id: Uuid,
    pub name: String,
    pub method: String,
    pub url: String,
    /// Optional dynamic gRPC configuration. HTTP requests keep this absent so
    /// old request files remain compact and fully backward-compatible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grpc: Option<GrpcRequest>,
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub query: Vec<KeyValue>,
    #[serde(default)]
    pub headers: Vec<HeaderEntry>,
    #[serde(default)]
    pub cookies: Vec<KeyValue>,
    /// Reusable text payloads for the native WebSocket workspace. These are
    /// intentionally kept separate from the live console history so they can
    /// be versioned with the request and reused across connections.
    #[serde(default)]
    pub websocket_messages: Vec<WebSocketMessage>,
    #[serde(default)]
    pub body: RequestBody,
    #[serde(default)]
    pub auth: Auth,
    #[serde(default)]
    pub pre_request_script: Option<String>,
    #[serde(default)]
    pub test_script: Option<String>,
    #[serde(default)]
    pub examples: Vec<ResponseExample>,
    #[serde(default)]
    pub assertions: Vec<Assertion>,
}

impl Request {
    pub fn new(name: impl Into<String>, method: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            method: method.into(),
            url: url.into(),
            grpc: None,
            folder: None,
            description: None,
            query: Vec::new(),
            headers: Vec::new(),
            cookies: Vec::new(),
            websocket_messages: Vec::new(),
            body: RequestBody::None,
            auth: Auth::None,
            pre_request_script: None,
            test_script: None,
            examples: Vec::new(),
            assertions: Vec::new(),
        }
    }
}

/// A named, Git-friendly text payload for a WebSocket request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebSocketMessage {
    pub name: String,
    pub text: String,
}

impl WebSocketMessage {
    pub fn new(name: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            text: text.into(),
        }
    }
}

/// Persisted configuration for a dynamic gRPC request.
///
/// The protobuf descriptor is compiled from `proto` when the request runs, or
/// discovered from the endpoint's reflection service when `reflection` is set;
/// generated source code is never written into the workspace. Relative proto
/// and include paths are resolved from the workspace root by the native GUI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrpcRequest {
    pub proto: String,
    #[serde(default)]
    pub reflection: bool,
    #[serde(default)]
    pub reflection_host: String,
    #[serde(default)]
    pub includes: Vec<String>,
    pub method: String,
    #[serde(default)]
    pub metadata: Vec<KeyValue>,
}

impl GrpcRequest {
    pub fn new(proto: impl Into<String>, method: impl Into<String>) -> Self {
        Self {
            proto: proto.into(),
            reflection: false,
            reflection_host: String::new(),
            includes: Vec::new(),
            method: method.into(),
            metadata: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JsonValueType {
    Null,
    Boolean,
    Number,
    String,
    Array,
    Object,
}

impl JsonValueType {
    pub fn label(self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Boolean => "boolean",
            Self::Number => "number",
            Self::String => "string",
            Self::Array => "array",
            Self::Object => "object",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Assertion {
    Status {
        expected: u16,
    },
    StatusRange {
        min: u16,
        max: u16,
    },
    HeaderPresent {
        name: String,
    },
    HeaderEquals {
        name: String,
        expected: String,
    },
    HeaderContains {
        name: String,
        value: String,
    },
    BodyContains {
        value: String,
    },
    BodyIsJson,
    CookiePresent {
        name: String,
    },
    CookieEquals {
        name: String,
        expected: String,
    },
    ResponseTimeUnder {
        max_ms: u64,
    },
    JsonPointerPresent {
        pointer: String,
    },
    JsonPointerNotPresent {
        pointer: String,
    },
    JsonPointerEquals {
        pointer: String,
        expected: serde_json::Value,
    },
    JsonPointerContains {
        pointer: String,
        expected: serde_json::Value,
    },
    JsonPointerType {
        pointer: String,
        expected: JsonValueType,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyValue {
    pub key: String,
    pub value: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl KeyValue {
    pub fn enabled(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeaderEntry {
    pub key: String,
    pub value: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl HeaderEntry {
    pub fn enabled(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RequestBody {
    #[default]
    None,
    Raw {
        text: String,
        #[serde(default)]
        content_type: Option<String>,
    },
    Json {
        value: serde_json::Value,
    },
    #[serde(rename = "graphql")]
    Graphql {
        query: String,
        #[serde(default)]
        variables: serde_json::Value,
        #[serde(default)]
        operation_name: Option<String>,
    },
    FormUrlEncoded {
        fields: Vec<KeyValue>,
    },
    Multipart {
        parts: Vec<MultipartPart>,
    },
    BinaryFile {
        path: String,
        #[serde(default)]
        content_type: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MultipartPart {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub file_path: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Auth {
    #[default]
    None,
    Basic {
        username: String,
        password: String,
    },
    Bearer {
        token: String,
    },
    ApiKey {
        key: String,
        value: String,
        #[serde(default)]
        location: ApiKeyLocation,
    },
    OAuth2ClientCredentials {
        token_url: String,
        client_id: String,
        client_secret: String,
        #[serde(default)]
        scope: Option<String>,
    },
    /// OAuth 2.0 Authorization Code with PKCE.
    ///
    /// The authorization step is intentionally explicit: a user completes the
    /// provider login in their browser, then supplies the returned code and
    /// verifier for the local token exchange. Postly never handles provider
    /// credentials or stores the resulting access token on disk.
    OAuth2AuthorizationCodePkce {
        authorization_url: String,
        token_url: String,
        client_id: String,
        redirect_uri: String,
        code: String,
        code_verifier: String,
        #[serde(default)]
        client_secret: Option<String>,
        #[serde(default)]
        scope: Option<String>,
    },
    /// OAuth 2.0 refresh-token exchange.
    OAuth2RefreshToken {
        token_url: String,
        client_id: String,
        refresh_token: String,
        #[serde(default)]
        client_secret: Option<String>,
        #[serde(default)]
        scope: Option<String>,
    },
    /// OAuth 2.0 Device Authorization Grant (RFC 8628).
    ///
    /// The user verification step is surfaced by the caller at runtime. The
    /// device and access tokens are never persisted in the request file.
    OAuth2DeviceCode {
        device_authorization_url: String,
        token_url: String,
        client_id: String,
        #[serde(default)]
        client_secret: Option<String>,
        #[serde(default)]
        scope: Option<String>,
    },
    /// AWS Signature Version 4 request signing.
    ///
    /// Credentials remain part of the local request model and should normally
    /// be supplied through variable or OS credential-store references. The
    /// signature itself is generated only at request time.
    AwsSignatureV4 {
        access_key_id: String,
        secret_access_key: String,
        region: String,
        service: String,
        #[serde(default)]
        session_token: Option<String>,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyLocation {
    #[default]
    Header,
    Query,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResponseExample {
    pub name: String,
    #[serde(default)]
    pub status: Option<u16>,
    #[serde(default)]
    pub headers: Vec<HeaderEntry>,
    #[serde(default)]
    pub body: Option<String>,
    /// Optional local mock delay. Postly-native data only; Postman exports
    /// preserve it under the `x-postly-delay-ms` extension.
    #[serde(default)]
    pub delay_ms: u64,
}
