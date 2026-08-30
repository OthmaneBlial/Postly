use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::Serialize;
use serde_json::{Map, Value};
use thiserror::Error;

use crate::{
    model::{ApiKeyLocation, Auth, Collection, KeyValue, Request, RequestBody},
    storage::{Workspace, WorkspaceError},
};

const HTTP_METHODS: [&str; 8] = [
    "get", "post", "put", "patch", "delete", "head", "options", "trace",
];

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

pub fn import_openapi(
    input_path: impl AsRef<Path>,
    output_directory: impl AsRef<Path>,
) -> Result<OpenApiImportReport, OpenApiImportError> {
    let input_path = input_path.as_ref().to_path_buf();
    let text = fs::read_to_string(&input_path).map_err(|source| OpenApiImportError::Io {
        path: input_path.clone(),
        source,
    })?;
    let document = parse_document(&input_path, &text)?;
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
    let mut collection_files = workspace.create_collection(&Collection::new(title))?;
    let mut warnings = Vec::new();
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
        workspace.save_collection(&collection_files)?;
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
                    "Skipped {method} {path_name}: operation references an external object."
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
            request_paths.push(workspace.save_request(&collection_files, &request)?);
            imported_operations += 1;
        }
    }
    workspace.save_collection(&collection_files)?;
    Ok(OpenApiImportReport {
        source: input_path,
        collection_path: collection_files.directory.join("postly.collection.toml"),
        imported_operations,
        request_paths,
        warnings,
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
        Ok(serde_json::from_str(text)?)
    }
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
                warnings.push("Skipped a parameter reference; local reference resolution is not implemented yet.".to_owned());
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
        .or_else(|| schema.get("default").and_then(value_to_text))
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
    let media_type = if content.contains_key("application/json") {
        "application/json"
    } else {
        content.keys().min().map(String::as_str).unwrap_or_default()
    };
    let Some(media) = content.get(media_type).and_then(Value::as_object) else {
        return;
    };
    if media_type == "application/json" || media_type.ends_with("+json") {
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
    } else if media_type.starts_with("text/") {
        request.body = RequestBody::Raw {
            text: media
                .get("example")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            content_type: Some(media_type.to_owned()),
        };
    } else {
        warnings.push(format!(
            "{media_type} request body was not mapped; it needs manual review."
        ));
    }
}

fn sample_from_schema(schema: &Value) -> Option<Value> {
    let schema = schema.as_object()?;
    if let Some(example) = schema.get("example") {
        return Some(example.clone());
    }
    if let Some(default) = schema.get("default") {
        return Some(default.clone());
    }
    match schema
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("object")
    {
        "object" => Some(Value::Object(Map::new())),
        "array" => Some(Value::Array(Vec::new())),
        "boolean" => Some(Value::Bool(false)),
        "integer" | "number" => Some(Value::Number(0.into())),
        "string" => Some(Value::String(String::new())),
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
}
