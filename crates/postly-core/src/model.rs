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
}

impl EnvironmentVariable {
    pub fn plain(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            enabled: true,
            secret: false,
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
            folder: None,
            description: None,
            query: Vec::new(),
            headers: Vec::new(),
            cookies: Vec::new(),
            body: RequestBody::None,
            auth: Auth::None,
            pre_request_script: None,
            test_script: None,
            examples: Vec::new(),
            assertions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Assertion {
    Status {
        expected: u16,
    },
    HeaderPresent {
        name: String,
    },
    HeaderEquals {
        name: String,
        expected: String,
    },
    BodyContains {
        value: String,
    },
    JsonPointerEquals {
        pointer: String,
        expected: serde_json::Value,
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
}
