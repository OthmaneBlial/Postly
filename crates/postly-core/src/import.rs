use std::{
    collections::{BTreeMap, BTreeSet},
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
    secrets::{SecretStore, SecretStoreError},
    storage::{CollectionFiles, Workspace, WorkspaceError, WorkspaceTransaction},
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
    #[error("secure storage error: {0}")]
    Secret(#[from] SecretStoreError),
    #[error("invalid dotenv entry on line {line}: {message}")]
    Dotenv { line: usize, message: String },
    #[error("dotenv secret key was not found: {0}")]
    MissingDotenvSecret(String),
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
    let mut transaction = workspace.begin_transaction();
    let collection = Collection::new(&name);
    let mut collection_files = transaction.create_collection(&collection)?;
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
        transaction.save_collection(&collection_files)?;
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
        transaction.save_collection(&collection_files)?;
    }
    transaction.save_collection(&collection_files)?;

    if let Some(items) = document.get("item").and_then(Value::as_array) {
        let inherited = InheritedItemContext {
            scripts: collection_scripts.clone(),
            auth: collection_files.collection.auth.clone(),
            auth_requires_review: collection_auth.requires_review,
        };
        for item in items {
            import_item(
                &mut transaction,
                &collection_files,
                item,
                None,
                &inherited,
                &mut report,
            )?;
        }
    }
    transaction.commit();
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
    let mut transaction = workspace.begin_transaction();
    transaction.save_environment(&environment)?;
    transaction.commit();
    Ok(report)
}

/// Import a conventional `.env` file into a native environment.
///
/// Parsing is deliberately conservative: values are not expanded, commands
/// are never executed, malformed assignments fail the import, and only keys
/// explicitly listed in `secret_keys` are written to the OS credential store.
pub fn import_dotenv(
    dotenv_path: impl AsRef<Path>,
    output_directory: impl AsRef<Path>,
    environment_name: &str,
    secret_keys: &[String],
    secret_store: &SecretStore,
) -> Result<ImportReport, ImportError> {
    let dotenv_path = dotenv_path.as_ref().to_path_buf();
    let text = fs::read_to_string(&dotenv_path).map_err(|source| ImportError::Read {
        path: dotenv_path.clone(),
        source,
    })?;
    let mut report = ImportReport {
        source: dotenv_path.display().to_string(),
        imported_environments: 1,
        ..ImportReport::default()
    };
    let values = parse_dotenv(&text, &mut report)?;
    let secret_keys = secret_keys
        .iter()
        .map(|key| {
            validate_dotenv_key(key)
                .map(|()| key.to_owned())
                .map_err(|message| ImportError::Dotenv { line: 0, message })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    for key in &secret_keys {
        if !values.contains_key(key) {
            return Err(ImportError::MissingDotenvSecret(key.clone()));
        }
    }

    let workspace = Workspace::open_or_init(&output_directory, "Postly workspace")?;
    let mut environment = Environment::new(environment_name);
    for (key, value) in values {
        if secret_keys.contains(&key) {
            let reference = secret_store.set_environment_secret(environment_name, &key, &value)?;
            environment
                .variables
                .insert(key, EnvironmentVariable::keychain(reference.into_string()));
        } else {
            environment
                .variables
                .insert(key, EnvironmentVariable::plain(value));
        }
    }
    let mut transaction = workspace.begin_transaction();
    transaction.save_environment(&environment)?;
    transaction.commit();
    Ok(report)
}

fn parse_dotenv(
    text: &str,
    report: &mut ImportReport,
) -> Result<BTreeMap<String, String>, ImportError> {
    let mut values = BTreeMap::new();
    for (index, raw_line) in text.lines().enumerate() {
        let line_number = index + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let assignment = line
            .strip_prefix("export")
            .filter(|rest| rest.chars().next().is_some_and(char::is_whitespace))
            .map(str::trim_start)
            .unwrap_or(line);
        let (raw_key, raw_value) =
            assignment
                .split_once('=')
                .ok_or_else(|| ImportError::Dotenv {
                    line: line_number,
                    message: "expected KEY=VALUE".to_owned(),
                })?;
        let key = raw_key.trim();
        validate_dotenv_key(key).map_err(|message| ImportError::Dotenv {
            line: line_number,
            message,
        })?;
        let value = parse_dotenv_value(raw_value.trim(), line_number)?;
        if values.insert(key.to_owned(), value).is_some() {
            report.warn(format!(
                "dotenv key {key} appeared more than once; the last value was imported."
            ));
        }
    }
    Ok(values)
}

fn validate_dotenv_key(key: &str) -> Result<(), String> {
    let mut characters = key.chars();
    let valid_start = characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic());
    if !valid_start
        || !characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
    {
        return Err(format!("invalid variable name {key:?}"));
    }
    Ok(())
}

fn parse_dotenv_value(value: &str, line: usize) -> Result<String, ImportError> {
    if let Some(quoted) = value.strip_prefix('\'') {
        let Some(end) = quoted.find('\'') else {
            return Err(ImportError::Dotenv {
                line,
                message: "unterminated single-quoted value".to_owned(),
            });
        };
        if !quoted[end + 1..].trim().is_empty() && !quoted[end + 1..].trim_start().starts_with('#')
        {
            return Err(ImportError::Dotenv {
                line,
                message: "unexpected content after quoted value".to_owned(),
            });
        }
        return Ok(quoted[..end].to_owned());
    }
    if let Some(quoted) = value.strip_prefix('"') {
        let mut escaped = false;
        let mut end = None;
        for (offset, character) in quoted.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            if character == '\\' {
                escaped = true;
            } else if character == '"' {
                end = Some(offset);
                break;
            }
        }
        let Some(end) = end else {
            return Err(ImportError::Dotenv {
                line,
                message: "unterminated double-quoted value".to_owned(),
            });
        };
        if !quoted[end + 1..].trim().is_empty() && !quoted[end + 1..].trim_start().starts_with('#')
        {
            return Err(ImportError::Dotenv {
                line,
                message: "unexpected content after quoted value".to_owned(),
            });
        }
        return decode_dotenv_escapes(&quoted[..end], line);
    }
    Ok(value
        .find(" #")
        .map(|index| value[..index].trim_end())
        .unwrap_or(value)
        .to_owned())
}

fn decode_dotenv_escapes(value: &str, line: usize) -> Result<String, ImportError> {
    let mut decoded = String::with_capacity(value.len());
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            decoded.push(character);
            continue;
        }
        let Some(escaped) = characters.next() else {
            return Err(ImportError::Dotenv {
                line,
                message: "double-quoted value ends with an escape".to_owned(),
            });
        };
        decoded.push(match escaped {
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            '"' => '"',
            '\\' => '\\',
            other => other,
        });
    }
    Ok(decoded)
}

#[derive(Debug, Clone)]
struct InheritedItemContext {
    scripts: ScriptSet,
    auth: Auth,
    auth_requires_review: bool,
}

fn import_item(
    transaction: &mut WorkspaceTransaction<'_>,
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
                transaction,
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
    let request_path = transaction.save_request(collection, &request)?;
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
        "awsv4" | "aws_signature_v4" => {
            let aws = value.get("awsv4").or_else(|| value.get("aws_signature_v4"));
            let access_key_id = auth_value_any(aws, &["accessKey", "access_key_id"]);
            let secret_access_key = auth_value_any(aws, &["secretKey", "secret_access_key"]);
            let region = auth_value_any(aws, &["region"]);
            let service = auth_value_any(aws, &["service"]);
            let session_token = auth_value_any(aws, &["sessionToken", "session_token"]);
            if access_key_id.is_empty()
                || secret_access_key.is_empty()
                || region.is_empty()
                || service.is_empty()
            {
                report.warn(format!(
                    "{subject} has incomplete AWS Signature V4 fields; authentication requires manual review."
                ));
                return ParsedAuth {
                    auth: Auth::None,
                    requires_review: true,
                };
            }
            Auth::AwsSignatureV4 {
                access_key_id,
                secret_access_key,
                region,
                service,
                session_token: (!session_token.is_empty()).then_some(session_token),
            }
        }
        "oauth2" => {
            let oauth = value.get("oauth2");
            let grant_type = auth_value_any(oauth, &["grant_type", "grantType"]);
            let grant_type = if grant_type.is_empty() {
                "client_credentials"
            } else {
                grant_type.as_str()
            };
            if !matches!(
                grant_type,
                "client_credentials"
                    | "authorization_code"
                    | "refresh_token"
                    | "device_code"
                    | "urn:ietf:params:oauth:grant-type:device_code"
            ) {
                report.warn(format!(
                    "{subject} uses unsupported OAuth 2.0 grant type {grant_type}; client_credentials, authorization_code + PKCE, refresh_token and device_code are currently supported."
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
            if grant_type == "authorization_code" {
                let authorization_url =
                    auth_value_any(oauth, &["authUrl", "authorizationUrl", "authorization_url"]);
                let redirect_uri = auth_value_any(oauth, &["redirectUri", "redirect_uri"]);
                let code = auth_value_any(oauth, &["code", "authorizationCode"]);
                let code_verifier = auth_value_any(oauth, &["codeVerifier", "code_verifier"]);
                if authorization_url.is_empty()
                    || token_url.is_empty()
                    || client_id.is_empty()
                    || redirect_uri.is_empty()
                    || code.is_empty()
                    || code_verifier.is_empty()
                {
                    report.warn(format!(
                        "{subject} has incomplete OAuth 2.0 authorization-code + PKCE fields; authentication requires manual review."
                    ));
                    return ParsedAuth {
                        auth: Auth::None,
                        requires_review: true,
                    };
                }
                Auth::OAuth2AuthorizationCodePkce {
                    authorization_url,
                    token_url,
                    client_id,
                    redirect_uri,
                    code,
                    code_verifier,
                    client_secret: (!client_secret.is_empty()).then_some(client_secret),
                    scope: (!scope.is_empty()).then_some(scope),
                }
            } else if grant_type == "refresh_token" {
                let refresh_token = auth_value_any(oauth, &["refreshToken", "refresh_token"]);
                if token_url.is_empty() || client_id.is_empty() || refresh_token.is_empty() {
                    report.warn(format!(
                        "{subject} has incomplete OAuth 2.0 refresh-token fields; authentication requires manual review."
                    ));
                    return ParsedAuth {
                        auth: Auth::None,
                        requires_review: true,
                    };
                }
                Auth::OAuth2RefreshToken {
                    token_url,
                    client_id,
                    refresh_token,
                    client_secret: (!client_secret.is_empty()).then_some(client_secret),
                    scope: (!scope.is_empty()).then_some(scope),
                }
            } else if matches!(
                grant_type,
                "device_code" | "urn:ietf:params:oauth:grant-type:device_code"
            ) {
                let device_authorization_url = auth_value_any(
                    oauth,
                    &[
                        "deviceAuthorizationUrl",
                        "device_authorization_url",
                        "deviceAuthUrl",
                    ],
                );
                if device_authorization_url.is_empty()
                    || token_url.is_empty()
                    || client_id.is_empty()
                {
                    report.warn(format!(
                        "{subject} has incomplete OAuth 2.0 device-code fields; authentication requires manual review."
                    ));
                    return ParsedAuth {
                        auth: Auth::None,
                        requires_review: true,
                    };
                }
                Auth::OAuth2DeviceCode {
                    device_authorization_url,
                    token_url,
                    client_id,
                    client_secret: (!client_secret.is_empty()).then_some(client_secret),
                    scope: (!scope.is_empty()).then_some(scope),
                }
            } else {
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
    fn imports_dotenv_with_explicit_keychain_selection() {
        let output = tempfile::tempdir().expect("output");
        let workspace_root = output.path().join("workspace");
        let input = output.path().join(".env");
        fs::write(
            &input,
            r#"# Values stay literal; Postly does not expand ${HOST}.
export API_URL="https://${HOST}/api"
TOKEN='secret value'
EMPTY=
TOKEN='last value'
"#,
        )
        .expect("dotenv fixture");
        let secret_store = crate::secrets::SecretStore::for_test(&workspace_root);

        let report = import_dotenv(
            &input,
            &workspace_root,
            "Local",
            &["TOKEN".to_owned()],
            &secret_store,
        )
        .expect("dotenv import");

        assert_eq!(report.imported_environments, 1);
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("appeared more than once")));
        let workspace = Workspace::open(&workspace_root).expect("workspace");
        let (_, environment) = workspace.environments().expect("environments").remove(0);
        assert_eq!(
            environment.variables["API_URL"].value,
            "https://${HOST}/api"
        );
        assert_eq!(environment.variables["EMPTY"].value, "");
        assert!(environment.variables["TOKEN"].secret_ref.is_some());
        assert!(toml::to_string(&environment)
            .expect("environment toml")
            .contains("secret_ref"));
        assert_eq!(
            secret_store
                .resolve_environment(&environment)
                .expect("resolved values")["TOKEN"],
            "last value"
        );
    }

    #[test]
    fn rejects_malformed_dotenv_without_creating_a_workspace() {
        let output = tempfile::tempdir().expect("output");
        let workspace_root = output.path().join("workspace");
        let input = output.path().join(".env");
        fs::write(&input, "NOT AN ASSIGNMENT\n").expect("dotenv fixture");
        let secret_store = crate::secrets::SecretStore::for_test(&workspace_root);

        let error = import_dotenv(&input, &workspace_root, "Local", &[], &secret_store)
            .expect_err("malformed dotenv must fail");
        assert!(error.to_string().contains("expected KEY=VALUE"));
        assert!(!workspace_root.exists());
    }

    #[test]
    fn imports_postman_url_body_and_review_variants_without_silent_loss() {
        let output = tempfile::tempdir().expect("output");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../compat/postman-import/variants-v2.1.json");

        let report = import_postman_collection(&fixture, output.path()).expect("import");
        assert_eq!(report.imported_requests, 8);
        assert_eq!(report.fully_supported_requests, 5);
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

        let aws = requests
            .iter()
            .find(|(_, request)| request.name == "AWS signed request")
            .expect("AWS request");
        assert_eq!(
            aws.1.auth,
            Auth::AwsSignatureV4 {
                access_key_id: "AKIDEXAMPLE".to_owned(),
                secret_access_key: "example-secret".to_owned(),
                region: "us-east-1".to_owned(),
                service: "execute-api".to_owned(),
                session_token: Some("example-session".to_owned()),
            }
        );
    }

    #[test]
    fn imports_postman_oauth_authorization_code_pkce() {
        let mut report = ImportReport::default();
        let value = serde_json::json!({
            "type": "oauth2",
            "oauth2": [
                { "key": "grant_type", "value": "authorization_code" },
                { "key": "authUrl", "value": "https://auth.example.test/authorize" },
                { "key": "accessTokenUrl", "value": "https://auth.example.test/token" },
                { "key": "clientId", "value": "postly" },
                { "key": "redirectUri", "value": "http://127.0.0.1:8787/callback" },
                { "key": "code", "value": "returned-code" },
                { "key": "codeVerifier", "value": "a".repeat(43) },
                { "key": "scope", "value": "read:users" }
            ]
        });
        let parsed = parse_auth(Some(&value), "PKCE request", &mut report);
        assert!(!parsed.requires_review);
        assert!(report.warnings.is_empty());
        assert_eq!(
            parsed.auth,
            Auth::OAuth2AuthorizationCodePkce {
                authorization_url: "https://auth.example.test/authorize".to_owned(),
                token_url: "https://auth.example.test/token".to_owned(),
                client_id: "postly".to_owned(),
                redirect_uri: "http://127.0.0.1:8787/callback".to_owned(),
                code: "returned-code".to_owned(),
                code_verifier: "a".repeat(43),
                client_secret: None,
                scope: Some("read:users".to_owned()),
            }
        );
    }

    #[test]
    fn imports_postman_oauth_refresh_token() {
        let mut report = ImportReport::default();
        let value = serde_json::json!({
            "type": "oauth2",
            "oauth2": [
                { "key": "grant_type", "value": "refresh_token" },
                { "key": "accessTokenUrl", "value": "https://auth.example.test/token" },
                { "key": "clientId", "value": "postly" },
                { "key": "refreshToken", "value": "refresh-123" },
                { "key": "scope", "value": "read:users" }
            ]
        });
        let parsed = parse_auth(Some(&value), "Refresh request", &mut report);
        assert!(!parsed.requires_review);
        assert!(report.warnings.is_empty());
        assert_eq!(
            parsed.auth,
            Auth::OAuth2RefreshToken {
                token_url: "https://auth.example.test/token".to_owned(),
                client_id: "postly".to_owned(),
                refresh_token: "refresh-123".to_owned(),
                client_secret: None,
                scope: Some("read:users".to_owned()),
            }
        );
    }

    #[test]
    fn imports_postman_oauth_device_code() {
        let mut report = ImportReport::default();
        let value = serde_json::json!({
            "type": "oauth2",
            "oauth2": [
                { "key": "grant_type", "value": "urn:ietf:params:oauth:grant-type:device_code" },
                { "key": "deviceAuthorizationUrl", "value": "https://auth.example.test/device" },
                { "key": "accessTokenUrl", "value": "https://auth.example.test/token" },
                { "key": "clientId", "value": "postly" },
                { "key": "scope", "value": "read:users" }
            ]
        });
        let parsed = parse_auth(Some(&value), "Device request", &mut report);
        assert!(!parsed.requires_review);
        assert!(report.warnings.is_empty());
        assert_eq!(
            parsed.auth,
            Auth::OAuth2DeviceCode {
                device_authorization_url: "https://auth.example.test/device".to_owned(),
                token_url: "https://auth.example.test/token".to_owned(),
                client_id: "postly".to_owned(),
                client_secret: None,
                scope: Some("read:users".to_owned()),
            }
        );
    }

    #[test]
    fn imports_postman_aws_signature_v4() {
        let mut report = ImportReport::default();
        let value = serde_json::json!({
            "type": "awsv4",
            "awsv4": [
                { "key": "accessKey", "value": "AKIDEXAMPLE" },
                { "key": "secretKey", "value": "secret" },
                { "key": "region", "value": "eu-west-1" },
                { "key": "service", "value": "execute-api" },
                { "key": "sessionToken", "value": "session" }
            ]
        });
        let parsed = parse_auth(Some(&value), "AWS request", &mut report);
        assert!(!parsed.requires_review);
        assert!(report.warnings.is_empty());
        assert_eq!(
            parsed.auth,
            Auth::AwsSignatureV4 {
                access_key_id: "AKIDEXAMPLE".to_owned(),
                secret_access_key: "secret".to_owned(),
                region: "eu-west-1".to_owned(),
                service: "execute-api".to_owned(),
                session_token: Some("session".to_owned()),
            }
        );
    }
}
