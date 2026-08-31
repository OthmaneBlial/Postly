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
        MultipartPart, Request, RequestBody, RequestTransportSettings, ResponseExample,
        ResponseExampleCookie,
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
            let Some(key) = variable.get("key").and_then(Value::as_str) else {
                report.warn("Skipped a collection variable without a key.");
                continue;
            };
            let disabled = variable
                .get("disabled")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || variable.get("enabled").and_then(Value::as_bool) == Some(false);
            if disabled {
                report.warn(format!(
                    "Collection variable {key} is disabled and was not activated."
                ));
                continue;
            }
            let value = postman_variable_value(variable);
            let Some(value) = value else {
                report.warn(format!(
                    "Skipped collection variable {key}: it has no usable value."
                ));
                continue;
            };
            collection_files
                .collection
                .variables
                .insert(key.to_owned(), value);
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
                &mut collection_files,
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
    import_environment_with_store(environment_path, output_directory, None)
}

/// Import a Postman environment, optionally moving entries marked `secret`
/// into the platform credential store before the native file is written.
///
/// Without a store, the legacy import remains available for compatibility but
/// reports that marked secrets are plaintext and should be migrated explicitly.
pub fn import_environment_with_store(
    environment_path: impl AsRef<Path>,
    output_directory: impl AsRef<Path>,
    secret_store: Option<&SecretStore>,
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
            let Some(value) = postman_environment_value(variable) else {
                report.warn(format!(
                    "Skipped environment variable {key}: it has no usable value."
                ));
                continue;
            };
            let enabled = variable
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let secret = variable.get("type").and_then(Value::as_str) == Some("secret");
            let (value, secret_ref) = if secret {
                if let Some(secret_store) = secret_store {
                    let reference = secret_store
                        .set_environment_secret(name, key, &value)
                        .map_err(ImportError::Secret)?;
                    report.warn(format!(
                        "Environment variable {key} was stored in the OS credential store during secure import."
                    ));
                    (String::new(), Some(reference.into_string()))
                } else {
                    report.warn(format!(
                        "Environment variable {key} is marked secret but was imported as plaintext; use --secure or env migrate before sharing the workspace."
                    ));
                    (value, None)
                }
            } else {
                (value, None)
            };
            environment.variables.insert(
                key.to_owned(),
                EnvironmentVariable {
                    value,
                    enabled,
                    secret,
                    secret_ref,
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
    collection: &mut CollectionFiles,
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
    import_url_variables(
        request_value.get("url"),
        &mut request,
        &mut collection.collection,
        name,
        report,
    );
    transaction.save_collection(collection)?;
    if request_value.get("auth").is_none() {
        request.auth = inherited.auth.clone();
        auth_requires_review = inherited.auth_requires_review;
    }
    let mut scripts = inherited.scripts.clone();
    scripts.extend(parse_event_scripts(item, name, report));
    scripts.apply_to_request(&mut request);
    let (examples, examples_require_review) = parse_examples(item, report);
    request.examples = examples;
    let request_path = transaction.save_request(collection, &request)?;
    report.imported_requests += 1;
    if auth_requires_review || examples_require_review || request_needs_review(&request) {
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

fn import_url_variables(
    value: Option<&Value>,
    request: &mut Request,
    collection: &mut Collection,
    subject: &str,
    report: &mut ImportReport,
) {
    let Some(variables) = value
        .and_then(Value::as_object)
        .and_then(|url| url.get("variable"))
        .and_then(Value::as_array)
    else {
        return;
    };
    for variable in variables {
        let Some(key) = variable.get("key").and_then(Value::as_str) else {
            report.warn(format!("{subject} has a path variable without a key."));
            continue;
        };
        if key.is_empty() {
            report.warn(format!("{subject} has an empty path variable key."));
            continue;
        }
        let placeholder = format!("{{{{{key}}}}}");
        request.url = request.url.replace(&format!(":{key}"), &placeholder);
        let disabled = variable
            .get("disabled")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || variable.get("enabled").and_then(Value::as_bool) == Some(false);
        if disabled {
            report.warn(format!(
                "Path variable {key} on {subject} is disabled and was not activated."
            ));
            continue;
        }
        let Some(value) = postman_variable_value(variable) else {
            report.warn(format!(
                "Path variable {key} on {subject} has no usable value."
            ));
            continue;
        };
        collection.variables.insert(key.to_owned(), value);
    }
}

fn parse_request(
    name: &str,
    value: &Value,
    folder: Option<String>,
    report: &mut ImportReport,
) -> (Request, bool) {
    let subject = format!("Request {name}");
    let method = value.get("method").and_then(Value::as_str).unwrap_or("GET");
    let url = parse_url(value.get("url")).unwrap_or_else(|| {
        report.warn(format!(
            "Request {name} has no usable URL; imported with an empty URL."
        ));
        String::new()
    });
    let mut request = Request::new(name, method, url);
    request.folder = folder;
    let mut malformed_entries_require_review = false;
    if let Some(url_object) = value.get("url").and_then(Value::as_object) {
        let (query, query_requires_review) =
            parse_pairs(url_object.get("query"), &subject, "query", report);
        request.query = query;
        malformed_entries_require_review |= query_requires_review;
        if !request.query.is_empty() {
            let (without_fragment, fragment) = request
                .url
                .split_once('#')
                .map_or((request.url.as_str(), ""), |(base, fragment)| {
                    (base, fragment)
                });
            let base = without_fragment
                .split_once('?')
                .map_or(without_fragment, |(base, _)| base);
            request.url = if fragment.is_empty() {
                base.to_owned()
            } else {
                format!("{base}#{fragment}")
            };
        }
    }
    request.description = value.get("description").and_then(description_text);
    let (headers, headers_require_review) = parse_headers(value.get("header"), &subject, report);
    request.headers = headers;
    malformed_entries_require_review |= headers_require_review;
    let (cookies, cookies_require_review) =
        parse_pairs(value.get("cookie"), &subject, "cookie", report);
    request.cookies = cookies;
    malformed_entries_require_review |= cookies_require_review;
    let parsed_auth = parse_auth(value.get("auth"), name, report);
    request.auth = parsed_auth.auth;
    let request_content_type = request
        .headers
        .iter()
        .find(|header| header.key.eq_ignore_ascii_case("content-type"))
        .map(|header| header.value.as_str());
    request.body = parse_body(name, value.get("body"), request_content_type, report);
    let (transport, transport_requires_review) =
        parse_request_transport_settings(value.get("protocolProfileBehavior"), name, report);
    request.transport = transport;
    let unsupported_fields_require_review = warn_unsupported_request_fields(name, value, report);
    (
        request,
        parsed_auth.requires_review
            || unsupported_fields_require_review
            || transport_requires_review
            || malformed_entries_require_review,
    )
}

fn warn_unsupported_request_fields(name: &str, value: &Value, report: &mut ImportReport) -> bool {
    let mut requires_review = false;
    for field in ["proxy", "certificate"] {
        let Some(field_value) = value.get(field) else {
            continue;
        };
        if !has_meaningful_value(field_value) {
            continue;
        }
        let detail = field_value
            .as_object()
            .map(|object| {
                let mut keys = object.keys().cloned().collect::<Vec<_>>();
                keys.sort();
                if keys.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", keys.join(", "))
                }
            })
            .unwrap_or_default();
        report.warn(format!(
            "Request {name} contains Postman {field}{detail}; this setting is not imported and requires manual review."
        ));
        requires_review = true;
    }
    requires_review
}

fn parse_request_transport_settings(
    value: Option<&Value>,
    name: &str,
    report: &mut ImportReport,
) -> (Option<RequestTransportSettings>, bool) {
    let Some(value) = value else {
        return (None, false);
    };
    let Some(object) = value.as_object() else {
        report.warn(format!(
            "Request {name} has a non-object Postman protocolProfileBehavior; it requires manual review."
        ));
        return (None, true);
    };
    let mut settings = RequestTransportSettings::default();
    let mut requires_review = false;
    for (key, value) in object {
        match key.as_str() {
            "followRedirects" => match value.as_bool() {
                Some(value) => settings.follow_redirects = Some(value),
                None => {
                    report.warn(format!(
                        "Request {name} has a non-boolean Postman followRedirects setting; it requires manual review."
                    ));
                    requires_review = true;
                }
            },
            "maxRedirects" => match value.as_u64().and_then(|value| usize::try_from(value).ok()) {
                Some(value) => settings.max_redirects = Some(value),
                None => {
                    report.warn(format!(
                        "Request {name} has an invalid Postman maxRedirects setting; it requires manual review."
                    ));
                    requires_review = true;
                }
            },
            "disableCookies" => match value.as_bool() {
                Some(value) => settings.disable_cookies = value,
                None => {
                    report.warn(format!(
                        "Request {name} has a non-boolean Postman disableCookies setting; it requires manual review."
                    ));
                    requires_review = true;
                }
            },
            other => {
                report.warn(format!(
                    "Request {name} contains unsupported Postman protocolProfileBehavior field {other}; it requires manual review."
                ));
                requires_review = true;
            }
        }
    }
    let settings = (!settings.is_empty()).then_some(settings);
    (settings, requires_review)
}

fn has_meaningful_value(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(value) => !value.trim().is_empty(),
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
        Value::Bool(_) | Value::Number(_) => true,
    }
}

fn parse_event_scripts(item: &Value, subject: &str, report: &mut ImportReport) -> ScriptSet {
    let mut scripts = ScriptSet::default();
    let Some(events) = item.get("event").and_then(Value::as_array) else {
        return scripts;
    };
    for event in events {
        let listen = event.get("listen").and_then(Value::as_str);
        let script = event.pointer("/script/exec").and_then(script_text);
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

fn script_text(value: &Value) -> Option<String> {
    match value {
        Value::String(script) => Some(script.clone()),
        Value::Array(lines) => Some(
            lines
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        _ => None,
    }
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
                    .filter_map(|part| string_value(Some(part)))
                    .collect::<Vec<_>>()
                    .join("."),
                Value::String(host) => host.clone(),
                _ => return None,
            };
            if host.is_empty() {
                return None;
            }
            let host = if host.contains(':') && !host.starts_with('[') {
                format!("[{host}]")
            } else {
                host
            };
            let mut url = format!("{protocol}://{host}");
            if let Some(port) = object
                .get("port")
                .and_then(|port| string_value(Some(port)))
                .filter(|port| !port.is_empty())
            {
                url.push(':');
                url.push_str(&port);
            }
            if let Some(path) = object.get("path") {
                let path = match path {
                    Value::Array(path) => path
                        .iter()
                        .filter_map(|part| string_value(Some(part)))
                        .collect::<Vec<_>>()
                        .join("/"),
                    Value::String(path) => path.clone(),
                    _ => String::new(),
                };
                if !path.is_empty() {
                    url.push('/');
                    url.push_str(path.trim_start_matches('/'));
                }
            }
            if let Some(hash) = object
                .get("hash")
                .and_then(|hash| string_value(Some(hash)))
                .filter(|hash| !hash.is_empty())
            {
                url.push('#');
                url.push_str(hash.trim_start_matches('#'));
            }
            Some(url)
        }
        _ => None,
    }
}

fn parse_body(
    name: &str,
    value: Option<&Value>,
    request_content_type: Option<&str>,
    report: &mut ImportReport,
) -> RequestBody {
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
                })
                .or_else(|| request_content_type.map(normalize_content_type));
            if content_type
                .as_deref()
                .is_some_and(|value| value == "application/json" || value.ends_with("+json"))
            {
                if let Ok(value) = serde_json::from_str(&text) {
                    return RequestBody::Json { value };
                }
            }
            RequestBody::Raw { text, content_type }
        }
        "urlencoded" => {
            let (fields, _) = parse_pairs(
                body.get("urlencoded"),
                &format!("Request {name} urlencoded body"),
                "field",
                report,
            );
            RequestBody::FormUrlEncoded { fields }
        }
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
                                enabled: postman_entry_enabled(part),
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

fn normalize_content_type(value: &str) -> String {
    value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

fn parse_pairs(
    value: Option<&Value>,
    subject: &str,
    kind: &str,
    report: &mut ImportReport,
) -> (Vec<KeyValue>, bool) {
    let Some(pairs) = value.and_then(Value::as_array) else {
        if value.is_some() {
            report.warn(format!(
                "{subject} has a non-array {kind} list; it was skipped."
            ));
            return (Vec::new(), true);
        }
        return (Vec::new(), false);
    };
    let mut requires_review = false;
    let pairs = pairs
        .iter()
        .enumerate()
        .filter_map(|(index, pair)| {
            let Some(key) = pair.get("key").and_then(Value::as_str) else {
                report.warn(format!(
                    "{subject} {kind} entry {index} has no usable key; it was skipped."
                ));
                requires_review = true;
                return None;
            };
            Some(KeyValue {
                key: key.to_owned(),
                value: string_value(pair.get("value")).unwrap_or_default(),
                enabled: postman_entry_enabled(pair),
            })
        })
        .collect();
    (pairs, requires_review)
}

fn postman_entry_enabled(value: &Value) -> bool {
    value.get("disabled").and_then(Value::as_bool) != Some(true)
        && value.get("enabled").and_then(Value::as_bool) != Some(false)
}

fn parse_header_entry(value: &Value) -> Option<HeaderEntry> {
    Some(HeaderEntry {
        key: value.get("key").and_then(Value::as_str)?.to_owned(),
        value: value
            .get("value")
            .and_then(|value| string_value(Some(value)))
            .unwrap_or_default(),
        enabled: postman_entry_enabled(value),
    })
}

fn parse_headers(
    value: Option<&Value>,
    subject: &str,
    report: &mut ImportReport,
) -> (Vec<HeaderEntry>, bool) {
    let Some(headers) = value.and_then(Value::as_array) else {
        if value.is_some() {
            report.warn(format!(
                "{subject} has a non-array header list; it was skipped."
            ));
            return (Vec::new(), true);
        }
        return (Vec::new(), false);
    };
    let mut requires_review = false;
    let headers = headers
        .iter()
        .enumerate()
        .filter_map(|(index, header)| {
            let parsed = parse_header_entry(header);
            if parsed.is_none() {
                report.warn(format!(
                    "{subject} header entry {index} has no usable key; it was skipped."
                ));
                requires_review = true;
            }
            parsed
        })
        .collect();
    (headers, requires_review)
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
        "digest" => Auth::Digest {
            username: auth_value_any(value.get("digest"), &["username", "user"]),
            password: auth_value_any(value.get("digest"), &["password", "passwd"]),
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
            let saved_access_token =
                auth_value_any(oauth, &["accessToken", "access_token", "token"]);
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
                if !saved_access_token.is_empty() {
                    report.warn(format!(
                        "{subject} uses unsupported OAuth 2.0 grant type {grant_type}; the saved access token was preserved as bearer authentication, but token renewal requires manual review."
                    ));
                    return ParsedAuth {
                        auth: Auth::Bearer {
                            token: saved_access_token,
                        },
                        requires_review: true,
                    };
                }
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
                    if !saved_access_token.is_empty() {
                        report.warn(format!(
                            "{subject} has incomplete OAuth 2.0 authorization-code + PKCE fields; the saved access token was preserved as bearer authentication, but token renewal requires manual review."
                        ));
                        return ParsedAuth {
                            auth: Auth::Bearer {
                                token: saved_access_token,
                            },
                            requires_review: true,
                        };
                    }
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
                    if !saved_access_token.is_empty() {
                        report.warn(format!(
                            "{subject} has incomplete OAuth 2.0 refresh-token fields; the saved access token was preserved as bearer authentication, but token renewal requires manual review."
                        ));
                        return ParsedAuth {
                            auth: Auth::Bearer {
                                token: saved_access_token,
                            },
                            requires_review: true,
                        };
                    }
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
                    if !saved_access_token.is_empty() {
                        report.warn(format!(
                            "{subject} has incomplete OAuth 2.0 device-code fields; the saved access token was preserved as bearer authentication, but token renewal requires manual review."
                        ));
                        return ParsedAuth {
                            auth: Auth::Bearer {
                                token: saved_access_token,
                            },
                            requires_review: true,
                        };
                    }
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
                    if !saved_access_token.is_empty() {
                        report.warn(format!(
                            "{subject} has incomplete OAuth 2.0 client credentials; the saved access token was preserved as bearer authentication, but token renewal requires manual review."
                        ));
                        return ParsedAuth {
                            auth: Auth::Bearer {
                                token: saved_access_token,
                            },
                            requires_review: true,
                        };
                    }
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

fn postman_variable_value(variable: &Value) -> Option<String> {
    let value = variable
        .get("value")
        .and_then(|value| string_value(Some(value)));
    let current = variable
        .get("current")
        .and_then(|value| string_value(Some(value)));
    match value {
        Some(value) if !value.is_empty() => Some(value),
        Some(value) => current.or(Some(value)),
        None => current,
    }
}

fn postman_environment_value(variable: &Value) -> Option<String> {
    let value = variable
        .get("value")
        .and_then(|value| string_value(Some(value)));
    let current = variable
        .get("currentValue")
        .or_else(|| variable.get("current"))
        .and_then(|value| string_value(Some(value)));
    let initial = variable
        .get("initialValue")
        .and_then(|value| string_value(Some(value)));
    match value {
        Some(value) if !value.is_empty() => Some(value),
        Some(value) => current.or(initial).or(Some(value)),
        None => current.or(initial),
    }
}

fn parse_examples(item: &Value, report: &mut ImportReport) -> (Vec<ResponseExample>, bool) {
    let mut requires_review = false;
    let examples = item
        .get("response")
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
                    let original_request = match example.get("originalRequest") {
                        Some(value) if has_meaningful_value(value) => {
                            if !value.is_object() {
                                report.warn(format!(
                                    "Response example {name} has a non-object originalRequest; it requires manual review."
                                ));
                                requires_review = true;
                                None
                            } else {
                                let embedded_name = format!("Response example {name} request");
                                let (request, request_requires_review) =
                                    parse_request(&embedded_name, value, None, report);
                                requires_review |= request_requires_review;
                                Some(Box::new(request))
                            }
                        }
                        _ => None,
                    };
                    let status = example
                        .get("code")
                        .and_then(Value::as_u64)
                        .and_then(|code| u16::try_from(code).ok());
                    let status_text = example
                        .get("status")
                        .and_then(Value::as_str)
                        .filter(|status| !status.trim().is_empty())
                        .map(ToOwned::to_owned);
                    let body = example
                        .get("body")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned);
                    let example_subject = format!("Response example {name}");
                    let (headers, headers_require_review) =
                        parse_headers(example.get("header"), &example_subject, report);
                    requires_review |= headers_require_review;
                    let cookies = example
                        .get("cookie")
                        .or_else(|| example.get("cookies"))
                        .and_then(Value::as_array)
                        .map(|cookies| {
                            cookies
                                .iter()
                                .filter_map(|cookie| {
                                    let cookie_name = cookie
                                        .get("name")
                                        .or_else(|| cookie.get("key"))
                                        .and_then(Value::as_str)
                                        .filter(|name| !name.is_empty())
                                        .map(ToOwned::to_owned);
                                    let Some(name) = cookie_name else {
                                        report.warn(format!(
                                            "Response example {name} contains a cookie without a usable name; it was skipped."
                                        ));
                                        return None;
                                    };
                                    Some(ResponseExampleCookie {
                                        name,
                                        value: string_value(cookie.get("value")).unwrap_or_default(),
                                        domain: cookie
                                            .get("domain")
                                            .and_then(|value| string_value(Some(value))),
                                        path: cookie
                                            .get("path")
                                            .and_then(|value| string_value(Some(value))),
                                        secure: cookie
                                            .get("secure")
                                            .and_then(Value::as_bool)
                                            .unwrap_or(false),
                                        http_only: cookie
                                            .get("httpOnly")
                                            .or_else(|| cookie.get("http_only"))
                                            .and_then(Value::as_bool)
                                            .unwrap_or(false),
                                        same_site: cookie
                                            .get("sameSite")
                                            .or_else(|| cookie.get("same_site"))
                                            .and_then(|value| string_value(Some(value))),
                                        expires: cookie
                                            .get("expires")
                                            .and_then(|value| string_value(Some(value))),
                                        max_age_seconds: cookie
                                            .get("maxAge")
                                            .or_else(|| cookie.get("max_age"))
                                            .or_else(|| cookie.get("max_age_seconds"))
                                            .and_then(|value| match value {
                                                Value::Number(value) => value.as_i64(),
                                                Value::String(value) => value.parse().ok(),
                                                _ => None,
                                            }),
                                    })
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
                        status_text,
                        headers,
                        cookies,
                        body,
                        original_request,
                        delay_ms,
                    }
                })
                .collect()
        })
        .unwrap_or_else(|| {
            if item.get("response").is_some() {
                report.warn("An example field was present but could not be parsed.");
                requires_review = true;
            }
            Vec::new()
        });
    (examples, requires_review)
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
        || matches!(
            &request.body,
            RequestBody::Graphql { query, variables, .. }
                if query.trim().is_empty() || (!variables.is_object() && !variables.is_null())
        )
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
        assert!(list
            .1
            .test_script
            .as_deref()
            .is_some_and(|script| script.contains("pm.response.to.have.status(200)")));
        assert!(matches!(list.1.auth, Auth::Bearer { .. }));
    }

    #[test]
    fn honors_both_postman_entry_enable_flags() {
        let mut report = ImportReport::default();
        let value = serde_json::json!({
            "method": "POST",
            "url": {
                "raw": "https://api.example.test/users?disabled=1&enabled=1",
                "query": [
                    { "key": "disabled", "value": "1", "disabled": true },
                    { "key": "enabled", "value": "1", "enabled": false },
                    { "key": "kept", "value": "1" }
                ]
            },
            "header": [
                { "key": "X-Disabled", "value": "1", "disabled": true },
                { "key": "X-Also-Disabled", "value": "1", "enabled": false },
                { "key": "X-Kept", "value": "1" }
            ],
            "cookie": [
                { "key": "disabled", "value": "1", "enabled": false },
                { "key": "kept", "value": "1" }
            ],
            "body": {
                "mode": "formdata",
                "formdata": [
                    { "key": "disabled", "value": "1", "enabled": false },
                    { "key": "kept", "value": "1" }
                ]
            }
        });
        let (request, needs_review) = parse_request("flags", &value, None, &mut report);
        assert!(!needs_review);
        assert_eq!(request.headers.len(), 3);
        assert!(!request.headers[0].enabled);
        assert!(!request.headers[1].enabled);
        assert!(request.headers[2].enabled);
        assert_eq!(request.query.len(), 3);
        assert!(!request.query[0].enabled);
        assert!(!request.query[1].enabled);
        assert!(request.query[2].enabled);
        assert!(!request.cookies[0].enabled);
        assert!(request.cookies[1].enabled);
        match request.body {
            RequestBody::Multipart { parts } => {
                assert!(!parts[0].enabled);
                assert!(parts[1].enabled);
            }
            other => panic!("expected multipart body, got {other:?}"),
        }

        let item = serde_json::json!({
            "response": [{
                "name": "disabled response header",
                "header": [
                    { "key": "X-Disabled", "value": "1", "disabled": true },
                    { "key": "X-Also-Disabled", "value": "1", "enabled": false },
                    { "key": "X-Kept", "value": "1" }
                ]
            }]
        });
        let (examples, examples_require_review) = parse_examples(&item, &mut report);
        assert!(!examples_require_review);
        assert_eq!(examples[0].headers.len(), 3);
        assert!(!examples[0].headers[0].enabled);
        assert!(!examples[0].headers[1].enabled);
        assert!(examples[0].headers[2].enabled);
    }

    #[test]
    fn reports_postman_transport_and_embedded_example_fields_for_manual_review() {
        let mut report = ImportReport::default();
        let value = serde_json::json!({
            "method": "GET",
            "url": "https://api.example.test/users",
            "protocolProfileBehavior": {
                "followRedirects": false,
                "disableCookies": true,
                "strictSSL": true
            },
            "proxy": { "host": "127.0.0.1", "port": 8080 },
            "certificate": { "matches": ["api.example.test"] }
        });
        let (request, needs_review) =
            parse_request("Transport settings", &value, None, &mut report);
        assert!(needs_review);
        assert_eq!(
            request.transport,
            Some(RequestTransportSettings {
                follow_redirects: Some(false),
                max_redirects: None,
                disable_cookies: true,
            })
        );
        assert!(report.warnings.iter().any(|warning| warning
            .contains("unsupported Postman protocolProfileBehavior field strictSSL")));
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("Postman proxy")));
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("Postman certificate")));

        let item = serde_json::json!({
            "response": [{
                "name": "Saved response",
                "code": 200,
                "originalRequest": { "method": "GET", "url": "https://example.test" }
            }]
        });
        let (examples, examples_require_review) = parse_examples(&item, &mut report);
        assert!(!examples_require_review);
        assert_eq!(examples.len(), 1);
        assert_eq!(
            examples[0]
                .original_request
                .as_ref()
                .map(|request| request.method.as_str()),
            Some("GET")
        );
        assert_eq!(
            examples[0]
                .original_request
                .as_ref()
                .map(|request| request.url.as_str()),
            Some("https://example.test")
        );

        let malformed = serde_json::json!({
            "method": "POST",
            "url": {
                "raw": "https://api.example.test/users",
                "query": [{ "value": "missing-key" }]
            },
            "header": [{ "value": "missing-key" }],
            "cookie": [{ "value": "missing-key" }],
            "body": {
                "mode": "urlencoded",
                "urlencoded": [{ "value": "missing-key" }]
            }
        });
        let (_, malformed_requires_review) =
            parse_request("Malformed entries", &malformed, None, &mut report);
        assert!(malformed_requires_review);
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("Malformed entries query entry 0")));
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("Malformed entries header entry 0")));
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("Malformed entries cookie entry 0")));
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("Malformed entries urlencoded body field entry 0")));
    }

    #[test]
    fn imports_collection_variable_variants_without_activating_disabled_values() {
        let output = tempfile::tempdir().expect("output");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../compat/postman-import/collection-variable-variants-v2.1.json");

        let report = import_postman_collection(&fixture, output.path()).expect("import");
        assert_eq!(report.imported_requests, 1);
        assert_eq!(report.fully_supported_requests, 1);
        assert_eq!(report.manual_review_requests, 0);
        assert_eq!(
            report
                .warnings
                .iter()
                .filter(|warning| warning.contains("disabled"))
                .count(),
            2
        );

        let workspace = Workspace::open(output.path()).expect("workspace");
        let collection = workspace.collections().expect("collections").remove(0);
        assert_eq!(
            collection.collection.variables.get("currentOnly"),
            Some(&"from-current".to_owned())
        );
        assert_eq!(
            collection.collection.variables.get("numericValue"),
            Some(&"42".to_owned())
        );
        assert!(!collection
            .collection
            .variables
            .contains_key("disabledValue"));
        assert!(!collection
            .collection
            .variables
            .contains_key("disabledByEnabled"));
    }

    #[test]
    fn imports_inherited_digest_and_explicit_no_auth_fixture() {
        let output = tempfile::tempdir().expect("output");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../compat/postman-import/auth-and-url-variants-v2.1.json");

        let report = import_postman_collection(&fixture, output.path()).expect("import");
        assert_eq!(report.imported_requests, 2);
        assert_eq!(report.fully_supported_requests, 2);
        assert_eq!(report.manual_review_requests, 0);

        let workspace = Workspace::open(output.path()).expect("workspace");
        let collection = workspace.collections().expect("collections").remove(0);
        assert!(matches!(collection.collection.auth, Auth::Digest { .. }));
        let requests = workspace.requests(&collection).expect("requests");
        let digest = requests
            .iter()
            .find(|(_, request)| request.name == "Inherited Digest JSON")
            .expect("digest request");
        assert_eq!(digest.1.query, vec![KeyValue::enabled("scope", "read")]);
        assert!(matches!(digest.1.auth, Auth::Digest { .. }));
        assert!(matches!(digest.1.body, RequestBody::Json { .. }));

        let no_auth = requests
            .iter()
            .find(|(_, request)| request.name == "Explicit no-auth path")
            .expect("no-auth request");
        assert_eq!(no_auth.1.auth, Auth::None);
        assert_eq!(no_auth.1.url, "{{baseUrl}}/users/{{userId}}");
    }

    #[test]
    fn imports_postman_digest_auth() {
        let mut report = ImportReport::default();
        let value = serde_json::json!({
            "type": "digest",
            "digest": [
                { "key": "username", "value": "Mufasa" },
                { "key": "password", "value": "Circle Of Life" }
            ]
        });
        let parsed = parse_auth(Some(&value), "Digest request", &mut report);
        assert!(!parsed.requires_review);
        assert_eq!(
            parsed.auth,
            Auth::Digest {
                username: "Mufasa".to_owned(),
                password: "Circle Of Life".to_owned(),
            }
        );
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
        assert_eq!(environment.variables["numericValue"].value, "42");
        assert_eq!(environment.variables["booleanValue"].value, "true");
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("accessToken") && warning.contains("plaintext")));
    }

    #[test]
    fn secure_environment_import_moves_marked_secrets_to_the_store() {
        let output = tempfile::tempdir().expect("output");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../compat/postman-import/basic-environment.json");
        let secret_store = SecretStore::for_test(output.path());

        let report = import_environment_with_store(&fixture, output.path(), Some(&secret_store))
            .expect("secure environment import");
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("accessToken")
                    && warning.contains("credential store"))
        );

        let workspace = Workspace::open(output.path()).expect("workspace");
        let (_, environment) = workspace.environments().expect("environments").remove(0);
        let access_token = &environment.variables["accessToken"];
        assert!(access_token.secret);
        assert_eq!(access_token.value, "");
        let reference = access_token
            .secret_ref
            .as_deref()
            .expect("secret reference");
        let resolved = secret_store
            .resolve_environment(&environment)
            .expect("resolve secure environment");
        assert_eq!(resolved.get("accessToken"), Some(&"replace-me".to_owned()));
        assert!(!reference.contains("replace-me"));
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
        assert_eq!(report.fully_supported_requests, 6);
        assert_eq!(report.manual_review_requests, 2);
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
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("saved access token was preserved")));

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
        assert_eq!(
            search.1.transport,
            Some(RequestTransportSettings {
                follow_redirects: Some(false),
                max_redirects: Some(0),
                disable_cookies: true,
            })
        );
        assert!(matches!(&search.1.body, RequestBody::FormUrlEncoded { .. }));
        assert_eq!(search.1.examples.len(), 1);
        assert_eq!(search.1.examples[0].cookies.len(), 1);
        assert_eq!(search.1.examples[0].cookies[0].name, "session");
        assert_eq!(search.1.examples[0].cookies[0].value, "fixture-session");
        assert!(search.1.examples[0].cookies[0].http_only);
        assert_eq!(search.1.examples[0].cookies[0].path.as_deref(), Some("/"));
        assert_eq!(search.1.examples[0].cookies[0].max_age_seconds, Some(120));
        assert_eq!(
            search.1.examples[0].status_text.as_deref(),
            Some("No Content")
        );

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
        assert_eq!(
            oauth.1.auth,
            Auth::Bearer {
                token: "{{accessToken}}".to_owned()
            }
        );
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
    fn imports_postman_structured_url_port_fragment_and_query() {
        let output = tempfile::tempdir().expect("output");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../compat/postman-import/body-and-url-variants-v2.1.json");

        let report = import_postman_collection(&fixture, output.path()).expect("import");
        assert_eq!(report.imported_requests, 7);
        assert_eq!(report.fully_supported_requests, 7);
        assert_eq!(report.manual_review_requests, 0);

        let workspace = Workspace::open(output.path()).expect("workspace");
        let collection = workspace.collections().expect("collections").remove(0);
        assert_eq!(
            collection.collection.variables.get("teamId"),
            Some(&"7".to_owned())
        );
        assert_eq!(
            collection.collection.variables.get("memberId"),
            Some(&"42".to_owned())
        );
        let requests = workspace.requests(&collection).expect("requests");
        let path_variables = requests
            .iter()
            .find(|(_, request)| request.name == "Path variable metadata")
            .expect("path variable request");
        assert_eq!(
            path_variables.1.url,
            "{{baseUrl}}/teams/{{teamId}}/members/{{memberId}}"
        );
        let request = requests
            .iter()
            .find(|(_, request)| request.name == "Header-inferred JSON")
            .expect("header-inferred JSON request");
        match &request.1.body {
            RequestBody::Json { value } => assert_eq!(value["event"], "created"),
            other => panic!("expected JSON body, got {other:?}"),
        }
        let request = requests
            .iter()
            .find(|(_, request)| request.name == "Port and fragment")
            .expect("port and fragment request");
        assert_eq!(
            request.1.url,
            "https://api.example.test:8443/users/42#details"
        );
        assert_eq!(request.1.query, vec![KeyValue::enabled("view", "full")]);

        let mut raw_report = ImportReport::default();
        let raw_value = serde_json::json!({
            "method": "GET",
            "url": {
                "raw": "https://api.example.test/search?old=1#details",
                "query": [{ "key": "new", "value": "2" }]
            }
        });
        let (raw_request, needs_review) =
            parse_request("Raw query and fragment", &raw_value, None, &mut raw_report);
        assert!(!needs_review);
        assert!(raw_report.warnings.is_empty());
        assert_eq!(raw_request.url, "https://api.example.test/search#details");
        assert_eq!(raw_request.query, vec![KeyValue::enabled("new", "2")]);
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
