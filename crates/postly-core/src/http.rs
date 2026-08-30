use std::{
    collections::HashMap,
    fs,
    hash::{Hash, Hasher},
    io::BufReader,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};

use cookie_store::{CookieStore as StoredCookieStore, RawCookie};
use quick_xml::{events::Event, Reader, Writer};
use reqwest::{
    cookie::CookieStore,
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
    /// Optional comma-separated host/IP bypass list for an explicit proxy.
    pub no_proxy: Option<String>,
    /// An additional PEM-encoded trust anchor for HTTPS connections.
    pub ca_cert: Option<PathBuf>,
    /// A PEM bundle containing the client certificate chain and private key.
    pub client_identity: Option<PathBuf>,
    /// Optional ignored local cookie-jar file for saved-request sessions.
    pub cookie_jar: Option<PathBuf>,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            accept_invalid_certs: false,
            max_redirects: 10,
            proxy: None,
            no_proxy: None,
            ca_cert: None,
            client_identity: None,
            cookie_jar: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum HttpError {
    #[error("request uses gRPC; use the gRPC transport instead of HTTP")]
    UnsupportedGrpcRequest,
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
    #[error("could not read CA certificate {path}: {source}")]
    CaCertificateFile {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid CA certificate {path}: {source}")]
    CaCertificate {
        path: String,
        source: reqwest::Error,
    },
    #[error("CA certificate bundle {path} contains no certificates")]
    CaCertificateEmpty { path: String },
    #[error("could not read client identity {path}: {source}")]
    ClientIdentityFile {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid client identity {path}: {source}")]
    ClientIdentity {
        path: String,
        source: reqwest::Error,
    },
    #[error("HTTP request failed: {0}")]
    Request(#[source] reqwest::Error),
    #[error("variable resolution failed")]
    VariableResolution(Vec<VariableDiagnostic>),
    #[error("invalid JSON body: {0}")]
    JsonBody(#[from] serde_json::Error),
    #[error("could not access cookie jar {path}: {message}")]
    CookieJar { path: String, message: String },
    #[error("OAuth 2.0 token request failed: {0}")]
    OAuthToken(String),
}

#[derive(Debug, Clone)]
pub struct HttpEngine {
    client: Client,
    cookie_jar: Arc<PersistentCookieJar>,
    oauth_tokens: Arc<Mutex<HashMap<OAuthTokenKey, CachedOAuthToken>>>,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct OAuthTokenKey {
    grant_type: String,
    token_url: String,
    client_id: String,
    scope: Option<String>,
    client_secret_fingerprint: u64,
    code_fingerprint: u64,
    code_verifier_fingerprint: u64,
    redirect_uri: Option<String>,
}

#[derive(Debug, Clone)]
struct CachedOAuthToken {
    access_token: String,
    token_type: String,
    expires_at: Instant,
}

const OAUTH_CACHE_SKEW: Duration = Duration::from_secs(30);
const MAX_OAUTH_RESPONSE_BYTES: usize = 1_048_576;
const MAX_OAUTH_CACHED_TOKENS: usize = 128;

const MAX_COOKIE_JAR_BYTES: usize = 1_048_576;

#[derive(Debug)]
struct PersistentCookieJar {
    store: RwLock<StoredCookieStore>,
    path: Option<PathBuf>,
}

impl PersistentCookieJar {
    fn load(path: Option<&Path>) -> Result<Self, HttpError> {
        let path = path.map(Path::to_path_buf);
        let store = if let Some(path) = path.as_deref().filter(|path| path.is_file()) {
            let metadata = fs::metadata(path).map_err(|source| HttpError::CookieJar {
                path: path.display().to_string(),
                message: source.to_string(),
            })?;
            if metadata.len() > MAX_COOKIE_JAR_BYTES as u64 {
                return Err(HttpError::CookieJar {
                    path: path.display().to_string(),
                    message: format!("file exceeds {MAX_COOKIE_JAR_BYTES} bytes"),
                });
            }
            let file = fs::File::open(path).map_err(|source| HttpError::CookieJar {
                path: path.display().to_string(),
                message: source.to_string(),
            })?;
            cookie_store::serde::json::load_all(BufReader::new(file)).map_err(|source| {
                HttpError::CookieJar {
                    path: path.display().to_string(),
                    message: source.to_string(),
                }
            })?
        } else {
            StoredCookieStore::default()
        };
        Ok(Self {
            store: RwLock::new(store),
            path,
        })
    }

    fn read_store(&self) -> std::sync::RwLockReadGuard<'_, StoredCookieStore> {
        self.store
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write_store(&self) -> std::sync::RwLockWriteGuard<'_, StoredCookieStore> {
        self.store
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn persist(&self, store: &StoredCookieStore) -> Result<(), HttpError> {
        let Some(path) = self.path.as_deref() else {
            return Ok(());
        };
        let mut bytes = Vec::new();
        cookie_store::serde::json::save_incl_expired_and_nonpersistent(store, &mut bytes).map_err(
            |source| HttpError::CookieJar {
                path: path.display().to_string(),
                message: source.to_string(),
            },
        )?;
        if bytes.len() > MAX_COOKIE_JAR_BYTES {
            return Err(HttpError::CookieJar {
                path: path.display().to_string(),
                message: format!("file would exceed {MAX_COOKIE_JAR_BYTES} bytes"),
            });
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| HttpError::CookieJar {
                path: path.display().to_string(),
                message: source.to_string(),
            })?;
        }
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, bytes).map_err(|source| HttpError::CookieJar {
            path: temporary.display().to_string(),
            message: source.to_string(),
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&temporary)
                .map_err(|source| HttpError::CookieJar {
                    path: temporary.display().to_string(),
                    message: source.to_string(),
                })?
                .permissions();
            permissions.set_mode(0o600);
            fs::set_permissions(&temporary, permissions).map_err(|source| {
                HttpError::CookieJar {
                    path: temporary.display().to_string(),
                    message: source.to_string(),
                }
            })?;
        }
        fs::rename(&temporary, path).map_err(|source| HttpError::CookieJar {
            path: path.display().to_string(),
            message: source.to_string(),
        })
    }
}

impl CookieStore for PersistentCookieJar {
    fn set_cookies(&self, cookie_headers: &mut dyn Iterator<Item = &HeaderValue>, url: &Url) {
        let cookies = cookie_headers.filter_map(|value| {
            let value = value.to_str().ok()?;
            RawCookie::parse(value)
                .ok()
                .map(|cookie| cookie.into_owned())
        });
        let mut store = self.write_store();
        store.store_response_cookies(cookies, url);
        if let Err(error) = self.persist(&store) {
            tracing::warn!(error = %error, "could not persist HTTP cookie jar");
        }
    }

    fn cookies(&self, url: &Url) -> Option<HeaderValue> {
        let values = self
            .read_store()
            .get_request_values(url)
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ");
        (!values.is_empty())
            .then(|| HeaderValue::from_str(&values).ok())
            .flatten()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<HeaderEntry>,
    pub body: Vec<u8>,
    /// Number of bytes received in the response body.
    #[serde(default)]
    pub response_size: usize,
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
        let looks_like_xml = self
            .content_type
            .as_deref()
            .is_some_and(|value| value.contains("xml"))
            || text.trim_start().starts_with("<?xml");
        if looks_like_xml {
            if let Some(formatted) = format_xml(&text) {
                return formatted;
            }
        }
        text
    }
}

fn format_xml(text: &str) -> Option<String> {
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(true);
    let mut writer = Writer::new_with_indent(Vec::new(), b' ', 2);
    loop {
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(event) => writer.write_event(event.into_owned()).ok()?,
            Err(_) => return None,
        }
    }
    String::from_utf8(writer.into_inner()).ok()
}

impl HttpEngine {
    pub fn new(options: &EngineOptions) -> Result<Self, HttpError> {
        let cookie_jar = Arc::new(PersistentCookieJar::load(options.cookie_jar.as_deref())?);
        let ca_cert_path = options
            .ca_cert
            .as_deref()
            .map(|path| path.display().to_string());
        let mut builder = Client::builder()
            .timeout(options.timeout)
            .danger_accept_invalid_certs(options.accept_invalid_certs)
            .redirect(reqwest::redirect::Policy::limited(options.max_redirects))
            .cookie_provider(Arc::clone(&cookie_jar));
        if let Some(proxy) = options.proxy.as_deref() {
            let no_proxy = options
                .no_proxy
                .as_deref()
                .and_then(reqwest::NoProxy::from_string);
            builder = builder.proxy(
                reqwest::Proxy::all(proxy)
                    .map_err(HttpError::Proxy)?
                    .no_proxy(no_proxy),
            );
        }
        if let Some(path) = options.ca_cert.as_deref() {
            let path_display = path.display().to_string();
            let pem = fs::read(path).map_err(|source| HttpError::CaCertificateFile {
                path: path_display.clone(),
                source,
            })?;
            let certificates = reqwest::Certificate::from_pem_bundle(&pem).map_err(|source| {
                HttpError::CaCertificate {
                    path: path_display.clone(),
                    source,
                }
            })?;
            if certificates.is_empty() {
                return Err(HttpError::CaCertificateEmpty { path: path_display });
            }
            for certificate in certificates {
                builder = builder.add_root_certificate(certificate);
            }
        }
        if let Some(path) = options.client_identity.as_deref() {
            let path_display = path.display().to_string();
            let pem = fs::read(path).map_err(|source| HttpError::ClientIdentityFile {
                path: path_display.clone(),
                source,
            })?;
            let identity =
                reqwest::Identity::from_pem(&pem).map_err(|source| HttpError::ClientIdentity {
                    path: path_display,
                    source,
                })?;
            builder = builder.identity(identity);
        }
        let client = builder.build().map_err(|source| match ca_cert_path {
            Some(path) => HttpError::CaCertificate { path, source },
            None => HttpError::Client(source),
        })?;
        Ok(Self {
            client,
            cookie_jar,
            oauth_tokens: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Add a manually-authored cookie to the jar for a URL.
    pub fn add_cookie(&self, url: &str, cookie: &str) -> Result<(), HttpError> {
        let url = Url::parse(url)?;
        let cookie = RawCookie::parse(cookie)
            .map_err(|error| HttpError::CookieJar {
                path: self
                    .cookie_jar
                    .path
                    .as_deref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "<memory>".to_owned()),
                message: format!("invalid cookie: {error}"),
            })?
            .into_owned();
        let mut store = self.cookie_jar.write_store();
        store.store_response_cookies(std::iter::once(cookie), &url);
        self.cookie_jar.persist(&store)
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
        let response_size = body.len();

        Ok(HttpResponse {
            status: status.as_u16(),
            status_text: status.canonical_reason().unwrap_or_default().to_owned(),
            headers,
            body,
            response_size,
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

    async fn oauth_access_token(
        &self,
        auth: &Auth,
        context: &VariableContext,
    ) -> Result<Option<CachedOAuthToken>, HttpError> {
        let (
            grant_type,
            token_url,
            client_id,
            client_secret,
            scope,
            code,
            code_verifier,
            redirect_uri,
        ) = match auth {
            Auth::OAuth2ClientCredentials {
                token_url,
                client_id,
                client_secret,
                scope,
            } => (
                "client_credentials",
                token_url,
                client_id,
                Some(client_secret),
                scope.as_ref(),
                None,
                None,
                None,
            ),
            Auth::OAuth2AuthorizationCodePkce {
                token_url,
                client_id,
                redirect_uri,
                code,
                code_verifier,
                client_secret,
                scope,
                ..
            } => (
                "authorization_code",
                token_url,
                client_id,
                client_secret.as_ref(),
                scope.as_ref(),
                Some(code),
                Some(code_verifier),
                Some(redirect_uri),
            ),
            _ => return Ok(None),
        };

        let token_url = context.resolve(token_url).value;
        let client_id = context.resolve(client_id).value;
        let client_secret = client_secret.map(|value| context.resolve(value).value);
        let scope = scope
            .map(|value| context.resolve(value).value)
            .filter(|value| !value.trim().is_empty());
        let code = code.map(|value| context.resolve(value).value);
        let code_verifier = code_verifier.map(|value| context.resolve(value).value);
        let redirect_uri = redirect_uri.map(|value| context.resolve(value).value);

        if token_url.trim().is_empty() || client_id.trim().is_empty() {
            return Err(HttpError::OAuthToken(
                "OAuth token URL and client ID cannot be empty".to_owned(),
            ));
        }
        if grant_type == "client_credentials"
            && client_secret.as_deref().unwrap_or_default().is_empty()
        {
            return Err(HttpError::OAuthToken(
                "OAuth client secret cannot be empty".to_owned(),
            ));
        }
        if grant_type == "authorization_code" {
            if code.as_deref().unwrap_or_default().is_empty()
                || code_verifier.as_deref().unwrap_or_default().is_empty()
                || redirect_uri.as_deref().unwrap_or_default().is_empty()
            {
                return Err(HttpError::OAuthToken(
                    "OAuth authorization code, code verifier and redirect URI are required"
                        .to_owned(),
                ));
            }
            let verifier_len = code_verifier.as_deref().unwrap_or_default().len();
            if !(43..=128).contains(&verifier_len)
                || !code_verifier
                    .as_deref()
                    .unwrap_or_default()
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-._~".contains(&byte))
            {
                return Err(HttpError::OAuthToken(
                    "OAuth PKCE code verifier must be 43-128 unreserved characters".to_owned(),
                ));
            }
        }
        let key = OAuthTokenKey {
            grant_type: grant_type.to_owned(),
            token_url: token_url.clone(),
            client_id: client_id.clone(),
            scope: scope.clone(),
            client_secret_fingerprint: secret_fingerprint(
                client_secret.as_deref().unwrap_or_default(),
            ),
            code_fingerprint: secret_fingerprint(code.as_deref().unwrap_or_default()),
            code_verifier_fingerprint: secret_fingerprint(
                code_verifier.as_deref().unwrap_or_default(),
            ),
            redirect_uri: redirect_uri.clone(),
        };
        let now = Instant::now();
        {
            let mut token_cache = self
                .oauth_tokens
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            token_cache.retain(|_, cached| cached.expires_at > now);
            if let Some(cached) = token_cache
                .get(&key)
                .filter(|cached| cached.expires_at > now + OAUTH_CACHE_SKEW)
                .cloned()
            {
                return Ok(Some(cached));
            }
        }

        let token_url = Url::parse(&token_url).map_err(|error| {
            HttpError::OAuthToken(format!("invalid token endpoint URL: {error}"))
        })?;
        let mut form = vec![
            ("grant_type", grant_type.to_owned()),
            ("client_id", client_id),
        ];
        if let Some(client_secret) = client_secret {
            form.push(("client_secret", client_secret));
        }
        if let Some(scope) = scope {
            form.push(("scope", scope));
        }
        if grant_type == "authorization_code" {
            form.push(("code", code.expect("validated authorization code")));
            form.push((
                "redirect_uri",
                redirect_uri.expect("validated redirect URI"),
            ));
            form.push((
                "code_verifier",
                code_verifier.expect("validated PKCE code verifier"),
            ));
        }
        let mut response = self
            .client
            .post(token_url)
            .form(&form)
            .send()
            .await
            .map_err(|error| {
                HttpError::OAuthToken(format!("could not reach token endpoint: {error}"))
            })?;
        let status = response.status();
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            HttpError::OAuthToken(format!("could not read token response: {error}"))
        })? {
            if body.len().saturating_add(chunk.len()) > MAX_OAUTH_RESPONSE_BYTES {
                return Err(HttpError::OAuthToken(format!(
                    "token response exceeds {MAX_OAUTH_RESPONSE_BYTES} bytes"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            return Err(HttpError::OAuthToken(format!(
                "token endpoint returned HTTP {}",
                status.as_u16()
            )));
        }
        let payload: serde_json::Value = serde_json::from_slice(&body).map_err(|error| {
            HttpError::OAuthToken(format!("token response is not valid JSON: {error}"))
        })?;
        let access_token = payload
            .get("access_token")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| HttpError::OAuthToken("token response has no access_token".to_owned()))?
            .to_owned();
        let token_type = payload
            .get("token_type")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("Bearer")
            .to_owned();
        let expires_in = payload
            .get("expires_in")
            .and_then(serde_json::Value::as_u64)
            .filter(|seconds| *seconds > OAUTH_CACHE_SKEW.as_secs());
        let cached = CachedOAuthToken {
            access_token,
            token_type,
            expires_at: now + Duration::from_secs(expires_in.unwrap_or(0)),
        };
        if expires_in.is_some() {
            let mut token_cache = self
                .oauth_tokens
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if token_cache.len() >= MAX_OAUTH_CACHED_TOKENS {
                if let Some(oldest) = token_cache.keys().next().cloned() {
                    token_cache.remove(&oldest);
                }
            }
            token_cache.insert(key, cached.clone());
        }
        Ok(Some(cached))
    }

    async fn prepare_builder(
        &self,
        request: &Request,
        context: &VariableContext,
    ) -> Result<reqwest::RequestBuilder, HttpError> {
        if request.grpc.is_some() {
            return Err(HttpError::UnsupportedGrpcRequest);
        }
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

        builder = apply_body(builder, &request.body, context).await?;
        let oauth_token = self.oauth_access_token(&request.auth, context).await?;
        builder = apply_auth(builder, &request.auth, context, oauth_token.as_ref())?;

        Ok(builder)
    }
}

fn secret_fingerprint(secret: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    secret.hash(&mut hasher);
    hasher.finish()
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
    oauth_token: Option<&CachedOAuthToken>,
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
        Auth::OAuth2ClientCredentials { .. } => {
            let token = oauth_token
                .ok_or_else(|| HttpError::OAuthToken("access token was not acquired".to_owned()))?;
            builder = builder.header(
                "authorization",
                format!("{} {}", token.token_type, token.access_token),
            );
        }
        Auth::OAuth2AuthorizationCodePkce { .. } => {
            let token = oauth_token
                .ok_or_else(|| HttpError::OAuthToken("access token was not acquired".to_owned()))?;
            builder = builder.header(
                "authorization",
                format!("{} {}", token.token_type, token.access_token),
            );
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
        Auth::OAuth2ClientCredentials {
            token_url,
            client_id,
            client_secret,
            scope,
        } => {
            diagnostics.extend(context.resolve(token_url).diagnostics);
            diagnostics.extend(context.resolve(client_id).diagnostics);
            diagnostics.extend(context.resolve(client_secret).diagnostics);
            if let Some(scope) = scope {
                diagnostics.extend(context.resolve(scope).diagnostics);
            }
        }
        Auth::OAuth2AuthorizationCodePkce {
            authorization_url,
            token_url,
            client_id,
            redirect_uri,
            code,
            code_verifier,
            client_secret,
            scope,
        } => {
            diagnostics.extend(context.resolve(authorization_url).diagnostics);
            diagnostics.extend(context.resolve(token_url).diagnostics);
            diagnostics.extend(context.resolve(client_id).diagnostics);
            diagnostics.extend(context.resolve(redirect_uri).diagnostics);
            diagnostics.extend(context.resolve(code).diagnostics);
            diagnostics.extend(context.resolve(code_verifier).diagnostics);
            if let Some(client_secret) = client_secret {
                diagnostics.extend(context.resolve(client_secret).diagnostics);
            }
            if let Some(scope) = scope {
                diagnostics.extend(context.resolve(scope).diagnostics);
            }
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
    use std::{
        io::{Cursor, Write},
        sync::Arc,
    };
    use tokio::{
        io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    use tokio_rustls::{
        rustls::{
            pki_types::{CertificateDer, PrivateKeyDer},
            server::WebPkiClientVerifier,
            RootCertStore, ServerConfig,
        },
        TlsAcceptor,
    };

    const TEST_CA_PEM: &str = include_str!("../testdata/tls/ca.pem");
    const TEST_SERVER_CERT_PEM: &str = include_str!("../testdata/tls/server.pem");
    const TEST_SERVER_KEY_PEM: &str = include_str!("../testdata/tls/server-key.pem");
    const TEST_CLIENT_CERT_PEM: &str = include_str!("../testdata/tls/client.pem");
    const TEST_CLIENT_KEY_PEM: &str = include_str!("../testdata/tls/client-key.pem");

    fn pem_certificates(pem: &str) -> Vec<CertificateDer<'static>> {
        rustls_pemfile::certs(&mut Cursor::new(pem.as_bytes()))
            .collect::<Result<Vec<_>, _>>()
            .expect("valid test certificate")
    }

    fn pem_private_key(pem: &str) -> PrivateKeyDer<'static> {
        rustls_pemfile::private_key(&mut Cursor::new(pem.as_bytes()))
            .expect("valid test private key")
            .expect("test private key")
    }

    fn test_tls_server_config(require_client: bool) -> ServerConfig {
        let certificate_chain = pem_certificates(TEST_SERVER_CERT_PEM);
        let private_key = pem_private_key(TEST_SERVER_KEY_PEM);
        if require_client {
            let mut roots = RootCertStore::empty();
            for certificate in pem_certificates(TEST_CA_PEM) {
                roots.add(certificate).expect("test CA");
            }
            let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
                .build()
                .expect("client verifier");
            ServerConfig::builder()
                .with_client_cert_verifier(verifier)
                .with_single_cert(certificate_chain, private_key)
                .expect("TLS server config")
        } else {
            ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(certificate_chain, private_key)
                .expect("TLS server config")
        }
    }

    fn write_test_pem(file: &mut tempfile::NamedTempFile, pem: &str) {
        file.write_all(pem.as_bytes()).expect("test PEM");
        file.flush().expect("flush test PEM");
    }

    #[test]
    fn reports_a_missing_ca_certificate_path() {
        let path = PathBuf::from("/definitely-not-a-postly-test-ca.pem");
        let error = HttpEngine::new(&EngineOptions {
            ca_cert: Some(path.clone()),
            ..EngineOptions::default()
        })
        .expect_err("missing CA must fail");
        assert!(matches!(error, HttpError::CaCertificateFile { .. }));
        assert!(error.to_string().contains(&path.display().to_string()));
    }

    #[test]
    fn rejects_invalid_certificate_material_without_building_a_client() {
        let mut ca_file = tempfile::NamedTempFile::new().expect("CA file");
        write_test_pem(
            &mut ca_file,
            "-----BEGIN CERTIFICATE-----\nnot a certificate\n-----END CERTIFICATE-----\n",
        );
        let ca_error = HttpEngine::new(&EngineOptions {
            ca_cert: Some(ca_file.path().to_path_buf()),
            ..EngineOptions::default()
        })
        .expect_err("invalid CA must fail");
        assert!(matches!(
            ca_error,
            HttpError::CaCertificate { .. } | HttpError::CaCertificateEmpty { .. }
        ));

        let mut identity_file = tempfile::NamedTempFile::new().expect("identity file");
        write_test_pem(
            &mut identity_file,
            "-----BEGIN PRIVATE KEY-----\nnot a client identity\n-----END PRIVATE KEY-----\n",
        );
        let identity_error = HttpEngine::new(&EngineOptions {
            client_identity: Some(identity_file.path().to_path_buf()),
            ..EngineOptions::default()
        })
        .expect_err("invalid identity must fail");
        assert!(matches!(identity_error, HttpError::ClientIdentity { .. }));
    }

    #[tokio::test]
    async fn sends_an_https_request_with_a_custom_ca() {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("TLS listener");
        let address = listener.local_addr().expect("TLS address");
        let acceptor = TlsAcceptor::from(Arc::new(test_tls_server_config(false)));
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("TLS connection");
            let mut socket = acceptor.accept(socket).await.expect("TLS handshake");
            let request = read_request_headers(&mut socket).await;
            assert!(request.contains("GET /secure HTTP/1.1"));
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 6\r\nconnection: close\r\n\r\ntls-ok",
                )
                .await
                .expect("TLS response");
            socket.shutdown().await.expect("TLS shutdown");
        });

        let mut ca_file = tempfile::NamedTempFile::new().expect("CA file");
        write_test_pem(&mut ca_file, TEST_CA_PEM);
        let engine = HttpEngine::new(&EngineOptions {
            ca_cert: Some(ca_file.path().to_path_buf()),
            ..EngineOptions::default()
        })
        .expect("HTTP engine with custom CA");
        let request = Request::new(
            "TLS request",
            "GET",
            format!("https://127.0.0.1:{}/secure", address.port()),
        );
        let response = engine
            .execute(&request, &VariableContext::default())
            .await
            .expect("HTTPS response");

        server.await.expect("TLS server");
        assert_eq!(response.status, 200);
        assert_eq!(response.response_size, response.body.len());
        assert_eq!(response.body_text(), "tls-ok");
    }

    #[tokio::test]
    async fn sends_a_client_identity_to_a_mutual_tls_server() {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("mTLS listener");
        let address = listener.local_addr().expect("mTLS address");
        let acceptor = TlsAcceptor::from(Arc::new(test_tls_server_config(true)));
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("mTLS connection");
            let mut socket = acceptor.accept(socket).await.expect("mTLS handshake");
            let request = read_request_headers(&mut socket).await;
            assert!(request.contains("GET /identity HTTP/1.1"));
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 7\r\nconnection: close\r\n\r\nmtls-ok",
                )
                .await
                .expect("mTLS response");
            socket.shutdown().await.expect("mTLS shutdown");
        });

        let mut ca_file = tempfile::NamedTempFile::new().expect("CA file");
        write_test_pem(&mut ca_file, TEST_CA_PEM);
        let mut identity_file = tempfile::NamedTempFile::new().expect("identity file");
        write_test_pem(
            &mut identity_file,
            &format!("{TEST_CLIENT_CERT_PEM}{TEST_CLIENT_KEY_PEM}"),
        );
        let engine = HttpEngine::new(&EngineOptions {
            ca_cert: Some(ca_file.path().to_path_buf()),
            client_identity: Some(identity_file.path().to_path_buf()),
            ..EngineOptions::default()
        })
        .expect("HTTP engine with client identity");
        let request = Request::new(
            "mTLS request",
            "GET",
            format!("https://127.0.0.1:{}/identity", address.port()),
        );
        let response = engine
            .execute(&request, &VariableContext::default())
            .await
            .expect("mTLS response");

        server.await.expect("mTLS server");
        assert_eq!(response.status, 200);
        assert_eq!(response.body_text(), "mtls-ok");
    }

    #[tokio::test]
    async fn exchanges_and_caches_oauth_client_credentials_tokens() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut token_socket, _) = listener.accept().await.expect("token connection");
            let token_request = read_request_headers(&mut token_socket).await;
            assert!(token_request.contains("POST /oauth/token HTTP/1.1"));
            assert!(token_request.contains("grant_type=client_credentials"));
            assert!(token_request.contains("client_id=postly-client"));
            assert!(token_request.contains("client_secret=local-secret"));
            assert!(token_request.contains("scope=read%3Ausers"));
            let token_body =
                br#"{"access_token":"access-123","token_type":"Bearer","expires_in":3600}"#;
            let token_response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                token_body.len(),
                String::from_utf8_lossy(token_body)
            );
            token_socket
                .write_all(token_response.as_bytes())
                .await
                .expect("token response");

            for path in ["/first", "/second"] {
                let (mut api_socket, _) = listener.accept().await.expect("API connection");
                let api_request = read_request_headers(&mut api_socket).await;
                assert!(api_request.contains(&format!("GET {path} HTTP/1.1")));
                assert!(api_request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer access-123"));
                api_socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                    )
                    .await
                    .expect("API response");
            }
        });

        let mut context = VariableContext::default();
        context.set_runtime("oauth_token_url", format!("http://{address}/oauth/token"));
        context.set_runtime("oauth_client_id", "postly-client");
        context.set_runtime("oauth_client_secret", "local-secret");
        context.set_runtime("oauth_scope", "read:users");
        let auth = Auth::OAuth2ClientCredentials {
            token_url: "{{oauth_token_url}}".to_owned(),
            client_id: "{{oauth_client_id}}".to_owned(),
            client_secret: "{{oauth_client_secret}}".to_owned(),
            scope: Some("{{oauth_scope}}".to_owned()),
        };
        let engine = HttpEngine::new(&EngineOptions::default()).expect("engine");
        for path in ["/first", "/second"] {
            let mut request =
                Request::new("OAuth request", "GET", format!("http://{address}{path}"));
            request.auth = auth.clone();
            let response = engine
                .execute(&request, &context)
                .await
                .expect("OAuth response");
            assert_eq!(response.status, 200);
        }

        server.await.expect("OAuth server");
    }

    #[tokio::test]
    async fn exchanges_oauth_authorization_code_with_pkce() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
        let address = listener.local_addr().expect("address");
        let verifier = "a".repeat(43);
        let expected_verifier = verifier.clone();
        let server = tokio::spawn(async move {
            let (mut token_socket, _) = listener.accept().await.expect("token connection");
            let token_request = read_request_headers(&mut token_socket).await;
            assert!(token_request.contains("POST /oauth/token HTTP/1.1"));
            assert!(token_request.contains("grant_type=authorization_code"));
            assert!(token_request.contains("client_id=postly-pkce"));
            assert!(token_request.contains("code=returned-code"));
            assert!(token_request.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A8787%2Fcallback"));
            assert!(token_request.contains(&format!("code_verifier={expected_verifier}")));
            let token_body =
                br#"{"access_token":"pkce-access","token_type":"Bearer","expires_in":3600}"#;
            let token_response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                token_body.len(),
                String::from_utf8_lossy(token_body)
            );
            token_socket
                .write_all(token_response.as_bytes())
                .await
                .expect("token response");

            let (mut api_socket, _) = listener.accept().await.expect("API connection");
            let api_request = read_request_headers(&mut api_socket).await;
            assert!(api_request.contains("GET /profile HTTP/1.1"));
            assert!(api_request
                .to_ascii_lowercase()
                .contains("authorization: bearer pkce-access"));
            api_socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 7\r\nconnection: close\r\n\r\nprofile",
                )
                .await
                .expect("API response");
        });

        let auth = Auth::OAuth2AuthorizationCodePkce {
            authorization_url: "https://auth.example.test/authorize".to_owned(),
            token_url: format!("http://{address}/oauth/token"),
            client_id: "postly-pkce".to_owned(),
            redirect_uri: "http://127.0.0.1:8787/callback".to_owned(),
            code: "returned-code".to_owned(),
            code_verifier: verifier,
            client_secret: None,
            scope: Some("read:profile".to_owned()),
        };
        let mut request = Request::new("PKCE request", "GET", format!("http://{address}/profile"));
        request.auth = auth;
        let engine = HttpEngine::new(&EngineOptions::default()).expect("engine");
        let response = engine
            .execute(&request, &VariableContext::default())
            .await
            .expect("PKCE response");
        assert_eq!(response.status, 200);
        assert_eq!(response.body_text(), "profile");
        server.await.expect("PKCE server");
    }

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

    #[test]
    fn formats_well_formed_xml_in_pretty_view() {
        let response = HttpResponse {
            status: 200,
            status_text: "OK".to_owned(),
            headers: Vec::new(),
            cookies: Vec::new(),
            body: br#"<root><item id="1">one</item><item>two</item></root>"#.to_vec(),
            response_size: 52,
            content_type: Some("application/xml".to_owned()),
            protocol: "HTTP/1.1".to_owned(),
            url: "http://example.test".to_owned(),
            duration_ms: 1,
        };
        assert_eq!(
            response.formatted_body(ResponseView::Pretty),
            "<root>\n  <item id=\"1\">one</item>\n  <item>two</item>\n</root>"
        );
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

    #[tokio::test]
    async fn bypasses_an_explicit_proxy_for_a_no_proxy_host() {
        let target_listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("target listener");
        let target_address = target_listener.local_addr().expect("target address");
        let target = tokio::spawn(async move {
            let (mut socket, _) = target_listener.accept().await.expect("target connection");
            let _ = read_request_headers(&mut socket).await;
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 10\r\n\r\ndirect-ok!",
                )
                .await
                .expect("target response");
        });
        let proxy_listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("proxy listener");
        let proxy_address = proxy_listener.local_addr().expect("proxy address");
        let proxy = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_millis(250), proxy_listener.accept())
                .await
                .is_err()
        });

        let engine = HttpEngine::new(&EngineOptions {
            proxy: Some(format!("http://{proxy_address}")),
            no_proxy: Some("127.0.0.1".to_owned()),
            ..EngineOptions::default()
        })
        .expect("engine");
        let request = Request::new("Direct", "GET", format!("http://{target_address}/direct"));
        let response = engine
            .execute(&request, &VariableContext::default())
            .await
            .expect("direct response");

        target.await.expect("target");
        assert!(proxy.await.expect("proxy observer"));
        assert_eq!(response.status, 200);
        assert_eq!(response.body_text(), "direct-ok!");
    }

    #[test]
    fn accepts_socks_proxy_urls_when_building_the_http_engine() {
        HttpEngine::new(&EngineOptions {
            proxy: Some("socks5h://127.0.0.1:1080".to_owned()),
            ..EngineOptions::default()
        })
        .expect("SOCKS proxy URL should be accepted");
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

    #[test]
    fn persists_manual_cookies_in_a_bounded_local_jar() {
        let directory = tempfile::tempdir().expect("directory");
        let jar_path = directory.path().join(".postly/cookies.json");
        let engine = HttpEngine::new(&EngineOptions {
            cookie_jar: Some(jar_path.clone()),
            ..EngineOptions::default()
        })
        .expect("engine");
        engine
            .add_cookie("https://example.test/api", "session=abc; Path=/")
            .expect("manual cookie");
        assert_eq!(
            engine
                .cookie_header("https://example.test/api/users")
                .expect("cookie header"),
            Some("session=abc".to_owned())
        );
        assert!(jar_path.is_file());
        drop(engine);

        let reopened = HttpEngine::new(&EngineOptions {
            cookie_jar: Some(jar_path.clone()),
            ..EngineOptions::default()
        })
        .expect("reopened engine");
        assert_eq!(
            reopened
                .cookie_header("https://example.test/api/users")
                .expect("reopened cookie header"),
            Some("session=abc".to_owned())
        );

        fs::write(&jar_path, vec![b'x'; MAX_COOKIE_JAR_BYTES + 1]).expect("oversized jar");
        let error = HttpEngine::new(&EngineOptions {
            cookie_jar: Some(jar_path),
            ..EngineOptions::default()
        })
        .expect_err("oversized cookie jar must be rejected");
        assert!(error.to_string().contains("exceeds"));
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

    async fn read_request_headers<R>(socket: &mut R) -> String
    where
        R: AsyncRead + Unpin,
    {
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
