use std::{path::Path, time::Duration};

use reqwest::{
    header::{HeaderName, HeaderValue},
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
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            accept_invalid_certs: false,
            max_redirects: 10,
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
        let client = Client::builder()
            .timeout(options.timeout)
            .danger_accept_invalid_certs(options.accept_invalid_certs)
            .redirect(reqwest::redirect::Policy::limited(options.max_redirects))
            .build()
            .map_err(HttpError::Client)?;
        Ok(Self { client })
    }

    pub async fn execute(
        &self,
        request: &Request,
        context: &VariableContext,
    ) -> Result<HttpResponse, HttpError> {
        let resolved_url = context.resolve(&request.url);
        let mut diagnostics = resolved_url.diagnostics;
        let mut url = Url::parse(&resolved_url.value)?;
        resolve_pairs(&mut diagnostics, &request.query, context);
        if !diagnostics.is_empty() {
            return Err(HttpError::VariableResolution(diagnostics));
        }
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
        let body = response.bytes().await.map_err(HttpError::Request)?.to_vec();

        Ok(HttpResponse {
            status: status.as_u16(),
            status_text: status.canonical_reason().unwrap_or_default().to_owned(),
            headers,
            body,
            content_type,
            duration_ms: started.elapsed().as_millis(),
            protocol,
            url: url.to_string(),
        })
    }
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
            builder = builder.json(value);
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
            let bytes = tokio::fs::read(path)
                .await
                .map_err(|source| HttpError::BodyFile {
                    path: path.clone(),
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
}