use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde_json::{json, Map, Value};
use thiserror::Error;

use crate::{
    model::{ApiKeyLocation, Auth, Collection, Environment, MultipartPart, Request, RequestBody},
    storage::{CollectionFiles, Workspace, WorkspaceError},
};

#[derive(Debug, Error)]
pub enum ExportError {
    #[error("workspace error: {0}")]
    Workspace(#[from] WorkspaceError),
    #[error("could not serialize Postman export: {0}")]
    Json(#[from] serde_json::Error),
    #[error("could not write export file {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ExportReport {
    pub output: String,
    pub collection_name: Option<String>,
    pub exported_requests: usize,
    pub warnings: Vec<String>,
}

pub fn export_postman_collection(
    workspace: &Workspace,
    collection: &CollectionFiles,
    output: impl AsRef<Path>,
) -> Result<ExportReport, ExportError> {
    let output = output.as_ref().to_path_buf();
    let requests = workspace.requests(collection)?;
    let mut root = FolderNode::default();
    for (_, request) in &requests {
        root.insert(request.clone());
    }
    let document = json!({
        "info": collection_info(&collection.collection),
        "item": node_items(&root)?,
        "variable": collection_variables(&collection.collection),
        "event": collection_events(&collection.collection),
    });
    write_json(&output, &document)?;
    Ok(ExportReport {
        output: output.display().to_string(),
        collection_name: Some(collection.collection.name.clone()),
        exported_requests: requests.len(),
        warnings: Vec::new(),
    })
}

pub fn export_postman_environment(
    environment: &Environment,
    output: impl AsRef<Path>,
) -> Result<ExportReport, ExportError> {
    let output = output.as_ref().to_path_buf();
    let values = environment
        .variables
        .iter()
        .map(|(key, variable)| {
            json!({
                "key": key,
                "value": variable.value,
                "enabled": variable.enabled,
                "type": if variable.secret { "secret" } else { "default" },
            })
        })
        .collect::<Vec<_>>();
    write_json(
        &output,
        &json!({
            "id": environment.name,
            "name": environment.name,
            "values": values,
            "_postman_variable_scope": "environment",
            "_postman_exported_using": "Postly",
        }),
    )?;
    Ok(ExportReport {
        output: output.display().to_string(),
        collection_name: Some(environment.name.clone()),
        exported_requests: 0,
        warnings: Vec::new(),
    })
}

#[derive(Debug, Default)]
struct FolderNode {
    folders: BTreeMap<String, FolderNode>,
    requests: Vec<Request>,
}

impl FolderNode {
    fn insert(&mut self, request: Request) {
        let mut node = self;
        if let Some(folder) = request.folder.as_deref() {
            for segment in folder
                .split(['/', '\\'])
                .map(str::trim)
                .filter(|segment| !segment.is_empty())
            {
                node = node.folders.entry(segment.to_owned()).or_default();
            }
        }
        node.requests.push(request);
    }
}

fn node_items(node: &FolderNode) -> Result<Vec<Value>, ExportError> {
    let mut items = Vec::new();
    for (name, folder) in &node.folders {
        items.push(json!({
            "name": name,
            "item": node_items(folder)?,
        }));
    }
    for request in &node.requests {
        items.push(postman_item(request)?);
    }
    Ok(items)
}

fn collection_info(collection: &Collection) -> Value {
    let mut info = Map::new();
    info.insert("_postman_id".to_owned(), json!(collection.id.to_string()));
    info.insert("name".to_owned(), json!(collection.name));
    info.insert(
        "schema".to_owned(),
        json!("https://schema.getpostman.com/json/collection/v2.1.0/collection.json"),
    );
    if let Some(description) = &collection.description {
        info.insert("description".to_owned(), json!(description));
    }
    Value::Object(info)
}

fn collection_variables(collection: &Collection) -> Vec<Value> {
    collection
        .variables
        .iter()
        .map(|(key, value)| json!({ "key": key, "value": value }))
        .collect()
}

fn collection_events(collection: &Collection) -> Vec<Value> {
    script_events(
        collection.pre_request_script.as_deref(),
        collection.test_script.as_deref(),
    )
}

fn postman_item(request: &Request) -> Result<Value, ExportError> {
    let mut request_value = Map::new();
    request_value.insert("method".to_owned(), json!(request.method));
    request_value.insert("header".to_owned(), json!(headers(&request.headers)));
    request_value.insert("url".to_owned(), request_url(request));
    if let Some(body) = body_value(&request.body)? {
        request_value.insert("body".to_owned(), body);
    }
    if let Some(auth) = postman_auth(&request.auth) {
        request_value.insert("auth".to_owned(), auth);
    }
    if !request.cookies.is_empty() {
        request_value.insert(
            "cookie".to_owned(),
            json!(request
                .cookies
                .iter()
                .map(|cookie| json!({
                    "key": cookie.key,
                    "value": cookie.value,
                    "disabled": !cookie.enabled,
                }))
                .collect::<Vec<_>>()),
        );
    }
    if let Some(description) = &request.description {
        request_value.insert("description".to_owned(), json!(description));
    }

    let mut item = Map::new();
    item.insert("name".to_owned(), json!(request.name));
    item.insert("request".to_owned(), Value::Object(request_value));
    if !request.examples.is_empty() {
        item.insert(
            "response".to_owned(),
            json!(request
                .examples
                .iter()
                .map(|example| {
                    json!({
                        "name": example.name,
                        "code": example.status,
                        "header": headers(&example.headers),
                        "body": example.body,
                    })
                })
                .collect::<Vec<_>>()),
        );
    }
    if request.pre_request_script.is_some() || request.test_script.is_some() {
        item.insert(
            "event".to_owned(),
            json!(script_events(
                request.pre_request_script.as_deref(),
                request.test_script.as_deref(),
            )),
        );
    }
    Ok(Value::Object(item))
}

fn request_url(request: &Request) -> Value {
    let mut url = Map::new();
    url.insert("raw".to_owned(), json!(request.url));
    if !request.query.is_empty() {
        url.insert(
            "query".to_owned(),
            json!(request
                .query
                .iter()
                .map(|pair| json!({
                    "key": pair.key,
                    "value": pair.value,
                    "disabled": !pair.enabled,
                }))
                .collect::<Vec<_>>()),
        );
    }
    Value::Object(url)
}

fn headers(headers: &[crate::model::HeaderEntry]) -> Vec<Value> {
    headers
        .iter()
        .map(|header| {
            json!({
                "key": header.key,
                "value": header.value,
                "disabled": !header.enabled,
            })
        })
        .collect()
}

fn body_value(body: &RequestBody) -> Result<Option<Value>, ExportError> {
    let body = match body {
        RequestBody::None => return Ok(None),
        RequestBody::Raw { text, content_type } => raw_body(text, content_type.as_deref()),
        RequestBody::Json { value } => {
            raw_body(&serde_json::to_string_pretty(value)?, Some("json"))
        }
        RequestBody::Graphql {
            query,
            variables,
            operation_name,
        } => {
            let mut graphql = Map::new();
            graphql.insert("query".to_owned(), json!(query));
            graphql.insert(
                "variables".to_owned(),
                json!(serde_json::to_string(variables)?),
            );
            if let Some(operation_name) = operation_name {
                graphql.insert("operationName".to_owned(), json!(operation_name));
            }
            json!({ "mode": "graphql", "graphql": Value::Object(graphql) })
        }
        RequestBody::FormUrlEncoded { fields } => json!({
            "mode": "urlencoded",
            "urlencoded": fields.iter().map(|field| json!({
                "key": field.key,
                "value": field.value,
                "disabled": !field.enabled,
            })).collect::<Vec<_>>(),
        }),
        RequestBody::Multipart { parts } => json!({
            "mode": "formdata",
            "formdata": parts.iter().map(multipart_part).collect::<Vec<_>>(),
        }),
        RequestBody::BinaryFile { path, .. } => json!({
            "mode": "file",
            "file": { "src": path },
        }),
    };
    Ok(Some(body))
}

fn raw_body(text: &str, content_type: Option<&str>) -> Value {
    let mut body = Map::new();
    body.insert("mode".to_owned(), json!("raw"));
    body.insert("raw".to_owned(), json!(text));
    if let Some(language) = raw_language(content_type) {
        body.insert(
            "options".to_owned(),
            json!({ "raw": { "language": language } }),
        );
    }
    Value::Object(body)
}

fn raw_language(content_type: Option<&str>) -> Option<&'static str> {
    let content_type = content_type?.to_ascii_lowercase();
    if content_type.contains("json") {
        Some("json")
    } else if content_type.contains("xml") {
        Some("xml")
    } else if content_type.contains("html") {
        Some("html")
    } else if content_type.contains("javascript") {
        Some("javascript")
    } else {
        None
    }
}

fn multipart_part(part: &MultipartPart) -> Value {
    let mut value = Map::new();
    value.insert("key".to_owned(), json!(part.name));
    value.insert("disabled".to_owned(), json!(!part.enabled));
    if let Some(path) = &part.file_path {
        value.insert("type".to_owned(), json!("file"));
        value.insert("src".to_owned(), json!(path));
    } else {
        value.insert("type".to_owned(), json!("text"));
        value.insert("value".to_owned(), json!(part.value));
    }
    if let Some(content_type) = &part.content_type {
        value.insert("contentType".to_owned(), json!(content_type));
    }
    Value::Object(value)
}

fn postman_auth(auth: &Auth) -> Option<Value> {
    let value = match auth {
        Auth::None => return None,
        Auth::Basic { username, password } => json!({
            "type": "basic",
            "basic": [
                { "key": "username", "value": username, "type": "string" },
                { "key": "password", "value": password, "type": "string" },
            ],
        }),
        Auth::Bearer { token } => json!({
            "type": "bearer",
            "bearer": [{ "key": "token", "value": token, "type": "string" }],
        }),
        Auth::ApiKey {
            key,
            value,
            location,
        } => json!({
            "type": "apikey",
            "apikey": [
                { "key": "key", "value": key, "type": "string" },
                { "key": "value", "value": value, "type": "string" },
                { "key": "in", "value": match location {
                    ApiKeyLocation::Header => "header",
                    ApiKeyLocation::Query => "query",
                }, "type": "string" },
            ],
        }),
    };
    Some(value)
}

fn script_events(pre_request: Option<&str>, test: Option<&str>) -> Vec<Value> {
    [("prerequest", pre_request), ("test", test)]
        .into_iter()
        .filter_map(|(listen, script)| {
            script.map(|script| {
                json!({
                    "listen": listen,
                    "script": {
                        "type": "text/javascript",
                        "exec": script.lines().collect::<Vec<_>>(),
                    },
                })
            })
        })
        .collect()
}

fn write_json(path: &Path, document: &Value) -> Result<(), ExportError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|source| ExportError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let mut text = serde_json::to_string_pretty(document)?;
    text.push('\n');
    fs::write(path, text).map_err(|source| ExportError::Write {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        import_postman_collection,
        model::{HeaderEntry, KeyValue, ResponseExample},
        Auth, Collection, EnvironmentVariable,
    };

    #[test]
    fn exports_a_collection_that_imports_back_with_core_semantics() {
        let directory = tempfile::tempdir().expect("tempdir");
        let workspace = Workspace::init(directory.path(), "Demo").expect("workspace");
        let mut collection = workspace
            .create_collection(&Collection::new("Users"))
            .expect("collection");
        collection.collection.description = Some("User API".to_owned());
        collection
            .collection
            .variables
            .insert("baseUrl".to_owned(), "https://example.com".to_owned());
        collection.collection.test_script =
            Some("pm.test('collection is available', function () {});".to_owned());
        workspace
            .save_collection(&collection)
            .expect("collection metadata");
        let mut request = Request::new("Create user", "POST", "{{baseUrl}}/users");
        request.folder = Some("Users / Write".to_owned());
        request.query.push(KeyValue::enabled("verbose", "true"));
        request
            .headers
            .push(HeaderEntry::enabled("Content-Type", "application/json"));
        request.cookies.push(KeyValue::enabled("session", "abc"));
        request.body = RequestBody::Json {
            value: json!({ "name": "Ada" }),
        };
        request.auth = Auth::Bearer {
            token: "{{token}}".to_owned(),
        };
        request.pre_request_script = Some("pm.variables.set('ready', 'yes');".to_owned());
        request.test_script = Some("pm.test('created', function () {});".to_owned());
        request.examples.push(ResponseExample {
            name: "Created".to_owned(),
            status: Some(201),
            headers: vec![HeaderEntry::enabled("content-type", "application/json")],
            body: Some("{\"id\":1}".to_owned()),
        });
        workspace
            .save_request(&collection, &request)
            .expect("request");

        let export_path = directory.path().join("users.postman.json");
        let report =
            export_postman_collection(&workspace, &collection, &export_path).expect("export");
        assert_eq!(report.exported_requests, 1);
        let document: Value =
            serde_json::from_str(&fs::read_to_string(&export_path).expect("export document"))
                .expect("json");
        assert_eq!(document["info"]["name"], "Users");
        assert_eq!(document["info"]["description"], "User API");
        assert_eq!(document["variable"][0]["key"], "baseUrl");
        assert_eq!(document["event"][0]["listen"], "test");
        assert_eq!(document["item"][0]["name"], "Users");
        assert_eq!(document["item"][0]["item"][0]["name"], "Write");
        assert_eq!(
            document["item"][0]["item"][0]["item"][0]["request"]["auth"]["type"],
            "bearer"
        );
        assert_eq!(
            document["item"][0]["item"][0]["item"][0]["request"]["cookie"][0]["key"],
            "session"
        );

        let imported_directory = directory.path().join("round-trip");
        let import_report = import_postman_collection(&export_path, &imported_directory)
            .expect("round-trip import");
        assert_eq!(import_report.imported_requests, 1);
        let imported_workspace = Workspace::open(&imported_directory).expect("imported workspace");
        let imported_collection = imported_workspace.collections().expect("collections");
        let imported_requests = imported_workspace
            .requests(&imported_collection[0])
            .expect("imported requests");
        assert_eq!(imported_requests[0].1.name, "Create user");
        assert_eq!(
            imported_requests[0].1.folder.as_deref(),
            Some("Users/Write")
        );
        assert!(matches!(
            imported_requests[0].1.body,
            RequestBody::Json { .. }
        ));
        assert!(matches!(imported_requests[0].1.auth, Auth::Bearer { .. }));
        assert_eq!(imported_requests[0].1.cookies[0].key, "session");
    }

    #[test]
    fn exports_environment_values_and_secret_markers() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut environment = Environment::new("Local");
        environment.variables.insert(
            "token".to_owned(),
            EnvironmentVariable {
                value: "secret".to_owned(),
                enabled: true,
                secret: true,
            },
        );
        let path = directory.path().join("local.json");
        export_postman_environment(&environment, &path).expect("environment export");
        let document: Value =
            serde_json::from_str(&fs::read_to_string(path).expect("environment document"))
                .expect("json");
        assert_eq!(document["name"], "Local");
        assert_eq!(document["values"][0]["type"], "secret");
    }

    #[test]
    fn exports_and_reimports_a_structured_graphql_body() {
        let directory = tempfile::tempdir().expect("workspace directory");
        let workspace = Workspace::init(directory.path(), "Demo").expect("workspace");
        let collection = workspace
            .create_collection(&Collection::new("GraphQL"))
            .expect("collection");
        let mut request = Request::new("Get user", "POST", "https://api.example.test/graphql");
        request.body = RequestBody::Graphql {
            query: "query User { user { id } }".to_owned(),
            variables: json!({ "id": "42" }),
            operation_name: Some("User".to_owned()),
        };
        workspace
            .save_request(&collection, &request)
            .expect("request");

        let export_path = directory.path().join("graphql.postman.json");
        export_postman_collection(&workspace, &collection, &export_path).expect("export");
        let document: Value =
            serde_json::from_str(&fs::read_to_string(&export_path).expect("export document"))
                .expect("json");
        assert_eq!(document["item"][0]["request"]["body"]["mode"], "graphql");
        assert_eq!(
            document["item"][0]["request"]["body"]["graphql"]["operationName"],
            "User"
        );

        let imported_directory = directory.path().join("round-trip");
        import_postman_collection(&export_path, &imported_directory).expect("import");
        let imported_workspace = Workspace::open(&imported_directory).expect("imported workspace");
        let imported_collection = imported_workspace.collections().expect("collections");
        let imported_requests = imported_workspace
            .requests(&imported_collection[0])
            .expect("imported requests");
        match &imported_requests[0].1.body {
            RequestBody::Graphql {
                query,
                variables,
                operation_name,
            } => {
                assert_eq!(query, "query User { user { id } }");
                assert_eq!(variables["id"], "42");
                assert_eq!(operation_name.as_deref(), Some("User"));
            }
            other => panic!("expected GraphQL body, got {other:?}"),
        }
    }
}
