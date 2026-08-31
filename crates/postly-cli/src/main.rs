use std::{
    fs,
    future::Future,
    io::{self, BufRead, BufReader, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    pin::Pin,
    task::{Context as TaskContext, Poll},
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use base64::Engine;
use clap::{Args, Parser, Subcommand, ValueEnum};
use futures_util::{SinkExt, StreamExt};
use hyper_util::rt::TokioIo;
use postly_core::{
    connect_socks5_stream, evaluate_response_assertions, export_openapi_collection,
    export_postman_collection, export_postman_environment_with_store, generate_code_snippet,
    generate_markdown_docs, import_curl_command, import_dotenv, import_environment_with_store,
    import_postman_collection, message_from_json, message_to_json, parse_graphql_response,
    parse_graphql_schema, parse_variables_json, run_requests, schema_introspection_query, Auth,
    CancellationToken, Collection, EngineOptions, Environment, EnvironmentVariable, GraphqlRequest,
    GrpcSchema, HeaderEntry, HistoryEntry, HistoryFilter, HistoryOutcome, HttpEngine, Request,
    RequestBody, ResponseExample, ResponseExampleCookie, RunnerOptions, ScriptResult,
    ScriptTestResult, SecretStore, SnippetLanguage, SseParser, VariableContext, Workspace,
};
use prost::Message as ProstMessage;
use prost_reflect::{DynamicMessage, MessageDescriptor};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::{
    client_async_tls_with_config, connect_async_tls_with_config,
    tungstenite::{
        client::IntoClientRequest,
        http::{header::HeaderName, HeaderValue},
        Message,
    },
    Connector,
};
use tracing_subscriber::EnvFilter;

fn parse_concurrency(value: &str) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|error| format!("concurrency must be a positive integer: {error}"))?;
    if (1..=64).contains(&parsed) {
        Ok(parsed)
    } else {
        Err("concurrency must be between 1 and 64".to_owned())
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "postly",
    version,
    about = "The Postman alternative without an account."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

struct ImmediateRequestOptions {
    url: String,
    method: String,
    query: Vec<String>,
    headers: Vec<String>,
    data: Option<String>,
    json_body: Option<String>,
    bearer: Option<String>,
    basic_user: Option<String>,
    basic_password: Option<String>,
    digest_user: Option<String>,
    digest_password: Option<String>,
    oauth_token_url: Option<String>,
    oauth_client_id: Option<String>,
    oauth_client_secret: Option<String>,
    oauth_scope: Option<String>,
    oauth_authorization_url: Option<String>,
    oauth_device_authorization_url: Option<String>,
    oauth_redirect_uri: Option<String>,
    oauth_code: Option<String>,
    oauth_code_verifier: Option<String>,
    oauth_refresh_token: Option<String>,
    oauth_browser: bool,
    aws_access_key_id: Option<String>,
    aws_secret_access_key: Option<String>,
    aws_region: Option<String>,
    aws_service: Option<String>,
    aws_session_token: Option<String>,
    timeout: u64,
    max_redirects: usize,
    proxy: Option<String>,
    no_proxy: Option<String>,
    ca_cert: Option<PathBuf>,
    client_identity: Option<PathBuf>,
    insecure: bool,
    output_json: bool,
}

struct GraphqlOptions {
    endpoint: String,
    query: Option<String>,
    query_file: Option<PathBuf>,
    variables: Vec<String>,
    variables_json: Option<String>,
    operation_name: Option<String>,
    headers: Vec<String>,
    bearer: Option<String>,
    basic_user: Option<String>,
    basic_password: Option<String>,
    timeout: u64,
    max_redirects: usize,
    proxy: Option<String>,
    no_proxy: Option<String>,
    ca_cert: Option<PathBuf>,
    client_identity: Option<PathBuf>,
    insecure: bool,
    output_json: bool,
}

struct GrpcCallOptions {
    endpoint: String,
    proto: PathBuf,
    includes: Vec<PathBuf>,
    method: String,
    message: Option<String>,
    message_file: Option<PathBuf>,
    metadata: Vec<String>,
    bearer: Option<String>,
    basic_user: Option<String>,
    basic_password: Option<String>,
    timeout: u64,
    proxy: Option<String>,
    no_proxy: Option<String>,
    ca_cert: Option<PathBuf>,
    client_identity: Option<PathBuf>,
    output_json: bool,
}

fn is_pkcs12_identity_path(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        let extension = extension.to_string_lossy();
        extension.eq_ignore_ascii_case("p12") || extension.eq_ignore_ascii_case("pfx")
    })
}

struct GrpcIdentityPem {
    certificate: Vec<u8>,
    private_key: Vec<u8>,
}

fn load_grpc_pkcs12_identity(path: &Path, passphrase: &str) -> Result<GrpcIdentityPem> {
    let der = fs::read(path)
        .with_context(|| format!("could not read gRPC client identity {}", path.display()))?;
    if der.is_empty() {
        bail!("gRPC client identity {} is empty", path.display());
    }
    let archive = openssl::pkcs12::Pkcs12::from_der(&der)
        .with_context(|| format!("invalid PKCS#12 gRPC client identity {}", path.display()))?;
    let parsed = archive.parse2(passphrase).with_context(|| {
        format!(
            "could not unlock PKCS#12 gRPC client identity {}",
            path.display()
        )
    })?;
    let certificate = parsed
        .cert
        .context("PKCS#12 gRPC client identity has no certificate")?;
    let private_key = parsed
        .pkey
        .context("PKCS#12 gRPC client identity has no private key")?;
    let mut certificate_pem = certificate.to_pem()?;
    if let Some(chain) = parsed.ca {
        for certificate in chain {
            certificate_pem.extend(certificate.to_pem()?);
        }
    }
    Ok(GrpcIdentityPem {
        certificate: certificate_pem,
        private_key: private_key.private_key_to_pem_pkcs8()?,
    })
}

fn configure_grpc_endpoint(
    endpoint_value: &str,
    timeout: u64,
    ca_cert: Option<&Path>,
    client_identity: Option<&Path>,
) -> Result<tonic::transport::Endpoint> {
    let passphrase = client_identity_passphrase(client_identity);
    configure_grpc_endpoint_with_passphrase(
        endpoint_value,
        timeout,
        ca_cert,
        client_identity,
        passphrase.as_deref(),
    )
}

fn configure_grpc_endpoint_with_passphrase(
    endpoint_value: &str,
    timeout: u64,
    ca_cert: Option<&Path>,
    client_identity: Option<&Path>,
    passphrase: Option<&str>,
) -> Result<tonic::transport::Endpoint> {
    let endpoint_url = url::Url::parse(endpoint_value)
        .with_context(|| format!("invalid gRPC endpoint: {endpoint_value}"))?;
    let mut endpoint = tonic::transport::Endpoint::from_shared(endpoint_value.to_owned())?
        .timeout(Duration::from_secs(timeout));
    match endpoint_url.scheme() {
        "http" => {
            if ca_cert.is_some() || client_identity.is_some() {
                bail!("gRPC CA and client identity options require an https:// endpoint");
            }
        }
        "https" => {
            let domain = endpoint_url
                .host_str()
                .context("HTTPS gRPC endpoint has no hostname")?;
            if let Some(path) = client_identity.filter(|path| is_pkcs12_identity_path(path)) {
                let passphrase = passphrase.context(format!(
                    "set POSTLY_CLIENT_IDENTITY_PASSPHRASE for PKCS#12 gRPC identity {}",
                    path.display()
                ))?;
                let identity = load_grpc_pkcs12_identity(path, passphrase)?;
                let mut tls = tonic::transport::ClientTlsConfig::new()
                    .domain_name(domain)
                    .with_webpki_roots();
                if let Some(ca_path) = ca_cert {
                    let pem = fs::read(ca_path).with_context(|| {
                        format!("could not read gRPC CA certificate {}", ca_path.display())
                    })?;
                    if pem.is_empty() {
                        bail!("gRPC CA certificate {} is empty", ca_path.display());
                    }
                    tls = tls.ca_certificate(tonic::transport::Certificate::from_pem(pem));
                }
                tls = tls.identity(tonic::transport::Identity::from_pem(
                    identity.certificate,
                    identity.private_key,
                ));
                endpoint = endpoint.tls_config(tls)?;
                return Ok(endpoint);
            }
            let mut tls = tonic::transport::ClientTlsConfig::new()
                .domain_name(domain)
                .with_webpki_roots();
            if let Some(path) = ca_cert {
                let pem = fs::read(path).with_context(|| {
                    format!("could not read gRPC CA certificate {}", path.display())
                })?;
                if pem.is_empty() {
                    bail!("gRPC CA certificate {} is empty", path.display());
                }
                tls = tls.ca_certificate(tonic::transport::Certificate::from_pem(pem));
            }
            if let Some(path) = client_identity {
                let pem = fs::read(path).with_context(|| {
                    format!("could not read gRPC client identity {}", path.display())
                })?;
                if pem.is_empty() {
                    bail!("gRPC client identity {} is empty", path.display());
                }
                tls = tls.identity(tonic::transport::Identity::from_pem(&pem, &pem));
            }
            endpoint = endpoint.tls_config(tls)?;
        }
        scheme => bail!("gRPC endpoint must use http:// or https://, got {scheme}://"),
    }
    Ok(endpoint)
}

fn client_identity_passphrase(path: Option<&Path>) -> Option<String> {
    let is_pkcs12 = path.and_then(Path::extension).is_some_and(|extension| {
        let extension = extension.to_string_lossy();
        extension.eq_ignore_ascii_case("p12") || extension.eq_ignore_ascii_case("pfx")
    });
    is_pkcs12
        .then(|| std::env::var("POSTLY_CLIENT_IDENTITY_PASSPHRASE").ok())
        .flatten()
}

#[derive(Clone)]
struct GrpcProxyConnector {
    proxy_host: String,
    proxy_port: u16,
    proxy_authorization: Option<String>,
    socks_proxy: Option<String>,
}

impl tonic::codegen::Service<http::Uri> for GrpcProxyConnector {
    type Response = TokioIo<tokio::net::TcpStream>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: http::Uri) -> Self::Future {
        let proxy_host = self.proxy_host.clone();
        let proxy_port = self.proxy_port;
        let proxy_authorization = self.proxy_authorization.clone();
        let socks_proxy = self.socks_proxy.clone();
        Box::pin(async move {
            let target_host = uri.host().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "gRPC URI has no hostname")
            })?;
            let target_port = uri
                .port_u16()
                .or_else(|| (uri.scheme_str() == Some("https")).then_some(443))
                .or_else(|| (uri.scheme_str() == Some("http")).then_some(80))
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "gRPC URI has no port")
                })?;
            if let Some(socks_proxy) = socks_proxy {
                let proxy = url::Url::parse(&socks_proxy).map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("invalid gRPC SOCKS proxy URL: {error}"),
                    )
                })?;
                let socket = connect_socks5_stream(&proxy, target_host, target_port)
                    .await
                    .map_err(io::Error::other)?;
                return Ok(TokioIo::new(socket));
            }
            let target_authority = if target_host.contains(':') {
                format!("[{target_host}]:{target_port}")
            } else {
                format!("{target_host}:{target_port}")
            };
            let mut socket =
                tokio::net::TcpStream::connect((proxy_host.as_str(), proxy_port)).await?;
            let mut connect_request =
                format!("CONNECT {target_authority} HTTP/1.1\r\nHost: {target_authority}\r\n");
            if let Some(proxy_authorization) = proxy_authorization {
                connect_request.push_str(&format!(
                    "Proxy-Authorization: Basic {proxy_authorization}\r\n"
                ));
            }
            connect_request.push_str("\r\n");
            socket.write_all(connect_request.as_bytes()).await?;

            let mut response = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !response.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = socket.read(&mut buffer).await?;
                if count == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "gRPC proxy closed the CONNECT handshake",
                    ));
                }
                if response.len().saturating_add(count) > 64 * 1024 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "gRPC proxy response exceeds 65536 bytes",
                    ));
                }
                response.extend_from_slice(&buffer[..count]);
            }
            let status = String::from_utf8_lossy(&response)
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|value| value.parse::<u16>().ok())
                .unwrap_or_default();
            if status != 200 {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    format!("gRPC proxy CONNECT failed with HTTP {status}"),
                ));
            }
            Ok(TokioIo::new(socket))
        })
    }
}

async fn connect_grpc_endpoint(
    endpoint: tonic::transport::Endpoint,
    endpoint_value: &str,
    proxy_url: Option<&str>,
    no_proxy: Option<&str>,
) -> Result<tonic::transport::Channel> {
    let Some(proxy_url) = proxy_url.filter(|value| !value.trim().is_empty()) else {
        return Ok(endpoint.connect().await?);
    };
    let target = url::Url::parse(endpoint_value)
        .with_context(|| format!("invalid gRPC endpoint: {endpoint_value}"))?;
    let target_host = target.host_str().context("gRPC endpoint has no hostname")?;
    let target_port = target
        .port_or_known_default()
        .context("gRPC endpoint has no port")?;
    if no_proxy.is_some_and(|rules| no_proxy_matches(target_host, target_port, rules)) {
        return Ok(endpoint.connect().await?);
    }

    let proxy = url::Url::parse(proxy_url)
        .with_context(|| format!("invalid gRPC proxy URL: {proxy_url}"))?;
    if !matches!(proxy.scheme(), "http" | "socks5" | "socks5h") {
        bail!(
            "gRPC proxy routing supports http://, socks5:// and socks5h:// proxies; {} is not supported",
            proxy.scheme()
        );
    }
    if matches!(proxy.scheme(), "socks5" | "socks5h") {
        return Ok(endpoint
            .connect_with_connector(GrpcProxyConnector {
                proxy_host: String::new(),
                proxy_port: 0,
                proxy_authorization: None,
                socks_proxy: Some(proxy.to_string()),
            })
            .await?);
    }
    let proxy_host = proxy
        .host_str()
        .context("gRPC proxy URL has no hostname")?
        .to_owned();
    let proxy_port = proxy
        .port_or_known_default()
        .context("gRPC proxy URL has no port")?;
    let proxy_authorization = (!proxy.username().is_empty()).then(|| {
        let credentials = if let Some(password) = proxy.password() {
            format!("{}:{password}", proxy.username())
        } else {
            format!("{}:", proxy.username())
        };
        base64::engine::general_purpose::STANDARD.encode(credentials)
    });
    endpoint
        .connect_with_connector(GrpcProxyConnector {
            proxy_host,
            proxy_port,
            proxy_authorization,
            socks_proxy: None,
        })
        .await
        .map_err(Into::into)
}

#[derive(Debug, Args)]
struct GrpcTlsArgs {
    #[arg(
        long,
        value_name = "PATH",
        help = "Trust an additional PEM-encoded CA certificate for HTTPS"
    )]
    ca_cert: Option<PathBuf>,
    #[arg(
        long,
        value_name = "PATH",
        help = "Use a combined PEM or password-protected PKCS#12 client identity for HTTPS"
    )]
    client_identity: Option<PathBuf>,
}

#[derive(Clone)]
struct DynamicGrpcCodec {
    output: MessageDescriptor,
}

struct DynamicGrpcEncoder;

struct DynamicGrpcDecoder {
    output: MessageDescriptor,
}

impl tonic::codec::Codec for DynamicGrpcCodec {
    type Encode = DynamicMessage;
    type Decode = DynamicMessage;
    type Encoder = DynamicGrpcEncoder;
    type Decoder = DynamicGrpcDecoder;

    fn encoder(&mut self) -> Self::Encoder {
        DynamicGrpcEncoder
    }

    fn decoder(&mut self) -> Self::Decoder {
        DynamicGrpcDecoder {
            output: self.output.clone(),
        }
    }
}

impl tonic::codec::Encoder for DynamicGrpcEncoder {
    type Item = DynamicMessage;
    type Error = tonic::Status;

    fn encode(
        &mut self,
        item: Self::Item,
        dst: &mut tonic::codec::EncodeBuf<'_>,
    ) -> Result<(), Self::Error> {
        ProstMessage::encode(&item, dst).map_err(|error| {
            tonic::Status::internal(format!("could not encode protobuf message: {error}"))
        })
    }
}

impl tonic::codec::Decoder for DynamicGrpcDecoder {
    type Item = DynamicMessage;
    type Error = tonic::Status;

    fn decode(
        &mut self,
        src: &mut tonic::codec::DecodeBuf<'_>,
    ) -> Result<Option<Self::Item>, Self::Error> {
        DynamicMessage::decode(self.output.clone(), src)
            .map(Some)
            .map_err(|error| {
                tonic::Status::internal(format!("could not decode protobuf message: {error}"))
            })
    }
}

fn parse_grpc_stream_messages(
    descriptor: MessageDescriptor,
    input: &str,
) -> Result<Vec<DynamicMessage>> {
    let values: Vec<serde_json::Value> = serde_json::from_str(input)
        .context("client-streaming gRPC input must be a JSON array of request objects")?;
    values
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            let raw = serde_json::to_string(&value)
                .with_context(|| format!("could not serialize gRPC message {index}"))?;
            message_from_json(descriptor.clone(), &raw)
                .with_context(|| format!("invalid gRPC message at stream index {index}"))
        })
        .collect()
}

fn apply_grpc_metadata<T>(
    request: &mut tonic::Request<T>,
    options: &GrpcCallOptions,
) -> Result<()> {
    for raw in &options.metadata {
        let (key, value) = raw
            .split_once('=')
            .with_context(|| format!("metadata must use key=value syntax: {raw}"))?;
        let key = key.trim().to_ascii_lowercase();
        if key.is_empty() {
            bail!("metadata key cannot be empty");
        }
        let key: tonic::metadata::MetadataKey<tonic::metadata::Ascii> = key
            .parse()
            .with_context(|| format!("invalid gRPC metadata key: {key}"))?;
        let value: tonic::metadata::MetadataValue<tonic::metadata::Ascii> = value
            .parse()
            .with_context(|| format!("invalid ASCII gRPC metadata value for {key}"))?;
        request.metadata_mut().insert(key, value);
    }
    match (
        options.bearer.as_deref(),
        options.basic_user.as_deref(),
        options.basic_password.as_deref(),
    ) {
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
            bail!("choose either --bearer or basic authentication")
        }
        (Some(token), None, None) => {
            request.metadata_mut().insert(
                "authorization",
                format!("Bearer {token}")
                    .parse()
                    .context("invalid bearer token")?,
            );
        }
        (None, Some(user), Some(password)) => {
            let credentials =
                base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
            request.metadata_mut().insert(
                "authorization",
                format!("Basic {credentials}")
                    .parse()
                    .context("invalid basic credentials")?,
            );
        }
        (None, Some(_), None) | (None, None, Some(_)) => {
            bail!("basic authentication requires --basic-user and --basic-password")
        }
        (None, None, None) => {}
    }
    Ok(())
}

struct SseOptions {
    endpoint: String,
    headers: Vec<String>,
    bearer: Option<String>,
    basic_user: Option<String>,
    basic_password: Option<String>,
    timeout: u64,
    max_redirects: usize,
    reconnect: u32,
    proxy: Option<String>,
    no_proxy: Option<String>,
    ca_cert: Option<PathBuf>,
    client_identity: Option<PathBuf>,
    insecure: bool,
    output_json: bool,
}

struct WebsocketOptions {
    endpoint: String,
    send: Vec<String>,
    headers: Vec<String>,
    bearer: Option<String>,
    basic_user: Option<String>,
    basic_password: Option<String>,
    timeout: u64,
    reconnect: u32,
    proxy: Option<String>,
    no_proxy: Option<String>,
    ca_cert: Option<PathBuf>,
    client_identity: Option<PathBuf>,
    insecure: bool,
    output_json: bool,
}

struct RunOptions<'a> {
    path: &'a Path,
    environment_name: Option<&'a str>,
    folder: Option<&'a str>,
    fail_fast: bool,
    scripts: bool,
    concurrency: usize,
    timeout: u64,
    max_redirects: usize,
    proxy: Option<&'a str>,
    no_proxy: Option<&'a str>,
    ca_cert: Option<&'a Path>,
    client_identity: Option<&'a Path>,
    reporter: Reporter,
    data_file: Option<&'a Path>,
}

struct SendOptions<'a> {
    file: &'a Path,
    environment_name: Option<&'a str>,
    scripts: bool,
    timeout: u64,
    max_redirects: usize,
    proxy: Option<&'a str>,
    no_proxy: Option<&'a str>,
    ca_cert: Option<&'a Path>,
    client_identity: Option<&'a Path>,
    insecure: bool,
    output_json: bool,
    oauth_browser: bool,
}

struct ExecuteOptions<'a> {
    timeout: u64,
    max_redirects: usize,
    proxy: Option<&'a str>,
    no_proxy: Option<&'a str>,
    ca_cert: Option<&'a Path>,
    client_identity: Option<&'a Path>,
    insecure: bool,
    cookie_jar: Option<&'a Path>,
    oauth_browser: bool,
}

struct NewRequestOptions {
    workspace: PathBuf,
    collection: String,
    name: String,
    url: String,
    method: String,
    folder: Option<String>,
    query: Vec<String>,
    headers: Vec<String>,
    data: Option<String>,
    json_body: Option<String>,
    bearer: Option<String>,
    basic_user: Option<String>,
    basic_password: Option<String>,
    digest_user: Option<String>,
    digest_password: Option<String>,
    oauth_token_url: Option<String>,
    oauth_client_id: Option<String>,
    oauth_client_secret: Option<String>,
    oauth_scope: Option<String>,
    oauth_authorization_url: Option<String>,
    oauth_device_authorization_url: Option<String>,
    oauth_redirect_uri: Option<String>,
    oauth_code: Option<String>,
    oauth_code_verifier: Option<String>,
    oauth_refresh_token: Option<String>,
    oauth_browser: bool,
    aws_access_key_id: Option<String>,
    aws_secret_access_key: Option<String>,
    aws_region: Option<String>,
    aws_service: Option<String>,
    aws_session_token: Option<String>,
}

#[derive(Debug, Default, Args)]
struct OAuthCliArgs {
    #[arg(long, value_name = "URL", help = "OAuth 2.0 token endpoint")]
    oauth_token_url: Option<String>,
    #[arg(long, value_name = "ID", help = "OAuth 2.0 client ID")]
    oauth_client_id: Option<String>,
    #[arg(long, value_name = "SECRET", help = "OAuth 2.0 client secret")]
    oauth_client_secret: Option<String>,
    #[arg(long, value_name = "SCOPE", help = "Optional OAuth 2.0 scope")]
    oauth_scope: Option<String>,
    #[arg(
        long,
        value_name = "URL",
        help = "OAuth 2.0 authorization endpoint for PKCE"
    )]
    oauth_authorization_url: Option<String>,
    #[arg(
        long,
        value_name = "URL",
        help = "OAuth 2.0 device authorization endpoint"
    )]
    oauth_device_authorization_url: Option<String>,
    #[arg(long, value_name = "URI", help = "OAuth 2.0 redirect URI for PKCE")]
    oauth_redirect_uri: Option<String>,
    #[arg(
        long,
        value_name = "CODE",
        help = "OAuth 2.0 authorization code for PKCE"
    )]
    oauth_code: Option<String>,
    #[arg(long, value_name = "VERIFIER", help = "OAuth 2.0 PKCE code verifier")]
    oauth_code_verifier: Option<String>,
    #[arg(long, value_name = "TOKEN", help = "OAuth 2.0 refresh token")]
    oauth_refresh_token: Option<String>,
    #[arg(long, help = "Open a local browser callback for OAuth 2.0 PKCE")]
    oauth_browser: bool,
    #[arg(long, value_name = "ID", help = "AWS Signature V4 access key ID")]
    aws_access_key_id: Option<String>,
    #[arg(
        long,
        value_name = "SECRET",
        help = "AWS Signature V4 secret access key"
    )]
    aws_secret_access_key: Option<String>,
    #[arg(long, value_name = "REGION", help = "AWS Signature V4 region")]
    aws_region: Option<String>,
    #[arg(long, value_name = "SERVICE", help = "AWS Signature V4 service name")]
    aws_service: Option<String>,
    #[arg(long, value_name = "TOKEN", help = "Optional AWS session token")]
    aws_session_token: Option<String>,
}

struct HistoryOptions {
    limit: usize,
    search: Option<String>,
    method: Option<String>,
    status: Option<u16>,
    errors_only: bool,
    clear: bool,
    output_json: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Reporter {
    Pretty,
    Json,
    Junit,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SnippetLanguageArg {
    Curl,
    Javascript,
    Python,
    Rust,
    Go,
    Java,
    Csharp,
    Php,
}

impl From<SnippetLanguageArg> for SnippetLanguage {
    fn from(language: SnippetLanguageArg) -> Self {
        match language {
            SnippetLanguageArg::Curl => Self::Curl,
            SnippetLanguageArg::Javascript => Self::Javascript,
            SnippetLanguageArg::Python => Self::Python,
            SnippetLanguageArg::Rust => Self::Rust,
            SnippetLanguageArg::Go => Self::Go,
            SnippetLanguageArg::Java => Self::Java,
            SnippetLanguageArg::Csharp => Self::Csharp,
            SnippetLanguageArg::Php => Self::Php,
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create an empty local Postly workspace.
    Init {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value = "Postly workspace")]
        name: String,
    },
    /// Create and persist a request in a local collection.
    New {
        #[command(subcommand)]
        kind: NewKind,
    },
    /// Send an unsaved HTTP request immediately.
    Request {
        url: String,
        #[arg(short, long, default_value = "GET")]
        method: String,
        #[arg(long = "query")]
        query: Vec<String>,
        #[arg(short = 'H', long = "header")]
        headers: Vec<String>,
        #[arg(long)]
        data: Option<String>,
        #[arg(long)]
        json: Option<String>,
        #[arg(long)]
        bearer: Option<String>,
        #[arg(long)]
        basic_user: Option<String>,
        #[arg(long)]
        basic_password: Option<String>,
        #[arg(long, help = "HTTP Digest username")]
        digest_user: Option<String>,
        #[arg(long, help = "HTTP Digest password")]
        digest_password: Option<String>,
        #[command(flatten)]
        oauth: Box<OAuthCliArgs>,
        #[arg(long, default_value_t = 30)]
        timeout: u64,
        #[arg(
            long,
            default_value_t = 10,
            help = "Maximum number of HTTP redirects to follow (0 disables redirects)"
        )]
        max_redirects: usize,
        #[arg(
            long,
            value_name = "URL",
            help = "Route the request through an HTTP(S) or SOCKS proxy"
        )]
        proxy: Option<String>,
        #[arg(
            long,
            requires = "proxy",
            value_name = "HOSTS",
            help = "Bypass the proxy for comma-separated hosts, domains or IP ranges"
        )]
        no_proxy: Option<String>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Trust an additional PEM-encoded CA certificate"
        )]
        ca_cert: Option<PathBuf>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Use a PEM client certificate and private-key identity"
        )]
        client_identity: Option<PathBuf>,
        #[arg(long)]
        insecure: bool,
        #[arg(long)]
        output_json: bool,
    },
    /// Execute a GraphQL query as a structured request.
    Graphql {
        endpoint: String,
        #[arg(long, conflicts_with_all = ["query", "query_file", "variables", "variables_json", "operation_name"], help = "Fetch and summarize the GraphQL schema through introspection")]
        introspect: bool,
        #[arg(short = 'q', long, conflicts_with = "query_file")]
        query: Option<String>,
        #[arg(long, conflicts_with = "query")]
        query_file: Option<PathBuf>,
        #[arg(long = "variable")]
        variables: Vec<String>,
        #[arg(long)]
        variables_json: Option<String>,
        #[arg(long)]
        operation_name: Option<String>,
        #[arg(short = 'H', long)]
        headers: Vec<String>,
        #[arg(long)]
        bearer: Option<String>,
        #[arg(long)]
        basic_user: Option<String>,
        #[arg(long)]
        basic_password: Option<String>,
        #[arg(long, default_value_t = 30)]
        timeout: u64,
        #[arg(
            long,
            default_value_t = 10,
            help = "Maximum number of HTTP redirects to follow (0 disables redirects)"
        )]
        max_redirects: usize,
        #[arg(
            long,
            value_name = "URL",
            help = "Route the request through an HTTP(S) or SOCKS proxy"
        )]
        proxy: Option<String>,
        #[arg(
            long,
            requires = "proxy",
            value_name = "HOSTS",
            help = "Bypass the proxy for comma-separated hosts, domains or IP ranges"
        )]
        no_proxy: Option<String>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Trust an additional PEM-encoded CA certificate"
        )]
        ca_cert: Option<PathBuf>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Use a PEM client certificate and private-key identity"
        )]
        client_identity: Option<PathBuf>,
        #[arg(long)]
        insecure: bool,
        #[arg(long)]
        output_json: bool,
    },
    /// Inspect local protobuf services or call a unary gRPC method.
    Grpc {
        #[command(subcommand)]
        kind: GrpcKind,
    },
    /// Subscribe to a Server-Sent Events endpoint until it closes.
    Sse {
        endpoint: String,
        #[arg(short = 'H', long)]
        headers: Vec<String>,
        #[arg(long)]
        bearer: Option<String>,
        #[arg(long)]
        basic_user: Option<String>,
        #[arg(long)]
        basic_password: Option<String>,
        #[arg(long, default_value_t = 300)]
        timeout: u64,
        #[arg(
            long,
            default_value_t = 10,
            help = "Maximum number of HTTP redirects to follow (0 disables redirects)"
        )]
        max_redirects: usize,
        #[arg(long, default_value_t = 0)]
        reconnect: u32,
        #[arg(
            long,
            value_name = "URL",
            help = "Route the stream through an HTTP(S) or SOCKS proxy"
        )]
        proxy: Option<String>,
        #[arg(
            long,
            requires = "proxy",
            value_name = "HOSTS",
            help = "Bypass the proxy for comma-separated hosts, domains or IP ranges"
        )]
        no_proxy: Option<String>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Trust an additional PEM-encoded CA certificate"
        )]
        ca_cert: Option<PathBuf>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Use a PEM client certificate and private-key identity"
        )]
        client_identity: Option<PathBuf>,
        #[arg(long)]
        insecure: bool,
        #[arg(long)]
        output_json: bool,
    },
    /// Connect to a WebSocket endpoint, send optional text messages and read until close.
    #[command(alias = "ws")]
    Websocket {
        endpoint: String,
        #[arg(long = "send")]
        send: Vec<String>,
        #[arg(short = 'H', long)]
        headers: Vec<String>,
        #[arg(long)]
        bearer: Option<String>,
        #[arg(long)]
        basic_user: Option<String>,
        #[arg(long)]
        basic_password: Option<String>,
        #[arg(long, default_value_t = 300)]
        timeout: u64,
        #[arg(long, default_value_t = 0)]
        reconnect: u32,
        #[arg(
            long,
            value_name = "URL",
            help = "Route the WebSocket through an HTTP proxy using CONNECT"
        )]
        proxy: Option<String>,
        #[arg(
            long,
            requires = "proxy",
            value_name = "HOSTS",
            help = "Bypass the WebSocket proxy for comma-separated hosts or domains"
        )]
        no_proxy: Option<String>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Trust an additional PEM-encoded CA certificate for wss://"
        )]
        ca_cert: Option<PathBuf>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Use a PEM or PKCS#12 client identity for wss://"
        )]
        client_identity: Option<PathBuf>,
        #[arg(long, help = "Disable wss:// certificate verification (unsafe)")]
        insecure: bool,
        #[arg(long)]
        output_json: bool,
    },
    /// Send a saved .postly.toml request file.
    Send {
        file: PathBuf,
        #[arg(long)]
        environment: Option<String>,
        #[arg(
            long,
            help = "Execute preserved pre-request and test scripts through Node.js"
        )]
        scripts: bool,
        #[arg(long)]
        output_json: bool,
        #[arg(long, default_value_t = 30)]
        timeout: u64,
        #[arg(
            long,
            default_value_t = 10,
            help = "Maximum number of HTTP redirects to follow (0 disables redirects)"
        )]
        max_redirects: usize,
        #[arg(
            long,
            value_name = "URL",
            help = "Route the request through an HTTP(S) or SOCKS proxy"
        )]
        proxy: Option<String>,
        #[arg(
            long,
            requires = "proxy",
            value_name = "HOSTS",
            help = "Bypass the proxy for comma-separated hosts, domains or IP ranges"
        )]
        no_proxy: Option<String>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Trust an additional PEM-encoded CA certificate"
        )]
        ca_cert: Option<PathBuf>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Use a PEM client certificate and private-key identity"
        )]
        client_identity: Option<PathBuf>,
        #[arg(long)]
        insecure: bool,
        #[arg(long, help = "Open a local browser callback for OAuth 2.0 PKCE")]
        oauth_browser: bool,
    },
    /// Import a Postman collection or environment into a local workspace.
    Import {
        #[command(subcommand)]
        kind: ImportKind,
    },
    /// Export local data to a compatible Postman JSON format.
    Export {
        #[command(subcommand)]
        kind: ExportKind,
    },
    /// Create or update a local environment without printing its values.
    /// `--secret` stores values in the operating-system keychain.
    Env {
        #[command(subcommand)]
        kind: EnvKind,
    },
    /// List collections and saved requests in a workspace.
    List {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Validate canonical workspace files without changing them.
    Validate {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        output_json: bool,
    },
    /// Search saved request metadata across every local collection.
    Search {
        query: String,
        #[arg(short, long, default_value = ".")]
        workspace: PathBuf,
        #[arg(long)]
        output_json: bool,
    },
    /// Generate deterministic local Markdown documentation from saved requests.
    Docs {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, help = "Document one collection; defaults to every collection")]
        collection: Option<String>,
        #[arg(short, long, help = "Write Markdown to a file instead of stdout")]
        output: Option<PathBuf>,
        #[arg(
            long,
            help = "Include response-example bodies; review the generated file before sharing"
        )]
        include_example_bodies: bool,
    },
    /// Show recent metadata-only executions from the local workspace.
    History {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
        #[arg(long, help = "Search request name, method or sanitized URL")]
        search: Option<String>,
        #[arg(long)]
        method: Option<String>,
        #[arg(long)]
        status: Option<u16>,
        #[arg(long)]
        errors_only: bool,
        #[arg(long, help = "Clear local metadata-only history")]
        clear: bool,
        #[arg(long)]
        output_json: bool,
    },
    /// Inspect or clear the local workspace cookie jar without printing values.
    Cookies {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, help = "Clear all cookies from this workspace's local jar")]
        clear: bool,
        #[arg(long)]
        output_json: bool,
    },
    /// Generate a reviewable code snippet from a saved request.
    Snippet {
        file: PathBuf,
        #[arg(short, long, value_enum, default_value_t = SnippetLanguageArg::Curl)]
        language: SnippetLanguageArg,
        #[arg(long)]
        output_json: bool,
    },
    /// Serve saved response examples as a deterministic local HTTP mock.
    Mock {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(
            long,
            help = "Resolve mock route and response placeholders from this environment"
        )]
        environment: Option<String>,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 3000)]
        port: u16,
        #[arg(long, help = "Serve one request, then exit; useful for local tests")]
        once: bool,
    },
    /// Execute every saved request in a collection.
    Run {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        environment: Option<String>,
        #[arg(
            long,
            value_name = "FOLDER",
            help = "Run this folder and its nested requests only"
        )]
        folder: Option<String>,
        #[arg(long)]
        fail_fast: bool,
        #[arg(
            long,
            help = "Execute preserved pre-request and test scripts through Node.js"
        )]
        scripts: bool,
        #[arg(
            long,
            default_value_t = 1,
            value_parser = parse_concurrency,
            value_name = "N",
            help = "Run up to N requests concurrently when scripts and delays are disabled"
        )]
        concurrency: usize,
        #[arg(long, default_value_t = 30)]
        timeout: u64,
        #[arg(
            long,
            default_value_t = 10,
            help = "Maximum number of HTTP redirects to follow (0 disables redirects)"
        )]
        max_redirects: usize,
        #[arg(
            long,
            value_name = "URL",
            help = "Route collection requests through an HTTP(S) or SOCKS proxy"
        )]
        proxy: Option<String>,
        #[arg(
            long,
            requires = "proxy",
            value_name = "HOSTS",
            help = "Bypass the proxy for comma-separated hosts, domains or IP ranges"
        )]
        no_proxy: Option<String>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Trust an additional PEM-encoded CA certificate"
        )]
        ca_cert: Option<PathBuf>,
        #[arg(
            long,
            value_name = "PATH",
            help = "Use a PEM client certificate and private-key identity"
        )]
        client_identity: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = Reporter::Pretty)]
        reporter: Reporter,
        #[arg(long)]
        data_file: Option<PathBuf>,
        #[arg(long)]
        output_json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum NewKind {
    Request {
        #[arg(long, default_value = ".")]
        workspace: PathBuf,
        #[arg(long, default_value = "My API")]
        collection: String,
        #[arg(long)]
        name: String,
        url: String,
        #[arg(short, long, default_value = "GET")]
        method: String,
        #[arg(long = "query")]
        query: Vec<String>,
        #[arg(long)]
        folder: Option<String>,
        #[arg(short = 'H', long = "header")]
        headers: Vec<String>,
        #[arg(long)]
        data: Option<String>,
        #[arg(long)]
        json: Option<String>,
        #[arg(long)]
        bearer: Option<String>,
        #[arg(long)]
        basic_user: Option<String>,
        #[arg(long)]
        basic_password: Option<String>,
        #[arg(long, help = "HTTP Digest username")]
        digest_user: Option<String>,
        #[arg(long, help = "HTTP Digest password")]
        digest_password: Option<String>,
        #[command(flatten)]
        oauth: Box<OAuthCliArgs>,
    },
}

#[derive(Debug, Args)]
struct GrpcCallCommand {
    endpoint: String,
    #[arg(long)]
    proto: PathBuf,
    #[arg(long)]
    method: String,
    #[arg(long, conflicts_with = "message_file")]
    message: Option<String>,
    #[arg(long, conflicts_with = "message")]
    message_file: Option<PathBuf>,
    #[arg(long = "include")]
    includes: Vec<PathBuf>,
    #[arg(short = 'H', long = "metadata")]
    metadata: Vec<String>,
    #[arg(long)]
    bearer: Option<String>,
    #[arg(long)]
    basic_user: Option<String>,
    #[arg(long)]
    basic_password: Option<String>,
    #[arg(long, default_value_t = 30)]
    timeout: u64,
    #[arg(
        long,
        value_name = "URL",
        help = "Route the gRPC channel through an HTTP proxy using CONNECT"
    )]
    proxy: Option<String>,
    #[arg(
        long,
        requires = "proxy",
        value_name = "HOSTS",
        help = "Bypass the gRPC proxy for comma-separated hosts or domains"
    )]
    no_proxy: Option<String>,
    #[command(flatten)]
    tls: Box<GrpcTlsArgs>,
    #[arg(long)]
    output_json: bool,
}

#[derive(Debug, Args)]
struct GrpcReflectCommand {
    endpoint: String,
    #[arg(long, help = "Host value sent to the reflection service")]
    host: Option<String>,
    #[arg(long, default_value_t = 30)]
    timeout: u64,
    #[arg(
        long,
        value_name = "URL",
        help = "Route the reflection channel through an HTTP proxy using CONNECT"
    )]
    proxy: Option<String>,
    #[arg(
        long,
        requires = "proxy",
        value_name = "HOSTS",
        help = "Bypass the reflection proxy for comma-separated hosts or domains"
    )]
    no_proxy: Option<String>,
    #[command(flatten)]
    tls: Box<GrpcTlsArgs>,
    #[arg(long)]
    output_json: bool,
}

#[derive(Debug, Subcommand)]
enum GrpcKind {
    /// Compile a local .proto file and list its services and methods.
    Describe {
        proto: PathBuf,
        #[arg(long = "include")]
        includes: Vec<PathBuf>,
        #[arg(long)]
        output_json: bool,
    },
    /// Call a gRPC method using a protobuf JSON request object or stream array.
    Call(Box<GrpcCallCommand>),
    /// Discover services and methods through the server reflection protocol.
    Reflect(Box<GrpcReflectCommand>),
}

#[derive(Debug, Subcommand)]
enum EnvKind {
    Set {
        #[arg(long, default_value = ".")]
        workspace: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long = "set")]
        values: Vec<String>,
        #[arg(long = "secret")]
        secrets: Vec<String>,
        #[arg(
            long = "secret-stdin",
            value_name = "KEY",
            help = "Read one secret value per key from stdin; the value is never a command-line argument"
        )]
        secret_stdin: Vec<String>,
    },
    /// Migrate legacy plaintext environment values into the OS credential store.
    Migrate {
        #[arg(long, default_value = ".")]
        workspace: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(
            long = "key",
            value_name = "KEY",
            help = "Migrate this plaintext variable; repeat for multiple keys"
        )]
        keys: Vec<String>,
        #[arg(
            long,
            help = "Migrate every enabled or disabled variable marked as secret"
        )]
        all: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ImportKind {
    Collection {
        input: PathBuf,
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
    },
    Environment {
        input: PathBuf,
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
        #[arg(
            long,
            help = "Store Postman variables marked secret in the OS credential store"
        )]
        secure: bool,
    },
    /// Import a local dotenv file without expanding variables or executing commands.
    Dotenv {
        input: PathBuf,
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
        #[arg(long, default_value = "Local")]
        name: String,
        #[arg(
            long = "secret",
            value_name = "KEY",
            help = "Store this key in the OS credential store; repeat for multiple keys"
        )]
        secrets: Vec<String>,
    },
    Openapi {
        input: PathBuf,
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
    },
    Curl {
        command: String,
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
        #[arg(long, default_value = "Imported cURL")]
        collection: String,
        #[arg(long, default_value = "Imported cURL request")]
        name: String,
    },
}

#[derive(Debug, Subcommand)]
enum ExportKind {
    /// Export a local collection as Postman Collection v2.1 JSON.
    Collection {
        #[arg(default_value = ".")]
        workspace: PathBuf,
        #[arg(
            long,
            help = "Collection name; required when multiple collections exist"
        )]
        collection: Option<String>,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Export a local collection as an OpenAPI 3.0 JSON or YAML document.
    Openapi {
        #[arg(default_value = ".")]
        workspace: PathBuf,
        #[arg(
            long,
            help = "Collection name; required when multiple collections exist"
        )]
        collection: Option<String>,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Export a local environment as a Postman environment JSON file.
    Environment {
        #[arg(default_value = ".")]
        workspace: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(short, long)]
        output: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_target(false)
        .with_ansi(io::stderr().is_terminal())
        .init();

    match Cli::parse().command {
        Command::Init { path, name } => init_workspace(&path, &name),
        Command::New { kind } => match kind {
            NewKind::Request {
                workspace,
                collection,
                name,
                url,
                method,
                query,
                folder,
                headers,
                data,
                json,
                bearer,
                basic_user,
                basic_password,
                digest_user,
                digest_password,
                oauth,
            } => create_request(NewRequestOptions {
                workspace,
                collection,
                name,
                url,
                method,
                folder,
                query,
                headers,
                data,
                json_body: json,
                bearer,
                basic_user,
                basic_password,
                digest_user,
                digest_password,
                oauth_token_url: oauth.oauth_token_url,
                oauth_client_id: oauth.oauth_client_id,
                oauth_client_secret: oauth.oauth_client_secret,
                oauth_scope: oauth.oauth_scope,
                oauth_authorization_url: oauth.oauth_authorization_url,
                oauth_device_authorization_url: oauth.oauth_device_authorization_url,
                oauth_redirect_uri: oauth.oauth_redirect_uri,
                oauth_code: oauth.oauth_code,
                oauth_code_verifier: oauth.oauth_code_verifier,
                oauth_refresh_token: oauth.oauth_refresh_token,
                oauth_browser: oauth.oauth_browser,
                aws_access_key_id: oauth.aws_access_key_id,
                aws_secret_access_key: oauth.aws_secret_access_key,
                aws_region: oauth.aws_region,
                aws_service: oauth.aws_service,
                aws_session_token: oauth.aws_session_token,
            }),
        },
        Command::Request {
            url,
            method,
            query,
            headers,
            data,
            json,
            bearer,
            basic_user,
            basic_password,
            digest_user,
            digest_password,
            oauth,
            timeout,
            max_redirects,
            proxy,
            no_proxy,
            ca_cert,
            client_identity,
            insecure,
            output_json,
        } => {
            send_unsaved_request(ImmediateRequestOptions {
                url,
                method,
                query,
                headers,
                data,
                json_body: json,
                bearer,
                basic_user,
                basic_password,
                digest_user,
                digest_password,
                oauth_token_url: oauth.oauth_token_url,
                oauth_client_id: oauth.oauth_client_id,
                oauth_client_secret: oauth.oauth_client_secret,
                oauth_scope: oauth.oauth_scope,
                oauth_authorization_url: oauth.oauth_authorization_url,
                oauth_device_authorization_url: oauth.oauth_device_authorization_url,
                oauth_redirect_uri: oauth.oauth_redirect_uri,
                oauth_code: oauth.oauth_code,
                oauth_code_verifier: oauth.oauth_code_verifier,
                oauth_refresh_token: oauth.oauth_refresh_token,
                oauth_browser: oauth.oauth_browser,
                aws_access_key_id: oauth.aws_access_key_id,
                aws_secret_access_key: oauth.aws_secret_access_key,
                aws_region: oauth.aws_region,
                aws_service: oauth.aws_service,
                aws_session_token: oauth.aws_session_token,
                timeout,
                max_redirects,
                proxy,
                no_proxy,
                ca_cert,
                client_identity,
                insecure,
                output_json,
            })
            .await
        }
        Command::Graphql {
            endpoint,
            introspect,
            query,
            query_file,
            variables,
            variables_json,
            operation_name,
            headers,
            bearer,
            basic_user,
            basic_password,
            timeout,
            max_redirects,
            proxy,
            no_proxy,
            ca_cert,
            client_identity,
            insecure,
            output_json,
        } => {
            let options = GraphqlOptions {
                endpoint,
                query,
                query_file,
                variables,
                variables_json,
                operation_name,
                headers,
                bearer,
                basic_user,
                basic_password,
                timeout,
                max_redirects,
                proxy,
                no_proxy,
                ca_cert,
                client_identity,
                insecure,
                output_json,
            };
            if introspect {
                introspect_graphql_schema(options).await
            } else {
                send_graphql_request(options).await
            }
        }
        Command::Grpc { kind } => match kind {
            GrpcKind::Describe {
                proto,
                includes,
                output_json,
            } => describe_grpc(&proto, &includes, output_json),
            GrpcKind::Call(call) => {
                let GrpcCallCommand {
                    endpoint,
                    proto,
                    method,
                    message,
                    message_file,
                    includes,
                    metadata,
                    bearer,
                    basic_user,
                    basic_password,
                    timeout,
                    proxy,
                    no_proxy,
                    tls,
                    output_json,
                } = *call;
                call_grpc(GrpcCallOptions {
                    endpoint,
                    proto,
                    includes,
                    method,
                    message,
                    message_file,
                    metadata,
                    bearer,
                    basic_user,
                    basic_password,
                    timeout,
                    proxy,
                    no_proxy,
                    ca_cert: tls.ca_cert,
                    client_identity: tls.client_identity,
                    output_json,
                })
                .await
            }
            GrpcKind::Reflect(command) => reflect_grpc(*command).await,
        },
        Command::Sse {
            endpoint,
            headers,
            bearer,
            basic_user,
            basic_password,
            timeout,
            max_redirects,
            reconnect,
            proxy,
            no_proxy,
            ca_cert,
            client_identity,
            insecure,
            output_json,
        } => {
            stream_sse(SseOptions {
                endpoint,
                headers,
                bearer,
                basic_user,
                basic_password,
                timeout,
                max_redirects,
                reconnect,
                proxy,
                no_proxy,
                ca_cert,
                client_identity,
                insecure,
                output_json,
            })
            .await
        }
        Command::Websocket {
            endpoint,
            send,
            headers,
            bearer,
            basic_user,
            basic_password,
            timeout,
            reconnect,
            proxy,
            no_proxy,
            ca_cert,
            client_identity,
            insecure,
            output_json,
        } => {
            run_websocket(WebsocketOptions {
                endpoint,
                send,
                headers,
                bearer,
                basic_user,
                basic_password,
                timeout,
                reconnect,
                proxy,
                no_proxy,
                ca_cert,
                client_identity,
                insecure,
                output_json,
            })
            .await
        }
        Command::Send {
            file,
            environment,
            scripts,
            output_json,
            timeout,
            max_redirects,
            proxy,
            no_proxy,
            ca_cert,
            client_identity,
            insecure,
            oauth_browser,
        } => {
            send_saved_request(SendOptions {
                file: &file,
                environment_name: environment.as_deref(),
                scripts,
                timeout,
                max_redirects,
                proxy: proxy.as_deref(),
                no_proxy: no_proxy.as_deref(),
                ca_cert: ca_cert.as_deref(),
                client_identity: client_identity.as_deref(),
                insecure,
                output_json,
                oauth_browser,
            })
            .await
        }
        Command::Import { kind } => import_command(kind).await,
        Command::Export { kind } => export_command(kind),
        Command::Env { kind } => match kind {
            EnvKind::Set {
                workspace,
                name,
                values,
                secrets,
                secret_stdin,
            } => set_environment(&workspace, &name, &values, &secrets, &secret_stdin),
            EnvKind::Migrate {
                workspace,
                name,
                keys,
                all,
            } => migrate_environment_secrets(&workspace, &name, &keys, all),
        },
        Command::List { path } => list_workspace(&path),
        Command::Validate { path, output_json } => validate_workspace(&path, output_json),
        Command::Search {
            query,
            workspace,
            output_json,
        } => search_workspace(&workspace, &query, output_json),
        Command::Docs {
            path,
            collection,
            output,
            include_example_bodies,
        } => generate_docs(
            &path,
            collection.as_deref(),
            output.as_deref(),
            include_example_bodies,
        ),
        Command::History {
            path,
            limit,
            search,
            method,
            status,
            errors_only,
            clear,
            output_json,
        } => list_history(
            &path,
            HistoryOptions {
                limit,
                search,
                method,
                status,
                errors_only,
                clear,
                output_json,
            },
        ),
        Command::Cookies {
            path,
            clear,
            output_json,
        } => list_cookies(&path, clear, output_json),
        Command::Snippet {
            file,
            language,
            output_json,
        } => print_snippet(&file, language.into(), output_json),
        Command::Mock {
            path,
            environment,
            host,
            port,
            once,
        } => run_mock_server(&path, environment.as_deref(), &host, port, once).await,
        Command::Run {
            path,
            environment,
            folder,
            fail_fast,
            scripts,
            concurrency,
            timeout,
            max_redirects,
            proxy,
            no_proxy,
            ca_cert,
            client_identity,
            reporter,
            data_file,
            output_json,
        } => {
            run_workspace(RunOptions {
                path: &path,
                environment_name: environment.as_deref(),
                folder: folder.as_deref(),
                fail_fast,
                scripts,
                concurrency,
                timeout,
                max_redirects,
                proxy: proxy.as_deref(),
                no_proxy: no_proxy.as_deref(),
                ca_cert: ca_cert.as_deref(),
                client_identity: client_identity.as_deref(),
                reporter: if output_json {
                    Reporter::Json
                } else {
                    reporter
                },
                data_file: data_file.as_deref(),
            })
            .await
        }
    }
}

fn init_workspace(path: &Path, name: &str) -> Result<()> {
    let workspace = Workspace::init(path, name)?;
    let collection = workspace.create_collection(&Collection::new("My API"))?;
    println!(
        "Initialized Postly workspace at {}",
        workspace.root().display()
    );
    println!("Created collection at {}", collection.directory.display());
    println!("No account or cloud service is required.");
    Ok(())
}

fn print_snippet(path: &Path, language: SnippetLanguage, output_json: bool) -> Result<()> {
    let workspace = find_workspace(path)?;
    let request = workspace.load_request(path)?;
    let snippet = generate_code_snippet(&request, language);
    if output_json {
        println!("{}", serde_json::to_string_pretty(&snippet)?);
    } else {
        for warning in &snippet.warnings {
            eprintln!("warning: {warning}");
        }
        println!("{}", snippet.code);
    }
    Ok(())
}

fn list_cookies(path: &Path, clear: bool, output_json: bool) -> Result<()> {
    let workspace = find_workspace(path)?;
    let engine = HttpEngine::new(&EngineOptions {
        cookie_jar: Some(workspace.root().join(".postly/cookies.json")),
        ..EngineOptions::default()
    })?;
    if clear {
        engine.clear_cookies()?;
        if output_json {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({"cleared": true}))?
            );
        } else {
            println!("Cleared the local Postly cookie jar.");
        }
        return Ok(());
    }

    let cookies = engine.cookie_snapshot();
    if output_json {
        let cookies = cookies
            .iter()
            .map(|cookie| {
                json!({
                    "name": cookie.name,
                    "value": "<masked>",
                    "domain": cookie.domain,
                    "path": cookie.path,
                    "secure": cookie.secure,
                    "http_only": cookie.http_only,
                    "same_site": cookie.same_site,
                    "persistent": cookie.persistent,
                })
            })
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "workspace": workspace.root(),
                "count": cookies.len(),
                "cookies": cookies,
            }))?
        );
    } else if cookies.is_empty() {
        println!("No active cookies in {}.", workspace.root().display());
    } else {
        println!(
            "{} active cookie(s) in {}:",
            cookies.len(),
            workspace.root().display()
        );
        for cookie in cookies {
            let mut flags = Vec::new();
            if cookie.secure {
                flags.push("Secure".to_owned());
            }
            if cookie.http_only {
                flags.push("HttpOnly".to_owned());
            }
            if let Some(same_site) = cookie.same_site {
                flags.push(format!("SameSite={same_site}"));
            }
            if cookie.persistent {
                flags.push("Persistent".to_owned());
            }
            println!(
                "- {}=<masked> domain={} path={}{}",
                cookie.name,
                cookie.domain,
                cookie.path,
                if flags.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", flags.join(", "))
                }
            );
        }
    }
    Ok(())
}

fn generate_docs(
    path: &Path,
    collection_name: Option<&str>,
    output: Option<&Path>,
    include_example_bodies: bool,
) -> Result<()> {
    let workspace = Workspace::open(path)?;
    if let Some(name) = collection_name {
        let exists = workspace.collections()?.iter().any(|collection| {
            collection.collection.name == name
                || collection.collection.name.eq_ignore_ascii_case(name)
        });
        if !exists {
            bail!("collection not found: {name}");
        }
    }
    let markdown = generate_markdown_docs(&workspace, collection_name, include_example_bodies)?;
    if let Some(output) = output {
        fs::write(output, markdown)
            .with_context(|| format!("could not write generated docs {}", output.display()))?;
        println!("Generated local API documentation at {}", output.display());
    } else {
        print!("{markdown}");
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct MockRoute {
    method: String,
    path: String,
    example: ResponseExample,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MockResponse {
    status: u16,
    status_text: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    delay_ms: u64,
}

fn mock_workspace_and_filter(path: &Path) -> Result<(Workspace, Option<PathBuf>)> {
    if path.join("postly.toml").is_file() {
        return Ok((Workspace::open(path)?, None));
    }
    if path.join("postly.collection.toml").is_file() {
        let collection = path
            .canonicalize()
            .with_context(|| format!("could not resolve mock collection {}", path.display()))?;
        let workspace_root = collection
            .parent()
            .and_then(Path::parent)
            .context("mock collection is not inside a Postly workspace")?;
        return Ok((Workspace::open(workspace_root)?, Some(collection)));
    }
    bail!(
        "{} is neither a Postly workspace nor a collection directory",
        path.display()
    )
}

fn load_mock_routes(path: &Path, environment_name: Option<&str>) -> Result<Vec<MockRoute>> {
    let (workspace, collection_filter) = mock_workspace_and_filter(path)?;
    let mut routes = Vec::new();
    for collection in workspace.collections()? {
        if collection_filter.as_ref().is_some_and(|filter| {
            filter
                != &collection
                    .directory
                    .canonicalize()
                    .unwrap_or_else(|_| collection.directory.clone())
        }) {
            continue;
        }
        let context =
            context_for_collection(&workspace, Some(&collection.collection), environment_name)?;
        for (_, request) in workspace.requests(&collection)? {
            let Some(route_path) = mock_route_path(&context.resolve(&request.url).value) else {
                continue;
            };
            for example in request.examples {
                routes.push(MockRoute {
                    method: request.method.to_ascii_uppercase(),
                    path: route_path.clone(),
                    example: resolve_mock_example(example, &context),
                });
            }
        }
    }
    if routes.is_empty() {
        bail!(
            "no saved response examples found under {}; import or save examples before starting the mock",
            path.display()
        );
    }
    Ok(routes)
}

fn resolve_mock_example(
    mut example: ResponseExample,
    context: &VariableContext,
) -> ResponseExample {
    for header in &mut example.headers {
        header.value = context.resolve(&header.value).value;
    }
    if let Some(body) = &mut example.body {
        *body = context.resolve(body).value;
    }
    for cookie in &mut example.cookies {
        cookie.value = context.resolve(&cookie.value).value;
        for value in [
            &mut cookie.domain,
            &mut cookie.path,
            &mut cookie.same_site,
            &mut cookie.expires,
        ]
        .into_iter()
        .flatten()
        {
            *value = context.resolve(value).value;
        }
    }
    example
}

fn mock_route_path(raw_url: &str) -> Option<String> {
    if let Ok(url) = url::Url::parse(raw_url) {
        return Some(if url.path().is_empty() {
            "/".to_owned()
        } else {
            url.path().to_owned()
        });
    }
    let template_end = raw_url.find("}}")? + 2;
    let suffix = raw_url
        .get(template_end..)?
        .split('?')
        .next()
        .unwrap_or_default();
    if suffix.is_empty() {
        Some("/".to_owned())
    } else if suffix.starts_with('/') {
        Some(suffix.to_owned())
    } else {
        None
    }
}

fn mock_response_for(routes: &[MockRoute], method: &str, target: &str) -> MockResponse {
    let path = target.split('?').next().unwrap_or("/");
    let method = method.to_ascii_uppercase();
    let Some(route) = routes
        .iter()
        .find(|route| route.method == method && route.path == path)
    else {
        return MockResponse {
            status: 404,
            status_text: "Not Found".to_owned(),
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body: br#"{"error":"No saved mock example matches this method and path"}"#.to_vec(),
            delay_ms: 0,
        };
    };
    let example = &route.example;
    let status = example.status.unwrap_or(200);
    let mut headers = example
        .headers
        .iter()
        .filter(|header| {
            header.enabled
                && !header.key.contains('\r')
                && !header.key.contains('\n')
                && !header.value.contains('\r')
                && !header.value.contains('\n')
        })
        .map(|header| (header.key.clone(), header.value.clone()))
        .collect::<Vec<_>>();
    if !headers
        .iter()
        .any(|(key, _)| key.eq_ignore_ascii_case("content-type"))
    {
        headers.push((
            "content-type".to_owned(),
            "text/plain; charset=utf-8".to_owned(),
        ));
    }
    headers.extend(
        example
            .cookies
            .iter()
            .filter_map(mock_set_cookie_header)
            .map(|value| ("set-cookie".to_owned(), value)),
    );
    MockResponse {
        status,
        status_text: example
            .status_text
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| status_text(status))
            .to_owned(),
        headers,
        body: example.body.clone().unwrap_or_default().into_bytes(),
        delay_ms: example.delay_ms,
    }
}

fn mock_set_cookie_header(cookie: &ResponseExampleCookie) -> Option<String> {
    cookie.to_set_cookie_header()
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Postly Mock Response",
    }
}

async fn read_mock_request(stream: &mut tokio::net::TcpStream) -> Result<(String, String)> {
    const MAX_HEADER_BYTES: usize = 64 * 1024;
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if bytes.len() > MAX_HEADER_BYTES {
            bail!("mock request headers exceed {MAX_HEADER_BYTES} bytes");
        }
    }
    let request_text = String::from_utf8_lossy(&bytes);
    let first_line = request_text
        .lines()
        .next()
        .context("mock request did not include an HTTP request line")?;
    let mut fields = first_line.split_whitespace();
    let method = fields.next().context("mock request method is missing")?;
    let target = fields.next().context("mock request target is missing")?;
    Ok((method.to_owned(), target.to_owned()))
}

async fn write_mock_response(
    stream: &mut tokio::net::TcpStream,
    response: &MockResponse,
) -> Result<()> {
    if response.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(response.delay_ms)).await;
    }
    let mut output = format!("HTTP/1.1 {} {}\r\n", response.status, response.status_text);
    for (key, value) in &response.headers {
        output.push_str(key);
        output.push_str(": ");
        output.push_str(value);
        output.push_str("\r\n");
    }
    output.push_str(&format!("Content-Length: {}\r\n", response.body.len()));
    output.push_str("Connection: close\r\n\r\n");
    stream.write_all(output.as_bytes()).await?;
    stream.write_all(&response.body).await?;
    Ok(())
}

async fn run_mock_server(
    path: &Path,
    environment_name: Option<&str>,
    host: &str,
    port: u16,
    once: bool,
) -> Result<()> {
    let routes = load_mock_routes(path, environment_name)?;
    let listener = tokio::net::TcpListener::bind((host, port))
        .await
        .with_context(|| format!("could not bind mock server to {host}:{port}"))?;
    let address = listener.local_addr()?;
    println!(
        "Postly mock listening on http://{}:{} ({} route example(s)); press Ctrl-C to stop",
        address.ip(),
        address.port(),
        routes.len()
    );
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (mut stream, _) = accepted?;
                match read_mock_request(&mut stream).await {
                    Ok((method, target)) => {
                        let response = mock_response_for(&routes, &method, &target);
                        write_mock_response(&mut stream, &response).await?;
                    }
                    Err(error) => {
                        let response = MockResponse {
                            status: 400,
                            status_text: "Bad Request".to_owned(),
                            headers: vec![("content-type".to_owned(), "text/plain".to_owned())],
                            body: error.to_string().into_bytes(),
                            delay_ms: 0,
                        };
                        write_mock_response(&mut stream, &response).await?;
                    }
                }
                if once {
                    return Ok(());
                }
            }
            _ = tokio::signal::ctrl_c() => {
                eprintln!("Postly mock stopped");
                return Ok(());
            }
        }
    }
}

fn create_request(options: NewRequestOptions) -> Result<()> {
    let workspace = Workspace::open_or_init(&options.workspace, "Postly workspace")?;
    let collection = workspace
        .collections()?
        .into_iter()
        .find(|collection| collection.collection.name == options.collection)
        .or_else(|| {
            workspace
                .create_collection(&Collection::new(&options.collection))
                .ok()
        })
        .with_context(|| format!("could not create collection {}", options.collection))?;
    let mut request = Request::new(options.name, options.method, options.url);
    request.folder = options.folder;
    request.query = parse_pairs_flags(&options.query)?;
    request.headers = parse_headers(&options.headers)?;
    request.auth = parse_auth_flags_with_oauth_and_digest(
        options.bearer,
        options.basic_user,
        options.basic_password,
        options.digest_user,
        options.digest_password,
        OAuthCliArgs {
            oauth_token_url: options.oauth_token_url,
            oauth_client_id: options.oauth_client_id,
            oauth_client_secret: options.oauth_client_secret,
            oauth_scope: options.oauth_scope,
            oauth_authorization_url: options.oauth_authorization_url,
            oauth_device_authorization_url: options.oauth_device_authorization_url,
            oauth_redirect_uri: options.oauth_redirect_uri,
            oauth_code: options.oauth_code,
            oauth_code_verifier: options.oauth_code_verifier,
            oauth_refresh_token: options.oauth_refresh_token,
            oauth_browser: options.oauth_browser,
            aws_access_key_id: options.aws_access_key_id,
            aws_secret_access_key: options.aws_secret_access_key,
            aws_region: options.aws_region,
            aws_service: options.aws_service,
            aws_session_token: options.aws_session_token,
        },
    )?;
    request.body = parse_cli_body(options.data, options.json_body)?;
    let path = workspace.save_request(&collection, &request)?;
    println!("Saved request at {}", path.display());
    Ok(())
}

async fn send_unsaved_request(options: ImmediateRequestOptions) -> Result<()> {
    let mut request = Request::new("CLI request", options.method, options.url);
    request.query = parse_pairs_flags(&options.query)?;
    request.headers = parse_headers(&options.headers)?;
    request.auth = parse_auth_flags_with_oauth_and_digest(
        options.bearer,
        options.basic_user,
        options.basic_password,
        options.digest_user,
        options.digest_password,
        OAuthCliArgs {
            oauth_token_url: options.oauth_token_url,
            oauth_client_id: options.oauth_client_id,
            oauth_client_secret: options.oauth_client_secret,
            oauth_scope: options.oauth_scope,
            oauth_authorization_url: options.oauth_authorization_url,
            oauth_device_authorization_url: options.oauth_device_authorization_url,
            oauth_redirect_uri: options.oauth_redirect_uri,
            oauth_code: options.oauth_code,
            oauth_code_verifier: options.oauth_code_verifier,
            oauth_refresh_token: options.oauth_refresh_token,
            oauth_browser: options.oauth_browser,
            aws_access_key_id: options.aws_access_key_id,
            aws_secret_access_key: options.aws_secret_access_key,
            aws_region: options.aws_region,
            aws_service: options.aws_service,
            aws_session_token: options.aws_session_token,
        },
    )?;
    request.body = parse_cli_body(options.data, options.json_body)?;
    let response = execute(
        &request,
        VariableContext::default(),
        ExecuteOptions {
            timeout: options.timeout,
            max_redirects: options.max_redirects,
            proxy: options.proxy.as_deref(),
            no_proxy: options.no_proxy.as_deref(),
            ca_cert: options.ca_cert.as_deref(),
            client_identity: options.client_identity.as_deref(),
            insecure: options.insecure,
            cookie_jar: None,
            oauth_browser: options.oauth_browser,
        },
    )
    .await?;
    print_response(&response, options.output_json)?;
    Ok(())
}

async fn send_graphql_request(options: GraphqlOptions) -> Result<()> {
    let query = match (options.query, options.query_file) {
        (Some(query), None) => query,
        (None, Some(path)) => fs::read_to_string(&path)
            .with_context(|| format!("could not read GraphQL query file {}", path.display()))?,
        (None, None) => bail!("provide either --query or --query-file"),
        (Some(_), Some(_)) => bail!("choose either --query or --query-file"),
    };
    let variables = parse_graphql_variables(options.variables_json, &options.variables)?;
    let mut request = GraphqlRequest {
        endpoint: options.endpoint,
        query,
        variables,
        operation_name: options.operation_name,
    }
    .into_http_request("GraphQL CLI request")?;
    request.headers.extend(parse_headers(&options.headers)?);
    request.auth = parse_auth_flags(options.bearer, options.basic_user, options.basic_password)?;

    let response = execute(
        &request,
        VariableContext::default(),
        ExecuteOptions {
            timeout: options.timeout,
            max_redirects: options.max_redirects,
            proxy: options.proxy.as_deref(),
            no_proxy: options.no_proxy.as_deref(),
            ca_cert: options.ca_cert.as_deref(),
            client_identity: options.client_identity.as_deref(),
            insecure: options.insecure,
            cookie_jar: None,
            oauth_browser: false,
        },
    )
    .await?;
    let graphql = parse_graphql_response(&response.body_text())?;
    if options.output_json {
        let payload = json!({
            "status": response.status,
            "status_text": response.status_text,
            "headers": response.headers,
            "duration_ms": response.duration_ms,
            "ttfb_ms": response.ttfb_ms,
            "download_ms": response.download_ms,
            "protocol": response.protocol,
            "url": response.url,
            "graphql": graphql,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!(
            "{} {} · {} ms · TTFB {} ms · download {} ms · {} bytes · {}",
            response.status,
            response.status_text,
            response.duration_ms,
            response.ttfb_ms,
            response.download_ms,
            response.response_size,
            response.protocol
        );
        println!("{}", serde_json::to_string_pretty(&graphql)?);
    }
    if graphql.has_errors() {
        let messages = graphql.error_messages();
        let detail = if messages.is_empty() {
            format!("{} GraphQL error(s)", graphql.errors.len())
        } else {
            messages.join("; ")
        };
        bail!("GraphQL response contains errors: {detail}");
    }
    Ok(())
}

async fn introspect_graphql_schema(options: GraphqlOptions) -> Result<()> {
    let mut request = GraphqlRequest::new(options.endpoint.clone(), schema_introspection_query())
        .into_http_request("GraphQL schema introspection")?;
    request.headers.extend(parse_headers(&options.headers)?);
    request.auth = parse_auth_flags(options.bearer, options.basic_user, options.basic_password)?;
    let response = execute(
        &request,
        VariableContext::default(),
        ExecuteOptions {
            timeout: options.timeout,
            max_redirects: options.max_redirects,
            proxy: options.proxy.as_deref(),
            no_proxy: options.no_proxy.as_deref(),
            ca_cert: options.ca_cert.as_deref(),
            client_identity: options.client_identity.as_deref(),
            insecure: options.insecure,
            cookie_jar: None,
            oauth_browser: false,
        },
    )
    .await?;
    if response.status >= 400 {
        bail!(
            "GraphQL introspection endpoint returned {} {}",
            response.status,
            response.status_text
        );
    }
    let graphql = parse_graphql_response(&response.body_text())?;
    let schema = parse_graphql_schema(&graphql)?;
    if options.output_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "status": response.status,
                "status_text": response.status_text,
                "duration_ms": response.duration_ms,
                "ttfb_ms": response.ttfb_ms,
                "download_ms": response.download_ms,
                "protocol": response.protocol,
                "url": response.url,
                "schema": schema,
            }))?
        );
        return Ok(());
    }

    println!(
        "GraphQL schema · {} {} · {} ms · TTFB {} ms · download {} ms",
        response.status,
        response.status_text,
        response.duration_ms,
        response.ttfb_ms,
        response.download_ms
    );
    println!(
        "Roots: query={} · mutation={} · subscription={}",
        schema.query_type.as_deref().unwrap_or("—"),
        schema.mutation_type.as_deref().unwrap_or("—"),
        schema.subscription_type.as_deref().unwrap_or("—")
    );
    println!("Named types: {}", schema.types.len());
    for (label, root) in [
        ("Query", schema.query_type.as_deref()),
        ("Mutation", schema.mutation_type.as_deref()),
        ("Subscription", schema.subscription_type.as_deref()),
    ] {
        let Some(root) = root.and_then(|name| schema.named_type(name)) else {
            continue;
        };
        println!();
        println!("{label} · {}", root.name);
        for field in &root.fields {
            let arguments = if field.arguments.is_empty() {
                String::new()
            } else {
                format!(
                    "({})",
                    field
                        .arguments
                        .iter()
                        .map(|argument| format!("{}: {}", argument.name, argument.type_name))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            let deprecated = if field.deprecated {
                " [deprecated]"
            } else {
                ""
            };
            println!(
                "  {}{}: {}{}",
                field.name, arguments, field.type_name, deprecated
            );
        }
    }
    Ok(())
}

fn describe_grpc(proto: &Path, includes: &[PathBuf], output_json: bool) -> Result<()> {
    let schema = GrpcSchema::from_proto(proto, includes)
        .with_context(|| format!("could not load protobuf schema {}", proto.display()))?;
    let services = schema.services();
    if output_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "source": schema.source(),
                "files": schema.files(),
                "services": services,
            }))?
        );
        return Ok(());
    }

    println!("gRPC schema: {}", schema.source().display());
    if services.is_empty() {
        println!("No services found.");
        return Ok(());
    }
    for service in services {
        println!("{}", service.full_name);
        for method in service.methods {
            let streaming = match (method.client_streaming, method.server_streaming) {
                (false, false) => "unary",
                (false, true) => "server-streaming",
                (true, false) => "client-streaming",
                (true, true) => "bidi-streaming",
            };
            println!(
                "  {}  [{}]  {} -> {}",
                method.path, streaming, method.input, method.output
            );
        }
    }
    Ok(())
}

async fn reflect_grpc(options: GrpcReflectCommand) -> Result<()> {
    let endpoint_url = url::Url::parse(&options.endpoint)
        .with_context(|| format!("invalid gRPC endpoint: {}", options.endpoint))?;
    let host = options.host.unwrap_or_default();
    let endpoint_config = configure_grpc_endpoint(
        &options.endpoint,
        options.timeout,
        options.tls.ca_cert.as_deref(),
        options.tls.client_identity.as_deref(),
    )?;
    let channel = connect_grpc_endpoint(
        endpoint_config,
        &options.endpoint,
        options.proxy.as_deref(),
        options.no_proxy.as_deref(),
    )
    .await
    .with_context(|| format!("could not connect to gRPC endpoint {}", options.endpoint))?;
    let schema = GrpcSchema::from_reflection(channel, host.clone())
        .await
        .with_context(|| format!("could not reflect gRPC endpoint {}", options.endpoint))?;
    let services = schema.services();
    if options.output_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "endpoint": endpoint_url.as_str(),
                "host": host,
                "source": schema.source(),
                "files": schema.files(),
                "services": services,
            }))?
        );
        return Ok(());
    }

    println!("gRPC reflection: {}", options.endpoint);
    if services.is_empty() {
        println!("No services found.");
        return Ok(());
    }
    for service in services {
        println!("{}", service.full_name);
        for method in service.methods {
            let streaming = match (method.client_streaming, method.server_streaming) {
                (false, false) => "unary",
                (false, true) => "server-streaming",
                (true, false) => "client-streaming",
                (true, true) => "bidi-streaming",
            };
            println!(
                "  {}  [{}]  {} -> {}",
                method.path, streaming, method.input, method.output
            );
        }
    }
    Ok(())
}

async fn call_grpc(options: GrpcCallOptions) -> Result<()> {
    let passphrase = client_identity_passphrase(options.client_identity.as_deref());
    call_grpc_with_passphrase(options, passphrase.as_deref()).await
}

async fn call_grpc_with_passphrase(
    options: GrpcCallOptions,
    passphrase: Option<&str>,
) -> Result<()> {
    let schema = GrpcSchema::from_proto(&options.proto, &options.includes)
        .with_context(|| format!("could not load protobuf schema {}", options.proto.display()))?;
    let method = schema
        .find_method(&options.method)
        .with_context(|| format!("gRPC method not found: {}", options.method))?;

    let message = match (&options.message, &options.message_file) {
        (Some(message), None) => message.clone(),
        (None, Some(path)) => fs::read_to_string(path)
            .with_context(|| format!("could not read protobuf JSON file {}", path.display()))?,
        (None, None) if method.is_client_streaming() => {
            bail!("client-streaming gRPC methods require --message or --message-file")
        }
        (None, None) => "{}".to_owned(),
        (Some(_), Some(_)) => bail!("choose either --message or --message-file"),
    };
    let request_message = if method.is_client_streaming() {
        None
    } else {
        Some(message_from_json(method.input(), &message)?)
    };
    let stream_messages = if method.is_client_streaming() {
        parse_grpc_stream_messages(method.input(), &message)?
    } else {
        Vec::new()
    };

    let endpoint_config = configure_grpc_endpoint_with_passphrase(
        &options.endpoint,
        options.timeout,
        options.ca_cert.as_deref(),
        options.client_identity.as_deref(),
        passphrase,
    )?;
    let channel = connect_grpc_endpoint(
        endpoint_config,
        &options.endpoint,
        options.proxy.as_deref(),
        options.no_proxy.as_deref(),
    )
    .await
    .with_context(|| format!("could not connect to gRPC endpoint {}", options.endpoint))?;
    let method_path = format!("/{}/{}", method.parent_service().full_name(), method.name());
    let path = http::uri::PathAndQuery::try_from(method_path.clone())?;
    let mut grpc = tonic::client::Grpc::new(channel);
    grpc.ready().await?;
    if method.is_client_streaming() {
        let input_count = stream_messages.len();
        let mut request = tonic::Request::new(futures_util::stream::iter(stream_messages));
        apply_grpc_metadata(&mut request, &options)?;
        if method.is_server_streaming() {
            let response = grpc
                .streaming(
                    request,
                    path,
                    DynamicGrpcCodec {
                        output: method.output(),
                    },
                )
                .await?;
            let mut stream = response.into_inner();
            let mut index = 0_u64;
            while let Some(message) = stream.message().await? {
                let message = message_to_json(&message)?;
                if options.output_json {
                    println!(
                        "{}",
                        serde_json::to_string(&json!({
                            "method": method_path,
                            "stream_index": index,
                            "input_count": input_count,
                            "response": message,
                        }))?
                    );
                } else {
                    println!("gRPC {method_path} · message {index}");
                    println!("{}", serde_json::to_string_pretty(&message)?);
                }
                index = index.saturating_add(1);
            }
        } else {
            let response = grpc
                .client_streaming(
                    request,
                    path,
                    DynamicGrpcCodec {
                        output: method.output(),
                    },
                )
                .await?;
            let response_message = message_to_json(&response.into_inner())?;
            if options.output_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "method": method_path,
                        "input_count": input_count,
                        "output": method.output().full_name(),
                        "response": response_message,
                    }))?
                );
            } else {
                println!("gRPC {method_path} · {input_count} input messages");
                println!("{}", serde_json::to_string_pretty(&response_message)?);
            }
        }
        return Ok(());
    }
    let mut request = tonic::Request::new(
        request_message.expect("non-client-streaming methods have a unary request message"),
    );
    apply_grpc_metadata(&mut request, &options)?;
    if method.is_server_streaming() {
        let response = grpc
            .server_streaming(
                request,
                path,
                DynamicGrpcCodec {
                    output: method.output(),
                },
            )
            .await?;
        let mut stream = response.into_inner();
        let mut index = 0_u64;
        while let Some(message) = stream.message().await? {
            let message = message_to_json(&message)?;
            if options.output_json {
                println!(
                    "{}",
                    serde_json::to_string(&json!({
                        "method": method_path,
                        "stream_index": index,
                        "response": message,
                    }))?
                );
            } else {
                println!("gRPC {method_path} · message {index}");
                println!("{}", serde_json::to_string_pretty(&message)?);
            }
            index = index.saturating_add(1);
        }
        return Ok(());
    }
    let response = grpc
        .unary(
            request,
            path,
            DynamicGrpcCodec {
                output: method.output(),
            },
        )
        .await?;
    let response_message = message_to_json(&response.into_inner())?;
    if options.output_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "method": method_path,
                "input": method.input().full_name(),
                "output": method.output().full_name(),
                "response": response_message,
            }))?
        );
    } else {
        println!("gRPC {method_path}");
        println!("{}", serde_json::to_string_pretty(&response_message)?);
    }
    Ok(())
}

async fn stream_sse(options: SseOptions) -> Result<()> {
    let mut base_request = Request::new("SSE CLI subscription", "GET", options.endpoint);
    base_request.headers = parse_headers(&options.headers)?;
    if !base_request
        .headers
        .iter()
        .any(|header| header.enabled && header.key.eq_ignore_ascii_case("accept"))
    {
        base_request
            .headers
            .push(HeaderEntry::enabled("accept", "text/event-stream"));
    }
    base_request.auth =
        parse_auth_flags(options.bearer, options.basic_user, options.basic_password)?;

    let engine = HttpEngine::new(&EngineOptions {
        timeout: Duration::from_secs(options.timeout),
        max_redirects: options.max_redirects,
        accept_invalid_certs: options.insecure,
        proxy: options.proxy.clone(),
        no_proxy: options.no_proxy.clone(),
        ca_cert: options.ca_cert.clone(),
        client_identity: options.client_identity.clone(),
        client_identity_passphrase: client_identity_passphrase(options.client_identity.as_deref()),
        cookie_jar: None,
        ..EngineOptions::default()
    })?;
    let mut reconnects_used = 0;
    let mut last_event_id = None;
    loop {
        let mut request = base_request.clone();
        if let Some(last_event_id) = &last_event_id {
            if let Some(header) = request
                .headers
                .iter_mut()
                .find(|header| header.enabled && header.key.eq_ignore_ascii_case("last-event-id"))
            {
                header.value.clone_from(last_event_id);
            } else {
                request
                    .headers
                    .push(HeaderEntry::enabled("last-event-id", last_event_id));
            }
        }
        let mut response = match engine
            .execute_stream(&request, &VariableContext::default())
            .await
        {
            Ok(response) => response,
            Err(error) if reconnects_used < options.reconnect => {
                reconnects_used += 1;
                print_sse_state("reconnecting", options.output_json)?;
                eprintln!("SSE connection failed, retrying: {error}");
                tokio::time::sleep(Duration::from_millis(250)).await;
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if response.status >= 400 {
            let body = response.response.text().await.unwrap_or_default();
            bail!(
                "SSE endpoint returned {} {}{}",
                response.status,
                response.status_text,
                if body.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", body.trim())
                }
            );
        }
        print_sse_state("connected", options.output_json)?;
        if !options.output_json {
            println!(
                "{} {} · {} · {}",
                response.status,
                response.status_text,
                response.content_type.as_deref().unwrap_or("SSE"),
                response.url
            );
        }

        let mut parser = SseParser::default();
        let mut retry_delay_ms = 250;
        while let Some(chunk) = response.response.chunk().await? {
            for event in parser.feed_bytes(&chunk)? {
                if let Some(id) = &event.id {
                    last_event_id = Some(id.clone());
                }
                if let Some(retry_ms) = event.retry_ms {
                    retry_delay_ms = retry_ms;
                }
                print_sse_event(&event, options.output_json)?;
            }
        }
        for event in parser.finish()? {
            if let Some(id) = &event.id {
                last_event_id = Some(id.clone());
            }
            if let Some(retry_ms) = event.retry_ms {
                retry_delay_ms = retry_ms;
            }
            print_sse_event(&event, options.output_json)?;
        }
        if reconnects_used >= options.reconnect {
            print_sse_state("closed", options.output_json)?;
            return Ok(());
        }
        reconnects_used += 1;
        print_sse_state("reconnecting", options.output_json)?;
        tokio::time::sleep(Duration::from_millis(retry_delay_ms)).await;
    }
}

fn print_sse_state(state: &str, output_json: bool) -> Result<()> {
    if output_json {
        println!(
            "{}",
            serde_json::to_string(&json!({"type": "connection", "state": state}))?
        );
    } else {
        println!("SSE {state}");
    }
    io::stdout().flush()?;
    Ok(())
}

async fn run_websocket(options: WebsocketOptions) -> Result<()> {
    let client_identity_passphrase = options
        .client_identity
        .as_deref()
        .and_then(|path| client_identity_passphrase(Some(path)));
    let tls_connector = build_websocket_tls_connector(
        &options.endpoint,
        options.ca_cert.as_deref(),
        options.client_identity.as_deref(),
        options.insecure,
        client_identity_passphrase.as_deref(),
    )?;
    let mut reconnects_used = 0;
    loop {
        let websocket_request = build_websocket_request(&options)?;
        let connection = tokio::time::timeout(
            Duration::from_secs(options.timeout),
            connect_websocket(
                websocket_request,
                options.proxy.as_deref(),
                options.no_proxy.as_deref(),
                tls_connector.clone(),
            ),
        )
        .await
        .with_context(|| {
            format!(
                "WebSocket handshake did not complete within {} seconds",
                options.timeout
            )
        })?;
        let (mut socket, response) = match connection {
            Ok(connection) => connection,
            Err(error) if reconnects_used < options.reconnect => {
                reconnects_used += 1;
                print_websocket_state("reconnecting", options.output_json)?;
                tokio::time::sleep(Duration::from_millis(250)).await;
                eprintln!("WebSocket connection failed, retrying: {error}");
                continue;
            }
            Err(error) => return Err(error),
        };
        print_websocket_state("connected", options.output_json)?;
        if !options.output_json {
            println!("HTTP handshake: {}", response.status());
        }
        for message in &options.send {
            socket.send(Message::text(message)).await?;
        }

        let server_closed = loop {
            let next = tokio::time::timeout(Duration::from_secs(options.timeout), socket.next())
                .await
                .with_context(|| {
                    format!(
                        "no WebSocket message received within {} seconds",
                        options.timeout
                    )
                })?;
            let Some(message) = next else {
                break true;
            };
            match message? {
                Message::Text(text) => {
                    print_websocket_message("text", text.to_string(), options.output_json)?
                }
                Message::Binary(bytes) => print_websocket_message(
                    "binary",
                    base64::engine::general_purpose::STANDARD.encode(bytes),
                    options.output_json,
                )?,
                Message::Ping(bytes) => {
                    socket.send(Message::Pong(bytes)).await?;
                }
                Message::Pong(bytes) => print_websocket_message(
                    "pong",
                    base64::engine::general_purpose::STANDARD.encode(bytes),
                    options.output_json,
                )?,
                Message::Close(_) => {
                    break true;
                }
                Message::Frame(_) => {}
            }
        };
        if server_closed && reconnects_used < options.reconnect {
            reconnects_used += 1;
            print_websocket_state("reconnecting", options.output_json)?;
            tokio::time::sleep(Duration::from_millis(250)).await;
            continue;
        }
        print_websocket_state("closed", options.output_json)?;
        return Ok(());
    }
}

fn build_websocket_tls_connector(
    endpoint: &str,
    ca_cert: Option<&Path>,
    client_identity: Option<&Path>,
    insecure: bool,
    client_identity_passphrase: Option<&str>,
) -> Result<Option<Connector>> {
    let scheme = url::Url::parse(endpoint)
        .with_context(|| format!("invalid WebSocket endpoint: {endpoint}"))?
        .scheme()
        .to_owned();
    let has_tls_options = ca_cert.is_some() || client_identity.is_some() || insecure;
    if scheme != "wss" {
        if has_tls_options {
            bail!("WebSocket CA, client identity and insecure options require a wss:// endpoint");
        }
        return Ok(None);
    }
    if !has_tls_options {
        return Ok(None);
    }

    let mut builder = native_tls::TlsConnector::builder();
    builder.danger_accept_invalid_certs(insecure);
    if let Some(path) = ca_cert {
        let pem = fs::read(path).with_context(|| {
            format!("could not read WebSocket CA certificate {}", path.display())
        })?;
        if pem.is_empty() {
            bail!("WebSocket CA certificate {} is empty", path.display());
        }
        let certificate = native_tls::Certificate::from_pem(&pem)
            .with_context(|| format!("invalid WebSocket CA certificate {}", path.display()))?;
        builder.add_root_certificate(certificate);
    }
    if let Some(path) = client_identity {
        let identity = fs::read(path).with_context(|| {
            format!(
                "could not read WebSocket client identity {}",
                path.display()
            )
        })?;
        if identity.is_empty() {
            bail!("WebSocket client identity {} is empty", path.display());
        }
        let identity = if path.extension().is_some_and(|extension| {
            let extension = extension.to_string_lossy();
            extension.eq_ignore_ascii_case("p12") || extension.eq_ignore_ascii_case("pfx")
        }) {
            let passphrase = client_identity_passphrase.context(
                "set POSTLY_CLIENT_IDENTITY_PASSPHRASE for a PKCS#12 WebSocket identity",
            )?;
            native_tls::Identity::from_pkcs12(&identity, passphrase).with_context(|| {
                format!(
                    "invalid PKCS#12 WebSocket client identity {}",
                    path.display()
                )
            })?
        } else {
            let (certificate, private_key) = split_pkcs8_pem_identity(&identity, path)?;
            native_tls::Identity::from_pkcs8(&certificate, &private_key).with_context(|| {
                format!("invalid PEM WebSocket client identity {}", path.display())
            })?
        };
        builder.identity(identity);
    }
    Ok(Some(Connector::NativeTls(
        builder
            .build()
            .context("could not build the WebSocket TLS connector")?,
    )))
}

fn split_pkcs8_pem_identity(pem: &[u8], path: &Path) -> Result<(Vec<u8>, Vec<u8>)> {
    let text = std::str::from_utf8(pem).with_context(|| {
        format!(
            "WebSocket client identity {} is not UTF-8 PEM",
            path.display()
        )
    })?;
    let key_start = text
        .find("-----BEGIN PRIVATE KEY-----")
        .context("WebSocket PEM client identity must contain a PKCS#8 PRIVATE KEY block")?;
    let key_end = text[key_start..]
        .find("-----END PRIVATE KEY-----")
        .map(|offset| key_start + offset + "-----END PRIVATE KEY-----".len())
        .context("WebSocket PEM client identity has an incomplete PRIVATE KEY block")?;
    let certificate = text[..key_start].trim();
    if !certificate.contains("-----BEGIN CERTIFICATE-----") {
        bail!("WebSocket PEM client identity must contain a certificate chain");
    }
    Ok((
        certificate.as_bytes().to_vec(),
        text.as_bytes()[key_start..key_end].to_vec(),
    ))
}

async fn connect_websocket(
    request: tokio_tungstenite::tungstenite::http::Request<()>,
    proxy_url: Option<&str>,
    no_proxy: Option<&str>,
    connector: Option<Connector>,
) -> Result<(
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tokio_tungstenite::tungstenite::handshake::client::Response,
)> {
    let Some(proxy_url) = proxy_url.filter(|value| !value.trim().is_empty()) else {
        return Ok(connect_async_tls_with_config(request, None, false, connector).await?);
    };
    let target_host = request
        .uri()
        .host()
        .context("WebSocket endpoint has no hostname")?;
    let target_port = request
        .uri()
        .port_u16()
        .or_else(|| (request.uri().scheme_str() == Some("wss")).then_some(443))
        .or_else(|| (request.uri().scheme_str() == Some("ws")).then_some(80))
        .context("WebSocket endpoint has no port")?;
    if no_proxy.is_some_and(|rules| no_proxy_matches(target_host, target_port, rules)) {
        return Ok(connect_async_tls_with_config(request, None, false, connector).await?);
    }

    let proxy = url::Url::parse(proxy_url)
        .with_context(|| format!("invalid WebSocket proxy URL: {proxy_url}"))?;
    if matches!(proxy.scheme(), "socks5" | "socks5h") {
        let socket = connect_socks5_stream(&proxy, target_host, target_port)
            .await
            .map_err(anyhow::Error::msg)?;
        return Ok(client_async_tls_with_config(request, socket, None, connector).await?);
    }
    if proxy.scheme() != "http" {
        bail!(
            "WebSocket proxy routing supports http://, socks5:// and socks5h:// proxies; {} is not supported",
            proxy.scheme()
        );
    }
    let proxy_host = proxy
        .host_str()
        .context("WebSocket proxy URL has no hostname")?;
    let proxy_port = proxy
        .port_or_known_default()
        .context("WebSocket proxy URL has no port")?;
    let mut socket = tokio::net::TcpStream::connect((proxy_host, proxy_port))
        .await
        .with_context(|| {
            format!("could not connect to WebSocket proxy {proxy_host}:{proxy_port}")
        })?;
    let mut connect_request = format!(
        "CONNECT {target_host}:{target_port} HTTP/1.1\r\nHost: {target_host}:{target_port}\r\n"
    );
    if !proxy.username().is_empty() {
        let credentials = if let Some(password) = proxy.password() {
            format!("{}:{password}", proxy.username())
        } else {
            format!("{}:", proxy.username())
        };
        connect_request.push_str(&format!(
            "Proxy-Authorization: Basic {}\r\n",
            base64::engine::general_purpose::STANDARD.encode(credentials)
        ));
    }
    connect_request.push_str("\r\n");
    socket.write_all(connect_request.as_bytes()).await?;

    let mut response = Vec::new();
    let mut buffer = [0_u8; 1024];
    while !response.windows(4).any(|window| window == b"\r\n\r\n") {
        let count = socket.read(&mut buffer).await?;
        if count == 0 {
            bail!("WebSocket proxy closed the CONNECT handshake");
        }
        if response.len().saturating_add(count) > 64 * 1024 {
            bail!("WebSocket proxy response exceeds 65536 bytes");
        }
        response.extend_from_slice(&buffer[..count]);
    }
    let status_line = String::from_utf8_lossy(&response)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or_default();
    if status != 200 {
        bail!("WebSocket proxy CONNECT failed with HTTP {status}");
    }
    Ok(client_async_tls_with_config(request, socket, None, connector).await?)
}

fn no_proxy_matches(host: &str, port: u16, rules: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    rules
        .split(|character: char| character == ',' || character.is_ascii_whitespace())
        .filter(|rule| !rule.trim().is_empty())
        .any(|rule| {
            let rule = rule.trim();
            if rule == "*" {
                return true;
            }
            let (rule, rule_port) = if !rule.starts_with('[') {
                rule.rsplit_once(':')
                    .and_then(|(host, port)| {
                        port.parse::<u16>().ok().map(|port| (host, Some(port)))
                    })
                    .unwrap_or((rule, None))
            } else {
                (rule, None)
            };
            if rule_port.is_some_and(|rule_port| rule_port != port) {
                return false;
            }
            let rule = rule.trim_start_matches('.').to_ascii_lowercase();
            host == rule || host.ends_with(&format!(".{rule}"))
        })
}

fn build_websocket_request(
    options: &WebsocketOptions,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>> {
    let mut websocket_request = options
        .endpoint
        .clone()
        .into_client_request()
        .context("WebSocket endpoint must be a valid ws:// or wss:// URL")?;
    for header in parse_headers(&options.headers)? {
        let name = HeaderName::from_bytes(header.key.as_bytes())
            .with_context(|| format!("invalid WebSocket header name: {}", header.key))?;
        let value = HeaderValue::from_str(&header.value)
            .with_context(|| format!("invalid WebSocket header value: {}", header.key))?;
        websocket_request.headers_mut().insert(name, value);
    }
    match parse_auth_flags(
        options.bearer.clone(),
        options.basic_user.clone(),
        options.basic_password.clone(),
    )? {
        Auth::None => {}
        Auth::Bearer { token } => {
            let value = HeaderValue::from_str(&format!("Bearer {token}"))?;
            websocket_request
                .headers_mut()
                .insert(HeaderName::from_static("authorization"), value);
        }
        Auth::Basic { username, password } => {
            let credentials =
                base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
            let value = HeaderValue::from_str(&format!("Basic {credentials}"))?;
            websocket_request
                .headers_mut()
                .insert(HeaderName::from_static("authorization"), value);
        }
        Auth::Digest { .. } => {
            unreachable!("CLI auth flags do not create Digest credentials")
        }
        Auth::ApiKey { .. } => unreachable!("CLI auth flags do not create API keys"),
        Auth::OAuth2ClientCredentials { .. } => {
            unreachable!("CLI auth flags do not create OAuth 2.0 credentials")
        }
        Auth::OAuth2AuthorizationCodePkce { .. } => {
            unreachable!("CLI auth flags do not create OAuth 2.0 credentials")
        }
        Auth::OAuth2RefreshToken { .. } => {
            unreachable!("CLI auth flags do not create OAuth 2.0 credentials")
        }
        Auth::OAuth2DeviceCode { .. } => {
            unreachable!("CLI auth flags do not create OAuth 2.0 credentials")
        }
        Auth::AwsSignatureV4 { .. } => {
            unreachable!("CLI auth flags do not create AWS Signature V4 credentials")
        }
    }
    Ok(websocket_request)
}

fn print_websocket_state(state: &str, output_json: bool) -> Result<()> {
    if output_json {
        println!(
            "{}",
            serde_json::to_string(&json!({"type": "connection", "state": state}))?
        );
    } else {
        println!("WebSocket {state}");
    }
    io::stdout().flush()?;
    Ok(())
}

fn print_websocket_message(kind: &str, data: String, output_json: bool) -> Result<()> {
    if output_json {
        println!(
            "{}",
            serde_json::to_string(&json!({"type": kind, "data": data}))?
        );
    } else {
        println!("{kind}: {data}");
    }
    io::stdout().flush()?;
    Ok(())
}

fn print_sse_event(event: &postly_core::SseEvent, output_json: bool) -> Result<()> {
    if output_json {
        println!("{}", serde_json::to_string(event)?);
    } else {
        let event_name = event.event.as_deref().unwrap_or("message");
        let event_id = event
            .id
            .as_deref()
            .map(|id| format!(" · id={id}"))
            .unwrap_or_default();
        println!("event {event_name}{event_id}");
        println!("{}", event.data);
        if let Some(retry_ms) = event.retry_ms {
            println!("retry: {retry_ms} ms");
        }
        println!();
    }
    io::stdout().flush()?;
    Ok(())
}

fn parse_graphql_variables(
    variables_json: Option<String>,
    variables: &[String],
) -> Result<serde_json::Value> {
    if variables_json.is_some() && !variables.is_empty() {
        bail!("choose either --variables-json or --variable");
    }
    if let Some(variables_json) = variables_json {
        return Ok(parse_variables_json(&variables_json)?);
    }
    let mut values = serde_json::Map::new();
    for variable in variables {
        let (key, value) = parse_assignment(variable)?;
        values.insert(key.to_owned(), serde_json::Value::String(value.to_owned()));
    }
    Ok(serde_json::Value::Object(values))
}

async fn send_saved_request(options: SendOptions<'_>) -> Result<()> {
    let workspace = find_workspace(options.file)?;
    let mut request = workspace.load_request(options.file)?;
    let collections = workspace.collections()?;
    let collection = collections
        .iter()
        .find(|collection| options.file.starts_with(&collection.directory));
    let mut context = context_for_collection(
        &workspace,
        collection.map(|collection| &collection.collection),
        options.environment_name,
    )?;
    if options.scripts {
        if let Some(script) = request.pre_request_script.clone() {
            let script_result =
                run_script_async(script, request.clone(), None, context.clone()).await?;
            script_result.apply(&mut request, &mut context)?;
        }
    }
    let started = Instant::now();
    let result = execute(
        &request,
        context.clone(),
        ExecuteOptions {
            timeout: options.timeout,
            max_redirects: options.max_redirects,
            proxy: options.proxy,
            no_proxy: options.no_proxy,
            ca_cert: options.ca_cert,
            client_identity: options.client_identity,
            insecure: options.insecure,
            cookie_jar: Some(&workspace.root().join(".postly/cookies.json")),
            oauth_browser: options.oauth_browser,
        },
    )
    .await;
    let history_entry = match &result {
        Ok(response) => HistoryEntry::from_response(&request, response),
        Err(_) => HistoryEntry::from_error(&request, started.elapsed().as_millis() as u64),
    };
    if let Err(error) = workspace.record_history(&history_entry) {
        tracing::warn!(error = %error, "could not write local request history");
    }
    let response = result?;
    let post_script = if options.scripts {
        if let Some(script) = request.test_script.clone() {
            Some(
                run_script_async(
                    script,
                    request.clone(),
                    Some(response.clone()),
                    context.clone(),
                )
                .await?,
            )
        } else {
            None
        }
    } else {
        None
    };
    if let Some(script_result) = &post_script {
        script_result.apply(&mut request, &mut context)?;
    }
    let assertion_failures = evaluate_response_assertions(&request.assertions, &response);
    print_response_with_tests(
        &response,
        options.output_json,
        post_script.as_ref().map(|script| script.tests.as_slice()),
        Some((request.assertions.len(), assertion_failures.as_slice())),
    )?;
    if !assertion_failures.is_empty() {
        bail!("native response assertions failed");
    }
    if post_script
        .as_ref()
        .is_some_and(|script| script.failed_tests().next().is_some())
    {
        bail!("script assertions failed");
    }
    Ok(())
}

async fn import_command(kind: ImportKind) -> Result<()> {
    match kind {
        ImportKind::Collection { input, output } => {
            let report = import_postman_collection(input, output)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        ImportKind::Environment {
            input,
            output,
            secure,
        } => {
            let secret_store = secure.then(|| SecretStore::for_workspace(&output));
            let report = import_environment_with_store(input, output, secret_store.as_ref())?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        ImportKind::Dotenv {
            input,
            output,
            name,
            secrets,
        } => {
            let secret_store = SecretStore::for_workspace(&output);
            let report = import_dotenv(input, &output, &name, &secrets, &secret_store)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        ImportKind::Openapi { input, output } => {
            let report = import_openapi_source(&input, &output).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        ImportKind::Curl {
            command,
            output,
            collection,
            name,
        } => {
            let result = import_curl_command(&command, output, &collection, &name)?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
    }
    Ok(())
}

const MAX_OPENAPI_DOWNLOAD_BYTES: usize = 16 * 1024 * 1024;

async fn import_openapi_source(
    input: &Path,
    output: &Path,
) -> Result<postly_core::OpenApiImportReport> {
    let input_label = input.to_string_lossy().to_string();
    let is_http_url = url::Url::parse(&input_label)
        .map(|url| matches!(url.scheme(), "http" | "https"))
        .unwrap_or(false);
    if !is_http_url {
        return Ok(postly_core::import_openapi_with_remote_refs(input, output).await?);
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .context("could not build the bounded OpenAPI HTTP client")?;
    let mut response = client
        .get(&input_label)
        .send()
        .await
        .with_context(|| format!("could not fetch OpenAPI URL {input_label}"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("OpenAPI URL returned HTTP {status}: {input_label}");
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_OPENAPI_DOWNLOAD_BYTES as u64)
    {
        bail!("OpenAPI URL response exceeds {MAX_OPENAPI_DOWNLOAD_BYTES} bytes");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("could not read OpenAPI URL response")?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_OPENAPI_DOWNLOAD_BYTES {
            bail!("OpenAPI URL response exceeds {MAX_OPENAPI_DOWNLOAD_BYTES} bytes");
        }
        bytes.extend_from_slice(&chunk);
    }
    let text = String::from_utf8(bytes).context("OpenAPI URL response is not UTF-8")?;
    Ok(
        postly_core::import_openapi_text_with_remote_refs(input, &input_label, &text, output)
            .await?,
    )
}

fn export_command(kind: ExportKind) -> Result<()> {
    match kind {
        ExportKind::Collection {
            workspace,
            collection,
            output,
        } => {
            let workspace = Workspace::open(&workspace)?;
            let collections = workspace.collections()?;
            let collection = match collection {
                Some(name) => collections
                    .iter()
                    .find(|candidate| {
                        candidate.collection.name == name
                            || candidate.collection.name.eq_ignore_ascii_case(&name)
                    })
                    .with_context(|| format!("collection not found: {name}"))?,
                None => collections.first().with_context(|| {
                    if collections.len() > 1 {
                        "multiple collections found; pass --collection"
                    } else {
                        "no collections found"
                    }
                })?,
            };
            let report = export_postman_collection(&workspace, collection, output)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        ExportKind::Openapi {
            workspace,
            collection,
            output,
        } => {
            let workspace = Workspace::open(&workspace)?;
            let collections = workspace.collections()?;
            let collection = match collection {
                Some(name) => collections
                    .iter()
                    .find(|candidate| {
                        candidate.collection.name == name
                            || candidate.collection.name.eq_ignore_ascii_case(&name)
                    })
                    .with_context(|| format!("collection not found: {name}"))?,
                None => collections.first().with_context(|| {
                    if collections.len() > 1 {
                        "multiple collections found; pass --collection"
                    } else {
                        "no collections found"
                    }
                })?,
            };
            let report = export_openapi_collection(&workspace, collection, output)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        ExportKind::Environment {
            workspace,
            name,
            output,
        } => {
            let workspace = Workspace::open(&workspace)?;
            let environment = workspace
                .environments()?
                .into_iter()
                .find(|(_, candidate)| {
                    candidate.name == name || candidate.name.eq_ignore_ascii_case(&name)
                })
                .with_context(|| format!("environment not found: {name}"))?
                .1;
            let secret_store = SecretStore::for_workspace(workspace.root());
            let report =
                export_postman_environment_with_store(&environment, &secret_store, output)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
    }
    Ok(())
}

fn set_environment(
    workspace_path: &Path,
    name: &str,
    values: &[String],
    secrets: &[String],
    secret_stdin: &[String],
) -> Result<()> {
    let stdin_secrets = read_secret_stdin(secret_stdin)?;
    let workspace = Workspace::open_or_init(workspace_path, "Postly workspace")?;
    let secret_store = SecretStore::for_workspace(workspace.root());
    let mut environment = workspace
        .environments()?
        .into_iter()
        .find(|(_, environment)| environment.name == name)
        .map(|(_, environment)| environment)
        .unwrap_or_else(|| Environment::new(name));
    for assignment in values {
        let (key, value) = parse_assignment(assignment)?;
        environment
            .variables
            .insert(key.to_owned(), EnvironmentVariable::plain(value));
    }
    for assignment in secrets {
        let (key, value) = parse_assignment(assignment)?;
        let reference = secret_store
            .set_environment_secret(name, key, value)
            .with_context(|| format!("could not store secret variable {key} in the OS keychain"))?;
        environment.variables.insert(
            key.to_owned(),
            EnvironmentVariable::keychain(reference.into_string()),
        );
    }
    for (key, value) in stdin_secrets {
        let reference = secret_store
            .set_environment_secret(name, &key, &value)
            .with_context(|| format!("could not store secret variable {key} in the OS keychain"))?;
        environment
            .variables
            .insert(key, EnvironmentVariable::keychain(reference.into_string()));
    }
    let path = workspace.save_environment(&environment)?;
    println!(
        "Saved environment {} with {} variables at {}",
        environment.name,
        environment.variables.len(),
        path.display()
    );
    Ok(())
}

fn read_secret_stdin(keys: &[String]) -> Result<Vec<(String, String)>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    read_secret_lines(&mut reader, keys)
}

fn read_secret_lines<R: BufRead>(reader: &mut R, keys: &[String]) -> Result<Vec<(String, String)>> {
    const MAX_SECRET_BYTES: usize = 1024 * 1024;
    let mut assignments = Vec::with_capacity(keys.len());
    for raw_key in keys {
        let key = parse_secret_key(raw_key)?.to_owned();
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            bail!("stdin ended before a value was provided for secret variable {key}");
        }
        if line.len() > MAX_SECRET_BYTES {
            bail!("stdin secret value for {key} exceeds {MAX_SECRET_BYTES} bytes");
        }
        let value = line.strip_suffix('\n').unwrap_or(&line);
        let value = value.strip_suffix('\r').unwrap_or(value).to_owned();
        assignments.push((key, value));
    }
    Ok(assignments)
}

fn migrate_environment_secrets(
    workspace_path: &Path,
    name: &str,
    keys: &[String],
    all: bool,
) -> Result<()> {
    if all && !keys.is_empty() {
        bail!("choose either --all or one or more --key values, not both");
    }
    if !all && keys.is_empty() {
        bail!("environment secret migration requires --key KEY or --all");
    }

    let workspace = Workspace::open(workspace_path)?;
    let (_, mut environment) = workspace
        .environments()?
        .into_iter()
        .find(|(_, environment)| {
            environment.name == name || environment.name.eq_ignore_ascii_case(name)
        })
        .with_context(|| format!("environment not found: {name}"))?;
    let secret_store = SecretStore::for_workspace(workspace.root());
    let requested = if all {
        environment
            .variables
            .iter()
            .filter(|(_, variable)| {
                variable.secret && variable.secret_ref.is_none() && !variable.value.is_empty()
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>()
    } else {
        keys.iter()
            .map(|key| parse_secret_key(key).map(str::to_owned))
            .collect::<Result<Vec<_>>>()?
    };

    let mut migrated = 0usize;
    let mut already_secure = 0usize;
    for key in requested {
        let variable = environment
            .variables
            .get_mut(&key)
            .with_context(|| format!("environment variable not found: {key}"))?;
        if variable.secret_ref.is_some() {
            already_secure += 1;
            continue;
        }
        if variable.value.is_empty() {
            bail!("environment variable {key} has no plaintext value to migrate");
        }
        let enabled = variable.enabled;
        let value = variable.value.clone();
        let reference = secret_store
            .set_environment_secret(&environment.name, &key, &value)
            .with_context(|| format!("could not store secret variable {key} in the OS keychain"))?;
        *variable = EnvironmentVariable::keychain(reference.into_string());
        variable.enabled = enabled;
        migrated += 1;
    }

    if migrated > 0 {
        workspace.save_environment(&environment)?;
    }
    println!(
        "Migrated {migrated} secret variable(s) in environment {}.",
        environment.name
    );
    if already_secure > 0 {
        println!("Skipped {already_secure} variable(s) already backed by the OS credential store.");
    }
    Ok(())
}

fn list_workspace(path: &Path) -> Result<()> {
    let workspace = Workspace::open(path)?;
    let manifest = workspace.manifest()?;
    println!("{} ({})", manifest.name, workspace.root().display());
    for collection in workspace.collections()? {
        println!("\nCollection: {}", collection.collection.name);
        for (request_path, request) in workspace.requests(&collection)? {
            println!(
                "  {} {} — {}",
                request.method,
                request.name,
                request_path.display()
            );
        }
    }
    let environments = workspace.environments()?;
    if !environments.is_empty() {
        println!("\nEnvironments:");
        for (_, environment) in environments {
            println!("  {}", environment.name);
        }
    }
    Ok(())
}

fn validate_workspace(path: &Path, output_json: bool) -> Result<()> {
    let workspace = Workspace::open(path)?;
    let report = workspace.validate()?;
    if output_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "valid": report.is_valid(),
                "collections": report.collections,
                "requests": report.requests,
                "environments": report.environments,
                "issues": report.issues,
            }))?
        );
    } else if report.is_valid() {
        println!(
            "Workspace is valid: {} collection(s), {} request(s), {} environment(s).",
            report.collections, report.requests, report.environments
        );
    } else {
        println!(
            "Workspace has {} issue(s): {} valid collection(s), {} valid request(s), {} valid environment(s).",
            report.issues.len(), report.collections, report.requests, report.environments
        );
        for issue in &report.issues {
            println!("  {} — {}", issue.path.display(), issue.message);
        }
    }
    if report.is_valid() {
        Ok(())
    } else {
        bail!("workspace validation failed")
    }
}

fn search_workspace(path: &Path, query: &str, output_json: bool) -> Result<()> {
    if query.trim().is_empty() {
        bail!("search query cannot be empty");
    }
    let workspace = Workspace::open(path)?;
    let results = workspace.search_requests(query)?;
    if output_json {
        println!("{}", serde_json::to_string_pretty(&results)?);
        return Ok(());
    }
    if results.is_empty() {
        println!("No saved requests matched {query:?}.");
        return Ok(());
    }
    println!("{} saved request(s) matched {query:?}:", results.len());
    for result in results {
        let location = result
            .folder
            .as_deref()
            .map(|folder| format!("{} / {folder}", result.collection))
            .unwrap_or(result.collection);
        println!(
            "{} {} — {} — {} ({})",
            result.method,
            result.name,
            location,
            result.url,
            result.path.display()
        );
    }
    Ok(())
}

fn list_history(path: &Path, options: HistoryOptions) -> Result<()> {
    let workspace = Workspace::open(path)?;
    if options.clear {
        workspace.clear_history()?;
        if options.output_json {
            println!("{{\"cleared\":true}}");
        } else {
            println!("Cleared local request history.");
        }
        return Ok(());
    }
    let entries = workspace.history_filtered(
        options.limit,
        &HistoryFilter {
            search: options.search,
            method: options.method,
            status: options.status,
            errors_only: options.errors_only,
        },
    )?;
    if options.output_json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }
    if entries.is_empty() {
        println!("No local request history.");
        return Ok(());
    }
    for entry in entries {
        let result = match entry.outcome {
            HistoryOutcome::Completed => entry
                .status
                .map(|status| status.to_string())
                .unwrap_or_else(|| "completed".to_owned()),
            HistoryOutcome::Error => "error".to_owned(),
        };
        println!(
            "{} {} {} — {} ({} ms)",
            entry.method, result, entry.request_name, entry.url, entry.duration_ms
        );
    }
    Ok(())
}

async fn run_workspace(options: RunOptions<'_>) -> Result<()> {
    let cancellation = CancellationToken::default();
    let signal_cancellation = cancellation.clone();
    let signal_task = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("Postly run cancellation requested");
            signal_cancellation.cancel();
        }
    });
    let result = run_workspace_with_cancellation(options, cancellation).await;
    signal_task.abort();
    result
}

async fn run_workspace_with_cancellation(
    options: RunOptions<'_>,
    cancellation: CancellationToken,
) -> Result<()> {
    let workspace = if options.path.join("postly.toml").is_file() {
        Workspace::open(options.path)?
    } else {
        find_workspace(options.path)?
    };
    let collections = workspace.collections()?;
    if collections.is_empty() {
        bail!("no collections found in {}", workspace.root().display());
    }
    let engine = HttpEngine::new(&EngineOptions {
        timeout: Duration::from_secs(options.timeout),
        max_redirects: options.max_redirects,
        proxy: options.proxy.map(ToOwned::to_owned),
        no_proxy: options.no_proxy.map(ToOwned::to_owned),
        ca_cert: options.ca_cert.map(Path::to_path_buf),
        client_identity: options.client_identity.map(Path::to_path_buf),
        client_identity_passphrase: client_identity_passphrase(options.client_identity),
        cookie_jar: Some(workspace.root().join(".postly/cookies.json")),
        ..EngineOptions::default()
    })?;
    let iterations = load_iteration_data(options.data_file)?;
    let mut summaries = Vec::new();
    let mut matched_any_request = false;
    for collection in collections {
        let requests = workspace.requests(&collection)?;
        let requests = if let Some(folder) = options.folder {
            requests
                .into_iter()
                .filter(|(_, request)| request_belongs_to_folder(request, folder))
                .collect::<Vec<_>>()
        } else {
            requests
        };
        if requests.is_empty() {
            continue;
        }
        matched_any_request = true;
        let context = context_for_collection(
            &workspace,
            Some(&collection.collection),
            options.environment_name,
        )?;
        let summary = run_requests(
            &engine,
            &requests,
            &context,
            &RunnerOptions {
                fail_fast: options.fail_fast,
                concurrency: options.concurrency,
                scripts: options.scripts,
                iterations: iterations.clone(),
                cancellation: cancellation.clone(),
                ..RunnerOptions::default()
            },
        )
        .await;
        if matches!(options.reporter, Reporter::Pretty) {
            for result in &summary.results {
                if let Some(status) = result.status {
                    println!(
                        "{} {} {} ({} ms, {} assertions)",
                        if result.passed { "PASS" } else { "FAIL" },
                        status,
                        result.name,
                        result.duration_ms,
                        result.assertions
                    );
                    for failure in &result.assertion_failures {
                        println!("  assertion: {failure}");
                    }
                    for test in &result.script_tests {
                        let state = if test.passed { "PASS" } else { "FAIL" };
                        println!(
                            "  {state} script test: {} ({} ms){}",
                            test.name,
                            test.duration_ms,
                            test.error
                                .as_deref()
                                .map(|error| format!(": {error}"))
                                .unwrap_or_default()
                        );
                    }
                } else {
                    eprintln!(
                        "FAIL {}: {}",
                        result.name,
                        result.error.as_deref().unwrap_or("unknown error")
                    );
                }
            }
            let statuses = summary
                .status_distribution
                .iter()
                .map(|(status, count)| format!("{status} x{count}"))
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "Summary: {} request(s), {} passed, {} failed{}",
                summary.requests,
                summary.passed,
                summary.failed,
                if statuses.is_empty() {
                    String::new()
                } else {
                    format!("; statuses: {statuses}")
                }
            );
        }
        let should_stop = options.fail_fast && summary.failed > 0;
        let cancelled = summary.cancelled;
        summaries.push(summary);
        if should_stop || cancelled {
            break;
        }
    }
    if let Some(folder) = options.folder {
        if !matched_any_request {
            bail!("no requests found in folder {folder:?}");
        }
    }
    match options.reporter {
        Reporter::Pretty => {}
        Reporter::Json => println!("{}", serde_json::to_string_pretty(&summaries)?),
        Reporter::Junit => println!("{}", render_junit(&summaries)),
    }
    if summaries.iter().any(|summary| summary.cancelled) {
        bail!("collection run cancelled");
    }
    if summaries.iter().any(|summary| !summary.succeeded()) {
        bail!("collection run failed");
    }
    Ok(())
}

fn request_belongs_to_folder(request: &Request, requested_folder: &str) -> bool {
    let requested_folder = normalize_folder(requested_folder);
    if requested_folder.is_empty() {
        return false;
    }
    let Some(actual_folder) = request.folder.as_deref() else {
        return false;
    };
    let actual_folder = normalize_folder(actual_folder);
    actual_folder == requested_folder
        || actual_folder
            .strip_prefix(&format!("{requested_folder}/"))
            .is_some()
}

fn normalize_folder(folder: &str) -> String {
    folder.replace('\\', "/").trim_matches('/').to_owned()
}

fn load_iteration_data(path: Option<&Path>) -> Result<Vec<postly_core::Variables>> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let text = fs::read_to_string(path)
        .with_context(|| format!("could not read iteration data file {}", path.display()))?;
    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("csv"))
    {
        return load_csv_iteration_data(&text, path);
    }
    let value: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("iteration data file is not valid JSON: {}", path.display()))?;
    let rows = match value {
        serde_json::Value::Array(rows) => rows,
        serde_json::Value::Object(_) => vec![value],
        _ => bail!("iteration data must be a JSON object or array of objects"),
    };
    rows.into_iter()
        .enumerate()
        .map(|(index, row)| {
            let object = row
                .as_object()
                .with_context(|| format!("iteration {index} is not a JSON object"))?;
            Ok(object
                .iter()
                .map(|(key, value)| (key.clone(), iteration_value(value)))
                .collect())
        })
        .collect()
}

fn load_csv_iteration_data(text: &str, path: &Path) -> Result<Vec<postly_core::Variables>> {
    let mut rows = parse_csv_rows(text)
        .with_context(|| format!("iteration data CSV is invalid: {}", path.display()))?;
    let Some(mut headers) = rows.first().cloned() else {
        bail!(
            "iteration data CSV must contain a header row: {}",
            path.display()
        );
    };
    if let Some(first) = headers.first_mut() {
        *first = first.trim_start_matches('\u{feff}').to_owned();
    }
    if headers.iter().any(|header| header.trim().is_empty()) {
        bail!(
            "iteration data CSV contains an empty header: {}",
            path.display()
        );
    }
    if headers
        .iter()
        .enumerate()
        .any(|(index, header)| headers[..index].contains(header))
    {
        bail!(
            "iteration data CSV contains duplicate headers: {}",
            path.display()
        );
    }

    rows.remove(0);
    rows.into_iter()
        .filter(|row| !(row.len() == 1 && row[0].is_empty()))
        .enumerate()
        .map(|(index, row)| {
            if row.len() > headers.len() {
                bail!(
                    "iteration CSV row {} has {} values but the header has {} columns: {}",
                    index + 2,
                    row.len(),
                    headers.len(),
                    path.display()
                );
            }
            Ok(headers
                .iter()
                .enumerate()
                .map(|(column, header)| {
                    (header.clone(), row.get(column).cloned().unwrap_or_default())
                })
                .collect())
        })
        .collect()
}

fn parse_csv_rows(text: &str) -> Result<Vec<Vec<String>>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut quote_closed = false;
    let mut field_started = false;
    let mut chars = text.chars().peekable();

    while let Some(character) = chars.next() {
        if in_quotes {
            match character {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => {
                    in_quotes = false;
                    quote_closed = true;
                }
                _ => field.push(character),
            }
            continue;
        }

        match character {
            ',' => {
                row.push(std::mem::take(&mut field));
                field_started = false;
                quote_closed = false;
            }
            '\n' | '\r' => {
                if character == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                field_started = false;
                quote_closed = false;
            }
            '"' if !field_started && field.is_empty() => {
                in_quotes = true;
                field_started = true;
            }
            '"' if quote_closed => {
                bail!("characters follow a closed quoted CSV field");
            }
            '"' => {
                bail!("unexpected quote in an unquoted CSV field");
            }
            _ if quote_closed => {
                bail!("characters follow a closed quoted CSV field");
            }
            _ => {
                field.push(character);
                field_started = true;
            }
        }
    }

    if in_quotes {
        bail!("unterminated quoted CSV field");
    }
    if field_started || !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    Ok(rows)
}

fn iteration_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Null => String::new(),
        value => value.to_string(),
    }
}

fn render_junit(summaries: &[postly_core::RunnerSummary]) -> String {
    let results = summaries.iter().flat_map(|summary| summary.results.iter());
    let tests = summaries
        .iter()
        .map(|summary| summary.requests)
        .sum::<usize>();
    let failures = summaries
        .iter()
        .map(|summary| summary.failed)
        .sum::<usize>();
    let skipped = summaries.iter().filter(|summary| summary.cancelled).count();
    let mut output = format!(
        "<testsuite name=\"postly\" tests=\"{tests}\" failures=\"{failures}\" skipped=\"{skipped}\">"
    );
    for result in results {
        output.push_str(&format!(
            "<testcase classname=\"{}\" name=\"{}\" time=\"{:.3}\">",
            xml_escape(&result.method),
            xml_escape(&format!("iteration {}: {}", result.iteration, result.name)),
            result.duration_ms as f64 / 1000.0
        ));
        if !result.passed {
            let message = result
                .error
                .as_deref()
                .or_else(|| result.status.map(|_status| "HTTP status failure"))
                .unwrap_or("request failed");
            let details = result.assertion_failures.join("\n");
            output.push_str(&format!(
                "<failure message=\"{}\">{}</failure>",
                xml_escape(message),
                xml_escape(&details)
            ));
        }
        if !result.script_tests.is_empty() {
            let details = result
                .script_tests
                .iter()
                .map(|test| {
                    format!(
                        "{} {} ({} ms){}",
                        if test.passed { "PASS" } else { "FAIL" },
                        test.name,
                        test.duration_ms,
                        test.error
                            .as_deref()
                            .map(|error| format!(": {error}"))
                            .unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            output.push_str(&format!(
                "<system-out>{}</system-out>",
                xml_escape(&details)
            ));
        }
        output.push_str("</testcase>");
    }
    output.push_str("</testsuite>");
    output
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

async fn execute(
    request: &Request,
    context: VariableContext,
    options: ExecuteOptions<'_>,
) -> Result<postly_core::HttpResponse> {
    let engine = HttpEngine::new(&EngineOptions {
        timeout: Duration::from_secs(options.timeout),
        max_redirects: options.max_redirects,
        accept_invalid_certs: options.insecure,
        proxy: options.proxy.map(ToOwned::to_owned),
        no_proxy: options.no_proxy.map(ToOwned::to_owned),
        ca_cert: options.ca_cert.map(Path::to_path_buf),
        client_identity: options.client_identity.map(Path::to_path_buf),
        client_identity_passphrase: client_identity_passphrase(options.client_identity),
        cookie_jar: options.cookie_jar.map(Path::to_path_buf),
        ..EngineOptions::default()
    })?;
    let response = if options.oauth_browser {
        engine
            .execute_with_pkce_browser(request, &context, open_browser_url)
            .await?
    } else {
        engine
            .execute_with_device_code_prompt(request, &context, |prompt| {
                eprintln!(
                    "OAuth device authorization required: visit {} and enter {} (expires in {}s).",
                    prompt.verification_uri,
                    prompt.user_code,
                    prompt.expires_in.as_secs()
                );
                if let Some(url) = prompt.verification_uri_complete.as_deref() {
                    eprintln!("Direct verification URL: {url}");
                }
                eprintln!("Waiting for authorization approval…");
            })
            .await?
    };
    Ok(response)
}

fn open_browser_url(url: &str) -> std::result::Result<(), String> {
    #[cfg(target_os = "macos")]
    let status = std::process::Command::new("open")
        .arg(url)
        .status()
        .map_err(|error| error.to_string())?;
    #[cfg(target_os = "windows")]
    let status = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .status()
        .map_err(|error| error.to_string())?;
    #[cfg(all(unix, not(target_os = "macos")))]
    let status = std::process::Command::new("xdg-open")
        .arg(url)
        .status()
        .map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("browser opener exited with {status}"))
    }
}

async fn run_script_async(
    script: String,
    request: Request,
    response: Option<postly_core::HttpResponse>,
    context: VariableContext,
) -> Result<ScriptResult> {
    Ok(tokio::task::spawn_blocking(move || {
        postly_core::run_script(&script, &request, response.as_ref(), &context)
    })
    .await??)
}

fn context_for_collection(
    workspace: &Workspace,
    collection: Option<&Collection>,
    environment_name: Option<&str>,
) -> Result<VariableContext> {
    let mut context = VariableContext::default();
    if let Some(collection) = collection {
        context.collection = collection.variables.clone();
    }
    if let Some(name) = environment_name {
        let (_, environment) = workspace
            .environments()?
            .into_iter()
            .find(|(_, environment)| {
                environment.name == name || environment.name.eq_ignore_ascii_case(name)
            })
            .with_context(|| format!("environment not found: {name}"))?;
        context.environment = SecretStore::for_workspace(workspace.root())
            .resolve_environment(&environment)
            .with_context(|| format!("could not resolve secrets for environment {name}"))?;
    }
    Ok(context)
}

fn parse_headers(headers: &[String]) -> Result<Vec<HeaderEntry>> {
    headers
        .iter()
        .map(|header| {
            let (key, value) = header
                .split_once(':')
                .with_context(|| format!("header must use KEY:VALUE syntax: {header}"))?;
            Ok(HeaderEntry::enabled(key.trim(), value.trim()))
        })
        .collect()
}

fn parse_pairs_flags(values: &[String]) -> Result<Vec<postly_core::KeyValue>> {
    values
        .iter()
        .map(|value| {
            let (key, value) = value
                .split_once('=')
                .with_context(|| format!("query must use KEY=VALUE syntax: {value}"))?;
            Ok(postly_core::KeyValue::enabled(key.trim(), value.trim()))
        })
        .collect()
}

fn parse_assignment(value: &str) -> Result<(&str, &str)> {
    let (key, value) = value
        .split_once('=')
        .with_context(|| format!("environment value must use KEY=VALUE syntax: {value}"))?;
    if key.trim().is_empty() {
        bail!("environment variable key cannot be empty");
    }
    Ok((key.trim(), value))
}

fn parse_secret_key(value: &str) -> Result<&str> {
    let key = value.trim();
    if key.is_empty() {
        bail!("environment variable key cannot be empty");
    }
    if key.contains('=') {
        bail!("--secret-stdin and --key expect a variable name, not KEY=VALUE");
    }
    Ok(key)
}

fn parse_auth_flags(
    bearer: Option<String>,
    basic_user: Option<String>,
    basic_password: Option<String>,
) -> Result<Auth> {
    parse_auth_flags_with_oauth(bearer, basic_user, basic_password, OAuthCliArgs::default())
}

fn parse_auth_flags_with_oauth(
    bearer: Option<String>,
    basic_user: Option<String>,
    basic_password: Option<String>,
    oauth: OAuthCliArgs,
) -> Result<Auth> {
    parse_auth_flags_with_oauth_and_digest(bearer, basic_user, basic_password, None, None, oauth)
}

fn parse_auth_flags_with_oauth_and_digest(
    bearer: Option<String>,
    basic_user: Option<String>,
    basic_password: Option<String>,
    digest_user: Option<String>,
    digest_password: Option<String>,
    oauth: OAuthCliArgs,
) -> Result<Auth> {
    let OAuthCliArgs {
        oauth_token_url,
        oauth_client_id,
        oauth_client_secret,
        oauth_scope,
        oauth_authorization_url,
        oauth_device_authorization_url,
        oauth_redirect_uri,
        oauth_code,
        oauth_code_verifier,
        oauth_refresh_token,
        oauth_browser,
        aws_access_key_id,
        aws_secret_access_key,
        aws_region,
        aws_service,
        aws_session_token,
    } = oauth;
    let has_aws = aws_access_key_id.is_some()
        || aws_secret_access_key.is_some()
        || aws_region.is_some()
        || aws_service.is_some()
        || aws_session_token.is_some();
    if has_aws {
        if oauth_token_url.is_some()
            || oauth_client_id.is_some()
            || oauth_client_secret.is_some()
            || oauth_scope.is_some()
            || oauth_authorization_url.is_some()
            || oauth_device_authorization_url.is_some()
            || oauth_redirect_uri.is_some()
            || oauth_code.is_some()
            || oauth_code_verifier.is_some()
            || oauth_refresh_token.is_some()
            || oauth_browser
        {
            bail!("choose either AWS Signature V4 or OAuth 2.0 authentication");
        }
        if bearer.is_some()
            || basic_user.is_some()
            || basic_password.is_some()
            || digest_user.is_some()
            || digest_password.is_some()
        {
            bail!("choose either AWS Signature V4 or bearer/basic/Digest authentication");
        }
        return Ok(Auth::AwsSignatureV4 {
            access_key_id: aws_access_key_id.context("--aws-access-key-id is required")?,
            secret_access_key: aws_secret_access_key
                .context("--aws-secret-access-key is required")?,
            region: aws_region.context("--aws-region is required")?,
            service: aws_service.context("--aws-service is required")?,
            session_token: aws_session_token,
        });
    }
    if oauth_token_url.is_some()
        || oauth_client_id.is_some()
        || oauth_client_secret.is_some()
        || oauth_scope.is_some()
        || oauth_authorization_url.is_some()
        || oauth_device_authorization_url.is_some()
        || oauth_redirect_uri.is_some()
        || oauth_code.is_some()
        || oauth_code_verifier.is_some()
        || oauth_refresh_token.is_some()
        || oauth_browser
    {
        if bearer.is_some()
            || basic_user.is_some()
            || basic_password.is_some()
            || digest_user.is_some()
            || digest_password.is_some()
        {
            bail!("choose either bearer/basic/Digest authentication or OAuth 2.0");
        }
        let token_url = oauth_token_url.context("--oauth-token-url is required for OAuth 2.0")?;
        let client_id = oauth_client_id.context("--oauth-client-id is required for OAuth 2.0")?;
        if let Some(device_authorization_url) = oauth_device_authorization_url {
            if oauth_authorization_url.is_some()
                || oauth_redirect_uri.is_some()
                || oauth_code.is_some()
                || oauth_code_verifier.is_some()
                || oauth_refresh_token.is_some()
                || oauth_browser
            {
                bail!("choose either OAuth 2.0 device code, PKCE or a refresh token");
            }
            return Ok(Auth::OAuth2DeviceCode {
                device_authorization_url,
                token_url,
                client_id,
                client_secret: oauth_client_secret,
                scope: oauth_scope,
            });
        }
        let is_pkce = oauth_authorization_url.is_some()
            || oauth_redirect_uri.is_some()
            || oauth_code.is_some()
            || oauth_code_verifier.is_some()
            || oauth_browser;
        if is_pkce {
            if oauth_refresh_token.is_some() {
                bail!("choose either OAuth 2.0 PKCE or a refresh token");
            }
            let authorization_url = oauth_authorization_url
                .context("--oauth-authorization-url is required for OAuth 2.0 PKCE")?;
            let redirect_uri = oauth_redirect_uri
                .context("--oauth-redirect-uri is required for OAuth 2.0 PKCE")?;
            let code = if oauth_browser {
                oauth_code.unwrap_or_default()
            } else {
                oauth_code.context("--oauth-code is required for OAuth 2.0 PKCE")?
            };
            let code_verifier = if oauth_browser {
                oauth_code_verifier.unwrap_or_default()
            } else {
                oauth_code_verifier
                    .context("--oauth-code-verifier is required for OAuth 2.0 PKCE")?
            };
            return Ok(Auth::OAuth2AuthorizationCodePkce {
                authorization_url,
                token_url,
                client_id,
                redirect_uri,
                code,
                code_verifier,
                client_secret: oauth_client_secret,
                scope: oauth_scope,
            });
        }
        if let Some(refresh_token) = oauth_refresh_token {
            return Ok(Auth::OAuth2RefreshToken {
                token_url,
                client_id,
                refresh_token,
                client_secret: oauth_client_secret,
                scope: oauth_scope,
            });
        }
        let client_secret =
            oauth_client_secret.context("--oauth-client-secret is required for OAuth 2.0")?;
        return Ok(Auth::OAuth2ClientCredentials {
            token_url,
            client_id,
            client_secret,
            scope: oauth_scope,
        });
    }
    if digest_user.is_some() || digest_password.is_some() {
        return Ok(Auth::Digest {
            username: digest_user.context("--digest-user is required")?,
            password: digest_password.unwrap_or_default(),
        });
    }
    match (bearer, basic_user, basic_password) {
        (Some(token), None, None) => Ok(Auth::Bearer { token }),
        (None, Some(username), password) => Ok(Auth::Basic {
            username,
            password: password.unwrap_or_default(),
        }),
        (None, None, None) => Ok(Auth::None),
        _ => bail!("choose either --bearer or --basic-user/--basic-password"),
    }
}

fn parse_cli_body(data: Option<String>, json_body: Option<String>) -> Result<RequestBody> {
    match (data, json_body) {
        (Some(_), Some(_)) => bail!("choose either --data or --json"),
        (Some(data), None) => Ok(RequestBody::Raw {
            text: data,
            content_type: None,
        }),
        (None, Some(json_body)) => Ok(RequestBody::Json {
            value: serde_json::from_str(&json_body).context("--json must contain valid JSON")?,
        }),
        (None, None) => {
            if io::stdin().is_terminal() {
                Ok(RequestBody::None)
            } else {
                let mut input = String::new();
                io::stdin().read_to_string(&mut input)?;
                if input.trim().is_empty() {
                    Ok(RequestBody::None)
                } else {
                    Ok(RequestBody::Raw {
                        text: input,
                        content_type: None,
                    })
                }
            }
        }
    }
}

fn print_response(response: &postly_core::HttpResponse, output_json: bool) -> Result<()> {
    print_response_with_tests(response, output_json, None, None)
}

fn print_response_with_tests(
    response: &postly_core::HttpResponse,
    output_json: bool,
    tests: Option<&[ScriptTestResult]>,
    native_assertions: Option<(usize, &[String])>,
) -> Result<()> {
    if output_json {
        let mut payload = json!({
            "status": response.status,
            "status_text": response.status_text,
            "headers": response.headers,
            "content_type": response.content_type,
            "response_size": response.response_size,
            "duration_ms": response.duration_ms,
            "ttfb_ms": response.ttfb_ms,
            "download_ms": response.download_ms,
            "protocol": response.protocol,
            "url": response.url,
            "body": response.formatted_body(postly_core::ResponseView::Pretty),
        });
        if let Some(tests) = tests {
            payload["tests"] = serde_json::to_value(tests)?;
        }
        if let Some((count, failures)) = native_assertions {
            payload["assertions"] = json!({
                "count": count,
                "failed": failures.len(),
                "failures": failures,
            });
        }
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!(
            "{} {} · {} ms · TTFB {} ms · download {} ms · {}",
            response.status,
            response.status_text,
            response.duration_ms,
            response.ttfb_ms,
            response.download_ms,
            response.protocol
        );
        println!(
            "{}",
            response.formatted_body(postly_core::ResponseView::Pretty)
        );
        if let Some(tests) = tests {
            for test in tests {
                if test.passed {
                    println!("PASS test: {} ({} ms)", test.name, test.duration_ms);
                } else {
                    println!(
                        "FAIL test: {} ({} ms) — {}",
                        test.name,
                        test.duration_ms,
                        test.error.as_deref().unwrap_or("assertion failed")
                    );
                }
            }
        }
        if let Some((count, failures)) = native_assertions {
            if failures.is_empty() {
                println!("PASS native assertions: {count}");
            } else {
                println!("FAIL native assertions: {} of {count}", failures.len());
                for failure in failures {
                    println!("FAIL assertion: {failure}");
                }
            }
        }
    }
    Ok(())
}

fn find_workspace(path: &Path) -> Result<Workspace> {
    let start = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().unwrap_or(Path::new(".")).to_path_buf()
    };
    for candidate in start.ancestors() {
        if candidate.join("postly.toml").is_file() {
            return Ok(Workspace::open(candidate)?);
        }
    }
    bail!("could not find postly.toml above {}", path.display());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        convert::Infallible,
        io::Cursor,
        pin::Pin,
        sync::Arc,
        task::{Context, Poll},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_rustls::{
        rustls::{
            pki_types::{CertificateDer, PrivateKeyDer},
            server::WebPkiClientVerifier,
            RootCertStore, ServerConfig,
        },
        TlsAcceptor,
    };

    const TEST_CA_PEM: &str = include_str!("../../postly-core/testdata/tls/ca.pem");
    const TEST_SERVER_CERT_PEM: &str = include_str!("../../postly-core/testdata/tls/server.pem");
    const TEST_SERVER_KEY_PEM: &str = include_str!("../../postly-core/testdata/tls/server-key.pem");
    const TEST_CLIENT_CERT_PEM: &str = include_str!("../../postly-core/testdata/tls/client.pem");
    const TEST_CLIENT_KEY_PEM: &str = include_str!("../../postly-core/testdata/tls/client-key.pem");
    const TEST_PKCS12_PASSWORD: &str = "postly-test-password";

    fn create_test_pkcs12_identity(directory: &Path) -> Option<PathBuf> {
        let certificate = directory.join("client.pem");
        let private_key = directory.join("client-key.pem");
        let identity = directory.join("client-identity.p12");
        std::fs::write(&certificate, TEST_CLIENT_CERT_PEM).expect("client certificate fixture");
        std::fs::write(&private_key, TEST_CLIENT_KEY_PEM).expect("client key fixture");
        let output = std::process::Command::new("openssl")
            .args(["pkcs12", "-export", "-inkey"])
            .arg(&private_key)
            .args(["-in"])
            .arg(&certificate)
            .arg("-passout")
            .arg(format!("pass:{TEST_PKCS12_PASSWORD}"))
            .arg("-out")
            .arg(&identity)
            .output()
            .expect("openssl is required for the PKCS#12 gRPC fixture");
        output.status.success().then_some(identity)
    }

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

    #[test]
    fn grpc_pkcs12_identity_requires_and_validates_its_passphrase() {
        let directory = tempfile::tempdir().expect("directory");
        let Some(identity) = create_test_pkcs12_identity(directory.path()) else {
            return;
        };

        let missing = configure_grpc_endpoint_with_passphrase(
            "https://localhost:50051",
            10,
            None,
            Some(&identity),
            None,
        )
        .expect_err("missing PKCS#12 passphrase must fail");
        assert!(missing
            .to_string()
            .contains("POSTLY_CLIENT_IDENTITY_PASSPHRASE"));

        let wrong = configure_grpc_endpoint_with_passphrase(
            "https://localhost:50051",
            10,
            None,
            Some(&identity),
            Some("wrong-passphrase"),
        )
        .expect_err("wrong PKCS#12 passphrase must fail");
        assert!(wrong.to_string().contains("could not unlock PKCS#12"));

        configure_grpc_endpoint_with_passphrase(
            "https://localhost:50051",
            10,
            None,
            Some(&identity),
            Some(TEST_PKCS12_PASSWORD),
        )
        .expect("valid PKCS#12 passphrase");
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

    #[test]
    fn parses_oauth_client_credentials_cli_flags() {
        let auth = parse_auth_flags_with_oauth(
            None,
            None,
            None,
            OAuthCliArgs {
                oauth_token_url: Some("https://auth.example.test/token".to_owned()),
                oauth_client_id: Some("postly".to_owned()),
                oauth_client_secret: Some("secret".to_owned()),
                oauth_scope: Some("read:users".to_owned()),
                ..OAuthCliArgs::default()
            },
        )
        .expect("OAuth flags");
        assert_eq!(
            auth,
            Auth::OAuth2ClientCredentials {
                token_url: "https://auth.example.test/token".to_owned(),
                client_id: "postly".to_owned(),
                client_secret: "secret".to_owned(),
                scope: Some("read:users".to_owned()),
            }
        );
        let error = parse_auth_flags_with_oauth(
            None,
            None,
            None,
            OAuthCliArgs {
                oauth_token_url: Some("https://auth.example.test/token".to_owned()),
                oauth_client_id: Some("postly".to_owned()),
                ..OAuthCliArgs::default()
            },
        )
        .expect_err("incomplete OAuth flags");
        assert!(error.to_string().contains("--oauth-client-secret"));
    }

    #[test]
    fn parses_cookie_inspection_command_without_value_output() {
        let cli = Cli::try_parse_from([
            "postly",
            "cookies",
            "./workspace",
            "--clear",
            "--output-json",
        ])
        .expect("cookie command");
        match cli.command {
            Command::Cookies {
                path,
                clear,
                output_json,
            } => {
                assert_eq!(path, PathBuf::from("./workspace"));
                assert!(clear);
                assert!(output_json);
            }
            command => panic!("unexpected command: {command:?}"),
        }
    }

    #[test]
    fn parses_bounded_run_concurrency() {
        let cli = Cli::try_parse_from(["postly", "run", "./workspace", "--concurrency", "4"])
            .expect("run concurrency flag");
        match cli.command {
            Command::Run { concurrency, .. } => assert_eq!(concurrency, 4),
            command => panic!("unexpected command: {command:?}"),
        }

        assert!(
            Cli::try_parse_from(["postly", "run", "./workspace", "--concurrency", "0"]).is_err()
        );
        assert!(
            Cli::try_parse_from(["postly", "run", "./workspace", "--concurrency", "65"]).is_err()
        );
    }

    #[test]
    fn parses_http_redirect_limit_with_a_safe_default() {
        let cli = Cli::try_parse_from(["postly", "request", "http://example.test"])
            .expect("default redirect limit");
        match cli.command {
            Command::Request { max_redirects, .. } => assert_eq!(max_redirects, 10),
            command => panic!("unexpected command: {command:?}"),
        }

        let cli = Cli::try_parse_from(["postly", "run", "./workspace", "--max-redirects", "0"])
            .expect("disabled redirect following");
        match cli.command {
            Command::Run { max_redirects, .. } => assert_eq!(max_redirects, 0),
            command => panic!("unexpected command: {command:?}"),
        }
    }

    #[test]
    fn loads_csv_iteration_data_with_quoted_values_and_short_rows() {
        let directory = tempfile::tempdir().expect("iteration directory");
        let path = directory.path().join("iterations.csv");
        std::fs::write(
            &path,
            "\u{feff}name,role,notes\r\nAda,admin,\"hello, world\"\r\nGrace,developer,\"line one\nline two\"\r\nLin,\r\n",
        )
        .expect("CSV");

        let rows = load_iteration_data(Some(&path)).expect("CSV iteration data");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["name"], "Ada");
        assert_eq!(rows[0]["notes"], "hello, world");
        assert_eq!(rows[1]["notes"], "line one\nline two");
        assert_eq!(rows[2]["role"], "");
        assert_eq!(rows[2]["notes"], "");
    }

    #[test]
    fn rejects_malformed_csv_iteration_data() {
        let directory = tempfile::tempdir().expect("iteration directory");
        let path = directory.path().join("iterations.csv");
        std::fs::write(&path, "name,role\n\"Ada,admin\n").expect("CSV");

        let error = load_iteration_data(Some(&path)).expect_err("malformed CSV");
        assert!(format!("{error:#}").contains("unterminated quoted CSV field"));
    }

    #[test]
    fn builds_websocket_tls_connector_from_local_certificate_files() {
        let directory = tempfile::tempdir().expect("tempdir");
        let ca_path = directory.path().join("ca.pem");
        let identity_path = directory.path().join("client.pem");
        std::fs::write(&ca_path, TEST_CA_PEM).expect("CA");
        std::fs::write(
            &identity_path,
            format!("{TEST_CLIENT_CERT_PEM}{TEST_CLIENT_KEY_PEM}"),
        )
        .expect("client identity");

        let connector = build_websocket_tls_connector(
            "wss://127.0.0.1:9443/socket",
            Some(&ca_path),
            Some(&identity_path),
            false,
            None,
        )
        .expect("WebSocket TLS connector");
        assert!(connector.is_some());

        let result = build_websocket_tls_connector(
            "ws://127.0.0.1:8080/socket",
            Some(&ca_path),
            None,
            false,
            None,
        );
        let error = match result {
            Ok(_) => panic!("TLS options on a plain WebSocket endpoint must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("require a wss:// endpoint"));
    }

    #[tokio::test]
    async fn websocket_command_uses_a_custom_ca_and_pem_client_identity() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("TLS listener");
        let address = listener.local_addr().expect("TLS address");
        let acceptor = TlsAcceptor::from(Arc::new(test_tls_server_config(true)));
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("TLS connection");
            let stream = acceptor.accept(stream).await.expect("TLS handshake");
            let mut socket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("WebSocket handshake");
            match socket.next().await.expect("message").expect("frame") {
                Message::Text(text) => assert_eq!(text.to_string(), "hello"),
                message => panic!("expected text message, got {message:?}"),
            }
            socket
                .send(Message::text("echo: hello"))
                .await
                .expect("echo");
            socket.send(Message::Close(None)).await.expect("close");
        });

        let directory = tempfile::tempdir().expect("tempdir");
        let ca_path = directory.path().join("ca.pem");
        let identity_path = directory.path().join("client.pem");
        std::fs::write(&ca_path, TEST_CA_PEM).expect("CA");
        std::fs::write(
            &identity_path,
            format!("{TEST_CLIENT_CERT_PEM}{TEST_CLIENT_KEY_PEM}"),
        )
        .expect("client identity");

        run_websocket(WebsocketOptions {
            endpoint: format!("wss://127.0.0.1:{}/socket", address.port()),
            send: vec!["hello".to_owned()],
            headers: Vec::new(),
            bearer: None,
            basic_user: None,
            basic_password: None,
            timeout: 10,
            reconnect: 0,
            proxy: None,
            no_proxy: None,
            ca_cert: Some(ca_path),
            client_identity: Some(identity_path),
            insecure: true,
            output_json: true,
        })
        .await
        .expect("WebSocket TLS command");
        server.await.expect("TLS server");
    }

    #[test]
    fn parses_oauth_pkce_cli_flags() {
        let auth = parse_auth_flags_with_oauth(
            None,
            None,
            None,
            OAuthCliArgs {
                oauth_token_url: Some("https://auth.example.test/token".to_owned()),
                oauth_client_id: Some("postly".to_owned()),
                oauth_scope: Some("read:users".to_owned()),
                oauth_authorization_url: Some("https://auth.example.test/authorize".to_owned()),
                oauth_redirect_uri: Some("http://127.0.0.1:8787/callback".to_owned()),
                oauth_code: Some("returned-code".to_owned()),
                oauth_code_verifier: Some("a".repeat(43)),
                ..OAuthCliArgs::default()
            },
        )
        .expect("PKCE flags");
        assert_eq!(
            auth,
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
    fn parses_oauth_browser_pkce_flags_without_persisted_credentials() {
        let auth = parse_auth_flags_with_oauth(
            None,
            None,
            None,
            OAuthCliArgs {
                oauth_token_url: Some("https://auth.example.test/token".to_owned()),
                oauth_client_id: Some("postly".to_owned()),
                oauth_authorization_url: Some("https://auth.example.test/authorize".to_owned()),
                oauth_redirect_uri: Some("http://127.0.0.1:0/callback".to_owned()),
                oauth_browser: true,
                ..OAuthCliArgs::default()
            },
        )
        .expect("browser PKCE flags");
        assert_eq!(
            auth,
            Auth::OAuth2AuthorizationCodePkce {
                authorization_url: "https://auth.example.test/authorize".to_owned(),
                token_url: "https://auth.example.test/token".to_owned(),
                client_id: "postly".to_owned(),
                redirect_uri: "http://127.0.0.1:0/callback".to_owned(),
                code: String::new(),
                code_verifier: String::new(),
                client_secret: None,
                scope: None,
            }
        );
    }

    #[test]
    fn parses_aws_signature_v4_flags() {
        let auth = parse_auth_flags_with_oauth(
            None,
            None,
            None,
            OAuthCliArgs {
                aws_access_key_id: Some("AKIDEXAMPLE".to_owned()),
                aws_secret_access_key: Some("secret".to_owned()),
                aws_region: Some("eu-west-1".to_owned()),
                aws_service: Some("execute-api".to_owned()),
                aws_session_token: Some("session".to_owned()),
                ..OAuthCliArgs::default()
            },
        )
        .expect("AWS Signature V4 flags");
        assert_eq!(
            auth,
            Auth::AwsSignatureV4 {
                access_key_id: "AKIDEXAMPLE".to_owned(),
                secret_access_key: "secret".to_owned(),
                region: "eu-west-1".to_owned(),
                service: "execute-api".to_owned(),
                session_token: Some("session".to_owned()),
            }
        );
    }

    #[test]
    fn parses_digest_cli_flags() {
        let auth = parse_auth_flags_with_oauth_and_digest(
            None,
            None,
            None,
            Some("Mufasa".to_owned()),
            Some("Circle Of Life".to_owned()),
            OAuthCliArgs::default(),
        )
        .expect("Digest flags");
        assert_eq!(
            auth,
            Auth::Digest {
                username: "Mufasa".to_owned(),
                password: "Circle Of Life".to_owned(),
            }
        );

        let cli = Cli::try_parse_from([
            "postly",
            "request",
            "https://api.example.test/users",
            "--digest-user",
            "Mufasa",
            "--digest-password",
            "Circle Of Life",
        ])
        .expect("Digest command flags");
        assert!(matches!(cli.command, Command::Request { .. }));
    }

    #[test]
    fn parses_oauth_refresh_token_cli_flags() {
        let auth = parse_auth_flags_with_oauth(
            None,
            None,
            None,
            OAuthCliArgs {
                oauth_token_url: Some("https://auth.example.test/token".to_owned()),
                oauth_client_id: Some("postly".to_owned()),
                oauth_refresh_token: Some("refresh-123".to_owned()),
                oauth_scope: Some("read:users".to_owned()),
                ..OAuthCliArgs::default()
            },
        )
        .expect("refresh-token flags");
        assert_eq!(
            auth,
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
    fn parses_oauth_device_code_cli_flags() {
        let auth = parse_auth_flags_with_oauth(
            None,
            None,
            None,
            OAuthCliArgs {
                oauth_token_url: Some("https://auth.example.test/token".to_owned()),
                oauth_device_authorization_url: Some("https://auth.example.test/device".to_owned()),
                oauth_client_id: Some("postly".to_owned()),
                oauth_scope: Some("read:users".to_owned()),
                ..OAuthCliArgs::default()
            },
        )
        .expect("device-code flags");
        assert_eq!(
            auth,
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
    fn reads_secret_values_from_stdin_without_assignment_syntax() {
        let mut reader = io::Cursor::new("first-value\r\nsecond-value\n");
        let keys = vec!["TOKEN".to_owned(), "CLIENT_SECRET".to_owned()];
        assert_eq!(
            read_secret_lines(&mut reader, &keys).expect("stdin secrets"),
            vec![
                ("TOKEN".to_owned(), "first-value".to_owned()),
                ("CLIENT_SECRET".to_owned(), "second-value".to_owned()),
            ]
        );
    }

    #[test]
    fn rejects_secret_stdin_key_assignment_and_missing_values() {
        let mut reader = io::Cursor::new("");
        let error = read_secret_lines(&mut reader, &["TOKEN=leaked".to_owned()])
            .expect_err("assignment syntax should fail");
        assert!(error.to_string().contains("variable name"));

        let mut reader = io::Cursor::new("");
        let error = read_secret_lines(&mut reader, &["TOKEN".to_owned()])
            .expect_err("missing stdin value should fail");
        assert!(error.to_string().contains("stdin ended"));
    }

    #[test]
    fn mock_router_returns_saved_example_without_query_data() {
        let routes = vec![MockRoute {
            method: "GET".to_owned(),
            path: "/health".to_owned(),
            example: ResponseExample {
                name: "Healthy".to_owned(),
                status: Some(201),
                status_text: Some("Fixture Created".to_owned()),
                headers: vec![HeaderEntry::enabled("content-type", "application/json")],
                cookies: vec![ResponseExampleCookie {
                    name: "sid".to_owned(),
                    value: "{{token}}".to_owned(),
                    domain: None,
                    path: Some("/".to_owned()),
                    secure: false,
                    http_only: true,
                    same_site: Some("Lax".to_owned()),
                    expires: None,
                    max_age_seconds: Some(60),
                }],
                body: Some(r#"{"ok":true}"#.to_owned()),
                delay_ms: 7,
            },
        }];

        let response = mock_response_for(&routes, "get", "/health?token=secret");

        assert_eq!(response.status, 201);
        assert_eq!(response.status_text, "Fixture Created");
        assert_eq!(response.body, br#"{"ok":true}"#);
        assert_eq!(response.delay_ms, 7);
        assert_eq!(response.headers.len(), 2);
        assert!(response.headers.iter().any(|(key, value)| {
            key == "set-cookie"
                && value == "sid={{token}}; Path=/; SameSite=Lax; Max-Age=60; HttpOnly"
        }));
    }

    #[test]
    fn mock_examples_resolve_selected_environment_placeholders() {
        let context = VariableContext::default().with_environment(
            [
                ("apiToken".to_owned(), "local-token".to_owned()),
                ("name".to_owned(), "Ada".to_owned()),
            ]
            .into_iter()
            .collect(),
        );
        let resolved = resolve_mock_example(
            ResponseExample {
                name: "Greeting".to_owned(),
                status: Some(200),
                status_text: None,
                headers: vec![HeaderEntry::enabled("x-api-token", "{{apiToken}}")],
                cookies: vec![ResponseExampleCookie {
                    name: "sid".to_owned(),
                    value: "{{apiToken}}".to_owned(),
                    domain: None,
                    path: None,
                    secure: false,
                    http_only: false,
                    same_site: None,
                    expires: None,
                    max_age_seconds: None,
                }],
                body: Some(r#"{"hello":"{{name}}"}"#.to_owned()),
                delay_ms: 0,
            },
            &context,
        );

        assert_eq!(resolved.headers[0].value, "local-token");
        assert_eq!(resolved.body.as_deref(), Some(r#"{"hello":"Ada"}"#));
        assert_eq!(resolved.cookies[0].value, "local-token");
    }

    #[test]
    fn mock_set_cookie_header_skips_header_injection_values() {
        let unsafe_name = ResponseExampleCookie {
            name: "sid\r\nX-Leak".to_owned(),
            value: "secret".to_owned(),
            domain: None,
            path: None,
            secure: false,
            http_only: false,
            same_site: None,
            expires: None,
            max_age_seconds: None,
        };
        assert!(mock_set_cookie_header(&unsafe_name).is_none());

        let unsafe_attribute = ResponseExampleCookie {
            name: "sid".to_owned(),
            value: "secret".to_owned(),
            domain: Some("example.test\nX-Leak".to_owned()),
            path: None,
            secure: false,
            http_only: false,
            same_site: None,
            expires: None,
            max_age_seconds: None,
        };
        assert_eq!(
            mock_set_cookie_header(&unsafe_attribute).as_deref(),
            Some("sid=secret")
        );
    }

    #[test]
    fn mock_route_path_accepts_variable_based_urls() {
        assert_eq!(
            mock_route_path("{{baseUrl}}/users?limit=10"),
            Some("/users".to_owned())
        );
        assert_eq!(mock_route_path("{{baseUrl}}"), Some("/".to_owned()));
    }

    #[test]
    fn mock_router_returns_generic_404_for_unknown_route() {
        let response = mock_response_for(&[], "GET", "/missing?token=secret");

        assert_eq!(response.status, 404);
        assert_eq!(response.status_text, "Not Found");
        assert!(!String::from_utf8_lossy(&response.body).contains("secret"));
    }

    #[derive(Clone, PartialEq, prost::Message)]
    struct EchoRequest {
        #[prost(string, tag = "1")]
        message: String,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    struct EchoResponse {
        #[prost(string, tag = "1")]
        message: String,
    }

    #[derive(Clone, Default)]
    struct TestGrpcService;

    impl tonic::codegen::Service<tonic::Request<EchoRequest>> for TestGrpcService {
        type Response = tonic::Response<EchoResponse>;
        type Error = tonic::Status;
        type Future = tonic::codegen::BoxFuture<Self::Response, Self::Error>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: tonic::Request<EchoRequest>) -> Self::Future {
            assert_eq!(
                request
                    .metadata()
                    .get("x-test")
                    .and_then(|value| value.to_str().ok()),
                Some("local")
            );
            let message = request.into_inner().message;
            Box::pin(async move {
                Ok(tonic::Response::new(EchoResponse {
                    message: format!("echo:{message}"),
                }))
            })
        }
    }

    #[derive(Clone, Default)]
    struct TestGrpcStreamingService;

    impl tonic::server::ServerStreamingService<EchoRequest> for TestGrpcStreamingService {
        type Response = EchoResponse;
        type ResponseStream =
            Pin<Box<dyn futures_util::Stream<Item = Result<EchoResponse, tonic::Status>> + Send>>;
        type Future =
            tonic::codegen::BoxFuture<tonic::Response<Self::ResponseStream>, tonic::Status>;

        fn call(&mut self, request: tonic::Request<EchoRequest>) -> Self::Future {
            assert_eq!(
                request
                    .metadata()
                    .get("x-test")
                    .and_then(|value| value.to_str().ok()),
                Some("local")
            );
            let message = request.into_inner().message;
            Box::pin(async move {
                let messages = vec![
                    Ok(EchoResponse {
                        message: format!("{message}:1"),
                    }),
                    Ok(EchoResponse {
                        message: format!("{message}:2"),
                    }),
                ];
                let stream: Self::ResponseStream = Box::pin(futures_util::stream::iter(messages));
                Ok(tonic::Response::new(stream))
            })
        }
    }

    #[derive(Clone, Default)]
    struct TestGrpcClientStreamingService;

    impl tonic::server::ClientStreamingService<EchoRequest> for TestGrpcClientStreamingService {
        type Response = EchoResponse;
        type Future = tonic::codegen::BoxFuture<tonic::Response<Self::Response>, tonic::Status>;

        fn call(&mut self, request: tonic::Request<tonic::Streaming<EchoRequest>>) -> Self::Future {
            assert_eq!(
                request
                    .metadata()
                    .get("x-test")
                    .and_then(|value| value.to_str().ok()),
                Some("local")
            );
            let mut stream = request.into_inner();
            Box::pin(async move {
                let mut messages = Vec::new();
                while let Some(message) = stream.message().await? {
                    messages.push(message.message);
                }
                Ok(tonic::Response::new(EchoResponse {
                    message: format!("client:{}", messages.join(",")),
                }))
            })
        }
    }

    #[derive(Clone, Default)]
    struct TestGrpcBidiStreamingService;

    impl tonic::server::StreamingService<EchoRequest> for TestGrpcBidiStreamingService {
        type Response = EchoResponse;
        type ResponseStream =
            Pin<Box<dyn futures_util::Stream<Item = Result<EchoResponse, tonic::Status>> + Send>>;
        type Future =
            tonic::codegen::BoxFuture<tonic::Response<Self::ResponseStream>, tonic::Status>;

        fn call(&mut self, request: tonic::Request<tonic::Streaming<EchoRequest>>) -> Self::Future {
            assert_eq!(
                request
                    .metadata()
                    .get("x-test")
                    .and_then(|value| value.to_str().ok()),
                Some("local")
            );
            let mut stream = request.into_inner();
            Box::pin(async move {
                let mut messages = Vec::new();
                while let Some(message) = stream.message().await? {
                    messages.push(message.message);
                }
                #[allow(clippy::result_large_err)]
                let responses = messages.into_iter().enumerate().map(|(index, message)| {
                    Ok(EchoResponse {
                        message: format!("bidi:{message}:{}", index + 1),
                    })
                });
                let response_stream: Self::ResponseStream =
                    Box::pin(futures_util::stream::iter(responses));
                Ok(tonic::Response::new(response_stream))
            })
        }
    }

    #[derive(Clone, Default)]
    struct TestGrpcServer;

    impl tonic::server::NamedService for TestGrpcServer {
        const NAME: &'static str = "demo.Echo";
    }

    impl tonic::codegen::Service<http::Request<tonic::body::Body>> for TestGrpcServer {
        type Response = http::Response<tonic::body::Body>;
        type Error = Infallible;
        type Future = tonic::codegen::BoxFuture<Self::Response, Self::Error>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: http::Request<tonic::body::Body>) -> Self::Future {
            if request.uri().path() == "/demo.Echo/Echo" {
                Box::pin(async move {
                    let mut grpc = tonic::server::Grpc::new(tonic::codec::ProstCodec::default());
                    Ok(grpc.unary(TestGrpcService, request).await)
                })
            } else if request.uri().path() == "/demo.Echo/EchoStream" {
                Box::pin(async move {
                    let mut grpc = tonic::server::Grpc::new(tonic::codec::ProstCodec::default());
                    Ok(grpc
                        .server_streaming(TestGrpcStreamingService, request)
                        .await)
                })
            } else if request.uri().path() == "/demo.Echo/EchoClient" {
                Box::pin(async move {
                    let mut grpc = tonic::server::Grpc::new(tonic::codec::ProstCodec::default());
                    Ok(grpc
                        .client_streaming(TestGrpcClientStreamingService, request)
                        .await)
                })
            } else if request.uri().path() == "/demo.Echo/EchoBidi" {
                Box::pin(async move {
                    let mut grpc = tonic::server::Grpc::new(tonic::codec::ProstCodec::default());
                    Ok(grpc.streaming(TestGrpcBidiStreamingService, request).await)
                })
            } else {
                Box::pin(async move {
                    let mut response = http::Response::new(tonic::body::Body::empty());
                    *response.status_mut() = http::StatusCode::NOT_FOUND;
                    Ok(response)
                })
            }
        }
    }

    #[tokio::test]
    async fn grpc_command_calls_a_dynamic_unary_method() {
        let directory = tempfile::tempdir().expect("tempdir");
        let proto = directory.path().join("echo.proto");
        std::fs::write(
            &proto,
            r#"
                syntax = "proto3";
                package demo;
                message EchoRequest { string message = 1; }
                message EchoResponse { string message = 1; }
                service Echo {
                    rpc Echo(EchoRequest) returns (EchoResponse);
                    rpc EchoStream(EchoRequest) returns (stream EchoResponse);
                    rpc EchoClient(stream EchoRequest) returns (EchoResponse);
                    rpc EchoBidi(stream EchoRequest) returns (stream EchoResponse);
                }
            "#,
        )
        .expect("proto");

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(TestGrpcServer)
                .serve_with_incoming_shutdown(
                    tonic::transport::server::TcpIncoming::from(listener),
                    async {
                        let _ = shutdown_rx.await;
                    },
                )
                .await
                .expect("gRPC server");
        });

        call_grpc(GrpcCallOptions {
            endpoint: format!("http://{address}"),
            proto,
            includes: Vec::new(),
            method: "/demo.Echo/Echo".to_owned(),
            message: Some(r#"{"message":"hello"}"#.to_owned()),
            message_file: None,
            metadata: vec!["x-test=local".to_owned()],
            bearer: None,
            basic_user: None,
            basic_password: None,
            timeout: 10,
            proxy: None,
            no_proxy: None,
            ca_cert: None,
            client_identity: None,
            output_json: true,
        })
        .await
        .expect("gRPC command");

        call_grpc(GrpcCallOptions {
            endpoint: format!("http://{address}"),
            proto: directory.path().join("echo.proto"),
            includes: Vec::new(),
            method: "/demo.Echo/EchoStream".to_owned(),
            message: Some(r#"{"message":"hello"}"#.to_owned()),
            message_file: None,
            metadata: vec!["x-test=local".to_owned()],
            bearer: None,
            basic_user: None,
            basic_password: None,
            timeout: 10,
            proxy: None,
            no_proxy: None,
            ca_cert: None,
            client_identity: None,
            output_json: true,
        })
        .await
        .expect("gRPC server-streaming command");

        call_grpc(GrpcCallOptions {
            endpoint: format!("http://{address}"),
            proto: directory.path().join("echo.proto"),
            includes: Vec::new(),
            method: "/demo.Echo/EchoClient".to_owned(),
            message: Some(r#"[{"message":"one"},{"message":"two"}]"#.to_owned()),
            message_file: None,
            metadata: vec!["x-test=local".to_owned()],
            bearer: None,
            basic_user: None,
            basic_password: None,
            timeout: 10,
            proxy: None,
            no_proxy: None,
            ca_cert: None,
            client_identity: None,
            output_json: true,
        })
        .await
        .expect("gRPC client-streaming command");

        call_grpc(GrpcCallOptions {
            endpoint: format!("http://{address}"),
            proto: directory.path().join("echo.proto"),
            includes: Vec::new(),
            method: "/demo.Echo/EchoBidi".to_owned(),
            message: Some(r#"[{"message":"one"},{"message":"two"}]"#.to_owned()),
            message_file: None,
            metadata: vec!["x-test=local".to_owned()],
            bearer: None,
            basic_user: None,
            basic_password: None,
            timeout: 10,
            proxy: None,
            no_proxy: None,
            ca_cert: None,
            client_identity: None,
            output_json: true,
        })
        .await
        .expect("gRPC bidirectional-streaming command");

        let proxy_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("proxy listener");
        let proxy_address = proxy_listener.local_addr().expect("proxy address");
        let proxy = tokio::spawn(async move {
            let (mut client, _) = proxy_listener.accept().await.expect("proxy connection");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = client.read(&mut buffer).await.expect("proxy request");
                assert!(count > 0, "proxy client closed before CONNECT");
                request.extend_from_slice(&buffer[..count]);
                assert!(request.len() <= 64 * 1024, "proxy request is too large");
            }
            let request = String::from_utf8_lossy(&request);
            assert!(request.starts_with(&format!("CONNECT {address} HTTP/1.1")));
            client
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .expect("proxy response");
            let mut target = tokio::net::TcpStream::connect(address)
                .await
                .expect("proxy target");
            tokio::io::copy_bidirectional(&mut client, &mut target)
                .await
                .expect("proxy relay");
        });

        call_grpc(GrpcCallOptions {
            endpoint: format!("http://{address}"),
            proto: directory.path().join("echo.proto"),
            includes: Vec::new(),
            method: "/demo.Echo/Echo".to_owned(),
            message: Some(r#"{"message":"through-proxy"}"#.to_owned()),
            message_file: None,
            metadata: vec!["x-test=local".to_owned()],
            bearer: None,
            basic_user: None,
            basic_password: None,
            timeout: 10,
            proxy: Some(format!("http://{proxy_address}")),
            no_proxy: None,
            ca_cert: None,
            client_identity: None,
            output_json: true,
        })
        .await
        .expect("gRPC proxy command");
        proxy.await.expect("proxy task");

        let socks_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("SOCKS proxy listener");
        let socks_address = socks_listener.local_addr().expect("SOCKS proxy address");
        let socks_proxy = tokio::spawn(async move {
            let (mut client, _) = socks_listener.accept().await.expect("SOCKS connection");
            let mut greeting = [0_u8; 3];
            client
                .read_exact(&mut greeting)
                .await
                .expect("SOCKS greeting");
            assert_eq!(greeting, [0x05, 0x01, 0x00]);
            client
                .write_all(&[0x05, 0x00])
                .await
                .expect("SOCKS greeting reply");

            let mut connect_header = [0_u8; 4];
            client
                .read_exact(&mut connect_header)
                .await
                .expect("SOCKS connect header");
            assert_eq!(connect_header, [0x05, 0x01, 0x00, 0x01]);
            let mut target_ip = [0_u8; 4];
            client
                .read_exact(&mut target_ip)
                .await
                .expect("SOCKS target IP");
            let mut target_port = [0_u8; 2];
            client
                .read_exact(&mut target_port)
                .await
                .expect("SOCKS target port");
            assert_eq!(target_ip, [127, 0, 0, 1]);
            assert_eq!(u16::from_be_bytes(target_port), address.port());

            let mut target = tokio::net::TcpStream::connect(address)
                .await
                .expect("SOCKS target");
            let mut reply = vec![0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1];
            reply.extend_from_slice(&address.port().to_be_bytes());
            client.write_all(&reply).await.expect("SOCKS connect reply");
            tokio::io::copy_bidirectional(&mut client, &mut target)
                .await
                .expect("SOCKS relay");
        });

        call_grpc(GrpcCallOptions {
            endpoint: format!("http://{address}"),
            proto: directory.path().join("echo.proto"),
            includes: Vec::new(),
            method: "/demo.Echo/Echo".to_owned(),
            message: Some(r#"{"message":"through-socks"}"#.to_owned()),
            message_file: None,
            metadata: vec!["x-test=local".to_owned()],
            bearer: None,
            basic_user: None,
            basic_password: None,
            timeout: 10,
            proxy: Some(format!("socks5://{socks_address}")),
            no_proxy: None,
            ca_cert: None,
            client_identity: None,
            output_json: true,
        })
        .await
        .expect("gRPC SOCKS proxy command");
        socks_proxy.await.expect("SOCKS proxy task");

        shutdown_tx.send(()).expect("shutdown");
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn grpc_command_supports_custom_ca_and_client_identity() {
        let directory = tempfile::tempdir().expect("directory");
        let proto = directory.path().join("echo.proto");
        std::fs::write(
            &proto,
            r#"
                syntax = "proto3";
                package demo;
                message EchoRequest { string message = 1; }
                message EchoResponse { string message = 1; }
                service Echo { rpc Echo(EchoRequest) returns (EchoResponse); }
            "#,
        )
        .expect("proto");
        let ca_path = directory.path().join("ca.pem");
        std::fs::write(&ca_path, TEST_CA_PEM).expect("CA");
        let client_identity_path = directory.path().join("client-identity.pem");
        std::fs::write(
            &client_identity_path,
            format!("{TEST_CLIENT_CERT_PEM}\n{TEST_CLIENT_KEY_PEM}"),
        )
        .expect("client identity");

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let tls = tonic::transport::ServerTlsConfig::new()
            .identity(tonic::transport::Identity::from_pem(
                TEST_SERVER_CERT_PEM,
                TEST_SERVER_KEY_PEM,
            ))
            .client_ca_root(tonic::transport::Certificate::from_pem(TEST_CA_PEM));
        let server = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .tls_config(tls)
                .expect("TLS config")
                .add_service(TestGrpcServer)
                .serve_with_incoming_shutdown(
                    tonic::transport::server::TcpIncoming::from(listener),
                    async {
                        let _ = shutdown_rx.await;
                    },
                )
                .await
                .expect("gRPC TLS server");
        });

        call_grpc(GrpcCallOptions {
            endpoint: format!("https://localhost:{}", address.port()),
            proto: proto.clone(),
            includes: Vec::new(),
            method: "/demo.Echo/Echo".to_owned(),
            message: Some(r#"{"message":"secure"}"#.to_owned()),
            message_file: None,
            metadata: vec!["x-test=local".to_owned()],
            bearer: None,
            basic_user: None,
            basic_password: None,
            timeout: 10,
            proxy: None,
            no_proxy: None,
            ca_cert: Some(ca_path.clone()),
            client_identity: Some(client_identity_path),
            output_json: true,
        })
        .await
        .expect("gRPC mTLS command");

        let Some(pkcs12_identity_path) = create_test_pkcs12_identity(directory.path()) else {
            return;
        };
        call_grpc_with_passphrase(
            GrpcCallOptions {
                endpoint: format!("https://localhost:{}", address.port()),
                proto,
                includes: Vec::new(),
                method: "/demo.Echo/Echo".to_owned(),
                message: Some(r#"{"message":"secure-pkcs12"}"#.to_owned()),
                message_file: None,
                metadata: vec!["x-test=local".to_owned()],
                bearer: None,
                basic_user: None,
                basic_password: None,
                timeout: 10,
                proxy: None,
                no_proxy: None,
                ca_cert: Some(ca_path),
                client_identity: Some(pkcs12_identity_path),
                output_json: true,
            },
            Some(TEST_PKCS12_PASSWORD),
        )
        .await
        .expect("gRPC PKCS#12 mTLS command");

        shutdown_tx.send(()).expect("shutdown");
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn graphql_command_sends_a_query_and_accepts_data_response() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("connection");
            let mut request = [0_u8; 8192];
            let length = socket.read(&mut request).await.expect("request");
            let request = String::from_utf8_lossy(&request[..length]);
            assert!(request.contains("POST /graphql HTTP/1.1"));
            assert!(request.contains("query User"));
            assert!(request.contains("\"id\":\"42\""));
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 29\r\n\r\n{\"data\":{\"user\":{\"id\":\"42\"}}}",
                )
                .await
                .expect("response");
        });

        send_graphql_request(GraphqlOptions {
            endpoint: format!("http://{address}/graphql"),
            query: Some("query User($id: ID!) { user(id: $id) { id } }".to_owned()),
            query_file: None,
            variables: vec!["id=42".to_owned()],
            variables_json: None,
            operation_name: Some("User".to_owned()),
            headers: Vec::new(),
            bearer: None,
            basic_user: None,
            basic_password: None,
            timeout: 10,
            max_redirects: 10,
            proxy: None,
            no_proxy: None,
            ca_cert: None,
            client_identity: None,
            insecure: false,
            output_json: true,
        })
        .await
        .expect("GraphQL command");
        server.await.expect("server");
    }

    #[tokio::test]
    async fn graphql_introspection_command_parses_schema() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("connection");
            let mut request = [0_u8; 8192];
            let length = socket.read(&mut request).await.expect("request");
            let request = String::from_utf8_lossy(&request[..length]);
            assert!(request.contains("POST /graphql HTTP/1.1"));
            assert!(request.contains("PostlySchemaIntrospection"));
            assert!(request.contains("__schema"));
            let body = br#"{"data":{"__schema":{"queryType":{"name":"Query"},"mutationType":null,"subscriptionType":null,"types":[{"kind":"OBJECT","name":"Query","description":null,"fields":[{"name":"health","description":"Health check","args":[],"type":{"kind":"SCALAR","name":"String","ofType":null},"isDeprecated":false,"deprecationReason":null}],"inputFields":null,"enumValues":null,"possibleTypes":null}]}}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
                body.len()
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("headers");
            socket.write_all(body).await.expect("response");
        });

        introspect_graphql_schema(GraphqlOptions {
            endpoint: format!("http://{address}/graphql"),
            query: None,
            query_file: None,
            variables: Vec::new(),
            variables_json: None,
            operation_name: None,
            headers: Vec::new(),
            bearer: None,
            basic_user: None,
            basic_password: None,
            timeout: 10,
            max_redirects: 10,
            proxy: None,
            no_proxy: None,
            ca_cert: None,
            client_identity: None,
            insecure: false,
            output_json: true,
        })
        .await
        .expect("GraphQL introspection command");
        server.await.expect("server");
    }

    #[tokio::test]
    async fn imports_openapi_from_a_local_http_url_and_preserves_source_label() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("connection");
            let mut request = [0_u8; 4096];
            let length = socket.read(&mut request).await.expect("request");
            let request = String::from_utf8_lossy(&request[..length]);
            assert!(request.contains("GET /schema HTTP/1.1"));
            let body = br#"openapi: 3.0.3
info:
  title: Remote API
paths:
  /health:
    get:
      operationId: health
"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/yaml\r\ncontent-length: {}\r\n\r\n",
                body.len()
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("headers");
            socket.write_all(body).await.expect("OpenAPI document");
        });

        let output = tempfile::tempdir().expect("output");
        let source = format!("http://{address}/schema");
        let report = import_openapi_source(Path::new(&source), output.path())
            .await
            .expect("URL import");
        assert_eq!(report.source, PathBuf::from(&source));
        assert_eq!(report.imported_operations, 1);
        let workspace = Workspace::open(output.path()).expect("workspace");
        assert_eq!(workspace.collections().expect("collections").len(), 1);
        server.await.expect("server");
    }

    #[tokio::test]
    async fn sse_command_streams_events_from_a_local_endpoint() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("connection");
            let mut request = [0_u8; 8192];
            let length = socket.read(&mut request).await.expect("request");
            let request = String::from_utf8_lossy(&request[..length]);
            assert!(request.contains("GET /events HTTP/1.1"));
            assert!(request.contains("accept: text/event-stream"));
            let body = b"id: 1\nevent: update\ndata: {\"ok\":true}\n\nid: 2\ndata: done";
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n",
                body.len()
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("headers");
            socket.write_all(body).await.expect("events");
        });

        stream_sse(SseOptions {
            endpoint: format!("http://{address}/events"),
            headers: Vec::new(),
            bearer: None,
            basic_user: None,
            basic_password: None,
            timeout: 10,
            max_redirects: 10,
            reconnect: 0,
            proxy: None,
            no_proxy: None,
            ca_cert: None,
            client_identity: None,
            insecure: false,
            output_json: true,
        })
        .await
        .expect("SSE command");
        server.await.expect("server");
    }

    #[tokio::test]
    async fn sse_command_reconnects_with_the_last_event_id() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let responses: [(Option<&str>, &[u8]); 2] = [
                (None, b"id: first\ndata: one\n\n"),
                (Some("first"), b"id: second\ndata: two\n\n"),
            ];
            for (last_event_id, body) in responses {
                let (mut socket, _) = listener.accept().await.expect("connection");
                let mut request = [0_u8; 8192];
                let length = socket.read(&mut request).await.expect("request");
                let request = String::from_utf8_lossy(&request[..length]);
                assert!(request.contains("GET /events HTTP/1.1"));
                if let Some(last_event_id) = last_event_id {
                    assert!(request.contains(&format!("last-event-id: {last_event_id}")));
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n",
                    body.len()
                );
                socket
                    .write_all(response.as_bytes())
                    .await
                    .expect("headers");
                socket.write_all(body).await.expect("events");
            }
        });

        stream_sse(SseOptions {
            endpoint: format!("http://{address}/events"),
            headers: Vec::new(),
            bearer: None,
            basic_user: None,
            basic_password: None,
            timeout: 10,
            max_redirects: 10,
            reconnect: 1,
            proxy: None,
            no_proxy: None,
            ca_cert: None,
            client_identity: None,
            insecure: false,
            output_json: true,
        })
        .await
        .expect("SSE reconnect command");
        server.await.expect("server");
    }

    #[tokio::test]
    async fn websocket_command_sends_text_and_receives_a_message() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("connection");
            let mut socket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("WebSocket handshake");
            match socket.next().await.expect("message").expect("frame") {
                Message::Text(text) => assert_eq!(text.to_string(), "hello"),
                message => panic!("expected text message, got {message:?}"),
            }
            socket
                .send(Message::text("echo: hello"))
                .await
                .expect("echo");
            socket.send(Message::Close(None)).await.expect("close");
        });

        run_websocket(WebsocketOptions {
            endpoint: format!("ws://{address}/socket"),
            send: vec!["hello".to_owned()],
            headers: Vec::new(),
            bearer: None,
            basic_user: None,
            basic_password: None,
            timeout: 10,
            reconnect: 0,
            proxy: None,
            no_proxy: None,
            ca_cert: None,
            client_identity: None,
            insecure: false,
            output_json: true,
        })
        .await
        .expect("WebSocket command");
        server.await.expect("server");
    }

    #[tokio::test]
    async fn websocket_command_routes_through_an_http_connect_proxy() {
        let target_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("target listener");
        let target_address = target_listener.local_addr().expect("target address");
        let target_server = tokio::spawn(async move {
            let (stream, _) = target_listener.accept().await.expect("target connection");
            let mut socket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("WebSocket handshake");
            match socket.next().await.expect("message").expect("frame") {
                Message::Text(text) => assert_eq!(text.to_string(), "through-proxy"),
                message => panic!("expected text message, got {message:?}"),
            }
            socket
                .send(Message::text("proxy echo"))
                .await
                .expect("echo");
            socket.send(Message::Close(None)).await.expect("close");
        });

        let proxy_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("proxy listener");
        let proxy_address = proxy_listener.local_addr().expect("proxy address");
        let proxy_server = tokio::spawn(async move {
            let (mut proxy_socket, _) = proxy_listener.accept().await.expect("proxy connection");
            let mut request = [0_u8; 4096];
            let length = proxy_socket
                .read(&mut request)
                .await
                .expect("CONNECT request");
            let request = String::from_utf8_lossy(&request[..length]);
            assert!(request.contains(&format!(
                "CONNECT 127.0.0.1:{} HTTP/1.1",
                target_address.port()
            )));
            let mut target_socket = tokio::net::TcpStream::connect(target_address)
                .await
                .expect("target connection through proxy");
            proxy_socket
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .expect("CONNECT response");
            tokio::io::copy_bidirectional(&mut proxy_socket, &mut target_socket)
                .await
                .expect("proxy relay");
        });

        run_websocket(WebsocketOptions {
            endpoint: format!("ws://{target_address}/socket"),
            send: vec!["through-proxy".to_owned()],
            headers: Vec::new(),
            bearer: None,
            basic_user: None,
            basic_password: None,
            timeout: 10,
            reconnect: 0,
            proxy: Some(format!("http://{proxy_address}")),
            no_proxy: None,
            ca_cert: None,
            client_identity: None,
            insecure: false,
            output_json: true,
        })
        .await
        .expect("WebSocket proxy command");
        target_server.await.expect("target server");
        proxy_server.await.expect("proxy server");
    }

    #[tokio::test]
    async fn websocket_command_routes_through_a_socks5_proxy() {
        let target_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("target listener");
        let target_address = target_listener.local_addr().expect("target address");
        let target_server = tokio::spawn(async move {
            let (stream, _) = target_listener.accept().await.expect("target connection");
            let mut socket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("WebSocket handshake");
            match socket.next().await.expect("message").expect("frame") {
                Message::Text(text) => assert_eq!(text.to_string(), "through-socks"),
                message => panic!("expected text message, got {message:?}"),
            }
            socket
                .send(Message::text("socks echo"))
                .await
                .expect("echo");
            socket.send(Message::Close(None)).await.expect("close");
        });

        let proxy_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("proxy listener");
        let proxy_address = proxy_listener.local_addr().expect("proxy address");
        let proxy_server = tokio::spawn(async move {
            let (mut client, _) = proxy_listener.accept().await.expect("proxy connection");
            let mut greeting = [0_u8; 3];
            client.read_exact(&mut greeting).await.expect("greeting");
            assert_eq!(greeting, [0x05, 0x01, 0x00]);
            client
                .write_all(&[0x05, 0x00])
                .await
                .expect("greeting reply");

            let mut connect_header = [0_u8; 4];
            client
                .read_exact(&mut connect_header)
                .await
                .expect("connect header");
            assert_eq!(connect_header, [0x05, 0x01, 0x00, 0x01]);
            let mut target_ip = [0_u8; 4];
            client.read_exact(&mut target_ip).await.expect("target ip");
            let mut target_port = [0_u8; 2];
            client
                .read_exact(&mut target_port)
                .await
                .expect("target port");
            assert_eq!(target_ip, [127, 0, 0, 1]);
            assert_eq!(u16::from_be_bytes(target_port), target_address.port());

            let mut target = tokio::net::TcpStream::connect(target_address)
                .await
                .expect("target relay connection");
            let mut reply = vec![0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1];
            reply.extend_from_slice(&target_address.port().to_be_bytes());
            client.write_all(&reply).await.expect("connect reply");
            tokio::io::copy_bidirectional(&mut client, &mut target)
                .await
                .expect("proxy relay");
        });

        run_websocket(WebsocketOptions {
            endpoint: format!("ws://{target_address}/socket"),
            send: vec!["through-socks".to_owned()],
            headers: Vec::new(),
            bearer: None,
            basic_user: None,
            basic_password: None,
            timeout: 10,
            reconnect: 0,
            proxy: Some(format!("socks5://{proxy_address}")),
            no_proxy: None,
            ca_cert: None,
            client_identity: None,
            insecure: false,
            output_json: true,
        })
        .await
        .expect("WebSocket SOCKS proxy command");
        target_server.await.expect("target server");
        proxy_server.await.expect("proxy server");
    }

    #[tokio::test]
    async fn websocket_command_reconnects_a_bounded_number_of_times() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            for reply in [None, Some("after reconnect")] {
                let (stream, _) = listener.accept().await.expect("connection");
                let mut socket = tokio_tungstenite::accept_async(stream)
                    .await
                    .expect("WebSocket handshake");
                match socket.next().await.expect("message").expect("frame") {
                    Message::Text(text) => assert_eq!(text.to_string(), "hello"),
                    message => panic!("expected text message, got {message:?}"),
                }
                if let Some(reply) = reply {
                    socket.send(Message::text(reply)).await.expect("reply");
                }
                socket.send(Message::Close(None)).await.expect("close");
            }
        });

        run_websocket(WebsocketOptions {
            endpoint: format!("ws://{address}/socket"),
            send: vec!["hello".to_owned()],
            headers: Vec::new(),
            bearer: None,
            basic_user: None,
            basic_password: None,
            timeout: 10,
            reconnect: 1,
            proxy: None,
            no_proxy: None,
            ca_cert: None,
            client_identity: None,
            insecure: false,
            output_json: true,
        })
        .await
        .expect("WebSocket reconnect command");
        server.await.expect("server");
    }

    #[test]
    fn folder_filter_includes_nested_requests_and_normalizes_separators() {
        let mut nested = Request::new("Nested", "GET", "https://example.test/nested");
        nested.folder = Some("Auth/OAuth".to_owned());
        let mut sibling = Request::new("Sibling", "GET", "https://example.test/sibling");
        sibling.folder = Some("Users".to_owned());

        assert!(request_belongs_to_folder(&nested, "Auth"));
        assert!(request_belongs_to_folder(&nested, "/Auth\\"));
        assert!(request_belongs_to_folder(&nested, "Auth/OAuth"));
        assert!(!request_belongs_to_folder(&nested, "Aut"));
        assert!(!request_belongs_to_folder(&sibling, "Auth"));
    }

    #[test]
    fn no_proxy_matches_exact_hosts_domains_and_ports() {
        assert!(no_proxy_matches("localhost", 80, "localhost,127.0.0.1"));
        assert!(no_proxy_matches(
            "api.internal.example",
            443,
            ".internal.example"
        ));
        assert!(no_proxy_matches(
            "api.example.test",
            8443,
            "api.example.test:8443"
        ));
        assert!(!no_proxy_matches(
            "api.example.test",
            443,
            "api.example.test:8443"
        ));
        assert!(no_proxy_matches("anything.example.test", 443, "*"));
    }

    #[test]
    fn junit_report_includes_script_test_details_and_escapes_errors() {
        let summary = postly_core::RunnerSummary {
            requests: 1,
            passed: 0,
            failed: 1,
            results: vec![postly_core::RunnerItemResult {
                path: PathBuf::from("requests/health.postly.toml"),
                iteration: 1,
                name: "Health".to_owned(),
                method: "GET".to_owned(),
                status: Some(500),
                duration_ms: 42,
                error: Some("1 assertion failed".to_owned()),
                passed: false,
                assertions: 1,
                assertion_failures: vec!["status: expected 200".to_owned()],
                script_tests: vec![ScriptTestResult {
                    name: "response body".to_owned(),
                    passed: false,
                    duration_ms: 7,
                    error: Some("expected <ok>".to_owned()),
                }],
            }],
            ..postly_core::RunnerSummary::default()
        };

        let report = render_junit(&[summary]);
        assert!(report
            .contains("<system-out>FAIL response body (7 ms): expected &lt;ok&gt;</system-out>"));
        assert!(report
            .contains("<failure message=\"1 assertion failed\">status: expected 200</failure>"));
    }

    #[tokio::test]
    async fn run_workspace_honors_a_pre_cancelled_token_without_network_work() {
        let directory = tempfile::tempdir().expect("tempdir");
        let workspace = Workspace::init(directory.path(), "Cancelled run").expect("workspace");
        let collection = workspace
            .create_collection(&Collection::new("API"))
            .expect("collection");
        workspace
            .save_request(
                &collection,
                &Request::new("Never sent", "GET", "http://127.0.0.1:1/never"),
            )
            .expect("request");

        let cancellation = CancellationToken::default();
        cancellation.cancel();
        let error = run_workspace_with_cancellation(
            RunOptions {
                path: directory.path(),
                environment_name: None,
                folder: None,
                fail_fast: false,
                scripts: false,
                concurrency: 1,
                timeout: 10,
                max_redirects: 10,
                proxy: None,
                no_proxy: None,
                ca_cert: None,
                client_identity: None,
                reporter: Reporter::Json,
                data_file: None,
            },
            cancellation,
        )
        .await
        .expect_err("cancelled run should fail explicitly");

        assert_eq!(error.to_string(), "collection run cancelled");
    }

    #[tokio::test]
    async fn run_workspace_executes_only_the_requested_folder() {
        let directory = tempfile::tempdir().expect("tempdir");
        let workspace = Workspace::init(directory.path(), "Folder runner").expect("workspace");
        let collection = workspace
            .create_collection(&Collection::new("API"))
            .expect("collection");
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("connection");
            let mut request = [0_u8; 2048];
            let length = socket.read(&mut request).await.expect("request");
            assert!(String::from_utf8_lossy(&request[..length]).contains("GET /auth HTTP/1.1"));
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nconnection: close\r\ncontent-length: 0\r\n\r\n")
                .await
                .expect("response");
        });

        let mut selected = Request::new("Auth request", "GET", format!("http://{address}/auth"));
        selected.folder = Some("Auth/OAuth".to_owned());
        workspace
            .save_request(&collection, &selected)
            .expect("selected");

        let mut skipped = Request::new("Skipped request", "GET", "http://127.0.0.1:0/skipped");
        skipped.folder = Some("Users".to_owned());
        workspace
            .save_request(&collection, &skipped)
            .expect("skipped");

        run_workspace(RunOptions {
            path: directory.path(),
            environment_name: None,
            folder: Some("Auth"),
            fail_fast: false,
            scripts: false,
            concurrency: 1,
            timeout: 10,
            max_redirects: 10,
            proxy: None,
            no_proxy: None,
            ca_cert: None,
            client_identity: None,
            reporter: Reporter::Json,
            data_file: None,
        })
        .await
        .expect("folder run");
        server.await.expect("server");
    }
}
