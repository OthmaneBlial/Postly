use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
};

use serde::Serialize;
use serde_json::{json, Map, Value};
use thiserror::Error;
use url::Url;

use crate::{
    model::{
        ApiKeyLocation, Auth, Collection, HeaderEntry, KeyValue, MultipartPart, Request,
        RequestBody, ResponseExampleCookie,
    },
    storage::{CollectionFiles, Workspace, WorkspaceError},
};

const HTTP_METHODS: [&str; 8] = [
    "get", "post", "put", "patch", "delete", "head", "options", "trace",
];
const MAX_OPENAPI_REMOTE_BYTES: usize = 16 * 1024 * 1024;
const MAX_OPENAPI_REMOTE_DOCUMENTS: usize = 32;

#[derive(Debug, Error)]
pub enum OpenApiImportError {
    #[error("could not read OpenAPI file at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid OpenAPI JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid OpenAPI YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("could not fetch OpenAPI reference {url}: {message}")]
    RemoteReference { url: String, message: String },
    #[error("OpenAPI document is not an object")]
    NotAnObject,
    #[error("unsupported or missing OpenAPI version: {0}")]
    UnsupportedVersion(String),
    #[error("workspace error: {0}")]
    Workspace(#[from] WorkspaceError),
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenApiImportReport {
    pub source: PathBuf,
    pub collection_path: PathBuf,
    pub imported_operations: usize,
    pub request_paths: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenApiExportReport {
    pub source_collection: String,
    pub output: PathBuf,
    pub exported_operations: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Error)]
pub enum OpenApiExportError {
    #[error("workspace error: {0}")]
    Workspace(#[from] WorkspaceError),
    #[error("could not serialize OpenAPI JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("could not serialize OpenAPI YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("could not write OpenAPI file {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

pub fn import_openapi(
    input_path: impl AsRef<Path>,
    output_directory: impl AsRef<Path>,
) -> Result<OpenApiImportReport, OpenApiImportError> {
    let input_path = input_path.as_ref().to_path_buf();
    let text = fs::read_to_string(&input_path).map_err(|source| OpenApiImportError::Io {
        path: input_path.clone(),
        source,
    })?;
    import_openapi_text(&input_path, &text, output_directory)
}

/// Import a local OpenAPI document while resolving absolute HTTP(S) `$ref`
/// documents with the same bounded remote loader used for URL imports.
pub async fn import_openapi_with_remote_refs(
    input_path: impl AsRef<Path>,
    output_directory: impl AsRef<Path>,
) -> Result<OpenApiImportReport, OpenApiImportError> {
    let input_path = input_path.as_ref().to_path_buf();
    let text = fs::read_to_string(&input_path).map_err(|source| OpenApiImportError::Io {
        path: input_path.clone(),
        source,
    })?;
    let document = parse_document(&input_path, &text)?;
    let mut warnings = Vec::new();
    let mut remote_documents = HashMap::new();
    prefetch_remote_references(&document, None, &mut remote_documents).await?;
    let source = input_path.canonicalize().ok().map(ReferenceSource::Local);
    let document = expand_references_with_sources(
        &document,
        source.as_ref(),
        &remote_documents,
        &mut warnings,
    );
    import_openapi_document(document, input_path, output_directory, warnings)
}

/// Import an OpenAPI document already fetched by a caller, preserving the
/// source label in the migration report. This is used by the CLI's explicit
/// URL import path and keeps document parsing independent from network I/O.
pub fn import_openapi_text(
    source: impl AsRef<Path>,
    text: &str,
    output_directory: impl AsRef<Path>,
) -> Result<OpenApiImportReport, OpenApiImportError> {
    let input_path = source.as_ref().to_path_buf();
    let document = parse_document(&input_path, text)?;
    let mut warnings = Vec::new();
    let document = expand_references(&document, &input_path, &mut warnings);
    import_openapi_document(document, input_path, output_directory, warnings)
}

/// Import an OpenAPI document with bounded HTTP(S) resolution for remote
/// `$ref` documents. The caller must pass the URL used to fetch `text` so
/// relative references can be resolved against the remote document's URL.
pub async fn import_openapi_text_with_remote_refs(
    source: impl AsRef<Path>,
    source_url: &str,
    text: &str,
    output_directory: impl AsRef<Path>,
) -> Result<OpenApiImportReport, OpenApiImportError> {
    let input_path = source.as_ref().to_path_buf();
    let base_url = Url::parse(source_url).map_err(|error| OpenApiImportError::RemoteReference {
        url: source_url.to_owned(),
        message: format!("invalid base URL: {error}"),
    })?;
    if !matches!(base_url.scheme(), "http" | "https") {
        return Err(OpenApiImportError::RemoteReference {
            url: source_url.to_owned(),
            message: "only http:// and https:// URLs are supported".to_owned(),
        });
    }
    let document = parse_document(&input_path, text)?;
    let mut warnings = Vec::new();
    let mut remote_documents = HashMap::new();
    remote_documents.insert(base_url.to_string(), document.clone());
    prefetch_remote_references(&document, Some(&base_url), &mut remote_documents).await?;
    let remote_source = ReferenceSource::Remote(base_url.to_string());
    let document = expand_references_with_sources(
        &document,
        Some(&remote_source),
        &remote_documents,
        &mut warnings,
    );
    import_openapi_document(document, input_path, output_directory, warnings)
}

fn import_openapi_document(
    document: Value,
    input_path: PathBuf,
    output_directory: impl AsRef<Path>,
    mut warnings: Vec<String>,
) -> Result<OpenApiImportReport, OpenApiImportError> {
    let root = document
        .as_object()
        .ok_or(OpenApiImportError::NotAnObject)?;
    let version = root
        .get("openapi")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !version.starts_with("3.") {
        return Err(OpenApiImportError::UnsupportedVersion(
            if version.is_empty() {
                "missing openapi field".to_owned()
            } else {
                version.to_owned()
            },
        ));
    }

    let title = root
        .get("info")
        .and_then(Value::as_object)
        .and_then(|info| info.get("title"))
        .and_then(Value::as_str)
        .filter(|title| !title.trim().is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            input_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "Imported OpenAPI".to_owned());
    let workspace = Workspace::open_or_init(output_directory, "Postly workspace")?;
    let mut transaction = workspace.begin_transaction();
    let mut collection_files = transaction.create_collection(&Collection::new(title))?;
    let server = resolve_server(root, &mut collection_files.collection, &mut warnings);
    let security_schemes = root
        .get("components")
        .and_then(Value::as_object)
        .and_then(|components| components.get("securitySchemes"))
        .and_then(Value::as_object);
    let mut request_paths = Vec::new();
    let mut imported_operations = 0;

    let Some(paths) = root.get("paths").and_then(Value::as_object) else {
        warnings.push("OpenAPI document has no paths object.".to_owned());
        transaction.save_collection(&collection_files)?;
        transaction.commit();
        return Ok(OpenApiImportReport {
            source: input_path,
            collection_path: collection_files.directory.join("postly.collection.toml"),
            imported_operations,
            request_paths,
            warnings,
        });
    };

    let mut path_names = paths.keys().cloned().collect::<Vec<_>>();
    path_names.sort();
    for path_name in path_names {
        let path_item = paths
            .get(&path_name)
            .and_then(Value::as_object)
            .ok_or_else(|| {
                warnings.push(format!("Skipped non-object path item {path_name}."));
                OpenApiImportError::NotAnObject
            })?;
        let mut operations = HTTP_METHODS
            .iter()
            .filter_map(|method| {
                path_item
                    .get(*method)
                    .map(|operation| ((*method).to_owned(), operation))
            })
            .collect::<Vec<_>>();
        operations.sort_by(|left, right| left.0.cmp(&right.0));
        for (method, operation_value) in operations {
            let Some(operation) = operation_value.as_object() else {
                warnings.push(format!(
                    "Skipped {method} {path_name}: operation is not an object."
                ));
                continue;
            };
            if operation.get("$ref").is_some() {
                warnings.push(format!(
                    "Skipped {method} {path_name}: operation reference could not be resolved."
                ));
                continue;
            }
            let mut request = Request::new(
                operation_name(&method, &path_name, operation),
                method.to_uppercase(),
                path_to_url(&server, &path_name),
            );
            request.folder = operation
                .get("tags")
                .and_then(Value::as_array)
                .and_then(|tags| tags.first())
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            request.description = operation
                .get("description")
                .or_else(|| operation.get("summary"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let parameters = merge_parameters(path_item, operation, &mut warnings);
            apply_parameters(
                &mut request,
                &path_name,
                &parameters,
                &mut collection_files.collection,
                &mut warnings,
            );
            apply_request_body(&mut request, operation, &mut warnings);
            apply_security(
                &mut request,
                root,
                operation,
                security_schemes,
                &mut warnings,
            );
            apply_response_examples(&mut request, operation, &mut warnings);
            request_paths.push(transaction.save_request(&collection_files, &request)?);
            imported_operations += 1;
        }
    }
    transaction.save_collection(&collection_files)?;
    transaction.commit();
    Ok(OpenApiImportReport {
        source: input_path,
        collection_path: collection_files.directory.join("postly.collection.toml"),
        imported_operations,
        request_paths,
        warnings,
    })
}

/// Export a native collection as an OpenAPI 3.0 document.
///
/// OpenAPI cannot represent every Postly feature, so unsupported operations
/// are left out of the standard paths map and described in the report and
/// x-postly-unmapped-requests extension instead of being silently changed.
pub fn export_openapi_collection(
    workspace: &Workspace,
    collection: &CollectionFiles,
    output: impl AsRef<Path>,
) -> Result<OpenApiExportReport, OpenApiExportError> {
    let output = output.as_ref().to_path_buf();
    let requests = workspace.requests(collection)?;
    let mut warnings = Vec::new();
    let mut paths = BTreeMap::<String, Map<String, Value>>::new();
    let mut security_schemes = Map::new();
    let mut root_security = None;
    let mut unmapped_requests = Vec::new();
    let mut server_url = None;
    let mut exported_operations = 0;

    if let Some(security_name) = security_scheme_for_auth(
        &collection.collection.auth,
        &mut security_schemes,
        &mut warnings,
    ) {
        root_security = Some(security_requirement(&security_name));
    }

    for (_, request) in &requests {
        if request.grpc.is_some() {
            warnings.push(format!(
                "Request {} is gRPC and has no standard OpenAPI operation; it was preserved in x-postly-unmapped-requests.",
                request.name
            ));
            unmapped_requests.push(json!({
                "name": request.name,
                "method": request.method,
                "url": request.url,
                "reason": "grpc request",
            }));
            continue;
        }

        let (request_server, path) = split_export_url(&request.url, &mut warnings);
        if server_url.is_none() {
            server_url = Some(request_server);
        } else if server_url.as_deref() != Some(request_server.as_str()) {
            warnings.push(format!(
                "Request {} uses a different server URL; the first server remains the document default.",
                request.name
            ));
        }

        let method = request.method.to_ascii_lowercase();
        if !HTTP_METHODS.contains(&method.as_str()) {
            warnings.push(format!(
                "Request {} uses custom method {}; it was preserved in x-postly-unmapped-requests.",
                request.name, request.method
            ));
            unmapped_requests.push(json!({
                "name": request.name,
                "method": request.method,
                "url": request.url,
                "reason": "custom HTTP method",
            }));
            continue;
        }

        let operation = export_operation(request, &path, &mut security_schemes, &mut warnings);
        paths
            .entry(path)
            .or_default()
            .insert(method, Value::Object(operation));
        exported_operations += 1;
    }

    let mut document = Map::new();
    document.insert("openapi".to_owned(), json!("3.0.3"));
    let mut info = Map::new();
    info.insert("title".to_owned(), json!(collection.collection.name));
    info.insert("version".to_owned(), json!("0.1.0"));
    if let Some(description) = &collection.collection.description {
        info.insert("description".to_owned(), json!(description));
    }
    document.insert("info".to_owned(), Value::Object(info));
    if let Some(server_url) = &server_url {
        document.insert(
            "servers".to_owned(),
            json!([openapi_server(server_url, &collection.collection)]),
        );
    }
    document.insert(
        "paths".to_owned(),
        Value::Object(
            paths
                .into_iter()
                .map(|(path, operations)| (path, Value::Object(operations)))
                .collect(),
        ),
    );
    if let Some(security) = root_security {
        document.insert("security".to_owned(), security);
    }
    if !security_schemes.is_empty() {
        document.insert(
            "components".to_owned(),
            json!({ "securitySchemes": security_schemes }),
        );
    }
    document.insert(
        "x-postly-collection-id".to_owned(),
        json!(collection.collection.id.to_string()),
    );
    if !unmapped_requests.is_empty() {
        document.insert(
            "x-postly-unmapped-requests".to_owned(),
            Value::Array(unmapped_requests),
        );
    }
    if !warnings.is_empty() {
        document.insert("x-postly-export-warnings".to_owned(), json!(warnings));
    }
    write_openapi(&output, &Value::Object(document))?;

    Ok(OpenApiExportReport {
        source_collection: collection.collection.name.clone(),
        output,
        exported_operations,
        warnings,
    })
}

fn export_operation(
    request: &Request,
    path: &str,
    security_schemes: &mut Map<String, Value>,
    warnings: &mut Vec<String>,
) -> Map<String, Value> {
    let mut operation = Map::new();
    operation.insert("operationId".to_owned(), json!(operation_id(request)));
    operation.insert("summary".to_owned(), json!(request.name));
    operation.insert("x-postly-original-url".to_owned(), json!(request.url));
    operation.insert(
        "x-postly-request-id".to_owned(),
        json!(request.id.to_string()),
    );
    if let Some(folder) = &request.folder {
        operation.insert("x-postly-folder".to_owned(), json!(folder));
    }
    if let Some(description) = &request.description {
        operation.insert("description".to_owned(), json!(description));
    }
    let parameters = export_parameters(request, path);
    if !parameters.is_empty() {
        operation.insert("parameters".to_owned(), Value::Array(parameters));
    }
    if let Some(body) = export_request_body(request, warnings) {
        operation.insert("requestBody".to_owned(), body);
    }
    operation.insert(
        "responses".to_owned(),
        export_responses(&request.examples, warnings),
    );
    if let Some(security_name) = security_scheme_for_auth(&request.auth, security_schemes, warnings)
    {
        operation.insert("security".to_owned(), security_requirement(&security_name));
    }
    if let Some(extension) = auth_extension(&request.auth) {
        operation.insert("x-postly-auth".to_owned(), extension);
    }
    operation
}

fn export_parameters(request: &Request, path: &str) -> Vec<Value> {
    let mut parameters = path_parameter_names(path)
        .into_iter()
        .map(|name| {
            json!({
                "name": name,
                "in": "path",
                "required": true,
                "schema": { "type": "string" },
            })
        })
        .collect::<Vec<_>>();
    parameters.extend(
        request
            .query
            .iter()
            .map(|parameter| export_key_value_parameter(parameter, "query")),
    );
    parameters.extend(
        request
            .headers
            .iter()
            .filter(|header| !header.key.eq_ignore_ascii_case("content-type"))
            .map(export_header_parameter),
    );
    parameters.extend(
        request
            .cookies
            .iter()
            .map(|parameter| export_key_value_parameter(parameter, "cookie")),
    );
    parameters
}

fn export_key_value_parameter(parameter: &KeyValue, location: &str) -> Value {
    let mut value = Map::new();
    value.insert("name".to_owned(), json!(parameter.key));
    value.insert("in".to_owned(), json!(location));
    value.insert("required".to_owned(), json!(location == "path"));
    value.insert("schema".to_owned(), json!({ "type": "string" }));
    value.insert("example".to_owned(), json!(parameter.value));
    if !parameter.enabled {
        value.insert("x-postly-disabled".to_owned(), json!(true));
    }
    Value::Object(value)
}

fn export_header_parameter(parameter: &HeaderEntry) -> Value {
    let mut value = Map::new();
    value.insert("name".to_owned(), json!(parameter.key));
    value.insert("in".to_owned(), json!("header"));
    value.insert("required".to_owned(), json!(false));
    value.insert("schema".to_owned(), json!({ "type": "string" }));
    value.insert("example".to_owned(), json!(parameter.value));
    if !parameter.enabled {
        value.insert("x-postly-disabled".to_owned(), json!(true));
    }
    Value::Object(value)
}

fn export_request_body(request: &Request, warnings: &mut Vec<String>) -> Option<Value> {
    let request_content_type = request
        .headers
        .iter()
        .find(|header| header.enabled && header.key.eq_ignore_ascii_case("content-type"))
        .map(|header| header.value.clone());
    let (media_type, schema, example, extension) = match &request.body {
        RequestBody::None => return None,
        RequestBody::Raw { text, content_type } => (
            content_type
                .clone()
                .or(request_content_type)
                .unwrap_or_else(|| "text/plain".to_owned()),
            json!({ "type": "string" }),
            Some(json!(text)),
            None,
        ),
        RequestBody::Json { value } => (
            request_content_type
                .filter(|value| value == "application/json" || value.ends_with("+json"))
                .unwrap_or_else(|| "application/json".to_owned()),
            schema_for_example(value),
            Some(value.clone()),
            None,
        ),
        RequestBody::Graphql {
            query,
            variables,
            operation_name,
        } => {
            let mut example = Map::new();
            example.insert("query".to_owned(), json!(query));
            example.insert("variables".to_owned(), variables.clone());
            if let Some(operation_name) = operation_name {
                example.insert("operationName".to_owned(), json!(operation_name));
            }
            warnings.push(format!(
                "Request {} exports GraphQL as an application/json envelope with x-postly-body-kind.",
                request.name
            ));
            (
                "application/json".to_owned(),
                json!({ "type": "object" }),
                Some(Value::Object(example)),
                Some(json!("graphql")),
            )
        }
        RequestBody::FormUrlEncoded { fields } => (
            "application/x-www-form-urlencoded".to_owned(),
            form_schema(fields.iter().map(|field| (&field.key, &field.value))),
            Some(form_example(
                fields.iter().map(|field| (&field.key, &field.value)),
            )),
            None,
        ),
        RequestBody::Multipart { parts } => {
            let mut properties = Map::new();
            let mut example = Map::new();
            for part in parts {
                if let Some(file_path) = &part.file_path {
                    warnings.push(format!(
                        "Multipart file path for {} in request {} is not embedded in OpenAPI: {}.",
                        part.name, request.name, file_path
                    ));
                    properties.insert(
                        part.name.clone(),
                        json!({ "type": "string", "format": "binary" }),
                    );
                } else {
                    properties.insert(part.name.clone(), json!({ "type": "string" }));
                    if part.enabled {
                        example.insert(part.name.clone(), json!(part.value));
                    }
                }
            }
            (
                "multipart/form-data".to_owned(),
                json!({ "type": "object", "properties": properties }),
                Some(Value::Object(example)),
                None,
            )
        }
        RequestBody::BinaryFile { content_type, path } => {
            warnings.push(format!(
                "Binary file path for request {} is not embedded in OpenAPI: {}.",
                request.name, path
            ));
            (
                content_type
                    .clone()
                    .unwrap_or_else(|| "application/octet-stream".to_owned()),
                json!({ "type": "string", "format": "binary" }),
                None,
                None,
            )
        }
    };
    let mut media = Map::new();
    media.insert("schema".to_owned(), schema);
    if let Some(example) = example {
        media.insert("example".to_owned(), example);
    }
    if let Some(extension) = extension {
        media.insert("x-postly-body-kind".to_owned(), extension);
    }
    let mut content = Map::new();
    content.insert(media_type, Value::Object(media));
    Some(json!({
        "required": false,
        "content": Value::Object(content),
    }))
}

fn export_responses(
    examples: &[crate::model::ResponseExample],
    warnings: &mut Vec<String>,
) -> Value {
    let mut responses = Map::new();
    if examples.is_empty() {
        responses.insert(
            "default".to_owned(),
            json!({ "description": "Response captured by Postly." }),
        );
        return Value::Object(responses);
    }
    for example in examples {
        let key = example
            .status
            .map(|status| status.to_string())
            .unwrap_or_else(|| "default".to_owned());
        if responses.contains_key(&key) {
            warnings.push(format!(
                "Response example {} duplicates status {}; only the first example is exported.",
                example.name, key
            ));
            continue;
        }
        let mut response = Map::new();
        let description = if example.name.trim().is_empty() {
            "Response captured by Postly.".to_owned()
        } else {
            example.name.clone()
        };
        response.insert("description".to_owned(), json!(description));
        let cookie_examples = example
            .cookies
            .iter()
            .filter_map(ResponseExampleCookie::to_set_cookie_header)
            .map(Value::String)
            .collect::<Vec<_>>();
        let use_structured_set_cookie = !cookie_examples.is_empty();
        let mut response_headers = example
            .headers
            .iter()
            .filter(|header| {
                !(use_structured_set_cookie && header.key.eq_ignore_ascii_case("set-cookie"))
            })
            .map(|header| {
                (
                    header.key.clone(),
                    json!({
                        "schema": { "type": "string" },
                        "example": header.value,
                    }),
                )
            })
            .collect::<Map<String, Value>>();
        if use_structured_set_cookie {
            response_headers.insert(
                "Set-Cookie".to_owned(),
                json!({
                    "schema": { "type": "array", "items": { "type": "string" } },
                    "example": cookie_examples,
                }),
            );
        }
        if !response_headers.is_empty() {
            response.insert("headers".to_owned(), Value::Object(response_headers));
        }
        if let Some(body) = &example.body {
            let (media_type, example_value) = match serde_json::from_str::<Value>(body) {
                Ok(value) => ("application/json".to_owned(), value),
                Err(_) => ("text/plain".to_owned(), json!(body)),
            };
            let mut media = Map::new();
            media.insert("schema".to_owned(), schema_for_example(&example_value));
            media.insert("example".to_owned(), example_value);
            let mut content = Map::new();
            content.insert(media_type, Value::Object(media));
            response.insert("content".to_owned(), Value::Object(content));
        }
        if example.delay_ms > 0 {
            response.insert("x-postly-delay-ms".to_owned(), json!(example.delay_ms));
        }
        responses.insert(key, Value::Object(response));
    }
    Value::Object(responses)
}

fn security_scheme_for_auth(
    auth: &Auth,
    schemes: &mut Map<String, Value>,
    warnings: &mut Vec<String>,
) -> Option<String> {
    let (name, scheme) = match auth {
        Auth::None => return None,
        Auth::Basic { .. } => ("basicAuth", json!({ "type": "http", "scheme": "basic" })),
        Auth::Digest { .. } => ("digestAuth", json!({ "type": "http", "scheme": "digest" })),
        Auth::Bearer { .. } => ("bearerAuth", json!({ "type": "http", "scheme": "bearer" })),
        Auth::ApiKey { key, location, .. } => (
            match location {
                ApiKeyLocation::Header => "apiKeyHeader",
                ApiKeyLocation::Query => "apiKeyQuery",
            },
            json!({
                "type": "apiKey",
                "name": key,
                "in": match location {
                    ApiKeyLocation::Header => "header",
                    ApiKeyLocation::Query => "query",
                },
            }),
        ),
        Auth::OAuth2ClientCredentials {
            token_url, scope, ..
        } => (
            "oauth2ClientCredentials",
            json!({
                "type": "oauth2",
                "flows": { "clientCredentials": {
                    "tokenUrl": normalize_openapi_template(token_url),
                    "scopes": openapi_scopes(scope.as_deref()),
                }}
            }),
        ),
        Auth::OAuth2AuthorizationCodePkce {
            authorization_url,
            token_url,
            scope,
            ..
        } => (
            "oauth2AuthorizationCode",
            json!({
                "type": "oauth2",
                "flows": { "authorizationCode": {
                    "authorizationUrl": normalize_openapi_template(authorization_url),
                    "tokenUrl": normalize_openapi_template(token_url),
                    "scopes": openapi_scopes(scope.as_deref()),
                }}
            }),
        ),
        Auth::OAuth2RefreshToken { .. } => {
            warnings.push(
                "Refresh-token-only authentication is approximated as bearer security; details are kept in x-postly-auth."
                    .to_owned(),
            );
            ("bearerAuth", json!({ "type": "http", "scheme": "bearer" }))
        }
        Auth::OAuth2DeviceCode { .. } => {
            warnings.push(
                "Device-code authentication is approximated as bearer security; details are kept in x-postly-auth."
                    .to_owned(),
            );
            ("bearerAuth", json!({ "type": "http", "scheme": "bearer" }))
        }
        Auth::AwsSignatureV4 { .. } => {
            warnings.push(
                "AWS Signature V4 is represented as an Authorization header; signing details are kept in x-postly-auth."
                    .to_owned(),
            );
            (
                "awsSignatureV4",
                json!({ "type": "apiKey", "name": "Authorization", "in": "header" }),
            )
        }
    };
    schemes.entry(name.to_owned()).or_insert(scheme);
    Some(name.to_owned())
}

fn security_requirement(name: &str) -> Value {
    let mut requirement = Map::new();
    requirement.insert(name.to_owned(), Value::Array(Vec::new()));
    Value::Array(vec![Value::Object(requirement)])
}

fn auth_extension(auth: &Auth) -> Option<Value> {
    match auth {
        Auth::OAuth2AuthorizationCodePkce {
            authorization_url,
            token_url,
            redirect_uri,
            ..
        } => Some(json!({
            "type": "oauth2_authorization_code_pkce",
            "authorizationUrl": authorization_url,
            "tokenUrl": token_url,
            "redirectUri": redirect_uri,
            "pkce": true,
        })),
        Auth::OAuth2RefreshToken {
            token_url,
            client_id,
            scope,
            ..
        } => Some(json!({
            "type": "oauth2_refresh_token",
            "tokenUrl": token_url,
            "clientId": client_id,
            "scope": scope,
        })),
        Auth::OAuth2DeviceCode {
            device_authorization_url,
            token_url,
            client_id,
            scope,
            ..
        } => Some(json!({
            "type": "oauth2_device_code",
            "deviceAuthorizationUrl": device_authorization_url,
            "tokenUrl": token_url,
            "clientId": client_id,
            "scope": scope,
        })),
        Auth::AwsSignatureV4 {
            access_key_id,
            region,
            service,
            ..
        } => Some(json!({
            "type": "aws_signature_v4",
            "accessKeyId": access_key_id,
            "region": region,
            "service": service,
        })),
        _ => None,
    }
}

fn openapi_scopes(scope: Option<&str>) -> Value {
    let mut scopes = Map::new();
    for name in scope.into_iter().flat_map(str::split_whitespace) {
        scopes.insert(name.to_owned(), json!(""));
    }
    Value::Object(scopes)
}

fn form_schema<'a>(fields: impl Iterator<Item = (&'a String, &'a String)>) -> Value {
    let properties = fields
        .map(|(key, _)| (key.clone(), json!({ "type": "string" })))
        .collect::<Map<String, Value>>();
    json!({ "type": "object", "properties": properties })
}

fn form_example<'a>(fields: impl Iterator<Item = (&'a String, &'a String)>) -> Value {
    Value::Object(
        fields
            .map(|(key, value)| (key.clone(), json!(value)))
            .collect(),
    )
}

fn schema_for_example(value: &Value) -> Value {
    match value {
        Value::Null => json!({ "nullable": true, "example": null }),
        Value::Bool(value) => json!({ "type": "boolean", "example": value }),
        Value::Number(number) => json!({
            "type": if number.is_i64() || number.is_u64() { "integer" } else { "number" },
            "example": number,
        }),
        Value::String(value) => {
            let mut schema = Map::new();
            schema.insert("type".to_owned(), json!("string"));
            schema.insert("example".to_owned(), json!(value));
            if let Some(format) = inferred_string_format(value) {
                schema.insert("format".to_owned(), json!(format));
            }
            Value::Object(schema)
        }
        Value::Array(values) => {
            let mut schema = Map::new();
            schema.insert("type".to_owned(), json!("array"));
            if let Some(items) = array_item_schema(values) {
                schema.insert("items".to_owned(), items);
            }
            schema.insert("example".to_owned(), value.clone());
            Value::Object(schema)
        }
        Value::Object(object) => {
            let properties = object
                .iter()
                .map(|(key, value)| (key.clone(), schema_for_example(value)))
                .collect::<Map<String, Value>>();
            json!({ "type": "object", "properties": properties, "example": value })
        }
    }
}

fn array_item_schema(values: &[Value]) -> Option<Value> {
    let mut variants = Vec::new();
    let mut shapes = Vec::new();

    for value in values {
        let schema = schema_for_example(value);
        let shape = schema_without_examples(&schema);
        if !shapes.iter().any(|candidate| candidate == &shape) {
            shapes.push(shape);
            variants.push(schema);
        }
    }

    match variants.as_slice() {
        [] => None,
        [schema] => Some(schema.clone()),
        _ => Some(json!({ "oneOf": variants })),
    }
}

fn schema_without_examples(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .filter(|(key, _)| key.as_str() != "example")
                .map(|(key, value)| (key.clone(), schema_without_examples(value)))
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(schema_without_examples).collect()),
        _ => value.clone(),
    }
}

fn inferred_string_format(value: &str) -> Option<&'static str> {
    if value.len() == 36
        && value.as_bytes().iter().enumerate().all(|(index, byte)| {
            matches!(index, 8 | 13 | 18 | 23) && *byte == b'-'
                || !matches!(index, 8 | 13 | 18 | 23) && byte.is_ascii_hexdigit()
        })
    {
        return Some("uuid");
    }
    if value.len() == 10
        && value.as_bytes().get(4) == Some(&b'-')
        && value.as_bytes().get(7) == Some(&b'-')
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return Some("date");
    }
    if value.contains('T')
        && (value.ends_with('Z') || value.contains('+') || value.rsplit_once('-').is_some())
        && value.len() >= 20
    {
        return Some("date-time");
    }
    if let Ok(url) = Url::parse(value) {
        if !url.scheme().is_empty() && url.host_str().is_some() {
            return Some("uri");
        }
    }
    let (local, domain) = value.split_once('@')?;
    if !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
    {
        Some("email")
    } else {
        None
    }
}

fn operation_id(request: &Request) -> String {
    let mut slug = String::new();
    for character in request.name.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('_') {
            slug.push('_');
        }
    }
    let slug = slug.trim_matches('_');
    let slug = if slug.is_empty() { "request" } else { slug };
    format!("{}_{}", slug, request.id.simple())
}

fn path_parameter_names(path: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find('}') else {
            break;
        };
        let name = &after_start[..end];
        if !name.is_empty() && !names.iter().any(|existing| existing == name) {
            names.push(name.to_owned());
        }
        rest = &after_start[end + 1..];
    }
    names
}

fn normalize_openapi_template(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("{{") {
        result.push_str(&rest[..start]);
        let after_start = &rest[start + 2..];
        let Some(end) = after_start.find("}}") else {
            result.push_str(&rest[start..]);
            return result;
        };
        result.push('{');
        result.push_str(&after_start[..end]);
        result.push('}');
        rest = &after_start[end + 2..];
    }
    result.push_str(rest);
    result
}

fn split_export_url(url: &str, warnings: &mut Vec<String>) -> (String, String) {
    let normalized = normalize_openapi_template(url.trim());
    if let Some(scheme_end) = normalized.find("://") {
        let scheme = &normalized[..scheme_end];
        if matches!(scheme, "http" | "https") {
            let rest = &normalized[scheme_end + 3..];
            let boundary = rest.find(['/', '?', '#']).unwrap_or(rest.len());
            let authority = &rest[..boundary];
            let authority = if let Some((_, host)) = authority.rsplit_once('@') {
                warnings.push(format!(
                    "Credentials embedded in URL {url} were omitted from the exported server."
                ));
                host
            } else {
                authority
            };
            let server = format!("{scheme}://{authority}");
            let suffix = &rest[boundary..];
            let path = suffix
                .split(['?', '#'])
                .next()
                .filter(|path| !path.is_empty())
                .unwrap_or("/");
            if suffix.contains('?') || suffix.contains('#') {
                warnings.push(format!(
                    "Query or fragment data embedded in URL {url} was omitted from the OpenAPI path; keep request query fields separately."
                ));
            }
            return (server, ensure_path(path));
        }
    }
    if normalized.starts_with('{') {
        if let Some(end) = normalized.find('}') {
            let server = normalized[..=end].to_owned();
            let suffix = &normalized[end + 1..];
            let path = suffix
                .split(['?', '#'])
                .next()
                .filter(|path| !path.is_empty())
                .unwrap_or("/");
            if suffix.contains('?') || suffix.contains('#') {
                warnings.push(format!(
                    "Query or fragment data embedded in URL {url} was omitted from the OpenAPI path."
                ));
            }
            return (server, ensure_path(path));
        }
    }
    warnings.push(format!(
        "URL {url} could not be split into an OpenAPI server and path; it was placed under https://postly.invalid."
    ));
    (
        "https://postly.invalid".to_owned(),
        ensure_path(&normalized),
    )
}

fn ensure_path(path: &str) -> String {
    if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    }
}

fn openapi_server(server: &str, collection: &Collection) -> Value {
    let mut object = Map::new();
    object.insert("url".to_owned(), json!(server));
    let variables = path_parameter_names(server)
        .into_iter()
        .map(|name| {
            let default = collection
                .variables
                .get(&name)
                .cloned()
                .unwrap_or_else(|| "https://api.example.invalid".to_owned());
            (name, json!({ "default": default }))
        })
        .collect::<Map<String, Value>>();
    if !variables.is_empty() {
        object.insert("variables".to_owned(), Value::Object(variables));
    }
    Value::Object(object)
}

fn write_openapi(path: &Path, document: &Value) -> Result<(), OpenApiExportError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|source| OpenApiExportError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let is_yaml = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "yaml" | "yml"));
    let text = if is_yaml {
        serde_yaml::to_string(document)?
    } else {
        let mut text = serde_json::to_string_pretty(document)?;
        text.push('\n');
        text
    };
    fs::write(path, text).map_err(|source| OpenApiExportError::Write {
        path: path.to_path_buf(),
        source,
    })
}

fn parse_document(path: &Path, text: &str) -> Result<Value, OpenApiImportError> {
    let is_yaml = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "yaml" | "yml"));
    if is_yaml {
        let yaml = serde_yaml::from_str::<serde_yaml::Value>(text)?;
        Ok(serde_json::to_value(yaml)?)
    } else {
        match serde_json::from_str(text) {
            Ok(document) => Ok(document),
            Err(json_error) => {
                let yaml = serde_yaml::from_str::<serde_yaml::Value>(text)
                    .map_err(|_| OpenApiImportError::Json(json_error))?;
                Ok(serde_json::to_value(yaml)?)
            }
        }
    }
}

#[derive(Clone, Debug)]
enum ReferenceSource {
    Local(PathBuf),
    Remote(String),
}

fn expand_references(document: &Value, source: &Path, warnings: &mut Vec<String>) -> Value {
    let source = source.canonicalize().ok().map(ReferenceSource::Local);
    expand_references_with_sources(document, source.as_ref(), &HashMap::new(), warnings)
}

fn expand_references_with_sources(
    document: &Value,
    source: Option<&ReferenceSource>,
    remote_documents: &HashMap<String, Value>,
    warnings: &mut Vec<String>,
) -> Value {
    struct Resolver<'a> {
        source_root: Option<PathBuf>,
        documents: HashMap<PathBuf, Value>,
        remote_documents: &'a HashMap<String, Value>,
        warnings: &'a mut Vec<String>,
    }

    impl Resolver<'_> {
        fn expand(
            &mut self,
            value: &Value,
            document: &Value,
            source: Option<&ReferenceSource>,
            stack: &mut Vec<String>,
        ) -> Value {
            let Value::Object(object) = value else {
                return match value {
                    Value::Array(values) => Value::Array(
                        values
                            .iter()
                            .map(|value| self.expand(value, document, source, stack))
                            .collect(),
                    ),
                    _ => value.clone(),
                };
            };

            if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                let (path_part, fragment) = reference.split_once('#').unwrap_or((reference, ""));
                let (target_document, target_source, reference_key) = if path_part.is_empty() {
                    (
                        document.clone(),
                        source.cloned(),
                        format!("{}#{fragment}", source_label(source)),
                    )
                } else if let Some((loaded_source, loaded, reference_key)) =
                    self.load_external_document(source, path_part, fragment)
                {
                    (loaded, Some(loaded_source), reference_key)
                } else {
                    return self.expand_object(object, document, source, stack);
                };

                if stack.contains(&reference_key) {
                    self.warnings.push(format!(
                        "OpenAPI reference cycle detected at {reference}; it was left unresolved."
                    ));
                    return self.expand_object(object, document, source, stack);
                }
                let target = if fragment.is_empty() {
                    Some(&target_document)
                } else if fragment.starts_with('/') {
                    target_document.pointer(fragment)
                } else {
                    self.warnings.push(format!(
                        "OpenAPI reference {reference} has an unsupported fragment; it was left unresolved."
                    ));
                    None
                };
                if let Some(target) = target {
                    stack.push(reference_key);
                    let expanded_target =
                        self.expand(target, &target_document, target_source.as_ref(), stack);
                    stack.pop();
                    if let Value::Object(mut resolved) = expanded_target {
                        for (key, value) in object {
                            if key != "$ref" {
                                resolved.insert(
                                    key.clone(),
                                    self.expand(value, document, source, stack),
                                );
                            }
                        }
                        return Value::Object(resolved);
                    }
                    return expanded_target;
                }
            }

            self.expand_object(object, document, source, stack)
        }

        fn expand_object(
            &mut self,
            object: &Map<String, Value>,
            document: &Value,
            source: Option<&ReferenceSource>,
            stack: &mut Vec<String>,
        ) -> Value {
            Value::Object(
                object
                    .iter()
                    .map(|(key, value)| (key.clone(), self.expand(value, document, source, stack)))
                    .collect(),
            )
        }

        fn load_external_document(
            &mut self,
            source: Option<&ReferenceSource>,
            reference_path: &str,
            fragment: &str,
        ) -> Option<(ReferenceSource, Value, String)> {
            if reference_path.starts_with("file:") {
                self.warnings.push(format!(
                    "OpenAPI reference {reference_path} is an absolute path and was rejected."
                ));
                return None;
            }
            if let Ok(url) = Url::parse(reference_path) {
                if !matches!(url.scheme(), "http" | "https") {
                    self.warnings.push(format!(
                        "OpenAPI reference {reference_path} uses an unsupported URL scheme."
                    ));
                    return None;
                }
                return self.load_remote_document(url, fragment);
            }

            match source {
                Some(ReferenceSource::Remote(base_url)) => {
                    let Ok(base_url) = Url::parse(base_url) else {
                        self.warnings.push(format!(
                            "OpenAPI reference {reference_path} has an invalid remote source URL."
                        ));
                        return None;
                    };
                    let Ok(url) = base_url.join(reference_path) else {
                        self.warnings.push(format!(
                            "OpenAPI reference {reference_path} is not a valid remote URL."
                        ));
                        return None;
                    };
                    self.load_remote_document(url, fragment)
                }
                Some(ReferenceSource::Local(source_path)) => {
                    let path = Path::new(reference_path);
                    if path.is_absolute() {
                        self.warnings.push(format!(
                            "OpenAPI reference {reference_path} is an absolute path and was rejected."
                        ));
                        return None;
                    }
                    let Some(source_root) = self.source_root.as_deref() else {
                        self.warnings.push(format!(
                            "OpenAPI reference {reference_path} has no local source root."
                        ));
                        return None;
                    };
                    let candidate = source_path.parent().unwrap_or(source_root).join(path);
                    let Ok(canonical) = fs::canonicalize(&candidate) else {
                        self.warnings.push(format!(
                            "OpenAPI reference {reference_path} does not resolve to a local file."
                        ));
                        return None;
                    };
                    if !canonical.starts_with(source_root) {
                        self.warnings.push(format!(
                            "OpenAPI reference {reference_path} points outside the source directory and was rejected."
                        ));
                        return None;
                    }
                    if let Some(document) = self.documents.get(&canonical) {
                        return Some((
                            ReferenceSource::Local(canonical.clone()),
                            document.clone(),
                            format!("{}#{fragment}", canonical.display()),
                        ));
                    }
                    let text = match fs::read_to_string(&canonical) {
                        Ok(text) => text,
                        Err(error) => {
                            self.warnings.push(format!(
                                "OpenAPI reference {reference_path} could not be read: {error}."
                            ));
                            return None;
                        }
                    };
                    let document = match parse_document(&canonical, &text) {
                        Ok(document) => document,
                        Err(error) => {
                            self.warnings.push(format!(
                                "OpenAPI reference {reference_path} is invalid: {error}."
                            ));
                            return None;
                        }
                    };
                    self.documents.insert(canonical.clone(), document.clone());
                    Some((
                        ReferenceSource::Local(canonical.clone()),
                        document,
                        format!("{}#{fragment}", canonical.display()),
                    ))
                }
                None => {
                    self.warnings.push(format!(
                        "OpenAPI reference {reference_path} is external and has no local source file."
                    ));
                    None
                }
            }
        }

        fn load_remote_document(
            &mut self,
            url: Url,
            fragment: &str,
        ) -> Option<(ReferenceSource, Value, String)> {
            let key = url.to_string();
            let Some(document) = self.remote_documents.get(&key) else {
                self.warnings.push(format!(
                    "OpenAPI remote reference {url} was not fetched and was left unresolved."
                ));
                return None;
            };
            Some((
                ReferenceSource::Remote(key.clone()),
                document.clone(),
                format!("{key}#{fragment}"),
            ))
        }
    }

    let source_root = source.and_then(|source| match source {
        ReferenceSource::Local(path) => path.parent().map(Path::to_path_buf),
        ReferenceSource::Remote(_) => None,
    });
    let mut resolver = Resolver {
        source_root,
        documents: HashMap::new(),
        remote_documents,
        warnings,
    };
    resolver.expand(document, document, source, &mut Vec::new())
}

fn source_label(source: Option<&ReferenceSource>) -> String {
    match source {
        Some(ReferenceSource::Local(source)) => source.display().to_string(),
        Some(ReferenceSource::Remote(source)) => source.clone(),
        None => "<inline>".to_owned(),
    }
}

fn collect_remote_reference_urls(value: &Value, base_url: Option<&Url>, urls: &mut Vec<Url>) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_remote_reference_urls(value, base_url, urls);
            }
        }
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                let (path_part, _) = reference.split_once('#').unwrap_or((reference, ""));
                if !path_part.is_empty() {
                    if let Ok(url) = Url::parse(path_part) {
                        if matches!(url.scheme(), "http" | "https") {
                            urls.push(url);
                        }
                    } else if let Some(base_url) = base_url {
                        if let Ok(url) = base_url.join(path_part) {
                            if matches!(url.scheme(), "http" | "https") {
                                urls.push(url);
                            }
                        }
                    }
                }
            }
            for value in object.values() {
                collect_remote_reference_urls(value, base_url, urls);
            }
        }
        _ => {}
    }
}

async fn prefetch_remote_references(
    document: &Value,
    base_url: Option<&Url>,
    remote_documents: &mut HashMap<String, Value>,
) -> Result<(), OpenApiImportError> {
    let mut queue = VecDeque::new();
    let mut queued = HashSet::new();
    let mut initial = Vec::new();
    collect_remote_reference_urls(document, base_url, &mut initial);
    for url in initial {
        if queued.insert(url.to_string()) {
            queue.push_back(url);
        }
    }

    while let Some(url) = queue.pop_front() {
        let key = url.to_string();
        if remote_documents.contains_key(&key) {
            continue;
        }
        if remote_documents.len() >= MAX_OPENAPI_REMOTE_DOCUMENTS {
            return Err(OpenApiImportError::RemoteReference {
                url: key,
                message: format!(
                    "remote reference limit of {MAX_OPENAPI_REMOTE_DOCUMENTS} documents exceeded"
                ),
            });
        }
        let document = fetch_remote_document(&url).await?;
        let mut nested = Vec::new();
        collect_remote_reference_urls(&document, Some(&url), &mut nested);
        remote_documents.insert(key, document);
        for nested_url in nested {
            if queued.insert(nested_url.to_string()) {
                queue.push_back(nested_url);
            }
        }
    }
    Ok(())
}

async fn fetch_remote_document(url: &Url) -> Result<Value, OpenApiImportError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|error| OpenApiImportError::RemoteReference {
            url: url.to_string(),
            message: error.to_string(),
        })?;
    let mut response = client.get(url.clone()).send().await.map_err(|error| {
        OpenApiImportError::RemoteReference {
            url: url.to_string(),
            message: error.to_string(),
        }
    })?;
    if !response.status().is_success() {
        return Err(OpenApiImportError::RemoteReference {
            url: url.to_string(),
            message: format!("server returned HTTP {}", response.status()),
        });
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_OPENAPI_REMOTE_BYTES as u64)
    {
        return Err(OpenApiImportError::RemoteReference {
            url: url.to_string(),
            message: format!("response exceeds {MAX_OPENAPI_REMOTE_BYTES} bytes"),
        });
    }
    let mut bytes = Vec::new();
    while let Some(chunk) =
        response
            .chunk()
            .await
            .map_err(|error| OpenApiImportError::RemoteReference {
                url: url.to_string(),
                message: error.to_string(),
            })?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_OPENAPI_REMOTE_BYTES {
            return Err(OpenApiImportError::RemoteReference {
                url: url.to_string(),
                message: format!("response exceeds {MAX_OPENAPI_REMOTE_BYTES} bytes"),
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    let text = String::from_utf8(bytes).map_err(|error| OpenApiImportError::RemoteReference {
        url: url.to_string(),
        message: format!("response is not UTF-8: {error}"),
    })?;
    let source = PathBuf::from(url.path());
    parse_document(&source, &text).map_err(|error| OpenApiImportError::RemoteReference {
        url: url.to_string(),
        message: error.to_string(),
    })
}

fn resolve_server(
    root: &Map<String, Value>,
    collection: &mut Collection,
    warnings: &mut Vec<String>,
) -> String {
    let Some(server) = root
        .get("servers")
        .and_then(Value::as_array)
        .and_then(|servers| servers.first())
        .and_then(Value::as_object)
    else {
        warnings.push(
            "OpenAPI document has no server; generated URLs use http://localhost:3000.".to_owned(),
        );
        return "http://localhost:3000".to_owned();
    };
    let mut url = server
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("http://localhost:3000")
        .to_owned();
    if let Some(variables) = server.get("variables").and_then(Value::as_object) {
        for (name, value) in variables {
            let Some(value_object) = value.as_object() else {
                continue;
            };
            let replacement = value_object
                .get("default")
                .and_then(value_to_text)
                .unwrap_or_default();
            if replacement.is_empty() {
                warnings.push(format!("Server variable {name} has no default value."));
                continue;
            }
            url = url.replace(&format!("{{{name}}}"), &replacement);
            collection.variables.insert(name.clone(), replacement);
        }
    }
    url
}

fn path_to_url(server: &str, path: &str) -> String {
    let base = server.trim_end_matches('/');
    let path = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    };
    replace_path_parameters(&format!("{base}{path}"))
}

fn replace_path_parameters(path: &str) -> String {
    let mut result = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        result.push_str(&rest[..start]);
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find('}') else {
            result.push_str(&rest[start..]);
            return result;
        };
        let name = &after_start[..end];
        if name.is_empty() {
            result.push_str("{}");
        } else {
            result.push_str("{{");
            result.push_str(name);
            result.push_str("}}");
        }
        rest = &after_start[end + 1..];
    }
    result.push_str(rest);
    result
}

fn operation_name(method: &str, path: &str, operation: &Map<String, Value>) -> String {
    operation
        .get("operationId")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("{} {}", method.to_uppercase(), path))
}

fn merge_parameters(
    path_item: &Map<String, Value>,
    operation: &Map<String, Value>,
    warnings: &mut Vec<String>,
) -> Vec<Value> {
    let mut parameters = BTreeMap::new();
    for source in [path_item.get("parameters"), operation.get("parameters")] {
        let Some(values) = source.and_then(Value::as_array) else {
            continue;
        };
        for parameter in values {
            let Some(object) = parameter.as_object() else {
                warnings.push("Skipped a non-object OpenAPI parameter.".to_owned());
                continue;
            };
            if object.get("$ref").is_some() {
                warnings
                    .push("Skipped a parameter reference that could not be resolved.".to_owned());
                continue;
            }
            let key = (
                object
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                object
                    .get("in")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            );
            parameters.insert(key, parameter.clone());
        }
    }
    parameters.into_values().collect()
}

fn apply_parameters(
    request: &mut Request,
    path: &str,
    parameters: &[Value],
    collection: &mut Collection,
    warnings: &mut Vec<String>,
) {
    for parameter in parameters {
        let Some(parameter) = parameter.as_object() else {
            continue;
        };
        let Some(name) = parameter.get("name").and_then(Value::as_str) else {
            warnings.push(format!("Skipped a parameter on {path} without a name."));
            continue;
        };
        let location = parameter
            .get("in")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let example = parameter_value(parameter);
        if location == "path" {
            if !path.contains(&format!("{{{name}}}")) {
                warnings.push(format!("Path parameter {name} is not present in {path}."));
            }
            if let Some(value) = &example {
                collection.variables.insert(name.to_owned(), value.clone());
            } else {
                warnings.push(format!("Path parameter {name} has no example/default and will need an environment value."));
            }
            continue;
        }
        let required = parameter
            .get("required")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let value = example.unwrap_or_else(|| format!("{{{{{name}}}}}"));
        let enabled = required || !value.starts_with("{{");
        if value.starts_with("{{") {
            warnings.push(format!(
                "{location} parameter {name} has no example/default."
            ));
        }
        match location {
            "query" => request.query.push(KeyValue {
                key: name.to_owned(),
                value,
                enabled,
            }),
            "header" => request.headers.push(crate::model::HeaderEntry {
                key: name.to_owned(),
                value,
                enabled,
            }),
            "cookie" => request.cookies.push(KeyValue {
                key: name.to_owned(),
                value,
                enabled,
            }),
            "path" => {}
            _ => warnings.push(format!(
                "Parameter {name} uses unsupported location {location}."
            )),
        }
    }
}

fn parameter_value(parameter: &Map<String, Value>) -> Option<String> {
    parameter
        .get("example")
        .and_then(value_to_text)
        .or_else(|| {
            parameter
                .get("examples")
                .and_then(Value::as_object)
                .and_then(|examples| examples.values().next())
                .and_then(|example| example.get("value").or(Some(example)))
                .and_then(value_to_text)
        })
        .or_else(|| {
            parameter
                .get("schema")
                .and_then(Value::as_object)
                .and_then(schema_value)
        })
}

fn schema_value(schema: &Map<String, Value>) -> Option<String> {
    schema
        .get("example")
        .and_then(value_to_text)
        .or_else(|| {
            schema
                .get("examples")
                .and_then(Value::as_array)
                .and_then(|examples| examples.first())
                .and_then(value_to_text)
        })
        .or_else(|| schema.get("default").and_then(value_to_text))
        .or_else(|| schema.get("const").and_then(value_to_text))
        .or_else(|| {
            schema
                .get("enum")
                .and_then(Value::as_array)
                .and_then(|values| values.first())
                .and_then(value_to_text)
        })
}

fn value_to_text(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Null => None,
        value => Some(value.to_string()),
    }
}

fn apply_request_body(
    request: &mut Request,
    operation: &Map<String, Value>,
    warnings: &mut Vec<String>,
) {
    let Some(body) = operation.get("requestBody").and_then(Value::as_object) else {
        return;
    };
    if body.get("$ref").is_some() {
        warnings.push(format!(
            "{} {} body reference needs manual review.",
            request.method, request.url
        ));
        return;
    }
    let Some(content) = body.get("content").and_then(Value::as_object) else {
        warnings.push(format!(
            "{} {} request body has no content map.",
            request.method, request.url
        ));
        return;
    };
    let media_type = content
        .keys()
        .find(|media_type| media_type.eq_ignore_ascii_case("application/json"))
        .cloned()
        .or_else(|| content.keys().min().cloned())
        .unwrap_or_default();
    let Some(media) = content.get(&media_type).and_then(Value::as_object) else {
        return;
    };
    let normalized_media_type = media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if normalized_media_type == "application/json" || normalized_media_type.ends_with("+json") {
        let value = media
            .get("example")
            .cloned()
            .or_else(|| {
                media
                    .get("examples")
                    .and_then(Value::as_object)
                    .and_then(|examples| examples.values().next())
                    .and_then(|example| example.get("value"))
                    .cloned()
            })
            .or_else(|| media.get("schema").and_then(sample_from_schema))
            .unwrap_or_else(|| {
                warnings.push(format!(
                    "{} {} JSON body has no example; generated an empty object.",
                    request.method, request.url
                ));
                Value::Object(Map::new())
            });
        request.body = RequestBody::Json { value };
    } else if normalized_media_type == "application/x-www-form-urlencoded" {
        let fields = openapi_form_fields(media);
        if fields.is_empty() {
            warnings.push(format!(
                "{} {} form body has no example or schema properties; imported an empty form body.",
                request.method, request.url
            ));
        }
        request.body = RequestBody::FormUrlEncoded { fields };
    } else if normalized_media_type == "multipart/form-data" {
        let parts = openapi_multipart_parts(media, warnings, request);
        if parts.is_empty() {
            warnings.push(format!(
                "{} {} multipart body has no example or schema properties; imported an empty multipart body.",
                request.method, request.url
            ));
        }
        request.body = RequestBody::Multipart { parts };
    } else if normalized_media_type.starts_with("text/") {
        let text = example_value(media)
            .and_then(|value| value_to_text(&value))
            .unwrap_or_default();
        if text.is_empty() {
            warnings.push(format!(
                "{} {} text body has no example; imported an empty text body.",
                request.method, request.url
            ));
        }
        request.body = RequestBody::Raw {
            text,
            content_type: Some(media_type.clone()),
        };
    } else if openapi_media_is_binary(&normalized_media_type, media) {
        let path = example_value(media)
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .unwrap_or_default();
        if path.is_empty() {
            warnings.push(format!(
                "{} {} binary body has no example path; choose a local file before sending.",
                request.method, request.url
            ));
        }
        request.body = RequestBody::BinaryFile {
            path,
            content_type: Some(media_type.clone()),
        };
    } else {
        warnings.push(format!(
            "{media_type} request body was not mapped; it needs manual review."
        ));
    }
}

fn openapi_form_fields(media: &Map<String, Value>) -> Vec<KeyValue> {
    let mut values = BTreeMap::new();
    if let Some(example) = example_value(media) {
        match example {
            Value::Object(object) => {
                for (name, value) in object {
                    if let Some(value) = value_to_text(&value) {
                        values.insert(name, value);
                    }
                }
            }
            Value::String(serialized) => {
                for (name, value) in url::form_urlencoded::parse(serialized.as_bytes()) {
                    values.insert(name.into_owned(), value.into_owned());
                }
            }
            _ => {}
        }
    }
    if let Some(properties) = openapi_schema_properties(media) {
        for (name, schema) in properties {
            values.entry(name.clone()).or_insert_with(|| {
                sample_from_schema(schema)
                    .and_then(|value| value_to_text(&value))
                    .unwrap_or_default()
            });
        }
    }
    values
        .into_iter()
        .map(|(key, value)| KeyValue::enabled(key, value))
        .collect()
}

fn openapi_multipart_parts(
    media: &Map<String, Value>,
    warnings: &mut Vec<String>,
    request: &Request,
) -> Vec<MultipartPart> {
    let mut values = BTreeMap::new();
    if let Some(Value::Object(object)) = example_value(media) {
        values.extend(object);
    }
    let properties = openapi_schema_properties(media);
    let mut names = values.keys().cloned().collect::<BTreeSet<_>>();
    if let Some(properties) = properties {
        names.extend(properties.keys().cloned());
    }
    let encoding = media.get("encoding").and_then(Value::as_object);
    names
        .into_iter()
        .map(|name| {
            let schema = properties.and_then(|properties| properties.get(&name));
            let binary = schema.is_some_and(openapi_schema_is_binary);
            let value = values
                .get(&name)
                .and_then(value_to_text)
                .or_else(|| {
                    schema
                        .and_then(sample_from_schema)
                        .and_then(|value| value_to_text(&value))
                })
                .unwrap_or_default();
            let file_path = binary.then(|| value.clone());
            if binary && value.is_empty() {
                warnings.push(format!(
                    "{} {} multipart binary field {name} has no example path; choose a local file before sending.",
                    request.method, request.url
                ));
            }
            let content_type = encoding
                .and_then(|encoding| encoding.get(&name))
                .and_then(Value::as_object)
                .and_then(|encoding| encoding.get("contentType"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            MultipartPart {
                name,
                value,
                file_path,
                content_type,
                enabled: true,
            }
        })
        .collect()
}

fn openapi_schema_properties(media: &Map<String, Value>) -> Option<&Map<String, Value>> {
    media
        .get("schema")
        .and_then(Value::as_object)
        .and_then(|schema| schema.get("properties"))
        .and_then(Value::as_object)
}

fn openapi_schema_is_binary(schema: &Value) -> bool {
    let Some(schema) = schema.as_object() else {
        return false;
    };
    schema
        .get("format")
        .and_then(Value::as_str)
        .is_some_and(|format| format.eq_ignore_ascii_case("binary"))
}

fn openapi_media_is_binary(media_type: &str, media: &Map<String, Value>) -> bool {
    media_type == "application/octet-stream"
        || media_type.starts_with("image/")
        || media_type.starts_with("audio/")
        || media_type.starts_with("video/")
        || media.get("schema").is_some_and(openapi_schema_is_binary)
}

fn apply_response_examples(
    request: &mut Request,
    operation: &Map<String, Value>,
    warnings: &mut Vec<String>,
) {
    let Some(responses) = operation.get("responses").and_then(Value::as_object) else {
        return;
    };

    let mut response_keys = responses.keys().cloned().collect::<Vec<_>>();
    response_keys.sort();
    for status_key in response_keys.into_iter().take(64) {
        let Some(response) = responses.get(&status_key).and_then(Value::as_object) else {
            warnings.push(format!(
                "{} {} response {status_key} is not an object.",
                request.method, request.url
            ));
            continue;
        };
        let status = if status_key.eq_ignore_ascii_case("default") {
            None
        } else {
            let parsed = status_key
                .parse::<u16>()
                .ok()
                .filter(|status| (100..=599).contains(status));
            if parsed.is_none() {
                warnings.push(format!(
                    "{} {} response key {status_key} is not a supported HTTP status.",
                    request.method, request.url
                ));
                continue;
            }
            parsed
        };
        let mut headers = Vec::new();
        let mut cookies = Vec::new();
        if let Some(response_headers) = response.get("headers").and_then(Value::as_object) {
            let mut header_names = response_headers.keys().cloned().collect::<Vec<_>>();
            header_names.sort();
            for name in header_names {
                let Some(header) = response_headers.get(&name).and_then(Value::as_object) else {
                    continue;
                };
                let is_set_cookie = name.eq_ignore_ascii_case("set-cookie");
                let example = example_value(header);
                if is_set_cookie {
                    if let Some(example) = example.as_ref() {
                        cookies.extend(response_cookies_from_openapi_value(example));
                    }
                }
                match example {
                    Some(Value::Array(values)) if is_set_cookie => {
                        for value in values
                            .into_iter()
                            .filter_map(|value| value.as_str().map(ToOwned::to_owned))
                        {
                            headers.push(HeaderEntry::enabled(name.clone(), value));
                        }
                    }
                    Some(value) => {
                        if let Some(value) = value_to_text(&value) {
                            headers.push(HeaderEntry::enabled(name, value));
                        }
                    }
                    None => {}
                }
            }
        }

        let mut body = None;
        if let Some(content) = response.get("content").and_then(Value::as_object) {
            let media_type = content
                .keys()
                .find(|media_type| {
                    media_type.eq_ignore_ascii_case("application/json")
                        || media_type.to_ascii_lowercase().ends_with("+json")
                })
                .cloned()
                .or_else(|| content.keys().next().cloned());
            if let Some(media_type) = media_type {
                if let Some(media) = content.get(&media_type).and_then(Value::as_object) {
                    if let Some(value) = example_value(media) {
                        body = Some(
                            if media_type.eq_ignore_ascii_case("application/json")
                                || media_type.to_ascii_lowercase().ends_with("+json")
                            {
                                value.to_string()
                            } else {
                                value
                                    .as_str()
                                    .map(ToOwned::to_owned)
                                    .unwrap_or_else(|| value.to_string())
                            },
                        );
                    }
                    if !headers
                        .iter()
                        .any(|header: &HeaderEntry| header.key.eq_ignore_ascii_case("content-type"))
                    {
                        headers.push(HeaderEntry::enabled("content-type", media_type));
                    }
                }
            }
        }

        let name = response
            .get("description")
            .and_then(Value::as_str)
            .filter(|description| !description.trim().is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| status.map(|status| format!("HTTP {status}")))
            .unwrap_or_else(|| "Default response".to_owned());
        let delay_ms = response
            .get("x-postly-delay-ms")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        request.examples.push(crate::model::ResponseExample {
            name,
            status,
            status_text: None,
            headers,
            cookies,
            body,
            original_request: None,
            delay_ms,
        });
    }
    if responses.len() > 64 {
        warnings.push(format!(
            "{} {} has more than 64 response examples; the remainder was skipped.",
            request.method, request.url
        ));
    }
}

fn response_cookies_from_openapi_value(value: &Value) -> Vec<ResponseExampleCookie> {
    match value {
        Value::Array(values) => values
            .iter()
            .filter_map(Value::as_str)
            .filter_map(parse_set_cookie_header)
            .collect(),
        Value::String(value) => parse_set_cookie_header(value).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn parse_set_cookie_header(value: &str) -> Option<ResponseExampleCookie> {
    let mut segments = value.split(';');
    let (name, cookie_value) = segments.next()?.trim().split_once('=')?;
    if name.is_empty() {
        return None;
    }
    let mut cookie = ResponseExampleCookie {
        name: name.to_owned(),
        value: cookie_value.trim().to_owned(),
        domain: None,
        path: None,
        secure: false,
        http_only: false,
        same_site: None,
        expires: None,
        max_age_seconds: None,
    };
    for segment in segments
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
    {
        let (attribute, attribute_value) = segment
            .split_once('=')
            .map_or((segment, None), |(attribute, value)| {
                (attribute, Some(value))
            });
        match attribute.trim().to_ascii_lowercase().as_str() {
            "domain" => cookie.domain = attribute_value.map(str::trim).map(ToOwned::to_owned),
            "path" => cookie.path = attribute_value.map(str::trim).map(ToOwned::to_owned),
            "expires" => cookie.expires = attribute_value.map(str::trim).map(ToOwned::to_owned),
            "max-age" => {
                cookie.max_age_seconds = attribute_value
                    .map(str::trim)
                    .and_then(|value| value.parse::<i64>().ok());
            }
            "samesite" => cookie.same_site = attribute_value.map(str::trim).map(ToOwned::to_owned),
            "secure" => cookie.secure = true,
            "httponly" => cookie.http_only = true,
            _ => {}
        }
    }
    Some(cookie)
}

fn example_value(object: &Map<String, Value>) -> Option<Value> {
    object
        .get("example")
        .cloned()
        .or_else(|| {
            object
                .get("examples")
                .and_then(Value::as_object)
                .and_then(|examples| examples.values().next())
                .and_then(|example| {
                    example
                        .get("value")
                        .cloned()
                        .or_else(|| Some(example.clone()))
                })
        })
        .or_else(|| object.get("schema").and_then(sample_from_schema))
}

fn sample_from_schema(schema: &Value) -> Option<Value> {
    let schema = schema.as_object()?;
    if let Some(example) = schema.get("example") {
        return Some(example.clone());
    }
    if let Some(example) = schema
        .get("examples")
        .and_then(Value::as_array)
        .and_then(|examples| examples.first())
    {
        return Some(example.clone());
    }
    if let Some(default) = schema.get("default") {
        return Some(default.clone());
    }
    if let Some(constant) = schema.get("const") {
        return Some(constant.clone());
    }
    if let Some(value) = schema
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
    {
        return Some(value.clone());
    }
    if let Some(composition) = schema
        .get("allOf")
        .and_then(Value::as_array)
        .filter(|schemas| !schemas.is_empty())
    {
        let mut merged = Map::new();
        let mut fallback = None;
        for part in composition {
            match sample_from_schema(part) {
                Some(Value::Object(object)) => merged.extend(object),
                Some(value) if fallback.is_none() => fallback = Some(value),
                _ => {}
            }
        }
        if !merged.is_empty() {
            return Some(Value::Object(merged));
        }
        if fallback.is_some() {
            return fallback;
        }
    }
    for keyword in ["oneOf", "anyOf"] {
        if let Some(value) = schema
            .get(keyword)
            .and_then(Value::as_array)
            .and_then(|schemas| schemas.iter().find_map(sample_from_schema))
        {
            return Some(value);
        }
    }
    let schema_type = match schema.get("type") {
        Some(Value::String(value)) => value.as_str(),
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(Value::as_str)
            .find(|value| *value != "null")
            .unwrap_or("object"),
        _ => "object",
    };
    match schema_type {
        "object" => {
            let mut object = Map::new();
            if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                let mut names = properties.keys().collect::<Vec<_>>();
                names.sort();
                for name in names {
                    if let Some(value) = sample_from_schema(&properties[name]) {
                        object.insert(name.clone(), value);
                    }
                }
            }
            Some(Value::Object(object))
        }
        "array" => Some(Value::Array(
            schema
                .get("items")
                .and_then(sample_from_schema)
                .into_iter()
                .collect(),
        )),
        "boolean" => Some(Value::Bool(false)),
        "integer" | "number" => Some(Value::Number(0.into())),
        "string" => Some(Value::String(
            match schema
                .get("format")
                .and_then(Value::as_str)
                .unwrap_or_default()
            {
                "date" => "2020-01-01".to_owned(),
                "date-time" => "2020-01-01T00:00:00Z".to_owned(),
                "email" => "user@example.invalid".to_owned(),
                "hostname" => "example.invalid".to_owned(),
                "ipv4" => "192.0.2.1".to_owned(),
                "ipv6" => "2001:db8::1".to_owned(),
                "uuid" => "00000000-0000-0000-0000-000000000000".to_owned(),
                "uri" | "uri-reference" | "uri-template" => "https://example.invalid".to_owned(),
                _ => String::new(),
            },
        )),
        _ => None,
    }
}

fn apply_security(
    request: &mut Request,
    root: &Map<String, Value>,
    operation: &Map<String, Value>,
    schemes: Option<&Map<String, Value>>,
    warnings: &mut Vec<String>,
) {
    let Some(security) = operation.get("security").or_else(|| root.get("security")) else {
        return;
    };
    let Some(requirements) = security.as_array() else {
        warnings.push(format!(
            "{} {} security is not an array.",
            request.method, request.url
        ));
        return;
    };
    let Some(first_requirement) = requirements.first().and_then(Value::as_object) else {
        return;
    };
    let Some((scheme_name, _)) = first_requirement.iter().next() else {
        return;
    };
    let Some(scheme) = schemes
        .and_then(|schemes| schemes.get(scheme_name))
        .and_then(Value::as_object)
    else {
        warnings.push(format!(
            "Security scheme {scheme_name} is not defined locally."
        ));
        return;
    };
    match scheme
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "http" => {
            match scheme
                .get("scheme")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str()
            {
                "bearer" => {
                    request.auth = Auth::Bearer {
                        token: format!("{{{{{scheme_name}}}}}"),
                    };
                    warnings.push(format!("Bearer security scheme {scheme_name} was imported as a variable placeholder."));
                }
                "basic" => {
                    request.auth = Auth::Basic {
                        username: format!("{{{{{scheme_name}_username}}}}"),
                        password: format!("{{{{{scheme_name}_password}}}}"),
                    };
                    warnings.push(format!("Basic security scheme {scheme_name} was imported with variable placeholders."));
                }
                "digest" => {
                    request.auth = Auth::Digest {
                        username: format!("{{{{{scheme_name}_username}}}}"),
                        password: format!("{{{{{scheme_name}_password}}}}"),
                    };
                    warnings.push(format!("Digest security scheme {scheme_name} was imported with variable placeholders."));
                }
                other => warnings.push(format!("HTTP auth scheme {other} was not mapped.")),
            }
        }
        "apikey" => {
            let name = scheme
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(scheme_name);
            let location = match scheme.get("in").and_then(Value::as_str) {
                Some("query") => ApiKeyLocation::Query,
                Some("header") => ApiKeyLocation::Header,
                _ => {
                    warnings.push(format!(
                        "API key scheme {scheme_name} has an unsupported location."
                    ));
                    return;
                }
            };
            request.auth = Auth::ApiKey {
                key: name.to_owned(),
                value: format!("{{{{{scheme_name}}}}}"),
                location,
            };
            warnings.push(format!(
                "API key security scheme {scheme_name} was imported as a variable placeholder."
            ));
        }
        "oauth2" | "openIdConnect" => warnings.push(format!(
            "Security scheme {scheme_name} requires OAuth/OpenID coordination."
        )),
        other => warnings.push(format!(
            "Security scheme {scheme_name} uses unsupported type {other}."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_openapi_yaml_into_git_friendly_requests() {
        let output = tempfile::tempdir().expect("output");
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../compat/openapi/basic-openapi-3.yaml");
        let report = import_openapi(&fixture, output.path()).expect("import");

        assert_eq!(report.imported_operations, 2);
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("token")));
        let workspace = Workspace::open(output.path()).expect("workspace");
        let collections = workspace.collections().expect("collections");
        assert_eq!(collections[0].collection.name, "Example API");
        assert_eq!(collections[0].collection.variables["version"], "v1");
        let requests = workspace.requests(&collections[0]).expect("requests");
        let get_users = requests
            .iter()
            .find(|(_, request)| request.name == "listUsers")
            .expect("list users");
        assert_eq!(get_users.1.url, "https://api.example.test/v1/users");
        assert_eq!(get_users.1.query[0], KeyValue::enabled("limit", "10"));
        let create_user = requests
            .iter()
            .find(|(_, request)| request.name == "createUser")
            .expect("create user");
        assert!(matches!(create_user.1.body, RequestBody::Json { .. }));
        assert!(matches!(create_user.1.auth, Auth::Bearer { .. }));
    }

    #[test]
    fn rejects_openapi_two_documents_explicitly() {
        let output = tempfile::tempdir().expect("output");
        let input = output.path().join("swagger.json");
        fs::write(
            &input,
            r#"{"swagger":"2.0","info":{"title":"Old"},"paths":{}}"#,
        )
        .expect("fixture");
        let error = import_openapi(&input, output.path()).expect_err("Swagger 2 must be explicit");
        assert!(error
            .to_string()
            .contains("unsupported or missing OpenAPI version"));
    }

    #[test]
    fn imports_openapi_31_local_references_and_schema_samples() {
        let output = tempfile::tempdir().expect("output");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../compat/openapi/openapi-3.1-refs.json");
        let report = import_openapi(&fixture, output.path()).expect("import");

        assert_eq!(report.imported_operations, 2);
        assert!(!report
            .warnings
            .iter()
            .any(|warning| warning.contains("local reference resolution")));
        let workspace = Workspace::open(output.path()).expect("workspace");
        let collections = workspace.collections().expect("collections");
        assert_eq!(collections[0].collection.variables["version"], "v2");
        assert_eq!(collections[0].collection.variables["userId"], "42");
        let requests = workspace.requests(&collections[0]).expect("requests");

        let get_user = requests
            .iter()
            .find(|(_, request)| request.name == "getUser")
            .expect("get user");
        assert_eq!(
            get_user.1.url,
            "https://api.example.test/v2/users/{{userId}}"
        );
        assert_eq!(get_user.1.query, vec![KeyValue::enabled("limit", "20")]);
        assert!(matches!(get_user.1.auth, Auth::Bearer { .. }));

        let replace_user = requests
            .iter()
            .find(|(_, request)| request.name == "replaceUser")
            .expect("replace user");
        match &replace_user.1.body {
            RequestBody::Json { value } => {
                assert_eq!(value["active"], true);
                assert_eq!(value["name"], "Ada");
            }
            other => panic!("expected JSON body, got {other:?}"),
        }
        assert!(matches!(replace_user.1.auth, Auth::Bearer { .. }));
    }

    #[test]
    fn imports_openapi_response_examples_into_native_requests() {
        let output = tempfile::tempdir().expect("output");
        let input = output.path().join("responses.openapi.yaml");
        fs::write(
            &input,
            r#"openapi: 3.0.3
info:
  title: Responses API
paths:
  /health:
    get:
      operationId: health
      responses:
        "200":
          description: Healthy
          headers:
            X-Request-Id:
              schema:
                type: string
              example: local-123
            Set-Cookie:
              schema:
                type: string
              example: "sid=fixture; Path=/; HttpOnly; SameSite=Lax; Max-Age=120"
          content:
            application/json:
              example:
                ok: true
          x-postly-delay-ms: 25
        "404":
          description: Missing
          content:
            text/plain:
              example: not found
        default:
          description: Unexpected response
"#,
        )
        .expect("OpenAPI fixture");

        let report = import_openapi(&input, output.path()).expect("import");
        assert_eq!(report.imported_operations, 1);
        let workspace = Workspace::open(output.path()).expect("workspace");
        let collections = workspace.collections().expect("collections");
        let requests = workspace.requests(&collections[0]).expect("requests");
        let examples = &requests[0].1.examples;
        assert_eq!(examples.len(), 3);
        assert_eq!(examples[0].status, Some(200));
        assert_eq!(examples[0].name, "Healthy");
        assert_eq!(examples[0].body.as_deref(), Some(r#"{"ok":true}"#));
        assert_eq!(examples[0].delay_ms, 25);
        assert_eq!(
            examples[0].headers,
            vec![
                HeaderEntry::enabled(
                    "Set-Cookie",
                    "sid=fixture; Path=/; HttpOnly; SameSite=Lax; Max-Age=120",
                ),
                HeaderEntry::enabled("X-Request-Id", "local-123"),
                HeaderEntry::enabled("content-type", "application/json"),
            ]
        );
        assert_eq!(examples[0].cookies.len(), 1);
        assert_eq!(examples[0].cookies[0].name, "sid");
        assert_eq!(examples[0].cookies[0].value, "fixture");
        assert_eq!(examples[0].cookies[0].path.as_deref(), Some("/"));
        assert!(examples[0].cookies[0].http_only);
        assert_eq!(examples[0].cookies[0].same_site.as_deref(), Some("Lax"));
        assert_eq!(examples[0].cookies[0].max_age_seconds, Some(120));
        assert_eq!(examples[1].status, Some(404));
        assert_eq!(examples[1].body.as_deref(), Some("not found"));
        assert_eq!(examples[2].status, None);
        assert_eq!(examples[2].name, "Unexpected response");
    }

    #[test]
    fn imports_openapi_non_json_request_bodies_into_native_models() {
        let output = tempfile::tempdir().expect("output");
        let input = output.path().join("bodies.openapi.yaml");
        fs::write(
            &input,
            r#"openapi: 3.0.3
info:
  title: Body API
servers:
  - url: https://api.example.test
paths:
  /login:
    post:
      operationId: login
      requestBody:
        content:
          application/x-www-form-urlencoded:
            schema:
              type: object
              properties:
                username:
                  type: string
                  example: Ada
                remember:
                  type: boolean
                  default: true
  /upload:
    post:
      operationId: upload
      requestBody:
        content:
          multipart/form-data:
            example:
              avatar: ./fixtures/avatar.png
              caption: Ada
            schema:
              type: object
              properties:
                avatar:
                  type: string
                  format: binary
                caption:
                  type: string
            encoding:
              avatar:
                contentType: image/png
  /archive:
    put:
      operationId: archive
      requestBody:
        content:
          application/octet-stream:
            example: ./fixtures/archive.bin
  /text:
    post:
      operationId: text
      requestBody:
        content:
          text/plain:
            example: hello from OpenAPI
"#,
        )
        .expect("OpenAPI fixture");

        let report = import_openapi(&input, output.path()).expect("import");
        assert_eq!(report.imported_operations, 4);
        assert!(!report
            .warnings
            .iter()
            .any(|warning| warning.contains("body was not mapped")));

        let workspace = Workspace::open(output.path()).expect("workspace");
        let collections = workspace.collections().expect("collections");
        let requests = workspace.requests(&collections[0]).expect("requests");

        let login = requests
            .iter()
            .find(|(_, request)| request.name == "login")
            .expect("login request");
        assert!(matches!(
            &login.1.body,
            RequestBody::FormUrlEncoded { fields }
                if fields == &vec![
                    KeyValue::enabled("remember", "true"),
                    KeyValue::enabled("username", "Ada")
                ]
        ));

        let upload = requests
            .iter()
            .find(|(_, request)| request.name == "upload")
            .expect("upload request");
        match &upload.1.body {
            RequestBody::Multipart { parts } => {
                assert_eq!(parts.len(), 2);
                assert_eq!(parts[0].name, "avatar");
                assert_eq!(parts[0].file_path.as_deref(), Some("./fixtures/avatar.png"));
                assert_eq!(parts[0].content_type.as_deref(), Some("image/png"));
                assert_eq!(parts[1].name, "caption");
                assert_eq!(parts[1].value, "Ada");
                assert!(parts[1].file_path.is_none());
            }
            other => panic!("expected multipart body, got {other:?}"),
        }

        let archive = requests
            .iter()
            .find(|(_, request)| request.name == "archive")
            .expect("archive request");
        assert!(matches!(
            &archive.1.body,
            RequestBody::BinaryFile { path, content_type }
                if path == "./fixtures/archive.bin"
                    && content_type.as_deref() == Some("application/octet-stream")
        ));

        let text = requests
            .iter()
            .find(|(_, request)| request.name == "text")
            .expect("text request");
        assert!(matches!(
            &text.1.body,
            RequestBody::Raw { text, content_type }
                if text == "hello from OpenAPI"
                    && content_type.as_deref() == Some("text/plain")
        ));
    }

    #[test]
    fn resolves_external_local_references_and_rejects_source_traversal() {
        let source = tempfile::tempdir().expect("source");
        let output = tempfile::tempdir().expect("output");
        let common = source.path().join("common.yaml");
        fs::write(
            &common,
            r#"components:
  schemas:
    User:
      type: object
      properties:
        name:
          type: string
          example: Ada
    Node:
      type: object
      properties:
        child:
          $ref: '#/components/schemas/Node'
"#,
        )
        .expect("common schema");
        let outside = source.path().join("..").join("postly-openapi-outside.yaml");
        fs::write(
            &outside,
            r#"components:
  schemas:
    Secret:
      type: string
"#,
        )
        .expect("outside schema");
        let root = source.path().join("openapi.yaml");
        fs::write(
            &root,
            r#"openapi: 3.0.3
info:
  title: Multi-file API
servers:
  - url: https://api.example.test
paths:
  /users:
    post:
      operationId: createUser
      requestBody:
        content:
          application/json:
            schema:
              $ref: ./common.yaml#/components/schemas/User
      parameters:
        - $ref: ../postly-openapi-outside.yaml#/components/schemas/Secret
components:
  schemas:
    Node:
      $ref: ./common.yaml#/components/schemas/Node
"#,
        )
        .expect("root document");

        let report = import_openapi(&root, output.path()).expect("import");
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("outside the source directory")));
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("cycle detected")));
        let workspace = Workspace::open(output.path()).expect("workspace");
        let collections = workspace.collections().expect("collections");
        let requests = workspace.requests(&collections[0]).expect("requests");
        match &requests[0].1.body {
            RequestBody::Json { value } => assert_eq!(value["name"], "Ada"),
            other => panic!("expected resolved JSON body, got {other:?}"),
        }
        fs::remove_file(outside).expect("cleanup outside schema");
    }

    #[tokio::test]
    async fn imports_bounded_remote_openapi_references() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let output = tempfile::tempdir().expect("output");
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("remote reference request");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).await.expect("request bytes");
                assert!(read > 0, "remote reference client closed early");
                request.extend_from_slice(&buffer[..read]);
            }
            let request = String::from_utf8_lossy(&request);
            assert!(request.starts_with("GET /components.yaml HTTP/1.1"));
            let body = br#"components:
  schemas:
    User:
      type: object
      properties:
        name:
          type: string
          example: Ada
"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/yaml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("response headers");
            stream.write_all(body).await.expect("response body");
        });

        let source_url = format!("http://{address}/openapi.yaml");
        let root = r#"openapi: 3.0.3
info:
  title: Remote API
servers:
  - url: https://api.example.test
paths:
  /users:
    post:
      operationId: createUser
      requestBody:
        content:
          application/json:
            schema:
              $ref: ./components.yaml#/components/schemas/User
"#;
        let report = import_openapi_text_with_remote_refs(
            Path::new("remote.openapi.yaml"),
            &source_url,
            root,
            output.path(),
        )
        .await
        .expect("remote import");
        assert_eq!(report.imported_operations, 1);
        assert!(report.warnings.is_empty());
        let workspace = Workspace::open(output.path()).expect("workspace");
        let collections = workspace.collections().expect("collections");
        let requests = workspace.requests(&collections[0]).expect("requests");
        match &requests[0].1.body {
            RequestBody::Json { value } => assert_eq!(value["name"], "Ada"),
            other => panic!("expected JSON body, got {other:?}"),
        }
        server.await.expect("remote server");
    }

    #[tokio::test]
    async fn local_openapi_can_resolve_absolute_remote_references() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let source = tempfile::tempdir().expect("source");
        let output = tempfile::tempdir().expect("output");
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("remote reference request");
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).await.expect("request bytes");
            assert!(String::from_utf8_lossy(&request[..read])
                .starts_with("GET /components.yaml HTTP/1.1"));
            let body = br#"components:
  schemas:
    Health:
      type: object
      properties:
        ok:
          type: boolean
          example: true
"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("response headers");
            stream.write_all(body).await.expect("response body");
        });

        let root = source.path().join("openapi.yaml");
        fs::write(
            &root,
            format!(
                "openapi: 3.0.3\ninfo:\n  title: Local root\nservers:\n  - url: https://api.example.test\npaths:\n  /health:\n    get:\n      operationId: health\n      requestBody:\n        content:\n          application/json:\n            schema:\n              $ref: http://{address}/components.yaml#/components/schemas/Health\n"
            ),
        )
        .expect("root document");

        let report = import_openapi_with_remote_refs(&root, output.path())
            .await
            .expect("local remote import");
        assert_eq!(report.imported_operations, 1);
        assert!(report.warnings.is_empty());
        let workspace = Workspace::open(output.path()).expect("workspace");
        let collections = workspace.collections().expect("collections");
        let requests = workspace.requests(&collections[0]).expect("requests");
        match &requests[0].1.body {
            RequestBody::Json { value } => assert_eq!(value["ok"], true),
            other => panic!("expected JSON body, got {other:?}"),
        }
        server.await.expect("remote server");
    }

    #[test]
    fn generates_composed_schema_samples_with_arrays_and_formats() {
        let schema = json!({
            "allOf": [
                {
                    "type": "object",
                    "properties": {
                        "id": { "type": "integer", "example": 42 }
                    }
                },
                {
                    "type": "object",
                    "properties": {
                        "email": { "type": "string", "format": "email" },
                        "tags": {
                            "type": "array",
                            "items": { "type": "string", "example": "api" }
                        }
                    }
                }
            ]
        });
        assert_eq!(
            sample_from_schema(&schema),
            Some(json!({
                "email": "user@example.invalid",
                "id": 42,
                "tags": ["api"]
            }))
        );

        assert_eq!(
            sample_from_schema(&json!({
                "oneOf": [
                    { "type": "string", "format": "uuid" },
                    { "type": "string", "example": "ignored" }
                ]
            })),
            Some(json!("00000000-0000-0000-0000-000000000000"))
        );
    }

    #[test]
    fn generates_export_schemas_with_examples_and_safe_string_formats() {
        let schema = schema_for_example(&json!({
            "id": "550e8400-e29b-41d4-a716-446655440000",
            "createdAt": "2024-01-02T03:04:05Z",
            "homepage": "https://api.example.test/users/1",
            "email": "ada@example.test",
            "deletedAt": null,
            "tags": ["api"]
        }));

        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["id"]["format"], "uuid");
        assert_eq!(schema["properties"]["createdAt"]["format"], "date-time");
        assert_eq!(schema["properties"]["homepage"]["format"], "uri");
        assert_eq!(schema["properties"]["email"]["format"], "email");
        assert_eq!(schema["properties"]["deletedAt"]["nullable"], true);
        assert_eq!(schema["properties"]["tags"]["items"]["example"], "api");
        assert_eq!(schema["properties"]["tags"]["example"][0], "api");

        let homogeneous = schema_for_example(&json!([{"id": 1}, {"id": 2}]));
        assert_eq!(homogeneous["items"]["type"], "object");
        assert_eq!(homogeneous["items"]["properties"]["id"]["example"], 1);
        assert!(homogeneous["items"].get("oneOf").is_none());

        let heterogeneous = schema_for_example(&json!([
            {"id": 1},
            {"name": "Ada"},
            {"id": 2}
        ]));
        assert_eq!(heterogeneous["items"]["oneOf"].as_array().unwrap().len(), 2);
        assert_eq!(
            heterogeneous["items"]["oneOf"][0]["properties"]["id"]["example"],
            1
        );
        assert_eq!(
            heterogeneous["items"]["oneOf"][1]["properties"]["name"]["example"],
            "Ada"
        );
    }

    #[test]
    fn exports_openapi_json_and_yaml_with_native_request_semantics() {
        let directory = tempfile::tempdir().expect("workspace directory");
        let workspace = Workspace::init(directory.path(), "Demo").expect("workspace");
        let mut collection = workspace
            .create_collection(&Collection::new("Users"))
            .expect("collection");
        collection.collection.description = Some("A local user API".to_owned());
        collection
            .collection
            .variables
            .insert("baseUrl".to_owned(), "https://api.example.test".to_owned());
        workspace.save_collection(&collection).expect("collection");

        let mut request = Request::new("Create user", "POST", "{{baseUrl}}/users/{{userId}}");
        request.folder = Some("Users / Write".to_owned());
        request.query.push(KeyValue::enabled("verbose", "true"));
        request
            .headers
            .push(HeaderEntry::enabled("Content-Type", "application/json"));
        request.auth = Auth::Bearer {
            token: "{{accessToken}}".to_owned(),
        };
        request.body = RequestBody::Json {
            value: json!({ "name": "Ada", "active": true }),
        };
        request.examples.push(crate::model::ResponseExample {
            name: "Created".to_owned(),
            status: Some(201),
            status_text: None,
            headers: vec![HeaderEntry::enabled("content-type", "application/json")],
            cookies: vec![crate::model::ResponseExampleCookie {
                name: "sid".to_owned(),
                value: "fixture".to_owned(),
                domain: Some("example.test".to_owned()),
                path: Some("/".to_owned()),
                secure: true,
                http_only: true,
                same_site: Some("Lax".to_owned()),
                expires: None,
                max_age_seconds: Some(120),
            }],
            body: Some(r#"{"id":1}"#.to_owned()),
            original_request: None,
            delay_ms: 0,
        });
        workspace
            .save_request(&collection, &request)
            .expect("request");

        let json_path = directory.path().join("users.openapi.json");
        let report =
            export_openapi_collection(&workspace, &collection, &json_path).expect("JSON export");
        assert_eq!(report.exported_operations, 1);
        assert!(report.warnings.is_empty());
        let document: Value =
            serde_json::from_str(&fs::read_to_string(&json_path).expect("JSON document"))
                .expect("OpenAPI JSON");
        assert_eq!(document["openapi"], "3.0.3");
        assert_eq!(document["servers"][0]["url"], "{baseUrl}");
        assert_eq!(
            document["servers"][0]["variables"]["baseUrl"]["default"],
            "https://api.example.test"
        );
        assert_eq!(
            document["paths"]["/users/{userId}"]["post"]["summary"],
            "Create user"
        );
        assert_eq!(
            document["paths"]["/users/{userId}"]["post"]["security"][0]["bearerAuth"],
            json!([])
        );
        assert_eq!(
            document["paths"]["/users/{userId}"]["post"]["requestBody"]["content"]
                ["application/json"]["example"]["name"],
            "Ada"
        );
        assert_eq!(
            document["paths"]["/users/{userId}"]["post"]["responses"]["201"]["content"]
                ["application/json"]["example"]["id"],
            1
        );
        assert_eq!(
            document["paths"]["/users/{userId}"]["post"]["responses"]["201"]["headers"]
                ["Set-Cookie"]["example"][0],
            "sid=fixture; Domain=example.test; Path=/; SameSite=Lax; Max-Age=120; Secure; HttpOnly"
        );

        let yaml_path = directory.path().join("users.openapi.yaml");
        export_openapi_collection(&workspace, &collection, &yaml_path).expect("YAML export");
        let yaml: serde_yaml::Value =
            serde_yaml::from_str(&fs::read_to_string(yaml_path).expect("YAML document"))
                .expect("OpenAPI YAML");
        assert_eq!(yaml["openapi"], "3.0.3");
    }
}
