use std::{
    collections::{BTreeMap, HashMap},
    fs,
    hash::{Hash, Hasher},
    io::BufReader,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use cookie_store::{CookieStore as StoredCookieStore, RawCookie};
use md5::Md5;
use quick_xml::{events::Event, Reader, Writer};
use reqwest::{
    cookie::CookieStore,
    header::{HeaderMap, HeaderName, HeaderValue, SET_COOKIE, WWW_AUTHENTICATE},
    Client, Method, Url,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use uuid::Uuid;

use crate::{
    model::{ApiKeyLocation, Auth, HeaderEntry, KeyValue, Request, RequestBody},
    variables::{VariableContext, VariableDiagnostic},
};

const DEFAULT_MAX_RESPONSE_BYTES: usize = 100 * 1024 * 1024;

fn is_pkcs12_identity_path(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        let extension = extension.to_string_lossy();
        extension.eq_ignore_ascii_case("p12") || extension.eq_ignore_ascii_case("pfx")
    })
}

#[derive(Debug, Clone)]
pub struct EngineOptions {
    pub timeout: Duration,
    pub accept_invalid_certs: bool,
    pub max_redirects: usize,
    /// Maximum buffered response body size for regular HTTP requests.
    pub max_response_bytes: usize,
    pub proxy: Option<String>,
    /// Optional comma-separated host/IP bypass list for an explicit proxy.
    pub no_proxy: Option<String>,
    /// An additional PEM-encoded trust anchor for HTTPS connections.
    pub ca_cert: Option<PathBuf>,
    /// A PEM bundle containing the client certificate chain and private key.
    pub client_identity: Option<PathBuf>,
    /// A transient passphrase for a `.p12`/`.pfx` client identity. Never persist it.
    pub client_identity_passphrase: Option<String>,
    /// Optional ignored local cookie-jar file for saved-request sessions.
    pub cookie_jar: Option<PathBuf>,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            accept_invalid_certs: false,
            max_redirects: 10,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            proxy: None,
            no_proxy: None,
            ca_cert: None,
            client_identity: None,
            client_identity_passphrase: None,
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
    #[error("PKCS#12 client identity {path} requires a passphrase")]
    ClientIdentityPassphraseRequired { path: String },
    #[error("could not unlock PKCS#12 client identity {path}: {source}")]
    ClientIdentityPassphrase {
        path: String,
        source: reqwest::Error,
    },
    #[error("a client-identity passphrase only applies to .p12 or .pfx files: {path}")]
    ClientIdentityPassphraseUnsupported { path: String },
    #[error("HTTP request failed: {0}")]
    Request(#[source] reqwest::Error),
    #[error("response body exceeds the configured limit of {limit} bytes")]
    ResponseBodyTooLarge { limit: usize },
    #[error("variable resolution failed")]
    VariableResolution(Vec<VariableDiagnostic>),
    #[error("invalid JSON body: {0}")]
    JsonBody(#[from] serde_json::Error),
    #[error("could not access cookie jar {path}: {message}")]
    CookieJar { path: String, message: String },
    #[error("OAuth 2.0 token request failed: {0}")]
    OAuthToken(String),
    #[error("AWS Signature V4 signing failed: {0}")]
    AwsSignature(String),
    #[error("Digest authentication failed: {0}")]
    Digest(String),
}

/// Open a TCP stream through a SOCKS5 proxy, keeping hostname resolution at
/// the proxy when the target is a domain name. The resulting stream is ready
/// for a protocol handshake such as WebSocket or gRPC.
pub async fn connect_socks5_stream(
    proxy: &url::Url,
    target_host: &str,
    target_port: u16,
) -> Result<TcpStream, String> {
    let proxy_host = proxy
        .host_str()
        .ok_or_else(|| "SOCKS proxy URL has no hostname".to_owned())?;
    let proxy_port = proxy
        .port_or_known_default()
        .ok_or_else(|| "SOCKS proxy URL has no port".to_owned())?;
    let mut socket = TcpStream::connect((proxy_host, proxy_port))
        .await
        .map_err(|error| format!("could not connect to SOCKS proxy: {error}"))?;

    let username = proxy.username().as_bytes();
    let password = proxy.password().unwrap_or_default().as_bytes();
    let has_credentials = !username.is_empty();
    if has_credentials && (username.len() > u8::MAX as usize || password.len() > u8::MAX as usize) {
        return Err("SOCKS proxy credentials exceed 255 bytes".to_owned());
    }
    let mut greeting = vec![0x05, 0x01, 0x00];
    if has_credentials {
        greeting[1] = 0x02;
        greeting.push(0x02);
    }
    socket
        .write_all(&greeting)
        .await
        .map_err(|error| format!("could not write SOCKS greeting: {error}"))?;
    let mut greeting = [0_u8; 2];
    socket
        .read_exact(&mut greeting)
        .await
        .map_err(|error| format!("could not read SOCKS greeting: {error}"))?;
    if greeting[0] != 0x05 {
        return Err("SOCKS proxy returned an invalid version".to_owned());
    }
    match greeting[1] {
        0x00 => {}
        0x02 if has_credentials => {
            let mut credentials = Vec::with_capacity(3 + username.len() + password.len());
            credentials.extend_from_slice(&[0x01, username.len() as u8]);
            credentials.extend_from_slice(username);
            credentials.push(password.len() as u8);
            credentials.extend_from_slice(password);
            socket
                .write_all(&credentials)
                .await
                .map_err(|error| format!("could not write SOCKS credentials: {error}"))?;
            let mut authentication = [0_u8; 2];
            socket
                .read_exact(&mut authentication)
                .await
                .map_err(|error| format!("could not read SOCKS credentials response: {error}"))?;
            if authentication != [0x01, 0x00] {
                return Err("SOCKS proxy rejected credentials".to_owned());
            }
        }
        0xFF => return Err("SOCKS proxy rejected all authentication methods".to_owned()),
        _ => return Err("SOCKS proxy selected unsupported authentication".to_owned()),
    }

    let mut connect_request = vec![0x05, 0x01, 0x00];
    match target_host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(address)) => {
            connect_request.push(0x01);
            connect_request.extend_from_slice(&address.octets());
        }
        Ok(std::net::IpAddr::V6(address)) => {
            connect_request.push(0x04);
            connect_request.extend_from_slice(&address.octets());
        }
        Err(_) => {
            let host = target_host.as_bytes();
            if host.is_empty() || host.len() > u8::MAX as usize {
                return Err("SOCKS target hostname exceeds 255 bytes".to_owned());
            }
            connect_request.push(0x03);
            connect_request.push(host.len() as u8);
            connect_request.extend_from_slice(host);
        }
    }
    connect_request.extend_from_slice(&target_port.to_be_bytes());
    socket
        .write_all(&connect_request)
        .await
        .map_err(|error| format!("could not write SOCKS connect request: {error}"))?;

    let mut response = [0_u8; 4];
    socket
        .read_exact(&mut response)
        .await
        .map_err(|error| format!("could not read SOCKS connect response: {error}"))?;
    if response[0] != 0x05 {
        return Err("SOCKS proxy returned an invalid connect version".to_owned());
    }
    if response[1] != 0x00 {
        return Err(format!(
            "SOCKS proxy connect failed with code 0x{:02x}",
            response[1]
        ));
    }
    match response[3] {
        0x01 => {
            let mut address = [0_u8; 4];
            socket
                .read_exact(&mut address)
                .await
                .map_err(|error| format!("could not read SOCKS IPv4 response: {error}"))?;
        }
        0x03 => {
            let mut length = [0_u8; 1];
            socket
                .read_exact(&mut length)
                .await
                .map_err(|error| format!("could not read SOCKS hostname response: {error}"))?;
            let mut address = vec![0_u8; length[0] as usize];
            socket
                .read_exact(&mut address)
                .await
                .map_err(|error| format!("could not read SOCKS hostname response: {error}"))?;
        }
        0x04 => {
            let mut address = [0_u8; 16];
            socket
                .read_exact(&mut address)
                .await
                .map_err(|error| format!("could not read SOCKS IPv6 response: {error}"))?;
        }
        _ => return Err("SOCKS proxy returned an invalid address type".to_owned()),
    }
    let mut port = [0_u8; 2];
    socket
        .read_exact(&mut port)
        .await
        .map_err(|error| format!("could not read SOCKS port response: {error}"))?;
    Ok(socket)
}

#[derive(Debug, Clone)]
pub struct HttpEngine {
    client: Client,
    cookie_jar: Arc<PersistentCookieJar>,
    oauth_tokens: Arc<Mutex<HashMap<OAuthTokenKey, CachedOAuthToken>>>,
    max_response_bytes: usize,
    options: EngineOptions,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct OAuthTokenKey {
    grant_type: String,
    token_url: String,
    device_authorization_url: Option<String>,
    client_id: String,
    scope: Option<String>,
    client_secret_fingerprint: u64,
    grant_credential_fingerprint: u64,
    code_verifier_fingerprint: u64,
    redirect_uri: Option<String>,
}

#[derive(Debug, Clone)]
struct CachedOAuthToken {
    access_token: String,
    token_type: String,
    expires_at: Instant,
}

/// User-facing instructions returned by an OAuth 2.0 Device Authorization
/// endpoint. The device code itself is intentionally never exposed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthDeviceCodePrompt {
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in: Duration,
    pub interval: Duration,
}

/// Parameters generated for a browser-based OAuth 2.0 Authorization Code +
/// PKCE exchange. The verifier and state stay in memory and are never written
/// to the request file or logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthAuthorizationRequest {
    pub url: String,
    pub redirect_uri: String,
}

const OAUTH_CACHE_SKEW: Duration = Duration::from_secs(30);
const MAX_OAUTH_RESPONSE_BYTES: usize = 1_048_576;
const MAX_OAUTH_CACHED_TOKENS: usize = 128;
const OAUTH_BROWSER_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_OAUTH_CALLBACK_BYTES: usize = 16 * 1024;

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
    /// Milliseconds from starting the HTTP exchange until response headers
    /// are available. This is a practical local TTFB measurement.
    #[serde(default)]
    pub ttfb_ms: u128,
    /// Milliseconds spent consuming the bounded response body after headers
    /// arrived. It excludes the header wait and request preparation.
    #[serde(default)]
    pub download_ms: u128,
    pub protocol: String,
    pub url: String,
    #[serde(default)]
    pub cookies: Vec<ResponseCookie>,
}

/// A local, inspectable summary of a cookie currently held by an HTTP engine.
/// The value is available to trusted callers such as the native GUI, but it is
/// never included in history, logs or serialized request data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCookieInfo {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: Option<String>,
    pub persistent: bool,
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
        let looks_like_yaml = self
            .content_type
            .as_deref()
            .is_some_and(|value| value.contains("yaml"));
        if looks_like_yaml {
            if let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(&text) {
                if let Ok(formatted) = serde_yaml::to_string(&value) {
                    return formatted.trim_end().to_owned();
                }
            }
        }
        let trimmed = text.trim_start();
        let looks_like_html = self
            .content_type
            .as_deref()
            .is_some_and(|value| value.contains("html"))
            || trimmed.starts_with("<!doctype html")
            || trimmed.starts_with("<html");
        if looks_like_html {
            if let Some(formatted) = format_html(&text) {
                return formatted;
            }
        }
        let looks_like_javascript = self
            .content_type
            .as_deref()
            .is_some_and(|value| value.contains("javascript") || value.contains("ecmascript"))
            || trimmed.starts_with("function ")
            || trimmed.starts_with("const ")
            || trimmed.starts_with("let ")
            || trimmed.starts_with("import ")
            || trimmed.starts_with("export ");
        if looks_like_javascript {
            if let Some(formatted) = format_javascript(&text) {
                return formatted;
            }
        }
        text
    }
}

fn append_preview_line(output: &mut String, indent: usize, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    if !output.is_empty() {
        output.push('\n');
    }
    output.push_str(&"  ".repeat(indent));
    output.push_str(text);
}

fn format_html(text: &str) -> Option<String> {
    let mut output = String::new();
    let mut indent = 0_usize;
    let mut cursor = 0_usize;
    while cursor < text.len() {
        let remainder = &text[cursor..];
        let Some(relative_open) = remainder.find('<') else {
            append_preview_line(&mut output, indent, remainder);
            break;
        };
        let open = cursor + relative_open;
        append_preview_line(&mut output, indent, &text[cursor..open]);

        let remainder = &text[open..];
        let tag_end = if remainder.starts_with("<!--") {
            remainder.find("-->").map(|end| open + end + 2)
        } else {
            find_markup_end(text, open + 1)
        }?;
        let tag = text[open..=tag_end].trim();
        if tag.starts_with("</") {
            indent = indent.saturating_sub(1);
        }
        append_preview_line(&mut output, indent, tag);
        let is_closing = tag.starts_with("</");
        let is_self_closing = tag.ends_with("/>")
            || tag.starts_with("<!")
            || tag.starts_with("<?")
            || html_void_element(tag);
        if !is_closing && !is_self_closing {
            indent = indent.saturating_add(1);
        }
        cursor = tag_end + 1;
    }
    let formatted = output.trim_end().to_owned();
    (!formatted.is_empty()).then_some(formatted)
}

fn find_markup_end(text: &str, start: usize) -> Option<usize> {
    let mut quote = None;
    for (offset, character) in text[start..].char_indices() {
        match (quote, character) {
            (Some(expected), character) if character == expected => quote = None,
            (None, '\'' | '"') => quote = Some(character),
            (None, '>') => return Some(start + offset),
            _ => {}
        }
    }
    None
}

fn html_void_element(tag: &str) -> bool {
    let name = tag
        .trim_start_matches('<')
        .trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_end_matches('>')
        .to_ascii_lowercase();
    matches!(
        name.as_str(),
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn format_javascript(text: &str) -> Option<String> {
    let mut output = String::new();
    let mut line = String::new();
    let mut indent = 0_usize;
    let mut parentheses = 0_usize;
    let mut quote = None;
    let mut escaped = false;
    let mut line_comment = false;
    let mut block_comment = false;
    let mut block_stack = Vec::new();
    let mut characters = text.chars().peekable();

    while let Some(character) = characters.next() {
        if line_comment {
            if character == '\n' {
                append_preview_line(&mut output, indent, &line);
                line.clear();
                line_comment = false;
            } else {
                line.push(character);
            }
            continue;
        }
        if block_comment {
            line.push(character);
            if character == '*' && characters.peek() == Some(&'/') {
                line.push(characters.next().expect("peeked slash"));
                block_comment = false;
            } else if character == '\n' {
                append_preview_line(&mut output, indent, &line);
                line.clear();
            }
            continue;
        }
        if let Some(expected_quote) = quote {
            line.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == expected_quote {
                quote = None;
            }
            continue;
        }

        match character {
            '\'' | '"' | '`' => {
                quote = Some(character);
                line.push(character);
            }
            '/' if characters.peek() == Some(&'/') => {
                line.push(character);
                line.push(characters.next().expect("peeked slash"));
                line_comment = true;
            }
            '/' if characters.peek() == Some(&'*') => {
                line.push(character);
                line.push(characters.next().expect("peeked star"));
                block_comment = true;
            }
            '(' => {
                parentheses = parentheses.saturating_add(1);
                line.push(character);
            }
            ')' => {
                parentheses = parentheses.saturating_sub(1);
                line.push(character);
            }
            '{' => {
                let previous = line.trim_end();
                let block = previous.ends_with(')')
                    || previous.ends_with('>')
                    || (!previous.ends_with('=')
                        && !previous.ends_with(':')
                        && !previous.ends_with('[')
                        && !previous.ends_with(','));
                line.push(character);
                block_stack.push(block);
                if block {
                    append_preview_line(&mut output, indent, &line);
                    line.clear();
                    indent = indent.saturating_add(1);
                }
            }
            '}' => {
                let block = block_stack.pop().unwrap_or(true);
                if block {
                    append_preview_line(&mut output, indent, &line);
                    line.clear();
                    indent = indent.saturating_sub(1);
                    line.push(character);
                } else {
                    line.push(character);
                }
            }
            ';' if parentheses == 0 => {
                line.push(character);
                append_preview_line(&mut output, indent, &line);
                line.clear();
            }
            '\n' => {
                append_preview_line(&mut output, indent, &line);
                line.clear();
            }
            _ => line.push(character),
        }
    }
    append_preview_line(&mut output, indent, &line);
    let formatted = output.trim_end().to_owned();
    (!formatted.is_empty()).then_some(formatted)
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

fn build_http_client(
    options: &EngineOptions,
    cookie_jar: Option<Arc<PersistentCookieJar>>,
) -> Result<Client, HttpError> {
    let ca_cert_path = options
        .ca_cert
        .as_deref()
        .map(|path| path.display().to_string());
    let client_identity_path = options
        .client_identity
        .as_deref()
        .map(|path| path.display().to_string());
    let uses_pkcs12_identity = options
        .client_identity
        .as_deref()
        .is_some_and(is_pkcs12_identity_path);
    let mut builder = Client::builder()
        .timeout(options.timeout)
        .danger_accept_invalid_certs(options.accept_invalid_certs)
        .redirect(if options.max_redirects == 0 {
            reqwest::redirect::Policy::none()
        } else {
            reqwest::redirect::Policy::limited(options.max_redirects)
        });
    if let Some(cookie_jar) = cookie_jar {
        builder = builder.cookie_provider(cookie_jar);
    }
    if uses_pkcs12_identity {
        builder = builder.use_native_tls();
    } else {
        builder = builder.use_rustls_tls();
    }
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
        let identity = if uses_pkcs12_identity {
            let passphrase = options
                .client_identity_passphrase
                .as_deref()
                .ok_or_else(|| HttpError::ClientIdentityPassphraseRequired {
                    path: path_display.clone(),
                })?;
            reqwest::Identity::from_pkcs12_der(&pem, passphrase).map_err(|source| {
                HttpError::ClientIdentityPassphrase {
                    path: path_display.clone(),
                    source,
                }
            })?
        } else {
            if options.client_identity_passphrase.is_some() {
                return Err(HttpError::ClientIdentityPassphraseUnsupported { path: path_display });
            }
            reqwest::Identity::from_pem(&pem).map_err(|source| HttpError::ClientIdentity {
                path: path_display,
                source,
            })?
        };
        builder = builder.identity(identity);
    }
    builder.build().map_err(|source| {
        if let Some(path) = client_identity_path {
            HttpError::ClientIdentity { path, source }
        } else if let Some(path) = ca_cert_path {
            HttpError::CaCertificate { path, source }
        } else {
            HttpError::Client(source)
        }
    })
}

impl HttpEngine {
    pub fn new(options: &EngineOptions) -> Result<Self, HttpError> {
        let cookie_jar = Arc::new(PersistentCookieJar::load(options.cookie_jar.as_deref())?);
        let client = build_http_client(options, Some(Arc::clone(&cookie_jar)))?;
        Ok(Self {
            client,
            cookie_jar,
            oauth_tokens: Arc::new(Mutex::new(HashMap::new())),
            max_response_bytes: options.max_response_bytes,
            options: options.clone(),
        })
    }

    fn client_for_request(&self, request: &Request) -> Result<Client, HttpError> {
        let Some(settings) = request.transport.as_ref() else {
            return Ok(self.client.clone());
        };
        let mut options = self.options.clone();
        if settings.follow_redirects == Some(false) {
            options.max_redirects = 0;
        } else if let Some(max_redirects) = settings.max_redirects {
            options.max_redirects = max_redirects;
        }
        if options.max_redirects == self.options.max_redirects && !settings.disable_cookies {
            return Ok(self.client.clone());
        }
        let cookie_jar = (!settings.disable_cookies).then(|| Arc::clone(&self.cookie_jar));
        build_http_client(&options, cookie_jar)
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

    /// Return unexpired cookies in a deterministic order for local inspection.
    pub fn cookie_snapshot(&self) -> Vec<StoredCookieInfo> {
        let mut cookies = self
            .cookie_jar
            .read_store()
            .iter_unexpired()
            .map(|cookie| StoredCookieInfo {
                name: cookie.name().to_owned(),
                value: cookie.value().to_owned(),
                domain: cookie
                    .domain
                    .as_cow()
                    .map(|value| value.into_owned())
                    .unwrap_or_default(),
                path: cookie.path.as_ref().to_owned(),
                secure: cookie.secure().unwrap_or(false),
                http_only: cookie.http_only().unwrap_or(false),
                same_site: cookie.same_site().map(|value| format!("{value:?}")),
                persistent: cookie.is_persistent(),
            })
            .collect::<Vec<_>>();
        cookies.sort_by(|left, right| {
            (&left.domain, &left.path, &left.name).cmp(&(&right.domain, &right.path, &right.name))
        });
        cookies
    }

    /// Clear all cookies held by this engine and persist the empty jar when
    /// the engine is backed by a workspace-local cookie file.
    pub fn clear_cookies(&self) -> Result<(), HttpError> {
        let mut store = self.cookie_jar.write_store();
        store.clear();
        self.cookie_jar.persist(&store)
    }

    pub async fn execute(
        &self,
        request: &Request,
        context: &VariableContext,
    ) -> Result<HttpResponse, HttpError> {
        self.execute_with_device_code_prompt(request, context, |_| {})
            .await
    }

    /// Execute a request and surface OAuth Device Authorization instructions
    /// before the bounded polling loop starts.
    pub async fn execute_with_device_code_prompt<F>(
        &self,
        request: &Request,
        context: &VariableContext,
        on_device_code: F,
    ) -> Result<HttpResponse, HttpError>
    where
        F: Fn(&OAuthDeviceCodePrompt) + Send + Sync,
    {
        let client = self.client_for_request(request)?;
        let builder = self
            .prepare_builder_with_device_code_prompt(&client, request, context, &on_device_code)
            .await?;
        let started = std::time::Instant::now();
        let mut http_request = builder.build().map_err(HttpError::Request)?;
        sign_aws_request(&mut http_request, &request.auth, context)?;
        let mut digest_retry_request = if matches!(&request.auth, Auth::Digest { .. }) {
            Some(http_request.try_clone().ok_or_else(|| {
                HttpError::Digest(
                    "the request body cannot be replayed after the server challenge".to_owned(),
                )
            })?)
        } else {
            None
        };
        let exchange_started = std::time::Instant::now();
        let mut response = client
            .execute(http_request)
            .await
            .map_err(HttpError::Request)?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            if let Some(mut retry_request) = digest_retry_request.take() {
                if let Some(challenge) = digest_challenge(response.headers())? {
                    let authorization = build_digest_authorization(
                        &retry_request,
                        &request.auth,
                        context,
                        &challenge,
                    )?;
                    let value = HeaderValue::from_str(&authorization).map_err(|error| {
                        HttpError::Digest(format!(
                            "generated Authorization header is invalid: {error}"
                        ))
                    })?;
                    retry_request
                        .headers_mut()
                        .insert(HeaderName::from_static("authorization"), value);
                    response = client
                        .execute(retry_request)
                        .await
                        .map_err(HttpError::Request)?;
                }
            }
        }
        let ttfb_ms = exchange_started.elapsed().as_millis();
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
        let body_started = std::time::Instant::now();
        let body = read_bounded_response_body(response, self.max_response_bytes).await?;
        let download_ms = body_started.elapsed().as_millis();
        let response_size = body.len();

        Ok(HttpResponse {
            status: status.as_u16(),
            status_text: status.canonical_reason().unwrap_or_default().to_owned(),
            headers,
            body,
            response_size,
            content_type,
            duration_ms: started.elapsed().as_millis(),
            ttfb_ms,
            download_ms,
            protocol,
            url: final_url,
            cookies,
        })
    }

    /// Execute an Authorization Code + PKCE request, opening a local browser
    /// callback only when the stored authorization code is empty. The caller
    /// owns the actual browser opener so the core remains usable by both the
    /// CLI and native GUI.
    pub async fn execute_with_pkce_browser<F>(
        &self,
        request: &Request,
        context: &VariableContext,
        open_authorization_url: F,
    ) -> Result<HttpResponse, HttpError>
    where
        F: Fn(&str) -> Result<(), String> + Send + Sync,
    {
        let Auth::OAuth2AuthorizationCodePkce {
            authorization_url,
            token_url,
            client_id,
            redirect_uri,
            code,
            code_verifier,
            client_secret,
            scope,
        } = &request.auth
        else {
            return self.execute(request, context).await;
        };
        if !code.trim().is_empty() {
            return self.execute(request, context).await;
        }

        let authorization_url =
            resolve_oauth_browser_value(context, authorization_url, "authorization URL")?;
        let client_id = resolve_oauth_browser_value(context, client_id, "client ID")?;
        let configured_redirect_uri =
            resolve_oauth_browser_value(context, redirect_uri, "redirect URI")?;
        let mut redirect = Url::parse(&configured_redirect_uri).map_err(|error| {
            HttpError::OAuthToken(format!("invalid OAuth redirect URI: {error}"))
        })?;
        validate_oauth_loopback_redirect(&redirect)?;
        let host = redirect
            .host_str()
            .ok_or_else(|| HttpError::OAuthToken("OAuth redirect URI has no host".to_owned()))?
            .to_owned();
        let port = redirect.port().ok_or_else(|| {
            HttpError::OAuthToken(
                "OAuth browser flow requires a loopback redirect URI with an explicit port"
                    .to_owned(),
            )
        })?;
        let listener = TcpListener::bind((host.as_str(), port))
            .await
            .map_err(|error| {
                HttpError::OAuthToken(format!(
                    "could not bind OAuth redirect listener on {configured_redirect_uri}: {error}"
                ))
            })?;
        if port == 0 {
            let actual_port = listener
                .local_addr()
                .map_err(|error| HttpError::OAuthToken(error.to_string()))?
                .port();
            redirect.set_port(Some(actual_port)).map_err(|_| {
                HttpError::OAuthToken("could not assign the OAuth redirect port".to_owned())
            })?;
        }
        let redirect_uri = redirect.to_string();
        let verifier = if code_verifier.trim().is_empty() {
            generate_pkce_verifier()
        } else {
            code_verifier.clone()
        };
        validate_pkce_verifier(&verifier)?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let state = Uuid::new_v4().to_string();
        let mut authorization = Url::parse(&authorization_url).map_err(|error| {
            HttpError::OAuthToken(format!("invalid OAuth authorization URL: {error}"))
        })?;
        if !matches!(authorization.scheme(), "http" | "https") {
            return Err(HttpError::OAuthToken(
                "OAuth authorization URL must use http or https".to_owned(),
            ));
        }
        authorization
            .query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &client_id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state);
        if let Some(scope) = scope
            .as_ref()
            .map(|value| resolve_oauth_browser_value(context, value, "scope"))
            .transpose()?
            .filter(|value| !value.trim().is_empty())
        {
            authorization.query_pairs_mut().append_pair("scope", &scope);
        }
        open_authorization_url(authorization.as_str()).map_err(|error| {
            HttpError::OAuthToken(format!("could not open OAuth authorization URL: {error}"))
        })?;

        let callback_result = tokio::time::timeout(OAUTH_BROWSER_TIMEOUT, async {
            let (mut stream, _) = listener.accept().await.map_err(|error| {
                HttpError::OAuthToken(format!("OAuth redirect listener failed: {error}"))
            })?;
            let result = read_oauth_callback(&mut stream, redirect.path(), &state).await;
            let success = result.is_ok();
            write_oauth_callback_response(&mut stream, success).await?;
            result
        })
        .await
        .map_err(|_| {
            HttpError::OAuthToken(format!(
                "OAuth browser authorization timed out after {} seconds",
                OAUTH_BROWSER_TIMEOUT.as_secs()
            ))
        })??;

        let mut browser_request = request.clone();
        browser_request.auth = Auth::OAuth2AuthorizationCodePkce {
            authorization_url: authorization_url.to_owned(),
            token_url: token_url.to_owned(),
            client_id: client_id.to_owned(),
            redirect_uri,
            code: callback_result,
            code_verifier: verifier,
            client_secret: client_secret.clone(),
            scope: scope.clone(),
        };
        self.execute(&browser_request, context).await
    }

    pub async fn execute_stream(
        &self,
        request: &Request,
        context: &VariableContext,
    ) -> Result<HttpStreamResponse, HttpError> {
        let client = self.client_for_request(request)?;
        let builder = self
            .prepare_builder_with_device_code_prompt(&client, request, context, &|_| {})
            .await?;
        let mut http_request = builder.build().map_err(HttpError::Request)?;
        sign_aws_request(&mut http_request, &request.auth, context)?;
        let response = client
            .execute(http_request)
            .await
            .map_err(HttpError::Request)?;
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
        on_device_code: &dyn Fn(&OAuthDeviceCodePrompt),
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
            Auth::OAuth2RefreshToken {
                token_url,
                client_id,
                refresh_token,
                client_secret,
                scope,
            } => (
                "refresh_token",
                token_url,
                client_id,
                client_secret.as_ref(),
                scope.as_ref(),
                Some(refresh_token),
                None,
                None,
            ),
            Auth::OAuth2DeviceCode {
                device_authorization_url: _,
                token_url,
                client_id,
                client_secret,
                scope,
            } => (
                "urn:ietf:params:oauth:grant-type:device_code",
                token_url,
                client_id,
                client_secret.as_ref(),
                scope.as_ref(),
                None,
                None,
                None,
            ),
            _ => return Ok(None),
        };

        let device_authorization_url = match auth {
            Auth::OAuth2DeviceCode {
                device_authorization_url,
                ..
            } => Some(context.resolve(device_authorization_url).value),
            _ => None,
        };

        let token_url = context.resolve(token_url).value;
        let client_id = context.resolve(client_id).value;
        let client_secret = client_secret.map(|value| context.resolve(value).value);
        let scope = scope
            .map(|value| context.resolve(value).value)
            .filter(|value| !value.trim().is_empty());
        let grant_credential = code.map(|value| context.resolve(value).value);
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
            if grant_credential.as_deref().unwrap_or_default().is_empty()
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
            device_authorization_url: device_authorization_url.clone(),
            client_id: client_id.clone(),
            scope: scope.clone(),
            client_secret_fingerprint: secret_fingerprint(
                client_secret.as_deref().unwrap_or_default(),
            ),
            grant_credential_fingerprint: secret_fingerprint(
                grant_credential.as_deref().unwrap_or_default(),
            ),
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

        if grant_type == "urn:ietf:params:oauth:grant-type:device_code" {
            let device_authorization_url = device_authorization_url
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    HttpError::OAuthToken(
                        "OAuth device authorization URL cannot be empty".to_owned(),
                    )
                })?;
            let device_authorization_url =
                Url::parse(device_authorization_url).map_err(|error| {
                    HttpError::OAuthToken(format!("invalid device authorization URL: {error}"))
                })?;
            let mut device_form = vec![("client_id", client_id.clone())];
            if let Some(scope) = scope.clone() {
                device_form.push(("scope", scope));
            }
            if let Some(client_secret) = client_secret.clone() {
                device_form.push(("client_secret", client_secret));
            }
            let response = self
                .client
                .post(device_authorization_url)
                .form(&device_form)
                .send()
                .await
                .map_err(|error| {
                    HttpError::OAuthToken(format!(
                        "could not reach device authorization endpoint: {error}"
                    ))
                })?;
            let status = response.status();
            let body = read_bounded_oauth_body(response).await?;
            if !status.is_success() {
                return Err(HttpError::OAuthToken(format!(
                    "device authorization endpoint returned HTTP {}",
                    status.as_u16()
                )));
            }
            let payload: serde_json::Value = serde_json::from_slice(&body).map_err(|error| {
                HttpError::OAuthToken(format!(
                    "device authorization response is not valid JSON: {error}"
                ))
            })?;
            let device_code = oauth_string(&payload, "device_code", "device authorization")?;
            let user_code = oauth_string(&payload, "user_code", "device authorization")?;
            let verification_uri = payload
                .get("verification_uri")
                .or_else(|| payload.get("verification_url"))
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    HttpError::OAuthToken(
                        "device authorization response has no verification_uri".to_owned(),
                    )
                })?
                .to_owned();
            let verification_uri_complete = payload
                .get("verification_uri_complete")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(ToOwned::to_owned);
            let expires_in = payload
                .get("expires_in")
                .and_then(serde_json::Value::as_u64)
                .filter(|seconds| (1..=86_400).contains(seconds))
                .ok_or_else(|| {
                    HttpError::OAuthToken(
                        "device authorization response has invalid expires_in".to_owned(),
                    )
                })?;
            let interval = payload
                .get("interval")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(5)
                .clamp(1, 60);
            let prompt = OAuthDeviceCodePrompt {
                user_code,
                verification_uri,
                verification_uri_complete,
                expires_in: Duration::from_secs(expires_in),
                interval: Duration::from_secs(interval),
            };
            on_device_code(&prompt);

            let deadline = Instant::now() + prompt.expires_in;
            let mut poll_interval = prompt.interval;
            loop {
                if Instant::now() >= deadline {
                    return Err(HttpError::OAuthToken(
                        "OAuth device authorization expired before approval".to_owned(),
                    ));
                }
                tokio::time::sleep(poll_interval).await;
                let mut poll_form = vec![
                    ("grant_type", grant_type.to_owned()),
                    ("device_code", device_code.clone()),
                    ("client_id", client_id.clone()),
                ];
                if let Some(client_secret) = client_secret.clone() {
                    poll_form.push(("client_secret", client_secret));
                }
                let response = self
                    .client
                    .post(token_url.clone())
                    .form(&poll_form)
                    .send()
                    .await
                    .map_err(|error| {
                        HttpError::OAuthToken(format!("could not reach token endpoint: {error}"))
                    })?;
                let status = response.status();
                let body = read_bounded_oauth_body(response).await?;
                let payload: serde_json::Value =
                    serde_json::from_slice(&body).map_err(|error| {
                        HttpError::OAuthToken(format!("token response is not valid JSON: {error}"))
                    })?;
                if status.is_success() {
                    let access_token = oauth_string(&payload, "access_token", "token")?;
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
                        expires_at: Instant::now() + Duration::from_secs(expires_in.unwrap_or(0)),
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
                    return Ok(Some(cached));
                }
                match payload.get("error").and_then(serde_json::Value::as_str) {
                    Some("authorization_pending") => {}
                    Some("slow_down") => {
                        poll_interval =
                            (poll_interval + Duration::from_secs(5)).min(Duration::from_secs(60));
                    }
                    Some(error) => {
                        return Err(HttpError::OAuthToken(format!(
                            "device authorization failed: {error}"
                        )));
                    }
                    None => {
                        return Err(HttpError::OAuthToken(format!(
                            "token endpoint returned HTTP {}",
                            status.as_u16()
                        )));
                    }
                }
            }
        }
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
            form.push((
                "code",
                grant_credential.expect("validated authorization code"),
            ));
            form.push((
                "redirect_uri",
                redirect_uri.expect("validated redirect URI"),
            ));
            form.push((
                "code_verifier",
                code_verifier.expect("validated PKCE code verifier"),
            ));
        } else if grant_type == "refresh_token" {
            if grant_credential.as_deref().unwrap_or_default().is_empty() {
                return Err(HttpError::OAuthToken(
                    "OAuth refresh token cannot be empty".to_owned(),
                ));
            }
            form.push((
                "refresh_token",
                grant_credential.expect("validated refresh token"),
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

    async fn prepare_builder_with_device_code_prompt(
        &self,
        client: &Client,
        request: &Request,
        context: &VariableContext,
        on_device_code: &dyn Fn(&OAuthDeviceCodePrompt),
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
        let mut builder = client.request(method, url.clone());
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
        let oauth_token = self
            .oauth_access_token(&request.auth, context, on_device_code)
            .await?;
        builder = apply_auth(builder, &request.auth, context, oauth_token.as_ref())?;

        Ok(builder)
    }
}

fn secret_fingerprint(secret: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    secret.hash(&mut hasher);
    hasher.finish()
}

fn sign_aws_request(
    request: &mut reqwest::Request,
    auth: &Auth,
    context: &VariableContext,
) -> Result<(), HttpError> {
    sign_aws_request_at(request, auth, context, Utc::now())
}

fn sign_aws_request_at(
    request: &mut reqwest::Request,
    auth: &Auth,
    context: &VariableContext,
    now: DateTime<Utc>,
) -> Result<(), HttpError> {
    let Auth::AwsSignatureV4 {
        access_key_id,
        secret_access_key,
        region,
        service,
        session_token,
    } = auth
    else {
        return Ok(());
    };
    let access_key_id = resolve_aws_value(context, access_key_id, "access key ID")?;
    let secret_access_key = resolve_aws_value(context, secret_access_key, "secret access key")?;
    let region = resolve_aws_value(context, region, "region")?.to_ascii_lowercase();
    let service = resolve_aws_value(context, service, "service")?.to_ascii_lowercase();
    if region.is_empty() || service.is_empty() {
        return Err(HttpError::AwsSignature(
            "AWS Signature V4 region and service cannot be empty".to_owned(),
        ));
    }
    let session_token = session_token
        .as_deref()
        .map(|value| resolve_aws_value(context, value, "session token"))
        .transpose()?;

    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date = now.format("%Y%m%d").to_string();
    request.headers_mut().insert(
        HeaderName::from_static("x-amz-date"),
        HeaderValue::from_str(&amz_date).map_err(|source| HttpError::InvalidHeaderValue {
            name: "x-amz-date".to_owned(),
            source,
        })?,
    );
    if let Some(session_token) = session_token.filter(|value| !value.is_empty()) {
        request.headers_mut().insert(
            HeaderName::from_static("x-amz-security-token"),
            HeaderValue::from_str(&session_token).map_err(|source| {
                HttpError::InvalidHeaderValue {
                    name: "x-amz-security-token".to_owned(),
                    source,
                }
            })?,
        );
    }
    if !request.headers().contains_key("host") {
        let host = aws_host(request.url());
        request.headers_mut().insert(
            HeaderName::from_static("host"),
            HeaderValue::from_str(&host).map_err(|source| HttpError::InvalidHeaderValue {
                name: "host".to_owned(),
                source,
            })?,
        );
    }

    let payload_hash = request
        .body()
        .and_then(|body| body.as_bytes())
        .map(sha256_hex_bytes)
        .unwrap_or_else(|| sha256_hex_bytes(&[]));
    let mut query = request
        .url()
        .query_pairs()
        .map(|(key, value)| (aws_uri_encode(&key, false), aws_uri_encode(&value, false)))
        .collect::<Vec<_>>();
    query.sort();
    let canonical_query = query
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&");

    let mut canonical_headers = BTreeMap::<String, Vec<String>>::new();
    for (name, value) in request.headers() {
        if name == "authorization" {
            continue;
        }
        let value = value.to_str().map_err(|_| {
            HttpError::AwsSignature(format!(
                "AWS Signature V4 cannot sign non-UTF-8 header {}",
                name.as_str()
            ))
        })?;
        canonical_headers
            .entry(name.as_str().to_ascii_lowercase())
            .or_default()
            .push(canonical_header_value(value));
    }
    let signed_headers = canonical_headers
        .keys()
        .cloned()
        .collect::<Vec<_>>()
        .join(";");
    let canonical_headers_text = canonical_headers
        .iter()
        .map(|(name, values)| format!("{name}:{}\n", values.join(",")))
        .collect::<String>();
    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        request.method().as_str(),
        aws_canonical_path(request.url()),
        canonical_query,
        canonical_headers_text,
        signed_headers,
        payload_hash
    );
    let credential_scope = format!("{date}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
        sha256_hex_bytes(canonical_request.as_bytes())
    );
    let date_key = hmac_sha256(
        format!("AWS4{secret_access_key}").as_bytes(),
        date.as_bytes(),
    );
    let region_key = hmac_sha256(&date_key, region.as_bytes());
    let service_key = hmac_sha256(&region_key, service.as_bytes());
    let signing_key = hmac_sha256(&service_key, b"aws4_request");
    let signature = hex_bytes(&hmac_sha256(&signing_key, string_to_sign.as_bytes()));
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}"
    );
    request.headers_mut().insert(
        HeaderName::from_static("authorization"),
        HeaderValue::from_str(&authorization).map_err(|source| HttpError::InvalidHeaderValue {
            name: "authorization".to_owned(),
            source,
        })?,
    );
    Ok(())
}

fn resolve_aws_value(
    context: &VariableContext,
    value: &str,
    label: &str,
) -> Result<String, HttpError> {
    let resolved = context.resolve(value);
    if !resolved.diagnostics.is_empty() {
        return Err(HttpError::VariableResolution(resolved.diagnostics));
    }
    if resolved.value.trim().is_empty() {
        return Err(HttpError::AwsSignature(format!(
            "AWS Signature V4 {label} cannot be empty"
        )));
    }
    Ok(resolved.value)
}

fn canonical_header_value(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn aws_host(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    url.port()
        .map(|port| format!("{host}:{port}"))
        .unwrap_or(host)
}

fn aws_canonical_path(url: &Url) -> String {
    let path = if url.path().is_empty() {
        "/"
    } else {
        url.path()
    };
    let mut encoded = String::with_capacity(path.len());
    let bytes = path.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%'
            && index + 2 < bytes.len()
            && is_hex(bytes[index + 1])
            && is_hex(bytes[index + 2])
        {
            encoded.push('%');
            encoded.push((bytes[index + 1] as char).to_ascii_uppercase());
            encoded.push((bytes[index + 2] as char).to_ascii_uppercase());
            index += 3;
        } else {
            encoded.push_str(&aws_uri_encode(
                &String::from_utf8_lossy(&[byte]),
                byte == b'/',
            ));
            index += 1;
        }
    }
    encoded
}

fn aws_uri_encode(value: &str, preserve_slashes: bool) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'_' | b'~')
            || (preserve_slashes && *byte == b'/')
        {
            encoded.push(*byte as char);
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }
    encoded
}

fn is_hex(byte: u8) -> bool {
    byte.is_ascii_hexdigit()
}

fn sha256_hex_bytes(value: &[u8]) -> String {
    hex_bytes(&Sha256::digest(value))
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK_SIZE: usize = 64;
    let mut normalized_key = [0_u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        normalized_key[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        normalized_key[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36_u8; BLOCK_SIZE];
    let mut outer_pad = [0x5c_u8; BLOCK_SIZE];
    for index in 0..BLOCK_SIZE {
        inner_pad[index] ^= normalized_key[index];
        outer_pad[index] ^= normalized_key[index];
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    outer.finalize().into()
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn resolve_oauth_browser_value(
    context: &VariableContext,
    value: &str,
    label: &str,
) -> Result<String, HttpError> {
    let resolved = context.resolve(value);
    if !resolved.diagnostics.is_empty() {
        return Err(HttpError::VariableResolution(resolved.diagnostics));
    }
    if resolved.value.trim().is_empty() {
        return Err(HttpError::OAuthToken(format!(
            "OAuth {label} cannot be empty"
        )));
    }
    Ok(resolved.value)
}

fn validate_pkce_verifier(verifier: &str) -> Result<(), HttpError> {
    let verifier_len = verifier.len();
    if !(43..=128).contains(&verifier_len)
        || !verifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._~".contains(&byte))
    {
        return Err(HttpError::OAuthToken(
            "OAuth PKCE code verifier must be 43-128 unreserved characters".to_owned(),
        ));
    }
    Ok(())
}

fn generate_pkce_verifier() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn validate_oauth_loopback_redirect(redirect: &Url) -> Result<(), HttpError> {
    if redirect.scheme() != "http"
        || redirect.username() != ""
        || redirect.password().is_some()
        || redirect.fragment().is_some()
    {
        return Err(HttpError::OAuthToken(
            "OAuth browser callbacks require an HTTP loopback redirect without credentials or fragments"
                .to_owned(),
        ));
    }
    let host = redirect.host_str().unwrap_or_default().to_ascii_lowercase();
    if !matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1") {
        return Err(HttpError::OAuthToken(
            "OAuth browser callbacks are restricted to localhost, 127.0.0.1 or ::1".to_owned(),
        ));
    }
    if redirect.port().is_none() {
        return Err(HttpError::OAuthToken(
            "OAuth browser callbacks require an explicit loopback port".to_owned(),
        ));
    }
    Ok(())
}

async fn read_oauth_callback(
    stream: &mut TcpStream,
    expected_path: &str,
    expected_state: &str,
) -> Result<String, HttpError> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 2048];
    loop {
        let count = stream.read(&mut buffer).await.map_err(|error| {
            HttpError::OAuthToken(format!("could not read OAuth callback: {error}"))
        })?;
        if count == 0 {
            break;
        }
        if request.len().saturating_add(count) > MAX_OAUTH_CALLBACK_BYTES {
            return Err(HttpError::OAuthToken(
                "OAuth callback request exceeded the safety limit".to_owned(),
            ));
        }
        request.extend_from_slice(&buffer[..count]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let request = std::str::from_utf8(&request)
        .map_err(|_| HttpError::OAuthToken("OAuth callback was not valid HTTP".to_owned()))?;
    let request_line = request.lines().next().ok_or_else(|| {
        HttpError::OAuthToken("OAuth callback did not include a request line".to_owned())
    })?;
    let mut parts = request_line.split_whitespace();
    if parts.next() != Some("GET") {
        return Err(HttpError::OAuthToken(
            "OAuth callback must use an HTTP GET request".to_owned(),
        ));
    }
    let target = parts.next().ok_or_else(|| {
        HttpError::OAuthToken("OAuth callback did not include a target".to_owned())
    })?;
    if !target.starts_with('/') {
        return Err(HttpError::OAuthToken(
            "OAuth callback must use an origin-form target".to_owned(),
        ));
    }
    let callback = Url::parse(&format!("http://postly.invalid{target}")).map_err(|error| {
        HttpError::OAuthToken(format!("invalid OAuth callback target: {error}"))
    })?;
    if callback.path() != expected_path {
        return Err(HttpError::OAuthToken(
            "OAuth callback path did not match the configured redirect URI".to_owned(),
        ));
    }
    let mut code = None;
    let mut state = None;
    let mut oauth_error = None;
    let mut error_description = None;
    for (key, value) in callback.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => oauth_error = Some(value.into_owned()),
            "error_description" => error_description = Some(value.into_owned()),
            _ => {}
        }
    }
    if let Some(error) = oauth_error {
        return Err(HttpError::OAuthToken(format!(
            "OAuth authorization was denied: {error}{}",
            error_description
                .map(|description| format!(" ({description})"))
                .unwrap_or_default()
        )));
    }
    if state.as_deref() != Some(expected_state) {
        return Err(HttpError::OAuthToken(
            "OAuth callback state did not match the authorization request".to_owned(),
        ));
    }
    code.filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            HttpError::OAuthToken("OAuth callback did not include an authorization code".to_owned())
        })
}

async fn write_oauth_callback_response(
    stream: &mut TcpStream,
    success: bool,
) -> Result<(), HttpError> {
    let body = if success {
        "<!doctype html><title>Postly authorization received</title><p>Authorization received. You can return to Postly.</p>"
    } else {
        "<!doctype html><title>Postly authorization failed</title><p>Postly could not complete this authorization. You can close this window.</p>"
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|error| {
            HttpError::OAuthToken(format!("could not respond to OAuth callback: {error}"))
        })
}

async fn read_bounded_oauth_body(mut response: reqwest::Response) -> Result<Vec<u8>, HttpError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| HttpError::OAuthToken(format!("could not read OAuth response: {error}")))?
    {
        if body.len().saturating_add(chunk.len()) > MAX_OAUTH_RESPONSE_BYTES {
            return Err(HttpError::OAuthToken(format!(
                "OAuth response exceeds {MAX_OAUTH_RESPONSE_BYTES} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn read_bounded_response_body(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, HttpError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(HttpError::ResponseBodyTooLarge { limit });
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(HttpError::Request)? {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(HttpError::ResponseBodyTooLarge { limit });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn oauth_string(
    payload: &serde_json::Value,
    field: &str,
    response_kind: &str,
) -> Result<String, HttpError> {
    payload
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| HttpError::OAuthToken(format!("{response_kind} response has no {field}")))
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

const MAX_DIGEST_CHALLENGE_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DigestAlgorithm {
    Md5,
    Md5Sess,
    Sha256,
    Sha256Sess,
}

impl DigestAlgorithm {
    fn parse(value: Option<&str>) -> Result<Self, HttpError> {
        match value.unwrap_or("MD5").trim().to_ascii_lowercase().as_str() {
            "md5" => Ok(Self::Md5),
            "md5-sess" => Ok(Self::Md5Sess),
            "sha-256" => Ok(Self::Sha256),
            "sha-256-sess" => Ok(Self::Sha256Sess),
            other => Err(HttpError::Digest(format!(
                "unsupported challenge algorithm {other}"
            ))),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Md5 => "MD5",
            Self::Md5Sess => "MD5-sess",
            Self::Sha256 => "SHA-256",
            Self::Sha256Sess => "SHA-256-sess",
        }
    }

    fn session(self) -> bool {
        matches!(self, Self::Md5Sess | Self::Sha256Sess)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DigestQop {
    Auth,
    AuthInt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DigestChallenge {
    realm: String,
    nonce: String,
    opaque: Option<String>,
    algorithm: DigestAlgorithm,
    qop: Option<DigestQop>,
}

fn digest_challenge(headers: &HeaderMap) -> Result<Option<DigestChallenge>, HttpError> {
    for value in headers.get_all(WWW_AUTHENTICATE).iter() {
        let value = value.to_str().map_err(|error| {
            HttpError::Digest(format!(
                "WWW-Authenticate header is not valid UTF-8: {error}"
            ))
        })?;
        if value
            .split_once(char::is_whitespace)
            .map(|(scheme, _)| scheme.eq_ignore_ascii_case("Digest"))
            .unwrap_or_else(|| value.eq_ignore_ascii_case("Digest"))
        {
            return parse_digest_challenge(value).map(Some);
        }
    }
    Ok(None)
}

fn parse_digest_challenge(value: &str) -> Result<DigestChallenge, HttpError> {
    if value.len() > MAX_DIGEST_CHALLENGE_BYTES {
        return Err(HttpError::Digest(format!(
            "WWW-Authenticate challenge exceeds {MAX_DIGEST_CHALLENGE_BYTES} bytes"
        )));
    }
    let mut chars = value.chars().peekable();
    let mut scheme = String::new();
    while let Some(character) = chars.peek().copied() {
        if character.is_ascii_whitespace() {
            break;
        }
        scheme.push(character);
        chars.next();
    }
    if !scheme.eq_ignore_ascii_case("Digest") {
        return Err(HttpError::Digest(
            "WWW-Authenticate header is not a Digest challenge".to_owned(),
        ));
    }
    while chars
        .peek()
        .is_some_and(|character| character.is_ascii_whitespace())
    {
        chars.next();
    }

    let mut parameters = HashMap::new();
    while chars.peek().is_some() {
        while chars
            .peek()
            .is_some_and(|character| character.is_ascii_whitespace() || *character == ',')
        {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let mut key = String::new();
        while let Some(character) = chars.peek().copied() {
            if character == '=' || character == ',' || character.is_ascii_whitespace() {
                break;
            }
            key.push(character.to_ascii_lowercase());
            chars.next();
        }
        while chars
            .peek()
            .is_some_and(|character| character.is_ascii_whitespace())
        {
            chars.next();
        }
        if chars.next() != Some('=') || key.is_empty() {
            return Err(HttpError::Digest(
                "malformed Digest challenge parameter".to_owned(),
            ));
        }
        while chars
            .peek()
            .is_some_and(|character| character.is_ascii_whitespace())
        {
            chars.next();
        }
        let parameter = if chars.peek() == Some(&'"') {
            chars.next();
            let mut output = String::new();
            let mut escaped = false;
            let mut closed = false;
            for character in chars.by_ref() {
                if escaped {
                    output.push(character);
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    closed = true;
                    break;
                } else {
                    output.push(character);
                }
            }
            if !closed || escaped {
                return Err(HttpError::Digest(
                    "unterminated quoted Digest challenge parameter".to_owned(),
                ));
            }
            output
        } else {
            let mut output = String::new();
            while let Some(character) = chars.peek().copied() {
                if character == ',' {
                    break;
                }
                output.push(character);
                chars.next();
            }
            output.trim().to_owned()
        };
        parameters.insert(key, parameter);
        while chars
            .peek()
            .is_some_and(|character| character.is_ascii_whitespace())
        {
            chars.next();
        }
        if chars.peek() == Some(&',') {
            chars.next();
        }
    }

    let realm = parameters
        .remove("realm")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| HttpError::Digest("Digest challenge has no realm".to_owned()))?;
    let nonce = parameters
        .remove("nonce")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| HttpError::Digest("Digest challenge has no nonce".to_owned()))?;
    let algorithm = DigestAlgorithm::parse(parameters.get("algorithm").map(String::as_str))?;
    let qop = parameters.get("qop").and_then(|value| {
        value
            .split(',')
            .map(str::trim)
            .find(|value| value.eq_ignore_ascii_case("auth"))
            .map(|_| DigestQop::Auth)
            .or_else(|| {
                value
                    .split(',')
                    .map(str::trim)
                    .find(|value| value.eq_ignore_ascii_case("auth-int"))
                    .map(|_| DigestQop::AuthInt)
            })
    });
    if parameters.get("qop").is_some_and(|_| qop.is_none()) {
        return Err(HttpError::Digest(
            "Digest challenge offers no supported qop (auth or auth-int)".to_owned(),
        ));
    }
    Ok(DigestChallenge {
        realm,
        nonce,
        opaque: parameters.remove("opaque"),
        algorithm,
        qop,
    })
}

fn digest_hash(algorithm: DigestAlgorithm, value: &[u8]) -> String {
    match algorithm {
        DigestAlgorithm::Md5 | DigestAlgorithm::Md5Sess => {
            format!("{:x}", Md5::digest(value))
        }
        DigestAlgorithm::Sha256 | DigestAlgorithm::Sha256Sess => {
            format!("{:x}", Sha256::digest(value))
        }
    }
}

fn digest_quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn build_digest_authorization(
    request: &reqwest::Request,
    auth: &Auth,
    context: &VariableContext,
    challenge: &DigestChallenge,
) -> Result<String, HttpError> {
    let Auth::Digest { username, password } = auth else {
        return Err(HttpError::Digest(
            "Digest challenge received for a non-Digest request".to_owned(),
        ));
    };
    let username = context.resolve(username).value;
    let password = context.resolve(password).value;
    if username.is_empty() {
        return Err(HttpError::Digest("username cannot be empty".to_owned()));
    }
    let uri = request.url().path().to_owned()
        + request
            .url()
            .query()
            .map(|query| format!("?{query}"))
            .as_deref()
            .unwrap_or_default();
    let body = request
        .body()
        .and_then(|body| body.as_bytes())
        .unwrap_or_default();
    let cnonce = Uuid::new_v4().simple().to_string();
    build_digest_authorization_with_cnonce(
        request.method().as_str(),
        &uri,
        body,
        &username,
        &password,
        challenge,
        &cnonce,
    )
}

fn build_digest_authorization_with_cnonce(
    method: &str,
    uri: &str,
    body: &[u8],
    username: &str,
    password: &str,
    challenge: &DigestChallenge,
    cnonce: &str,
) -> Result<String, HttpError> {
    let algorithm = challenge.algorithm;
    let ha1_base = digest_hash(
        algorithm,
        format!("{username}:{}:{password}", challenge.realm).as_bytes(),
    );
    let ha1 = if algorithm.session() {
        digest_hash(
            algorithm,
            format!("{ha1_base}:{}:{cnonce}", challenge.nonce).as_bytes(),
        )
    } else {
        ha1_base
    };
    let ha2 = match challenge.qop {
        Some(DigestQop::AuthInt) => digest_hash(
            algorithm,
            format!("{method}:{uri}:{}", digest_hash(algorithm, body)).as_bytes(),
        ),
        Some(DigestQop::Auth) | None => {
            digest_hash(algorithm, format!("{method}:{uri}").as_bytes())
        }
    };
    let nonce_count = "00000001";
    let response = match challenge.qop {
        Some(qop) => {
            let qop = match qop {
                DigestQop::Auth => "auth",
                DigestQop::AuthInt => "auth-int",
            };
            digest_hash(
                algorithm,
                format!(
                    "{ha1}:{}:{nonce_count}:{cnonce}:{qop}:{ha2}",
                    challenge.nonce
                )
                .as_bytes(),
            )
        }
        None => digest_hash(
            algorithm,
            format!("{ha1}:{}:{ha2}", challenge.nonce).as_bytes(),
        ),
    };
    let mut fields = vec![
        format!("username={}", digest_quoted(username)),
        format!("realm={}", digest_quoted(&challenge.realm)),
        format!("nonce={}", digest_quoted(&challenge.nonce)),
        format!("uri={}", digest_quoted(uri)),
        format!("response={}", digest_quoted(&response)),
        format!("algorithm={}", algorithm.label()),
    ];
    if let Some(qop) = challenge.qop {
        fields.push(format!(
            "qop={}",
            match qop {
                DigestQop::Auth => "auth",
                DigestQop::AuthInt => "auth-int",
            }
        ));
        fields.push(format!("nc={nonce_count}"));
        fields.push(format!("cnonce={}", digest_quoted(cnonce)));
    }
    if let Some(opaque) = &challenge.opaque {
        fields.push(format!("opaque={}", digest_quoted(opaque)));
    }
    Ok(format!("Digest {}", fields.join(", ")))
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
        Auth::Digest { .. } => {}
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
        Auth::OAuth2RefreshToken { .. } => {
            let token = oauth_token
                .ok_or_else(|| HttpError::OAuthToken("access token was not acquired".to_owned()))?;
            builder = builder.header(
                "authorization",
                format!("{} {}", token.token_type, token.access_token),
            );
        }
        Auth::OAuth2DeviceCode { .. } => {
            let token = oauth_token
                .ok_or_else(|| HttpError::OAuthToken("access token was not acquired".to_owned()))?;
            builder = builder.header(
                "authorization",
                format!("{} {}", token.token_type, token.access_token),
            );
        }
        Auth::AwsSignatureV4 { .. } => {}
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
        Auth::Digest { username, password } => {
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
        Auth::OAuth2RefreshToken {
            token_url,
            client_id,
            refresh_token,
            client_secret,
            scope,
        } => {
            diagnostics.extend(context.resolve(token_url).diagnostics);
            diagnostics.extend(context.resolve(client_id).diagnostics);
            diagnostics.extend(context.resolve(refresh_token).diagnostics);
            if let Some(client_secret) = client_secret {
                diagnostics.extend(context.resolve(client_secret).diagnostics);
            }
            if let Some(scope) = scope {
                diagnostics.extend(context.resolve(scope).diagnostics);
            }
        }
        Auth::OAuth2DeviceCode {
            device_authorization_url,
            token_url,
            client_id,
            client_secret,
            scope,
        } => {
            diagnostics.extend(context.resolve(device_authorization_url).diagnostics);
            diagnostics.extend(context.resolve(token_url).diagnostics);
            diagnostics.extend(context.resolve(client_id).diagnostics);
            if let Some(client_secret) = client_secret {
                diagnostics.extend(context.resolve(client_secret).diagnostics);
            }
            if let Some(scope) = scope {
                diagnostics.extend(context.resolve(scope).diagnostics);
            }
        }
        Auth::AwsSignatureV4 {
            access_key_id,
            secret_access_key,
            region,
            service,
            session_token,
        } => {
            diagnostics.extend(context.resolve(access_key_id).diagnostics);
            diagnostics.extend(context.resolve(secret_access_key).diagnostics);
            diagnostics.extend(context.resolve(region).diagnostics);
            diagnostics.extend(context.resolve(service).diagnostics);
            if let Some(session_token) = session_token {
                diagnostics.extend(context.resolve(session_token).diagnostics);
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
    use crate::model::{KeyValue, RequestTransportSettings};
    use std::{
        io::{Cursor, Write},
        process::Command,
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

    fn create_test_pkcs12_identity(directory: &Path) -> Option<PathBuf> {
        let certificate = directory.join("client.pem");
        let private_key = directory.join("client-key.pem");
        let identity = directory.join("client-identity.p12");
        fs::write(&certificate, TEST_CLIENT_CERT_PEM).expect("client certificate fixture");
        fs::write(&private_key, TEST_CLIENT_KEY_PEM).expect("client key fixture");
        let output = Command::new("openssl")
            .args(["pkcs12", "-export", "-inkey"])
            .arg(&private_key)
            .args(["-in"])
            .arg(&certificate)
            .args([
                "-out",
                identity.to_str().expect("UTF-8 test path"),
                "-passout",
                "pass:postly-test-password",
            ])
            .output()
            .ok()?;
        output.status.success().then_some(identity)
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

    #[test]
    fn requires_a_passphrase_for_pkcs12_identity() {
        let directory = tempfile::tempdir().expect("tempdir");
        let identity = directory.path().join("client-identity.p12");
        fs::write(&identity, b"not a PKCS#12 container").expect("identity fixture");
        let error = HttpEngine::new(&EngineOptions {
            client_identity: Some(identity.clone()),
            ..EngineOptions::default()
        })
        .expect_err("PKCS#12 passphrase must be required");
        assert!(matches!(
            error,
            HttpError::ClientIdentityPassphraseRequired { .. }
        ));
        assert!(error.to_string().contains("client-identity.p12"));
    }

    #[test]
    fn accepts_a_password_protected_pkcs12_identity() {
        let directory = tempfile::tempdir().expect("tempdir");
        let Some(identity) = create_test_pkcs12_identity(directory.path()) else {
            return;
        };
        let engine = HttpEngine::new(&EngineOptions {
            client_identity: Some(identity),
            client_identity_passphrase: Some("postly-test-password".to_owned()),
            ..EngineOptions::default()
        })
        .expect("valid PKCS#12 identity");
        drop(engine);
    }

    #[test]
    fn rejects_an_incorrect_pkcs12_passphrase() {
        let directory = tempfile::tempdir().expect("tempdir");
        let Some(identity) = create_test_pkcs12_identity(directory.path()) else {
            return;
        };
        let error = HttpEngine::new(&EngineOptions {
            client_identity: Some(identity),
            client_identity_passphrase: Some("wrong-password".to_owned()),
            ..EngineOptions::default()
        })
        .expect_err("incorrect PKCS#12 passphrase must fail");
        assert!(matches!(error, HttpError::ClientIdentityPassphrase { .. }));
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
    async fn sends_a_pkcs12_client_identity_to_a_mutual_tls_server() {
        let directory = tempfile::tempdir().expect("tempdir");
        let Some(identity_path) = create_test_pkcs12_identity(directory.path()) else {
            return;
        };
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("mTLS listener");
        let address = listener.local_addr().expect("mTLS address");
        let acceptor = TlsAcceptor::from(Arc::new(test_tls_server_config(true)));
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("mTLS connection");
            let mut socket = acceptor.accept(socket).await.expect("mTLS handshake");
            let request = read_request_headers(&mut socket).await;
            assert!(request.contains("GET /pkcs12 HTTP/1.1"));
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 11\r\nconnection: close\r\n\r\npkcs12-mtls",
                )
                .await
                .expect("mTLS response");
            socket.shutdown().await.expect("TLS shutdown");
        });

        let mut ca_file = tempfile::NamedTempFile::new().expect("CA file");
        write_test_pem(&mut ca_file, TEST_CA_PEM);
        let engine = HttpEngine::new(&EngineOptions {
            accept_invalid_certs: true,
            ca_cert: Some(ca_file.path().to_path_buf()),
            client_identity: Some(identity_path),
            client_identity_passphrase: Some("postly-test-password".to_owned()),
            ..EngineOptions::default()
        })
        .expect("HTTP engine with PKCS#12 client identity");
        let request = Request::new(
            "PKCS#12 mTLS request",
            "GET",
            format!("https://127.0.0.1:{}/pkcs12", address.port()),
        );
        let response = engine
            .execute(&request, &VariableContext::default())
            .await
            .expect("PKCS#12 mTLS response");

        server.await.expect("TLS server");
        assert_eq!(response.status, 200);
        assert_eq!(response.body_text(), "pkcs12-mtls");
    }

    #[tokio::test]
    async fn retries_a_digest_challenge_once_with_rfc7616_auth() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut first_socket, _) = listener.accept().await.expect("first connection");
            let first_request = read_request_headers(&mut first_socket).await;
            assert!(first_request.contains("GET /dir/index.html?query=1 HTTP/1.1"));
            assert!(!first_request
                .to_ascii_lowercase()
                .contains("authorization: digest"));
            first_socket
                .write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Digest realm=\"local\", nonce=\"nonce-123\", qop=\"auth\", opaque=\"opaque-456\"\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await
                .expect("challenge response");

            let (mut second_socket, _) = listener.accept().await.expect("retry connection");
            let second_request = read_request_headers(&mut second_socket).await;
            let lower = second_request.to_ascii_lowercase();
            assert!(lower.contains("authorization: digest"));
            assert!(lower.contains("username=\"postly\""));
            assert!(lower.contains("realm=\"local\""));
            assert!(lower.contains("nonce=\"nonce-123\""));
            assert!(lower.contains("uri=\"/dir/index.html?query=1\""));
            assert!(lower.contains("qop=auth"));
            assert!(lower.contains("opaque=\"opaque-456\""));
            second_socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 9\r\nconnection: close\r\n\r\ndigest-ok",
                )
                .await
                .expect("success response");
        });

        let mut request = Request::new(
            "Digest request",
            "GET",
            format!("http://{address}/dir/index.html?query=1"),
        );
        request.auth = Auth::Digest {
            username: "postly".to_owned(),
            password: "local-secret".to_owned(),
        };
        let engine = HttpEngine::new(&EngineOptions::default()).expect("engine");
        let response = engine
            .execute(&request, &VariableContext::default())
            .await
            .expect("Digest response");
        assert_eq!(response.status, 200);
        assert_eq!(response.body_text(), "digest-ok");
        server.await.expect("Digest server");
    }

    #[test]
    fn matches_the_rfc7616_md5_digest_vector() {
        let challenge = DigestChallenge {
            realm: "testrealm@host.com".to_owned(),
            nonce: "dcd98b7102dd2f0e8b11d0f600bfb0c093".to_owned(),
            opaque: Some("5ccc069c403ebaf9f0171e9517f40e41".to_owned()),
            algorithm: DigestAlgorithm::Md5,
            qop: Some(DigestQop::Auth),
        };
        let authorization = build_digest_authorization_with_cnonce(
            "GET",
            "/dir/index.html",
            &[],
            "Mufasa",
            "Circle Of Life",
            &challenge,
            "0a4f113b",
        )
        .expect("Digest authorization");
        assert!(authorization.contains("response=\"6629fae49393a05397450978507c4ef1\""));
        assert!(authorization.contains("algorithm=MD5"));
    }

    #[test]
    fn parses_digest_qop_and_sha256_challenges() {
        let challenge = parse_digest_challenge(
            "Digest realm=\"local\", nonce=\"n\", algorithm=SHA-256, qop=\"auth-int,auth\"",
        )
        .expect("Digest challenge");
        assert_eq!(challenge.algorithm, DigestAlgorithm::Sha256);
        assert_eq!(challenge.qop, Some(DigestQop::Auth));
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
    async fn exchanges_oauth_authorization_code_through_a_loopback_browser_callback() {
        let token_listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("token listener");
        let token_address = token_listener.local_addr().expect("token address");
        let api_listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("API listener");
        let api_address = api_listener.local_addr().expect("API address");
        let token_server = tokio::spawn(async move {
            let (mut token_socket, _) = token_listener.accept().await.expect("token connection");
            let token_request = read_request_headers(&mut token_socket).await;
            assert!(token_request.contains("POST /oauth/token HTTP/1.1"));
            let token_body = token_request
                .split_once("\r\n\r\n")
                .map(|(_, body)| body)
                .expect("token request body");
            let fields = url::form_urlencoded::parse(token_body.as_bytes())
                .into_owned()
                .collect::<std::collections::HashMap<_, _>>();
            assert_eq!(
                fields.get("grant_type").map(String::as_str),
                Some("authorization_code")
            );
            assert_eq!(
                fields.get("client_id").map(String::as_str),
                Some("postly-browser")
            );
            assert_eq!(fields.get("code").map(String::as_str), Some("browser-code"));
            assert!(fields.get("redirect_uri").is_some_and(|value| value
                .starts_with("http://127.0.0.1:")
                && value.ends_with("/callback")));
            assert!(fields
                .get("code_verifier")
                .is_some_and(|value| value.len() == 64));
            let token_body =
                br#"{"access_token":"browser-access","token_type":"Bearer","expires_in":3600}"#;
            let token_response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                token_body.len(),
                String::from_utf8_lossy(token_body)
            );
            token_socket
                .write_all(token_response.as_bytes())
                .await
                .expect("token response");
        });
        let api_server = tokio::spawn(async move {
            let (mut api_socket, _) = api_listener.accept().await.expect("API connection");
            let api_request = read_request_headers(&mut api_socket).await;
            assert!(api_request.contains("GET /browser-profile HTTP/1.1"));
            assert!(api_request
                .to_ascii_lowercase()
                .contains("authorization: bearer browser-access"));
            api_socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 15\r\nconnection: close\r\n\r\nbrowser-profile",
                )
                .await
                .expect("API response");
        });

        let callback_task = Arc::new(std::sync::Mutex::new(None));
        let callback_task_for_opener = Arc::clone(&callback_task);
        let opener = move |authorization_url: &str| {
            let authorization = Url::parse(authorization_url).expect("authorization URL");
            assert_eq!(
                authorization
                    .query_pairs()
                    .find(|(key, _)| key == "response_type")
                    .map(|(_, value)| value),
                Some("code".into())
            );
            assert_eq!(
                authorization
                    .query_pairs()
                    .find(|(key, _)| key == "code_challenge_method")
                    .map(|(_, value)| value),
                Some("S256".into())
            );
            let state = authorization
                .query_pairs()
                .find(|(key, _)| key == "state")
                .map(|(_, value)| value.into_owned())
                .expect("OAuth state");
            let redirect = authorization
                .query_pairs()
                .find(|(key, _)| key == "redirect_uri")
                .map(|(_, value)| Url::parse(&value).expect("redirect URI"))
                .expect("redirect URI");
            let port = redirect.port().expect("dynamic callback port");
            assert_ne!(port, 0);
            assert_eq!(redirect.path(), "/callback");
            assert!(authorization
                .query_pairs()
                .any(|(key, value)| key == "scope" && value == "read:profile"));

            let callback_path = redirect.path().to_owned();
            let callback_task = tokio::spawn(async move {
                let mut socket = TcpStream::connect(("127.0.0.1", port))
                    .await
                    .expect("callback connection");
                let request = format!(
                    "GET {callback_path}?code=browser-code&state={state} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
                );
                socket
                    .write_all(request.as_bytes())
                    .await
                    .expect("callback request");
                let mut response = Vec::new();
                socket
                    .read_to_end(&mut response)
                    .await
                    .expect("callback response");
                assert!(String::from_utf8_lossy(&response).contains("Authorization received"));
            });
            *callback_task_for_opener.lock().expect("callback lock") = Some(callback_task);
            Ok(())
        };

        let mut request = Request::new(
            "Browser PKCE request",
            "GET",
            format!("http://{api_address}/browser-profile"),
        );
        request.auth = Auth::OAuth2AuthorizationCodePkce {
            authorization_url: "https://auth.example.test/authorize".to_owned(),
            token_url: format!("http://{token_address}/oauth/token"),
            client_id: "postly-browser".to_owned(),
            redirect_uri: "http://127.0.0.1:0/callback".to_owned(),
            code: String::new(),
            code_verifier: String::new(),
            client_secret: None,
            scope: Some("read:profile".to_owned()),
        };
        let engine = HttpEngine::new(&EngineOptions::default()).expect("engine");
        let response = engine
            .execute_with_pkce_browser(&request, &VariableContext::default(), opener)
            .await
            .expect("browser PKCE response");
        assert_eq!(response.status, 200);
        assert_eq!(response.body_text(), "browser-profile");
        let callback_task = callback_task.lock().expect("callback lock").take();
        if let Some(callback_task) = callback_task {
            callback_task.await.expect("callback task");
        }
        token_server.await.expect("token server");
        api_server.await.expect("API server");
    }

    #[tokio::test]
    async fn signs_aws_signature_v4_requests_with_runtime_headers() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("AWS connection");
            let request = read_request_headers(&mut socket).await;
            assert!(request.contains("POST /iam HTTP/1.1"));
            let lowercase = request.to_ascii_lowercase();
            assert!(lowercase.contains("authorization: aws4-hmac-sha256"));
            assert!(lowercase.contains("credential=akidexample/"));
            assert!(lowercase.contains("/us-east-1/iam/aws4_request"));
            assert!(lowercase
                .contains("signedheaders=content-type;host;x-amz-date;x-amz-security-token"));
            assert!(lowercase.contains("x-amz-security-token: session-token"));
            assert!(lowercase.contains("x-amz-date: 20"));
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{\"ok\":true}",
                )
                .await
                .expect("AWS response");
        });
        let mut request = Request::new("AWS request", "POST", format!("http://{address}/iam"));
        request.headers.push(HeaderEntry::enabled(
            "Content-Type",
            "application/x-www-form-urlencoded",
        ));
        request.body = RequestBody::Raw {
            text: "Action=ListUsers&Version=2010-05-08".to_owned(),
            content_type: None,
        };
        request.auth = Auth::AwsSignatureV4 {
            access_key_id: "AKIDEXAMPLE".to_owned(),
            secret_access_key: "secret-key".to_owned(),
            region: "us-east-1".to_owned(),
            service: "iam".to_owned(),
            session_token: Some("session-token".to_owned()),
        };
        let engine = HttpEngine::new(&EngineOptions::default()).expect("engine");
        let response = engine
            .execute(&request, &VariableContext::default())
            .await
            .expect("AWS response");
        assert_eq!(response.status, 200);
        assert_eq!(response.body_text(), "{\"ok\":true}");
        server.await.expect("AWS server");
    }

    #[test]
    fn computes_hmac_sha256_reference_vector() {
        assert_eq!(
            hex_bytes(&hmac_sha256(
                b"key",
                b"The quick brown fox jumps over the lazy dog"
            )),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn matches_aws_s3_signature_calculation_example() {
        let mut request = reqwest::Client::new()
            .get("https://examplebucket.s3.amazonaws.com/test.txt")
            .header("Range", "bytes=0-9")
            .header(
                "x-amz-content-sha256",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            )
            .build()
            .expect("request");
        let auth = Auth::AwsSignatureV4 {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_owned(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_owned(),
            region: "us-east-1".to_owned(),
            service: "s3".to_owned(),
            session_token: None,
        };
        let timestamp = chrono::DateTime::parse_from_rfc3339("2013-05-24T00:00:00Z")
            .expect("timestamp")
            .with_timezone(&Utc);
        sign_aws_request_at(&mut request, &auth, &VariableContext::default(), timestamp)
            .expect("signed request");
        assert_eq!(
            request.headers().get("authorization").unwrap(),
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[tokio::test]
    async fn exchanges_oauth_refresh_token() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut token_socket, _) = listener.accept().await.expect("token connection");
            let token_request = read_request_headers(&mut token_socket).await;
            assert!(token_request.contains("POST /oauth/token HTTP/1.1"));
            assert!(token_request.contains("grant_type=refresh_token"));
            assert!(token_request.contains("client_id=postly-refresh"));
            assert!(token_request.contains("refresh_token=refresh-123"));
            let token_body =
                br#"{"access_token":"refresh-access","token_type":"Bearer","expires_in":3600}"#;
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
            assert!(api_request.contains("GET /account HTTP/1.1"));
            assert!(api_request
                .to_ascii_lowercase()
                .contains("authorization: bearer refresh-access"));
            api_socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 7\r\nconnection: close\r\n\r\naccount",
                )
                .await
                .expect("API response");
        });

        let mut request = Request::new(
            "Refresh request",
            "GET",
            format!("http://{address}/account"),
        );
        request.auth = Auth::OAuth2RefreshToken {
            token_url: format!("http://{address}/oauth/token"),
            client_id: "postly-refresh".to_owned(),
            refresh_token: "refresh-123".to_owned(),
            client_secret: None,
            scope: Some("read:account".to_owned()),
        };
        let engine = HttpEngine::new(&EngineOptions::default()).expect("engine");
        let response = engine
            .execute(&request, &VariableContext::default())
            .await
            .expect("refresh response");
        assert_eq!(response.status, 200);
        assert_eq!(response.body_text(), "account");
        server.await.expect("refresh server");
    }

    #[tokio::test]
    async fn exchanges_oauth_device_code_after_user_approval() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut device_socket, _) = listener.accept().await.expect("device connection");
            let device_request = read_request_headers(&mut device_socket).await;
            assert!(device_request.contains("POST /oauth/device HTTP/1.1"));
            assert!(device_request.contains("client_id=postly-device"));
            assert!(device_request.contains("scope=read%3Ausers"));
            let device_body = br#"{"device_code":"device-secret","user_code":"ABCD-EFGH","verification_uri":"https://auth.example.test/device","verification_uri_complete":"https://auth.example.test/device?user_code=ABCD-EFGH","expires_in":10,"interval":1}"#;
            let device_response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                device_body.len(),
                String::from_utf8_lossy(device_body)
            );
            device_socket
                .write_all(device_response.as_bytes())
                .await
                .expect("device response");

            let (mut pending_socket, _) = listener.accept().await.expect("pending connection");
            let pending_request = read_request_headers(&mut pending_socket).await;
            assert!(pending_request
                .contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"));
            assert!(pending_request.contains("device_code=device-secret"));
            let pending_body = br#"{"error":"authorization_pending"}"#;
            let pending_response = format!(
                "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                pending_body.len(),
                String::from_utf8_lossy(pending_body)
            );
            pending_socket
                .write_all(pending_response.as_bytes())
                .await
                .expect("pending response");

            let (mut token_socket, _) = listener.accept().await.expect("token connection");
            let token_request = read_request_headers(&mut token_socket).await;
            assert!(token_request.contains("device_code=device-secret"));
            let token_body =
                br#"{"access_token":"device-access","token_type":"Bearer","expires_in":3600}"#;
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
            assert!(api_request.contains("GET /device-protected HTTP/1.1"));
            assert!(api_request
                .to_ascii_lowercase()
                .contains("authorization: bearer device-access"));
            api_socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 9\r\nconnection: close\r\n\r\ndevice-ok",
                )
                .await
                .expect("API response");
        });

        let auth = Auth::OAuth2DeviceCode {
            device_authorization_url: format!("http://{address}/oauth/device"),
            token_url: format!("http://{address}/oauth/token"),
            client_id: "postly-device".to_owned(),
            client_secret: None,
            scope: Some("read:users".to_owned()),
        };
        let mut request = Request::new(
            "Device request",
            "GET",
            format!("http://{address}/device-protected"),
        );
        request.auth = auth;
        let engine = HttpEngine::new(&EngineOptions::default()).expect("engine");
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let observed_prompts = Arc::clone(&prompts);
        let response = engine
            .execute_with_device_code_prompt(&request, &VariableContext::default(), move |prompt| {
                observed_prompts
                    .lock()
                    .expect("prompt lock")
                    .push(prompt.clone())
            })
            .await
            .expect("device-code response");

        server.await.expect("device-code server");
        assert_eq!(response.body_text(), "device-ok");
        assert_eq!(
            prompts.lock().expect("prompt lock").as_slice(),
            &[OAuthDeviceCodePrompt {
                user_code: "ABCD-EFGH".to_owned(),
                verification_uri: "https://auth.example.test/device".to_owned(),
                verification_uri_complete: Some(
                    "https://auth.example.test/device?user_code=ABCD-EFGH".to_owned(),
                ),
                expires_in: Duration::from_secs(10),
                interval: Duration::from_secs(1),
            }]
        );
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
            tokio::time::sleep(Duration::from_millis(10)).await;
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 28\r\n\r\n",
                )
                .await
                .expect("write headers");
            tokio::time::sleep(Duration::from_millis(10)).await;
            socket
                .write_all(b"{\"ok\":true,\"source\":\"local\"}")
                .await
                .expect("write body");
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
        assert!(response.ttfb_ms >= 5, "TTFB was {} ms", response.ttfb_ms);
        assert!(
            response.download_ms >= 5,
            "download was {} ms",
            response.download_ms
        );
        assert!(response.ttfb_ms <= response.duration_ms);
        assert!(response.download_ms <= response.duration_ms);
    }

    #[tokio::test]
    async fn honors_the_configured_http_redirect_limit() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.expect("initial connection");
            assert!(read_request_headers(&mut first)
                .await
                .contains("GET /start HTTP/1.1"));
            first
                .write_all(
                    b"HTTP/1.1 302 Found\r\nlocation: /final\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await
                .expect("redirect response");

            let (mut second, _) = listener.accept().await.expect("redirected connection");
            assert!(read_request_headers(&mut second)
                .await
                .contains("GET /final HTTP/1.1"));
            second
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-length: 10\r\nconnection: close\r\n\r\nredirected",
                )
                .await
                .expect("final response");
        });

        let request = Request::new("Redirect", "GET", format!("http://{address}/start"));
        let engine = HttpEngine::new(&EngineOptions {
            max_redirects: 1,
            ..EngineOptions::default()
        })
        .expect("redirect engine");
        let response = engine
            .execute(&request, &VariableContext::default())
            .await
            .expect("redirected response");
        server.await.expect("redirect server");
        assert_eq!(response.status, 200);
        assert_eq!(response.body_text(), "redirected");

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("connection");
            assert!(read_request_headers(&mut socket)
                .await
                .contains("GET /start HTTP/1.1"));
            socket
                .write_all(
                    b"HTTP/1.1 302 Found\r\nlocation: /final\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await
                .expect("redirect response");
        });
        let engine = HttpEngine::new(&EngineOptions {
            max_redirects: 0,
            ..EngineOptions::default()
        })
        .expect("non-redirecting engine");
        let response = engine
            .execute(
                &Request::new("No redirect", "GET", format!("http://{address}/start")),
                &VariableContext::default(),
            )
            .await
            .expect("stopped redirect response");
        server.await.expect("non-redirecting server");
        assert_eq!(response.status, 302);

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("connection");
            assert!(read_request_headers(&mut socket)
                .await
                .contains("GET /start HTTP/1.1"));
            socket
                .write_all(
                    b"HTTP/1.1 302 Found\r\nlocation: /final\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await
                .expect("redirect response");
        });
        let mut request = Request::new(
            "Per-request no redirect",
            "GET",
            format!("http://{address}/start"),
        );
        request.transport = Some(RequestTransportSettings {
            follow_redirects: Some(false),
            ..RequestTransportSettings::default()
        });
        let engine = HttpEngine::new(&EngineOptions::default()).expect("default engine");
        let response = engine
            .execute(&request, &VariableContext::default())
            .await
            .expect("per-request redirect response");
        server.await.expect("per-request redirect server");
        assert_eq!(response.status, 302);
    }

    #[test]
    fn deserializes_legacy_responses_without_timing_breakdown() {
        let response: HttpResponse = serde_json::from_value(serde_json::json!({
            "status": 200,
            "status_text": "OK",
            "headers": [],
            "body": [],
            "response_size": 0,
            "content_type": null,
            "duration_ms": 4,
            "protocol": "HTTP/1.1",
            "url": "http://example.test",
            "cookies": []
        }))
        .expect("legacy response");
        assert_eq!(response.duration_ms, 4);
        assert_eq!(response.ttfb_ms, 0);
        assert_eq!(response.download_ms, 0);
    }

    #[tokio::test]
    async fn rejects_a_response_that_exceeds_the_configured_body_limit() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("connection");
            let mut request = [0_u8; 1024];
            let _ = socket.read(&mut request).await.expect("read");
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 16\r\n\r\n0123456789abcdef",
                )
                .await
                .expect("write");
        });

        let engine = HttpEngine::new(&EngineOptions {
            max_response_bytes: 8,
            ..EngineOptions::default()
        })
        .expect("engine");
        let request = Request::new("Oversized", "GET", format!("http://{address}/oversized"));
        let error = engine
            .execute(&request, &VariableContext::default())
            .await
            .expect_err("oversized response must be rejected");

        server.await.expect("server");
        assert!(matches!(
            error,
            HttpError::ResponseBodyTooLarge { limit: 8 }
        ));
        assert!(error.to_string().contains("8 bytes"));
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
            ttfb_ms: 0,
            download_ms: 0,
        };
        assert_eq!(
            response.formatted_body(ResponseView::Pretty),
            "<root>\n  <item id=\"1\">one</item>\n  <item>two</item>\n</root>"
        );
    }

    #[test]
    fn formats_yaml_in_pretty_view() {
        let response = HttpResponse {
            status: 200,
            status_text: "OK".to_owned(),
            headers: Vec::new(),
            cookies: Vec::new(),
            body: b"service: postly\nfeatures:\n- local\n- private\n".to_vec(),
            response_size: 46,
            content_type: Some("application/yaml; charset=utf-8".to_owned()),
            protocol: "HTTP/1.1".to_owned(),
            url: "http://example.test".to_owned(),
            duration_ms: 1,
            ttfb_ms: 0,
            download_ms: 0,
        };
        assert_eq!(
            response.formatted_body(ResponseView::Pretty),
            "service: postly\nfeatures:\n- local\n- private"
        );
    }

    #[test]
    fn formats_html_and_javascript_in_pretty_view() {
        let html = HttpResponse {
            status: 200,
            status_text: "OK".to_owned(),
            headers: Vec::new(),
            cookies: Vec::new(),
            body: b"<main><h1>Postly</h1><br/><p>Local first</p></main>".to_vec(),
            response_size: 52,
            content_type: Some("text/html".to_owned()),
            protocol: "HTTP/1.1".to_owned(),
            url: "http://example.test".to_owned(),
            duration_ms: 1,
            ttfb_ms: 0,
            download_ms: 0,
        };
        assert_eq!(
            html.formatted_body(ResponseView::Pretty),
            "<main>\n  <h1>\n    Postly\n  </h1>\n  <br/>\n  <p>\n    Local first\n  </p>\n</main>"
        );

        let javascript = HttpResponse {
            status: 200,
            status_text: "OK".to_owned(),
            headers: Vec::new(),
            cookies: Vec::new(),
            body: b"const answer = {ok: true}; function run() { return answer; }".to_vec(),
            response_size: 59,
            content_type: Some("text/javascript".to_owned()),
            protocol: "HTTP/1.1".to_owned(),
            url: "http://example.test".to_owned(),
            duration_ms: 1,
            ttfb_ms: 0,
            download_ms: 0,
        };
        assert_eq!(
            javascript.formatted_body(ResponseView::Pretty),
            "const answer = {ok: true};\nfunction run() {\n  return answer;\n}"
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

    #[tokio::test]
    async fn connects_a_stream_through_socks5_with_remote_hostname_resolution() {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("proxy listener");
        let address = listener.local_addr().expect("proxy address");
        let proxy = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("proxy connection");
            let mut greeting = [0_u8; 3];
            socket.read_exact(&mut greeting).await.expect("greeting");
            assert_eq!(greeting, [0x05, 0x01, 0x00]);
            socket
                .write_all(&[0x05, 0x00])
                .await
                .expect("greeting reply");

            let mut header = [0_u8; 4];
            socket
                .read_exact(&mut header)
                .await
                .expect("connect header");
            assert_eq!(header, [0x05, 0x01, 0x00, 0x03]);
            let mut length = [0_u8; 1];
            socket
                .read_exact(&mut length)
                .await
                .expect("hostname length");
            let mut hostname = vec![0_u8; length[0] as usize];
            socket.read_exact(&mut hostname).await.expect("hostname");
            let mut port = [0_u8; 2];
            socket.read_exact(&mut port).await.expect("port");
            assert_eq!(hostname, b"api.example.test");
            assert_eq!(u16::from_be_bytes(port), 443);
            socket
                .write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0, 0])
                .await
                .expect("connect reply");
        });
        let proxy_url = url::Url::parse(&format!("socks5h://{address}")).expect("proxy URL");
        let socket = connect_socks5_stream(&proxy_url, "api.example.test", 443)
            .await
            .expect("SOCKS stream");
        drop(socket);
        proxy.await.expect("proxy task");
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
        let snapshot = engine.cookie_snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].name, "session");
        assert_eq!(snapshot[0].value, "abc");
        assert_eq!(snapshot[0].path, "/");
        assert!(snapshot[0].http_only);
        assert_eq!(snapshot[0].same_site.as_deref(), Some("Lax"));

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
    async fn disables_the_cookie_jar_for_one_request() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.expect("first connection");
            let first_request = read_request_headers(&mut first).await;
            assert!(first_request.contains("GET /login HTTP/1.1"));
            first
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nset-cookie: session=abc; Path=/\r\nconnection: close\r\n\r\n",
                )
                .await
                .expect("first response");
            drop(first);

            let (mut second, _) = listener.accept().await.expect("second connection");
            let second_request = read_request_headers(&mut second).await;
            assert!(!second_request.to_ascii_lowercase().contains("cookie:"));
            second
                .write_all(
                    b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await
                .expect("second response");
        });

        let engine = HttpEngine::new(&EngineOptions::default()).expect("engine");
        let login = Request::new("Login", "GET", format!("http://{address}/login"));
        engine
            .execute(&login, &VariableContext::default())
            .await
            .expect("login response");

        let mut request = Request::new("No cookies", "GET", format!("http://{address}/next"));
        request.transport = Some(RequestTransportSettings {
            disable_cookies: true,
            ..RequestTransportSettings::default()
        });
        let response = engine
            .execute(&request, &VariableContext::default())
            .await
            .expect("cookie-disabled response");
        server.await.expect("cookie-disabled server");
        assert_eq!(response.status, 204);
        assert_eq!(
            engine
                .cookie_header(&format!("http://{address}/next"))
                .expect("stored cookie"),
            Some("session=abc".to_owned())
        );
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
        assert_eq!(reopened.cookie_snapshot().len(), 1);
        reopened.clear_cookies().expect("clear cookie jar");
        assert!(reopened.cookie_snapshot().is_empty());
        assert_eq!(
            reopened
                .cookie_header("https://example.test/api/users")
                .expect("cleared cookie header"),
            None
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
