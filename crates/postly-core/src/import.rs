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

#[derive(Debug, Clone, Default)]
struct ScriptSet {
    pre_request: Vec<String>,
    test: Vec<String>,
}

impl ScriptSet {
    fn extend(&mut self, other: Self) {
        self.pre_request.extend(other.pre_request);
        self.test.extend(other.test);
    }

    fn apply_to_request(&self, request: &mut Request) {
        if !self.pre_request.is_empty() {
            request.pre_request_script = Some(self.pre_request.join("\n\n"));
        }
        if !self.test.is_empty() {
            request.test_script = Some(self.test.join("\n\n"));
        }
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
    let collection_auth = parse_auth(document.get("auth"), "Collection", &mut report);
    collection_files.collection.auth = collection_auth.auth;
    let collection_scripts = parse_event_scripts(&document, "Collection", &mut report);
    collection_files.collection.pre_request_script = (!collection_scripts.pre_request.is_empty())
        .then(|| collection_scripts.pre_request.join("\n\n"));
    collection_files.collection.test_script =
        (!collection_scripts.test.is_empty()).then(|| collection_scripts.test.join("\n\n"));

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
    workspace.save_collection(&collection_files)?;

    if let Some(items) = document.get("item").and_then(Value::as_array) {
        let inherited = InheritedItemContext {
            scripts: collection_scripts.clone(),
            auth: collection_files.collection.auth.clone(),
            auth_requires_review: collection_auth.requires_review,
        };
        for item in items {
            import_item(
                &workspace,
                &collection_files,
                item,
                None,
                &inherited,
                &mut report,
            )?;
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
                    secret_ref: None,
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

#[derive(Debug, Clone)]
struct InheritedItemContext {
    scripts: ScriptSet,
    auth: Auth,
    auth_requires_review: bool,
}

fn import_item(
    workspace: &Workspace,
    collection: &CollectionFiles,
    item: &Value,
    folder: Option<String>,
    inherited: &InheritedItemContext,
    report: &mut ImportReport,
) -> Result<(), ImportError> {
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Unnamed item");
    if let Some(children) = item.get("item").and_then(Value::as_array) {
        let mut next_scripts = inherited.scripts.clone();
        next_scripts.extend(parse_event_scripts(item, name, report));
        let (next_auth, next_auth_requires_review) = if item.get("auth").is_some() {
            let parsed = parse_auth(item.get("auth"), name, report);
            (parsed.auth, parsed.requires_review)
        } else {
            (inherited.auth.clone(), inherited.auth_requires_review)
        };
        let next_context = InheritedItemContext {
            scripts: next_scripts,
            auth: next_auth,
            auth_requires_review: next_auth_requires_review,
        };
        let next_folder = Some(match folder {
            Some(folder) => format!("{folder}/{name}"),
            None => name.to_owned(),
        });
        for child in children {
            import_item(
                workspace,
                collection,
                child,
                next_folder.clone(),
                &next_context,
                report,
            )?;
        }
        return Ok(());
    }

    let Some(request_value) = item.get("request") else {
        report.warn(format!(
            "Skipped item {name}: it has neither request nor nested items."
        ));
        return Ok(());
    };
    let (mut request, mut auth_requires_review) =
        parse_request(name, request_value, folder, report);
    if request_value.get("auth").is_none() {
        request.auth = inherited.auth.clone();
        auth_requires_review = inherited.auth_requires_review;
    }
    let mut scripts = inherited.scripts.clone();
    scripts.extend(parse_event_scripts(item, name, report));
    scripts.apply_to_request(&mut request);
    request.examples = parse_examples(item, report);
    let request_path = workspace.save_request(collection, &request)?;
    report.imported_requests += 1;
    if auth_requires_review || request_needs_review(&request) {
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
) -> (Request, bool) {
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
                        .and_then(|value| string_value(Some(value)))
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
    request.cookies = parse_pairs(value.get("cookie"));
    let parsed_auth = parse_auth(value.get("auth"), name, report);
    request.auth = parsed_auth.auth;
    request.body = parse_body(name, value.get("body"), report);
    (request, parsed_auth.requires_review)
}

fn parse_event_scripts(item: &Value, subject: &str, report: &mut ImportReport) -> ScriptSet {
    let mut scripts = ScriptSet::default();
    let Some(events) = item.get("event").and_then(Value::as_array) else {
        return scripts;
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
            (Some("prerequest"), Some(script)) => scripts.pre_request.push(script),
            (Some("test"), Some(script)) => scripts.test.push(script),
            (Some(other), _) => report.warn(format!(
                "{subject} contains unsupported event type {other}."
            )),
            _ => report.warn(format!(
                "{subject} contains an event without executable script lines."
            )),
        }
    }
    scripts
}

fn parse_url(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(url) if !url.trim().is_empty() => Some(url.clone()),
        Value::Object(object) => {
            if let Some(raw) = object.get("raw").and_then(Value::as_str) {
                if !raw.trim().is_empty() {
                    return Some(raw.to_owned());
                }
            }
            let protocol = object.get("protocol").and_then(Value::as_str)?;
            let host = match object.get("host")? {
                Value::Array(host) => host
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("."),
                Value::String(host) => host.clone(),
                _ => return None,
            };
            if host.is_empty() {
                return None;
            }
            let mut url = format!("{protocol}://{host}");
            if let Some(path) = object.get("path") {
                url.push('/');
                match path {
                    Value::Array(path) => url.push_str(
                        &path
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("/"),
                    ),
                    Value::String(path) => url.push_str(path),
                    _ => {}
                }
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
        "formdata" => {
            let parts = body
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
                                value: string_value(part.get("value")).unwrap_or_default(),
                                file_path,
                                content_type: part
                                    .get("contentType")
                                    .or_else(|| part.get("content_type"))
                                    .and_then(Value::as_str)
                                    .map(ToOwned::to_owned),
                                enabled: !part
                                    .get("disabled")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(false),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if parts.iter().any(|part| part.file_path.is_some()) {
                report.warn(format!(
                    "Request {name} imports a file body via multipart; verify the relative paths."
                ));
            }
            RequestBody::Multipart { parts }
        }
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
            let Some(graphql) = body.get("graphql").and_then(Value::as_object) else {
                report.warn(format!(
                    "Request {name} has malformed GraphQL body metadata."
                ));
                return RequestBody::Graphql {
                    query: String::new(),
                    variables: Value::Object(serde_json::Map::new()),
                    operation_name: None,
                };
            };
            let query = graphql
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let variables = match graphql.get("variables") {
                Some(Value::String(value)) => match serde_json::from_str(value) {
                    Ok(value) => value,
                    Err(error) => {
                        report.warn(format!(
                            "Request {name} has invalid GraphQL variables JSON: {error}."
                        ));
                        Value::Object(serde_json::Map::new())
                    }
                },
                Some(value) => value.clone(),
                None => Value::Object(serde_json::Map::new()),
            };
            let operation_name = graphql
                .get("operationName")
                .or_else(|| graphql.get("operation_name"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            report.warn(format!(
                "Request {name} uses Postman GraphQL body metadata; verify the query and schema."
            ));
            RequestBody::Graphql {
                query,
                variables,
                operation_name,
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
                        value: string_value(pair.get("value")).unwrap_or_default(),
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

#[derive(Debug, Clone)]
struct ParsedAuth {
    auth: Auth,
    requires_review: bool,
}

fn parse_auth(value: Option<&Value>, subject: &str, report: &mut ImportReport) -> ParsedAuth {
    let Some(value) = value else {
        return ParsedAuth {
            auth: Auth::None,
            requires_review: false,
        };
    };
    let auth_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("noauth");
    let auth = match auth_type {
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
        "oauth2" => {
            let oauth = value.get("oauth2");
            let grant_type = auth_value_any(oauth, &["grant_type", "grantType"]);
            if !grant_type.is_empty() && grant_type != "client_credentials" {
                report.warn(format!(
                    "{subject} uses OAuth 2.0 grant type {grant_type}; only client_credentials is currently supported."
                ));
                return ParsedAuth {
                    auth: Auth::None,
                    requires_review: true,
                };
            }
            let token_url = auth_value_any(
                oauth,
                &[
                    "accessTokenUrl",
                    "tokenUrl",
                    "token_url",
                    "access_token_url",
                ],
            );
            let client_id = auth_value_any(oauth, &["clientId", "client_id"]);
            let client_secret = auth_value_any(oauth, &["clientSecret", "client_secret"]);
            let scope = auth_value_any(oauth, &["scope"]);
            if token_url.is_empty() || client_id.is_empty() || client_secret.is_empty() {
                report.warn(format!(
                    "{subject} has incomplete OAuth 2.0 client credentials; authentication requires manual review."
                ));
                return ParsedAuth {
                    auth: Auth::None,
                    requires_review: true,
                };
            }
            Auth::OAuth2ClientCredentials {
                token_url,
                client_id,
                client_secret,
                scope: (!scope.is_empty()).then_some(scope),
            }
        }
        "noauth" => Auth::None,
        other => {
            report.warn(format!(
                "{subject} uses unsupported Postman auth type {other}; authentication was not executed."
            ));
            return ParsedAuth {
                auth: Auth::None,
                requires_review: true,
            };
        }
    };
    ParsedAuth {
        auth,
        requires_review: false,
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
        .and_then(|entry| string_value(entry.get("value")))
        .unwrap_or_default()
}

fn auth_value_any(value: Option<&Value>, keys: &[&str]) -> String {
    keys.iter()
        .map(|key| auth_value(value, key))
        .find(|value| !value.is_empty())
        .unwrap_or_default()
}

fn string_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) => Some(value.clone()),
        Value::Null => None,
        value => Some(value.to_string()),
    }
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
                                            .and_then(|value| string_value(Some(value)))
                                            .unwrap_or_default(),
                                    ))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let delay_ms = example
                        .get("x-postly-delay-ms")
                        .and_then(Value::as_u64)
                        .unwrap_or_default();
                    ResponseExample {
                        name,
                        status,
                        headers,
                        body,
                        delay_ms,
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
        || matches!(request.body, RequestBody::Graphql { .. })
        || matches!(
            &request.body,
            RequestBody::Multipart { parts }
                if parts.iter().any(|part| part.file_path.is_some())
        )
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
        assert_eq!(report.fully_supported_requests, 0);
        assert_eq!(report.manual_review_requests, 2);
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
        assert!(collection.collection.pre_request_script.is_some());
        assert!(matches!(collection.collection.auth, Auth::Bearer { .. }));
        let requests = workspace.requests(&collection).expect("requests");
        let list = requests
            .iter()
            .find(|(_, request)| request.name == "List users")
            .expect("list request");
        assert_eq!(list.1.query, vec![KeyValue::enabled("limit", "10")]);
        assert!(list
            .1
            .pre_request_script
            .as_deref()
            .is_some_and(|script| script.contains("collection pre-request")));
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

    #[test]
    fn imports_postman_url_body_and_review_variants_without_silent_loss() {
        let output = tempfile::tempdir().expect("output");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../compat/postman-import/variants-v2.1.json");

        let report = import_postman_collection(&fixture, output.path()).expect("import");
        assert_eq!(report.imported_requests, 7);
        assert_eq!(report.fully_supported_requests, 4);
        assert_eq!(report.manual_review_requests, 3);
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("file body")));
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("GraphQL")));
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("unsupported event type unknown")));

        let workspace = Workspace::open(output.path()).expect("workspace");
        let collection = workspace.collections().expect("collections").remove(0);
        let requests = workspace.requests(&collection).expect("requests");

        let search = requests
            .iter()
            .find(|(_, request)| request.name == "Search users")
            .expect("search request");
        assert_eq!(search.1.folder.as_deref(), Some("Structured URLs"));
        assert_eq!(search.1.url, "https://api.example.test/users/search");
        assert_eq!(search.1.query[0], KeyValue::enabled("q", "Ada"));
        assert_eq!(
            search.1.query[1],
            KeyValue {
                key: "page".to_owned(),
                value: "2".to_owned(),
                enabled: false,
            }
        );
        assert!(matches!(
            &search.1.auth,
            Auth::ApiKey {
                location: ApiKeyLocation::Query,
                ..
            }
        ));
        assert!(matches!(&search.1.body, RequestBody::FormUrlEncoded { .. }));

        let upload = requests
            .iter()
            .find(|(_, request)| request.name == "Upload avatar")
            .expect("upload request");
        assert!(matches!(upload.1.body, RequestBody::Multipart { .. }));

        let oauth = requests
            .iter()
            .find(|(_, request)| request.name == "OAuth client credentials")
            .expect("OAuth client credentials request");
        assert_eq!(
            oauth.1.auth,
            Auth::OAuth2ClientCredentials {
                token_url: "{{baseUrl}}/oauth/token".to_owned(),
                client_id: "postly".to_owned(),
                client_secret: "{{clientSecret}}".to_owned(),
                scope: Some("read:users".to_owned()),
            }
        );

        let graphql = requests
            .iter()
            .find(|(_, request)| request.name == "GraphQL review")
            .expect("graphql request");
        assert!(matches!(&graphql.1.body, RequestBody::Graphql { .. }));

        let oauth = requests
            .iter()
            .find(|(_, request)| request.name == "OAuth review")
            .expect("oauth request");
        assert_eq!(oauth.1.url, "{{baseUrl}}/oauth-check");
        assert_eq!(oauth.1.query, vec![KeyValue::enabled("scope", "read")]);
        assert_eq!(oauth.1.headers[0], HeaderEntry::enabled("X-Retry", "3"));
        assert!(matches!(&oauth.1.body, RequestBody::Json { .. }));

        let html = requests
            .iter()
            .find(|(_, request)| request.name == "HTML payload")
            .expect("html request");
        assert!(matches!(
            &html.1.body,
            RequestBody::Raw {
                content_type: Some(content_type),
                ..
            } if content_type == "text/html"
        ));
    }
}
