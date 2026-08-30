use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    model::{
        ApiKeyLocation, Auth, Collection, Environment, EnvironmentVariable, HeaderEntry, KeyValue,
        MultipartPart, Request, RequestBody, ResponseExample,
    },
    storage::{CollectionFiles, Workspace, WorkspaceError},
};

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("could not read import file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid JSON in {path}: {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("Postman collection is missing info.name")]
    MissingCollectionName,
    #[error("workspace error: {0}")]
    Workspace(#[from] WorkspaceError),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImportReport {
    pub source: String,
    pub collection_name: Option<String>,
    pub imported_requests: usize,
    pub fully_supported_requests: usize,
    pub manual_review_requests: usize,
    pub imported_environments: usize,
    pub warnings: Vec<String>,
}

impl ImportReport {
    fn warn(&mut self, message: impl Into<String>) {
        self.warnings.push(message.into());
    }
}

pub fn import_postman_collection(
    collection_path: impl AsRef<Path>,
    output_directory: impl AsRef<Path>,
) -> Result<ImportReport, ImportError> {
    let collection_path = collection_path.as_ref().to_path_buf();
    let text = fs::read_to_string(&collection_path).map_err(|source| ImportError::Read {
        path: collection_path.clone(),
        source,
    })?;
    let document: Value = serde_json::from_str(&text).map_err(|source| ImportError::Json {
        path: collection_path.clone(),
        source,
    })?;
    let name = document
        .pointer("/info/name")
        .and_then(Value::as_str)
        .ok_or(ImportError::MissingCollectionName)?
        .to_owned();
    let workspace = Workspace::open_or_init(&output_directory, name.clone())?;
    let collection = Collection::new(&name);
    let mut collection_files = workspace.create_collection(&collection)?;
    let mut report = ImportReport {
        source: collection_path.display().to_string(),
        collection_name: Some(name),
        ..ImportReport::default()
    };

    if let Some(description) = document
        .pointer("/info/description")
        .and_then(description_text)
    {
        collection_files.collection.description = Some(description);
        workspace.save_collection(&collection_files)?;
    }

    if let Some(variables) = document.get("variable").and_then(Value::as_array) {
        for variable in variables {
            if let (Some(key), Some(value)) = (
                variable.get("key").and_then(Value::as_str),
                variable.get("value").and_then(Value::as_str),
            ) {
                collection_files
                    .collection
                    .variables
                    .insert(key.to_owned(), value.to_owned());
            }
        }
        workspace.save_collection(&collection_files)?;
    }

    if let Some(items) = document.get("item").and_then(Value::as_array) {
        for item in items {
            import_item(&workspace, &collection_files, item, None, &mut report)?;
        }
    }
    Ok(report)
}

pub fn import_environment(
    environment_path: impl AsRef<Path>,
    output_directory: impl AsRef<Path>,
) -> Result<ImportReport, ImportError> {
    let environment_path = environment_path.as_ref().to_path_buf();
    let text = fs::read_to_string(&environment_path).map_err(|source| ImportError::Read {
        path: environment_path.clone(),
        source,
    })?;
    let document: Value = serde_json::from_str(&text).map_err(|source| ImportError::Json {
        path: environment_path.clone(),
        source,
    })?;
    let name = document
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Imported environment");
    let workspace = Workspace::open_or_init(&output_directory, "Postly workspace")?;
    let mut environment = Environment::new(name);
    let mut report = ImportReport {
        source: environment_path.display().to_string(),
        imported_environments: 1,
        ..ImportReport::default()
    };
    if let Some(values) = document.get("values").and_then(Value::as_array) {
        for variable in values {
            let Some(key) = variable.get("key").and_then(Value::as_str) else {
                report.warn("Skipped an environment entry without a key.");
                continue;
            };
            let value = variable
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let enabled = variable
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let secret = variable.get("type").and_then(Value::as_str) == Some("secret");
            environment.variables.insert(
                key.to_owned(),
                EnvironmentVariable {
                    value: value.to_owned(),
                    enabled,
                    secret,
                },
            );
            if !enabled {
                report.warn(format!(
                    "Environment variable {key} is disabled and was preserved as disabled."
                ));
            }
        }
    } else {
        report.warn("Environment has no values array.");
    }
    workspace.save_environment(&environment)?;
    Ok(report)
}

fn import_item(
    workspace: &Workspace,
    collection: &CollectionFiles,
    item: &Value,
    folder: Option<String>,
    report: &mut ImportReport,
) -> Result<(), ImportError> {
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Unnamed item");
    if let Some(children) = item.get("item").and_then(Value::as_array) {
        let next_folder = Some(match folder {
            Some(folder) => format!("{folder}/{name}"),
            None => name.to_owned(),
        });
        for child in children {
            import_item(workspace, collection, child, next_folder.clone(), report)?;
        }
        return Ok(());
    }

    let Some(request_value) = item.get("request") else {
        report.warn(format!(
            "Skipped item {name}: it has neither request nor nested items."
        ));
        return Ok(());
    };
    let mut request = parse_request(name, request_value, folder, report);
    apply_events(&mut request, item, name, report);
    request.examples = parse_examples(item, report);
    let request_path = workspace.save_request(collection, &request)?;
    report.imported_requests += 1;
    if request_needs_review(&request) {
        report.manual_review_requests += 1;
        report.warn(format!(
            "Request {name} requires manual review after import ({})",
            request_path.display()
        ));
    } else {
        report.fully_supported_requests += 1;
    }
    Ok(())
}

fn parse_request(
    name: &str,
    value: &Value,
    folder: Option<String>,
    report: &mut ImportReport,
) -> Request {
    let method = value.get("method").and_then(Value::as_str).unwrap_or("GET");
    let url = parse_url(value.get("url")).unwrap_or_else(|| {
        report.warn(format!(
            "Request {name} has no usable URL; imported with an empty URL."
        ));
        String::new()
    });
    let mut request = Request::new(name, method, url);
    request.folder = folder;
    if let Some(url_object) = value.get("url").and_then(Value::as_object) {
        request.query = parse_pairs(url_object.get("query"));
        if !request.query.is_empty() {
            request.url = request
                .url
                .split_once('?')
                .map(|(base, _)| base.to_owned())
                .unwrap_or(request.url);
        }
    }
    request.description = value.get("description").and_then(description_text);
    request.headers = value
        .get("header")
        .and_then(Value::as_array)
        .map(|headers| {
            headers
                .iter()
                .filter_map(|header| {
                    let key = header.get("key").and_then(Value::as_str)?;
                    let value = header
                        .get("value")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    Some(HeaderEntry {
                        key: key.to_owned(),
                        value: value.to_owned(),
                        enabled: !header
                            .get("disabled")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    request.auth = parse_auth(value.get("auth"));
    request.body = parse_body(name, value.get("body"), report);
    request
}

fn apply_events(request: &mut Request, item: &Value, name: &str, report: &mut ImportReport) {
    let Some(events) = item.get("event").and_then(Value::as_array) else {
        return;
    };
    for event in events {
        let listen = event.get("listen").and_then(Value::as_str);
        let script = event
            .pointer("/script/exec")
            .and_then(Value::as_array)
            .map(|lines| {
                lines
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            });
        match (listen, script) {
            (Some("prerequest"), Some(script)) => request.pre_request_script = Some(script),
            (Some("test"), Some(script)) => request.test_script = Some(script),
            (Some(other), _) => report.warn(format!(
                "Request {name} contains unsupported event type {other}."
            )),
            _ => report.warn(format!(
                "Request {name} contains an event without executable script lines."
            )),
        }
    }
}

fn parse_url(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(url) => Some(url.clone()),
        Value::Object(object) => {
            if let Some(raw) = object.get("raw").and_then(Value::as_str) {
                return Some(raw.to_owned());
            }
            let protocol = object.get("protocol").and_then(Value::as_str)?;
            let host = object.get("host").and_then(Value::as_array)?;
            let mut url = format!(
                "{protocol}://{}",
                host.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(".")
            );
            if let Some(path) = object.get("path").and_then(Value::as_array) {
                url.push('/');
                url.push_str(
                    &path
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("/"),
                );
            }
            Some(url)
        }
        _ => None,
    }
}

fn parse_body(name: &str, value: Option<&Value>, report: &mut ImportReport) -> RequestBody {
    let Some(body) = value else {
        return RequestBody::None;
    };
    let mode = body.get("mode").and_then(Value::as_str).unwrap_or("none");
    match mode {
        "raw" => {
            let text = body
                .get("raw")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let content_type = body
                .pointer("/options/raw/language")
                .and_then(Value::as_str)
                .map(|language| match language {
                    "json" => "application/json".to_owned(),
                    "xml" => "application/xml".to_owned(),
                    "html" => "text/html".to_owned(),
                    _ => format!("text/{language}"),
                });
            if content_type.as_deref() == Some("application/json") {
                if let Ok(value) = serde_json::from_str(&text) {
                    return RequestBody::Json { value };
                }
            }
            RequestBody::Raw { text, content_type }
        }
        "urlencoded" => RequestBody::FormUrlEncoded {
            fields: parse_pairs(body.get("urlencoded")),
        },
        "formdata" => RequestBody::Multipart {
            parts: body
                .get("formdata")
                .and_then(Value::as_array)
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(|part| {
                            let name = part.get("key").and_then(Value::as_str)?.to_owned();
                            let file_path = part
                                .get("src")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned);
                            Some(MultipartPart {
                                name,
                                value: part
                                    .get("value")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned(),
                                file_path,
                                content_type: part
                                    .get("type")
                                    .and_then(Value::as_str)
                                    .map(ToOwned::to_owned),
                                enabled: !part
                                    .get("disabled")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(false),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
        },
        "file" => {
            let path = body
                .pointer("/file/src")
                .and_then(Value::as_str)
                .unwrap_or_default();
            report.warn(format!(
                "Request {name} imports a file body; verify the relative path {path}."
            ));
            RequestBody::BinaryFile {
                path: path.to_owned(),
                content_type: None,
            }
        }
        "graphql" => {
            report.warn(format!("Request {name} uses Postman GraphQL body metadata; imported as raw JSON for review."));
            RequestBody::Raw {
                text: body
                    .get("graphql")
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                content_type: Some("application/json".to_owned()),
            }
        }
        "none" => RequestBody::None,
        other => {
            report.warn(format!(
                "Request {name} uses unsupported body mode {other}."
            ));
            RequestBody::Raw {
                text: body.to_string(),
                content_type: Some("application/json".to_owned()),
            }
        }
    }
}

fn parse_pairs(value: Option<&Value>) -> Vec<KeyValue> {
    value
        .and_then(Value::as_array)
        .map(|pairs| {
            pairs
                .iter()
                .filter_map(|pair| {
                    Some(KeyValue {
                        key: pair.get("key").and_then(Value::as_str)?.to_owned(),
                        value: pair
                            .get("value")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        enabled: !pair
                            .get("disabled")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_auth(value: Option<&Value>) -> Auth {
    let Some(value) = value else {
        return Auth::None;
    };
    match value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("noauth")
    {
        "basic" => Auth::Basic {
            username: auth_value(value.get("basic"), "username"),
            password: auth_value(value.get("basic"), "password"),
        },
        "bearer" => Auth::Bearer {
            token: auth_value(value.get("bearer"), "value"),
        },
        "apikey" => Auth::ApiKey {
            key: auth_value(value.get("apikey"), "key"),
            value: auth_value(value.get("apikey"), "value"),
            location: match auth_value(value.get("apikey"), "in").as_str() {
                "query" => ApiKeyLocation::Query,
                _ => ApiKeyLocation::Header,
            },
        },
        _ => Auth::None,
    }
}

fn auth_value(value: Option<&Value>, key: &str) -> String {
    value
        .and_then(Value::as_array)
        .and_then(|values| {
            values
                .iter()
                .find(|entry| entry.get("key").and_then(Value::as_str) == Some(key))
        })
        .and_then(|entry| entry.get("value").and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned()
}

fn parse_examples(item: &Value, report: &mut ImportReport) -> Vec<ResponseExample> {
    item.get("response")
        .and_then(Value::as_array)
        .map(|examples| {
            examples
                .iter()
                .map(|example| {
                    let name = example
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("Example")
                        .to_owned();
                    let status = example
                        .get("code")
                        .and_then(Value::as_u64)
                        .and_then(|code| u16::try_from(code).ok());
                    let body = example
                        .get("body")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned);
                    let headers = example
                        .get("header")
                        .and_then(Value::as_array)
                        .map(|headers| {
                            headers
                                .iter()
                                .filter_map(|header| {
                                    Some(HeaderEntry::enabled(
                                        header.get("key").and_then(Value::as_str)?,
                                        header
                                            .get("value")
                                            .and_then(Value::as_str)
                                            .unwrap_or_default(),
                                    ))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    ResponseExample {
                        name,
                        status,
                        headers,
                        body,
                    }
                })
                .collect()
        })
        .unwrap_or_else(|| {
            if item.get("response").is_some() {
                report.warn("An example field was present but could not be parsed.");
            }
            Vec::new()
        })
}

fn description_text(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Object(object) => object
            .get("content")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        _ => None,
    }
}

fn request_needs_review(request: &Request) -> bool {
    request.url.is_empty()
        || matches!(request.body, RequestBody::BinaryFile { .. })
        || request.pre_request_script.is_some()
        || request.test_script.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_sanitized_collection_fixture_with_a_report() {
        let output = tempfile::tempdir().expect("output");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../compat/postman-import/basic-v2.1.json");

        let report = import_postman_collection(&fixture, output.path()).expect("import");
        assert_eq!(report.imported_requests, 2);
        assert_eq!(report.fully_supported_requests, 1);
        assert_eq!(report.manual_review_requests, 1);
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("manual review")));

        let workspace = Workspace::open(output.path()).expect("workspace");
        let collection = workspace.collections().expect("collections").remove(0);
        assert_eq!(collection.collection.name, "Postly migration fixture");
        assert_eq!(
            collection.collection.variables.get("baseUrl"),
            Some(&"https://api.example.test".to_owned())
        );
        let requests = workspace.requests(&collection).expect("requests");
        let list = requests
            .iter()
            .find(|(_, request)| request.name == "List users")
            .expect("list request");
        assert_eq!(list.1.query, vec![KeyValue::enabled("limit", "10")]);
        assert!(list.1.test_script.is_some());
        assert!(matches!(list.1.auth, Auth::Bearer { .. }));
    }

    #[test]
    fn imports_environment_and_preserves_disabled_values() {
        let output = tempfile::tempdir().expect("output");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../compat/postman-import/basic-environment.json");

        let report = import_environment(&fixture, output.path()).expect("environment");
        assert_eq!(report.imported_environments, 1);
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("disabled")));

        let workspace = Workspace::open(output.path()).expect("workspace");
        let (_, environment) = workspace.environments().expect("environments").remove(0);
        assert!(environment.variables["accessToken"].secret);
        assert!(!environment.variables["disabledValue"].enabled);
    }
}