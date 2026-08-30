use std::{path::Path, sync::Arc, time::Duration};

use reqwest::{
    cookie::{CookieStore, Jar},
    header::{HeaderName, HeaderValue, SET_COOKIE},
    Client, Method, Url,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    model::{ApiKeyLocation, Auth, HeaderEntry, KeyValue, Request, RequestBody},
    variables::{VariableContext, VariableDiagnostic},
};

#[derive(Debug, Clone)]
pub struct EngineOptions {
    pub timeout: Duration,
    pub accept_invalid_certs: bool,
    pub max_redirects: usize,
    pub proxy: Option<String>,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            accept_invalid_certs: false,
            max_redirects: 10,
            proxy: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum HttpError {
    #[error("invalid HTTP method {0}")]
    InvalidMethod(String),
    #[error("invalid URL after variable resolution: {0}")]
    InvalidUrl(#[from] url::ParseError),
    #[error("invalid request header {name}: {source}")]
    InvalidHeader {
        name: String,
        source: http::header::InvalidHeaderName,
    },
    #[error("invalid request header value for {name}: {source}")]
    InvalidHeaderValue {
        name: String,
        source: http::header::InvalidHeaderValue,
    },
    #[error("invalid multipart content type: {0}")]
    InvalidMime(String),
    #[error("request body file {path} could not be read: {source}")]
    BodyFile {
        path: String,
        source: std::io::Error,
    },
    #[error("could not build HTTP client: {0}")]
    Client(#[source] reqwest::Error),
    #[error("invalid HTTP proxy configuration: {0}")]
    Proxy(#[source] reqwest::Error),
    #[error("HTTP request failed: {0}")]
    Request(#[source] reqwest::Error),
    #[error("variable resolution failed")]
    VariableResolution(Vec<VariableDiagnostic>),
    #[error("invalid JSON body: {0}")]
    JsonBody(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct HttpEngine {
    client: Client,
    cookie_jar: Arc<Jar>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<HeaderEntry>,
    pub body: Vec<u8>,
    pub content_type: Option<String>,
    pub duration_ms: u128,
    pub protocol: String,
    pub url: String,
    #[serde(default)]
    pub cookies: Vec<ResponseCookie>,
}

/// Response metadata plus a live body for protocols that deliver events over
/// an HTTP connection, such as Server-Sent Events.
pub struct HttpStreamResponse {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<HeaderEntry>,
    pub content_type: Option<String>,
    pub protocol: String,
    pub url: String,
    pub response: reqwest::Response,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseCookie {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub secure: bool,
    #[serde(default)]
    pub http_only: bool,
    #[serde(default)]
    pub same_site: Option<String>,
    #[serde(default)]
    pub expires: Option<String>,
    #[serde(default)]
    pub max_age_seconds: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseView {
    Pretty,
    Raw,
}

impl HttpResponse {
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn formatted_body(&self, view: ResponseView) -> String {
        let text = self.body_text();
        if view == ResponseView::Raw {
            return text;
        }
        let looks_like_json = self
            .content_type
            .as_deref()
            .is_some_and(|value| value.contains("json"))
            || text.trim_start().starts_with('{')
            || text.trim_start().starts_with('[');
        if looks_like_json {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Ok(formatted) = serde_json::to_string_pretty(&value) {
                    return formatted;
                }
            }
        }
        text
    }
}

impl HttpEngine {
    pub fn new(options: &EngineOptions) -> Result<Self, HttpError> {
        let cookie_jar = Arc::new(Jar::default());
        let mut builder = Client::builder()
            .timeout(options.timeout)
            .danger_accept_invalid_certs(options.accept_invalid_certs)
            .redirect(reqwest::redirect::Policy::limited(options.max_redirects))
            .cookie_provider(Arc::clone(&cookie_jar));
        if let Some(proxy) = options.proxy.as_deref() {
            builder = builder.proxy(reqwest::Proxy::all(proxy).map_err(HttpError::Proxy)?);
        }
        let client = builder.build().map_err(HttpError::Client)?;
        Ok(Self { client, cookie_jar })
    }

    /// Add a manually-authored cookie to the in-memory jar for a URL.
    ///
    /// The cookie is scoped by the URL and is discarded when this engine is
    /// dropped. This deliberately does not persist cookie values to disk.
    pub fn add_cookie(&self, url: &str, cookie: &str) -> Result<(), HttpError> {
        let url = Url::parse(url)?;
        self.cookie_jar.add_cookie_str(cookie, &url);
        Ok(())
    }

    /// Return the cookie header currently applicable to a URL, if any.
    pub fn cookie_header(&self, url: &str) -> Result<Option<String>, HttpError> {
        let url = Url::parse(url)?;
        Ok(self
            .cookie_jar
            .cookies(&url)
            .and_then(|value| value.to_str().ok().map(ToOwned::to_owned)))
    }

    pub async fn execute(
        &self,
        request: &Request,
        context: &VariableContext,
    ) -> Result<HttpResponse, HttpError> {
        let builder = self.prepare_builder(request, context).await?;
        let started = std::time::Instant::now();
        let response = builder.send().await.map_err(HttpError::Request)?;
        let status = response.status();
        let protocol = format!("{:?}", response.version());
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                HeaderEntry::enabled(name.as_str(), value.to_str().unwrap_or("<binary>"))
            })
            .collect();
        let cookies = response
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .filter_map(parse_set_cookie)
            .collect();
        let final_url = response.url().to_string();
        let body = response.bytes().await.map_err(HttpError::Request)?.to_vec();

        Ok(HttpResponse {
            status: status.as_u16(),
            status_text: status.canonical_reason().unwrap_or_default().to_owned(),
            headers,
            body,
            content_type,
            duration_ms: started.elapsed().as_millis(),
            protocol,
            url: final_url,
            cookies,
        })
    }

    pub async fn execute_stream(
        &self,
        request: &Request,
        context: &VariableContext,
    ) -> Result<HttpStreamResponse, HttpError> {
        let builder = self.prepare_builder(request, context).await?;
        let response = builder.send().await.map_err(HttpError::Request)?;
        let status = response.status();
        let protocol = format!("{:?}", response.version());
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                HeaderEntry::enabled(name.as_str(), value.to_str().unwrap_or("<binary>"))
            })
            .collect();
        let final_url = response.url().to_string();

        Ok(HttpStreamResponse {
            status: status.as_u16(),
            status_text: status.canonical_reason().unwrap_or_default().to_owned(),
            headers,
            content_type,
            protocol,
            url: final_url,
            response,
        })
    }

    async fn prepare_builder(
        &self,
        request: &Request,
        context: &VariableContext,
    ) -> Result<reqwest::RequestBuilder, HttpError> {
        let resolved_url = context.resolve(&request.url);
        let mut diagnostics = resolved_url.diagnostics;
        resolve_pairs(&mut diagnostics, &request.query, context);
        for header in request.headers.iter().filter(|header| header.enabled) {
            diagnostics.extend(context.resolve(&header.key).diagnostics);
            diagnostics.extend(context.resolve(&header.value).diagnostics);
        }
        for cookie in request.cookies.iter().filter(|cookie| cookie.enabled) {
            diagnostics.extend(context.resolve(&cookie.key).diagnostics);
            diagnostics.extend(context.resolve(&cookie.value).diagnostics);
        }
        resolve_auth(&mut diagnostics, &request.auth, context);
        resolve_body(&mut diagnostics, &request.body, context);
        if !diagnostics.is_empty() {
            return Err(HttpError::VariableResolution(diagnostics));
        }
        let mut url = Url::parse(&resolved_url.value)?;
        for pair in &request.query {
            if pair.enabled {
                url.query_pairs_mut().append_pair(
                    &context.resolve(&pair.key).value,
                    &context.resolve(&pair.value).value,
                );
            }
        }

        let method = Method::from_bytes(request.method.as_bytes())
            .map_err(|_| HttpError::InvalidMethod(request.method.clone()))?;
        let mut builder = self.client.request(method, url.clone());
        for header in request.headers.iter().filter(|header| header.enabled) {
            let name = context.resolve(&header.key).value;
            let value = context.resolve(&header.value).value;
            let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|source| {
                HttpError::InvalidHeader {
                    name: name.clone(),
                    source,
                }
            })?;
            let header_value = value
                .parse::<HeaderValue>()
                .map_err(|source| HttpError::InvalidHeaderValue { name, source })?;
            builder = builder.header(header_name, header_value);
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
                    format!(
                        "{}={}",
                        context.resolve(&pair.key).value,
                        context.resolve(&pair.value).value
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            if !cookie.is_empty() {
                builder = builder.header("cookie", cookie);
            }
        }

        builder = apply_auth(builder, &request.auth, context)?;
        builder = apply_body(builder, &request.body, context).await?;

        Ok(builder)
    }
}

fn parse_set_cookie(value: &HeaderValue) -> Option<ResponseCookie> {
    let text = value.to_str().ok()?;
    let mut segments = text.split(';');
    let pair = segments.next()?.trim();
    let (name, value) = pair.split_once('=')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }

    let mut cookie = ResponseCookie {
        name: name.to_owned(),
        value: value.trim().to_owned(),
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
        let Some((key, attribute_value)) = segment.split_once('=') else {
            if segment.eq_ignore_ascii_case("secure") {
                cookie.secure = true;
            } else if segment.eq_ignore_ascii_case("httponly") {
                cookie.http_only = true;
            }
            continue;
        };
        let key = key.trim();
        let attribute_value = attribute_value.trim();
        if key.eq_ignore_ascii_case("domain") {
            cookie.domain = Some(attribute_value.to_owned());
        } else if key.eq_ignore_ascii_case("path") {
            cookie.path = Some(attribute_value.to_owned());
        } else if key.eq_ignore_ascii_case("samesite") {
            cookie.same_site = Some(attribute_value.to_owned());
        } else if key.eq_ignore_ascii_case("expires") {
            cookie.expires = Some(attribute_value.to_owned());
        } else if key.eq_ignore_ascii_case("max-age") {
            cookie.max_age_seconds = attribute_value.parse().ok();
        }
    }
    Some(cookie)
}

fn resolve_pairs(
    diagnostics: &mut Vec<VariableDiagnostic>,
    pairs: &[KeyValue],
    context: &VariableContext,
) {
    for pair in pairs.iter().filter(|pair| pair.enabled) {
        diagnostics.extend(context.resolve(&pair.key).diagnostics);
        diagnostics.extend(context.resolve(&pair.value).diagnostics);
    }
}

fn apply_auth(
    mut builder: reqwest::RequestBuilder,
    auth: &Auth,
    context: &VariableContext,
) -> Result<reqwest::RequestBuilder, HttpError> {
    match auth {
        Auth::None => {}
        Auth::Basic { username, password } => {
            builder = builder.basic_auth(
                context.resolve(username).value,
                Some(context.resolve(password).value),
            );
        }
        Auth::Bearer { token } => {
            builder = builder.bearer_auth(context.resolve(token).value);
        }
        Auth::ApiKey {
            key,
            value,
            location,
        } => {
            let key = context.resolve(key).value;
            let value = context.resolve(value).value;
            match location {
                ApiKeyLocation::Header => builder = builder.header(key, value),
                ApiKeyLocation::Query => {
                    builder = builder.query(&[(key, value)]);
                }
            }
        }
    }
    Ok(builder)
}

async fn apply_body(
    mut builder: reqwest::RequestBuilder,
    body: &RequestBody,
    context: &VariableContext,
) -> Result<reqwest::RequestBuilder, HttpError> {
    match body {
        RequestBody::None => {}
        RequestBody::Raw { text, content_type } => {
            builder = builder.body(context.resolve(text).value);
            if let Some(content_type) = content_type {
                builder = builder.header("content-type", context.resolve(content_type).value);
            }
        }
        RequestBody::Json { value } => {
            builder = builder.json(&resolve_json(value, context));
        }
        RequestBody::Graphql {
            query,
            variables,
            operation_name,
        } => {
            let mut payload = serde_json::Map::new();
            payload.insert(
                "query".to_owned(),
                serde_json::Value::String(context.resolve(query).value),
            );
            payload.insert("variables".to_owned(), resolve_json(variables, context));
            if let Some(operation_name) = operation_name {
                payload.insert(
                    "operationName".to_owned(),
                    serde_json::Value::String(context.resolve(operation_name).value),
                );
            }
            builder = builder.json(&serde_json::Value::Object(payload));
        }
        RequestBody::FormUrlEncoded { fields } => {
            let values = fields
                .iter()
                .filter(|field| field.enabled)
                .map(|field| {
                    (
                        context.resolve(&field.key).value,
                        context.resolve(&field.value).value,
                    )
                })
                .collect::<Vec<_>>();
            builder = builder.form(&values);
        }
        RequestBody::Multipart { parts } => {
            let mut form = reqwest::multipart::Form::new();
            for part in parts.iter().filter(|part| part.enabled) {
                let name = context.resolve(&part.name).value;
                if let Some(file_path) = &part.file_path {
                    let path = Path::new(file_path);
                    let bytes =
                        tokio::fs::read(path)
                            .await
                            .map_err(|source| HttpError::BodyFile {
                                path: file_path.clone(),
                                source,
                            })?;
                    let mut file = reqwest::multipart::Part::bytes(bytes).file_name(
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("upload.bin")
                            .to_owned(),
                    );
                    if let Some(content_type) = &part.content_type {
                        file = file
                            .mime_str(&context.resolve(content_type).value)
                            .map_err(|source| HttpError::InvalidMime(source.to_string()))?;
                    }
                    form = form.part(name, file);
                } else {
                    form = form.text(name, context.resolve(&part.value).value);
                }
            }
            builder = builder.multipart(form);
        }
        RequestBody::BinaryFile { path, content_type } => {
            let resolved_path = context.resolve(path).value;
            let bytes =
                tokio::fs::read(&resolved_path)
                    .await
                    .map_err(|source| HttpError::BodyFile {
                        path: resolved_path,
                        source,
                    })?;
            builder = builder.body(bytes);
            if let Some(content_type) = content_type {
                builder = builder.header("content-type", context.resolve(content_type).value);
            }
        }
    }
    Ok(builder)
}

fn resolve_auth(diagnostics: &mut Vec<VariableDiagnostic>, auth: &Auth, context: &VariableContext) {
    match auth {
        Auth::None => {}
        Auth::Basic { username, password } => {
            diagnostics.extend(context.resolve(username).diagnostics);
            diagnostics.extend(context.resolve(password).diagnostics);
        }
        Auth::Bearer { token } => diagnostics.extend(context.resolve(token).diagnostics),
        Auth::ApiKey { key, value, .. } => {
            diagnostics.extend(context.resolve(key).diagnostics);
            diagnostics.extend(context.resolve(value).diagnostics);
        }
    }
}

fn resolve_body(
    diagnostics: &mut Vec<VariableDiagnostic>,
    body: &RequestBody,
    context: &VariableContext,
) {
    match body {
        RequestBody::None => {}
        RequestBody::Raw { text, content_type } => {
            diagnostics.extend(context.resolve(text).diagnostics);
            if let Some(content_type) = content_type {
                diagnostics.extend(context.resolve(content_type).diagnostics);
            }
        }
        RequestBody::Json { value } => resolve_json_diagnostics(value, diagnostics, context),
        RequestBody::Graphql {
            query,
            variables,
            operation_name,
        } => {
            diagnostics.extend(context.resolve(query).diagnostics);
            resolve_json_diagnostics(variables, diagnostics, context);
            if let Some(operation_name) = operation_name {
                diagnostics.extend(context.resolve(operation_name).diagnostics);
            }
        }
        RequestBody::FormUrlEncoded { fields } => resolve_pairs(diagnostics, fields, context),
        RequestBody::Multipart { parts } => {
            for part in parts.iter().filter(|part| part.enabled) {
                diagnostics.extend(context.resolve(&part.name).diagnostics);
                diagnostics.extend(context.resolve(&part.value).diagnostics);
                if let Some(path) = &part.file_path {
                    diagnostics.extend(context.resolve(path).diagnostics);
                }
                if let Some(content_type) = &part.content_type {
                    diagnostics.extend(context.resolve(content_type).diagnostics);
                }
            }
        }
        RequestBody::BinaryFile { path, content_type } => {
            diagnostics.extend(context.resolve(path).diagnostics);
            if let Some(content_type) = content_type {
                diagnostics.extend(context.resolve(content_type).diagnostics);
            }
        }
    }
}

fn resolve_json(value: &serde_json::Value, context: &VariableContext) -> serde_json::Value {
    match value {
        serde_json::Value::String(value) => serde_json::Value::String(context.resolve(value).value),
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .iter()
                .map(|value| resolve_json(value, context))
                .collect(),
        ),
        serde_json::Value::Object(values) => serde_json::Value::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), resolve_json(value, context)))
                .collect(),
        ),
        value => value.clone(),
    }
}

fn resolve_json_diagnostics(
    value: &serde_json::Value,
    diagnostics: &mut Vec<VariableDiagnostic>,
    context: &VariableContext,
) {
    match value {
        serde_json::Value::String(value) => diagnostics.extend(context.resolve(value).diagnostics),
        serde_json::Value::Array(values) => {
            for value in values {
                resolve_json_diagnostics(value, diagnostics, context);
            }
        }
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                diagnostics.extend(context.resolve(key).diagnostics);
                resolve_json_diagnostics(value, diagnostics, context);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::KeyValue;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[tokio::test]
    async fn executes_a_real_local_request_and_formats_json() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("connection");
            let mut request = [0_u8; 4096];
            let length = socket.read(&mut request).await.expect("read");
            let request = String::from_utf8_lossy(&request[..length]);
            assert!(request.contains("GET /health?probe=1 HTTP/1.1"));
            assert!(request.contains("x-test: local"));
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 28\r\n\r\n{\"ok\":true,\"source\":\"local\"}",
                )
                .await
                .expect("write");
        });

        let mut request = Request::new("Health", "GET", format!("http://{address}/health"));
        request.query.push(KeyValue::enabled("probe", "1"));
        request
            .headers
            .push(HeaderEntry::enabled("x-test", "local"));
        let engine = HttpEngine::new(&EngineOptions::default()).expect("engine");
        let response = engine
            .execute(&request, &VariableContext::default())
            .await
            .expect("response");

        server.await.expect("server");
        assert_eq!(response.status, 200);
        assert_eq!(
            response.formatted_body(ResponseView::Pretty),
            "{\n  \"ok\": true,\n  \"source\": \"local\"\n}"
        );
        assert!(response.duration_ms < 5_000);
    }

    #[tokio::test]
    async fn routes_http_requests_through_an_explicit_proxy() {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("proxy listener");
        let proxy_address = listener.local_addr().expect("proxy address");
        let proxy = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("proxy connection");
            let request = read_request_headers(&mut socket).await;
            assert!(request.starts_with("GET http://example.test/through-proxy HTTP/1.1"));
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 10\r\n\r\nproxied-ok",
                )
                .await
                .expect("proxy response");
        });

        let engine = HttpEngine::new(&EngineOptions {
            proxy: Some(format!("http://{proxy_address}")),
            ..EngineOptions::default()
        })
        .expect("engine");
        let request = Request::new("Proxied", "GET", "http://example.test/through-proxy");
        let response = engine
            .execute(&request, &VariableContext::default())
            .await
            .expect("response through proxy");

        proxy.await.expect("proxy");
        assert_eq!(response.status, 200);
        assert_eq!(response.body_text(), "proxied-ok");
    }

    #[test]
    fn rejects_an_invalid_proxy_before_building_the_client() {
        let error = HttpEngine::new(&EngineOptions {
            proxy: Some("not a proxy URL".to_owned()),
            ..EngineOptions::default()
        })
        .expect_err("invalid proxy must fail");
        assert!(matches!(error, HttpError::Proxy(_)));
    }

    #[tokio::test]
    async fn executes_a_graphql_request_as_a_structured_json_envelope() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("connection");
            let request = read_request_headers(&mut socket).await;
            assert!(request.contains("POST /graphql HTTP/1.1"));
            assert!(request.contains("\"query\":\"query User"));
            assert!(request.contains("\"variables\":{\"id\":\"42\"}"));
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 29\r\n\r\n{\"data\":{\"user\":{\"id\":\"42\"}}}",
                )
                .await
                .expect("response");
        });

        let mut graphql = crate::graphql::GraphqlRequest::new(
            format!("http://{address}/graphql"),
            "query User($id: ID!) { user(id: $id) { id } }",
        );
        graphql.variables =
            crate::graphql::parse_variables_json(r#"{"id":"42"}"#).expect("variables");
        let request = graphql.into_http_request("Get user").expect("request");
        let engine = HttpEngine::new(&EngineOptions::default()).expect("engine");
        let response = engine
            .execute(&request, &VariableContext::default())
            .await
            .expect("response");

        server.await.expect("server");
        assert_eq!(response.status, 200);
        let graphql_response =
            crate::graphql::parse_response(&response.body_text()).expect("GraphQL response");
        assert!(!graphql_response.has_errors());
        assert_eq!(graphql_response.data.expect("data")["user"]["id"], "42");
    }

    #[tokio::test]
    async fn reuses_session_cookies_and_exposes_response_attributes() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.expect("first connection");
            let first_request = read_request_headers(&mut first).await;
            assert!(first_request.contains("GET /login HTTP/1.1"));
            first
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nset-cookie: session=abc; Path=/; HttpOnly; SameSite=Lax\r\nconnection: close\r\n\r\nok",
                )
                .await
                .expect("first response");
            drop(first);

            let (mut second, _) = listener.accept().await.expect("second connection");
            let second_request = read_request_headers(&mut second).await;
            assert!(second_request
                .to_ascii_lowercase()
                .contains("cookie: session=abc"));
            second
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")
                .await
                .expect("second response");
        });

        let engine = HttpEngine::new(&EngineOptions::default()).expect("engine");
        let first_request = Request::new("Login", "GET", format!("http://{address}/login"));
        let first_response = engine
            .execute(&first_request, &VariableContext::default())
            .await
            .expect("first response");
        assert_eq!(first_response.status, 200);
        assert_eq!(first_response.cookies.len(), 1);
        let cookie = &first_response.cookies[0];
        assert_eq!(cookie.name, "session");
        assert_eq!(cookie.value, "abc");
        assert_eq!(cookie.path.as_deref(), Some("/"));
        assert!(cookie.http_only);
        assert_eq!(cookie.same_site.as_deref(), Some("Lax"));
        assert_eq!(
            engine
                .cookie_header(&format!("http://{address}/next"))
                .expect("cookie header"),
            Some("session=abc".to_owned())
        );

        let second_request = Request::new("Next", "GET", format!("http://{address}/next"));
        let second_response = engine
            .execute(&second_request, &VariableContext::default())
            .await
            .expect("second response");
        assert_eq!(second_response.status, 200);
        assert!(second_response.cookies.is_empty());
        server.await.expect("server");
    }

    #[tokio::test]
    async fn rejects_undefined_values_before_network_io() {
        let mut request = Request::new("Invalid", "GET", "http://127.0.0.1:1/{{missing}}");
        request
            .headers
            .push(HeaderEntry::enabled("x-token", "{{secret}}"));
        request.auth = Auth::Bearer {
            token: "{{token}}".to_owned(),
        };
        let engine = HttpEngine::new(&EngineOptions::default()).expect("engine");

        let error = engine
            .execute(&request, &VariableContext::default())
            .await
            .expect_err("undefined values must fail");

        match error {
            HttpError::VariableResolution(diagnostics) => {
                assert!(diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.name == "missing"));
                assert!(diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.name == "secret"));
                assert!(diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.name == "token"));
            }
            other => panic!("expected variable diagnostics, got {other}"),
        }
    }

    async fn read_request_headers(socket: &mut tokio::net::TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = socket.read(&mut buffer).await.expect("request read");
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        let header_end = bytes
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|position| position + 4)
            .unwrap_or(bytes.len());
        let headers = String::from_utf8_lossy(&bytes[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.strip_prefix("content-length:")
                    .or_else(|| line.strip_prefix("Content-Length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        while bytes.len() < header_end + content_length {
            let count = socket.read(&mut buffer).await.expect("request body read");
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }
}
