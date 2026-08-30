use std::{
    fs,
    io::{self, BufRead, BufReader, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use base64::Engine;
use clap::{Args, Parser, Subcommand, ValueEnum};
use futures_util::{SinkExt, StreamExt};
use postly_core::{
    export_postman_collection, export_postman_environment_with_store, generate_code_snippet,
    generate_markdown_docs, import_curl_command, import_dotenv, import_environment,
    import_postman_collection, message_from_json, message_to_json, parse_graphql_response,
    parse_graphql_schema, parse_variables_json, run_requests, schema_introspection_query, Auth,
    Collection, EngineOptions, Environment, EnvironmentVariable, GraphqlRequest, GrpcSchema,
    HeaderEntry, HistoryEntry, HistoryFilter, HistoryOutcome, HttpEngine, Request, RequestBody,
    ResponseExample, RunnerOptions, ScriptResult, ScriptTestResult, SecretStore, SnippetLanguage,
    SseParser, VariableContext, Workspace,
};
use prost::Message as ProstMessage;
use prost_reflect::{DynamicMessage, MessageDescriptor};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest,
        http::{header::HeaderName, HeaderValue},
        Message,
    },
};
use tracing_subscriber::EnvFilter;

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
    oauth_token_url: Option<String>,
    oauth_client_id: Option<String>,
    oauth_client_secret: Option<String>,
    oauth_scope: Option<String>,
    oauth_authorization_url: Option<String>,
    oauth_redirect_uri: Option<String>,
    oauth_code: Option<String>,
    oauth_code_verifier: Option<String>,
    oauth_refresh_token: Option<String>,
    timeout: u64,
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
    ca_cert: Option<PathBuf>,
    client_identity: Option<PathBuf>,
    output_json: bool,
}

fn configure_grpc_endpoint(
    endpoint_value: &str,
    timeout: u64,
    ca_cert: Option<&Path>,
    client_identity: Option<&Path>,
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
        help = "Use a combined PEM client certificate and private key for HTTPS"
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
    output_json: bool,
}

struct RunOptions<'a> {
    path: &'a Path,
    environment_name: Option<&'a str>,
    folder: Option<&'a str>,
    fail_fast: bool,
    scripts: bool,
    timeout: u64,
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
    proxy: Option<&'a str>,
    no_proxy: Option<&'a str>,
    ca_cert: Option<&'a Path>,
    client_identity: Option<&'a Path>,
    insecure: bool,
    output_json: bool,
}

struct ExecuteOptions<'a> {
    timeout: u64,
    proxy: Option<&'a str>,
    no_proxy: Option<&'a str>,
    ca_cert: Option<&'a Path>,
    client_identity: Option<&'a Path>,
    insecure: bool,
    cookie_jar: Option<&'a Path>,
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
    oauth_token_url: Option<String>,
    oauth_client_id: Option<String>,
    oauth_client_secret: Option<String>,
    oauth_scope: Option<String>,
    oauth_authorization_url: Option<String>,
    oauth_redirect_uri: Option<String>,
    oauth_code: Option<String>,
    oauth_code_verifier: Option<String>,
    oauth_refresh_token: Option<String>,
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
        #[command(flatten)]
        oauth: Box<OAuthCliArgs>,
        #[arg(long, default_value_t = 30)]
        timeout: u64,
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
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 3000)]
        port: u16,
        #[arg(long, help = "Serve one request, then exit; useful for local tests")]
        once: bool,
    },
    /// Execute every saved request in a collection, sequentially.
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
        #[arg(long, default_value_t = 30)]
        timeout: u64,
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
                oauth_token_url: oauth.oauth_token_url,
                oauth_client_id: oauth.oauth_client_id,
                oauth_client_secret: oauth.oauth_client_secret,
                oauth_scope: oauth.oauth_scope,
                oauth_authorization_url: oauth.oauth_authorization_url,
                oauth_redirect_uri: oauth.oauth_redirect_uri,
                oauth_code: oauth.oauth_code,
                oauth_code_verifier: oauth.oauth_code_verifier,
                oauth_refresh_token: oauth.oauth_refresh_token,
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
            oauth,
            timeout,
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
                oauth_token_url: oauth.oauth_token_url,
                oauth_client_id: oauth.oauth_client_id,
                oauth_client_secret: oauth.oauth_client_secret,
                oauth_scope: oauth.oauth_scope,
                oauth_authorization_url: oauth.oauth_authorization_url,
                oauth_redirect_uri: oauth.oauth_redirect_uri,
                oauth_code: oauth.oauth_code,
                oauth_code_verifier: oauth.oauth_code_verifier,
                oauth_refresh_token: oauth.oauth_refresh_token,
                timeout,
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
            proxy,
            no_proxy,
            ca_cert,
            client_identity,
            insecure,
        } => {
            send_saved_request(SendOptions {
                file: &file,
                environment_name: environment.as_deref(),
                scripts,
                timeout,
                proxy: proxy.as_deref(),
                no_proxy: no_proxy.as_deref(),
                ca_cert: ca_cert.as_deref(),
                client_identity: client_identity.as_deref(),
                insecure,
                output_json,
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
        Command::Snippet {
            file,
            language,
            output_json,
        } => print_snippet(&file, language.into(), output_json),
        Command::Mock {
            path,
            host,
            port,
            once,
        } => run_mock_server(&path, &host, port, once).await,
        Command::Run {
            path,
            environment,
            folder,
            fail_fast,
            scripts,
            timeout,
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
                timeout,
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

fn load_mock_routes(path: &Path) -> Result<Vec<MockRoute>> {
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
        for (_, request) in workspace.requests(&collection)? {
            let Some(route_path) = mock_route_path(&request.url) else {
                continue;
            };
            for example in request.examples {
                routes.push(MockRoute {
                    method: request.method.to_ascii_uppercase(),
                    path: route_path.clone(),
                    example,
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
    MockResponse {
        status: example.status.unwrap_or(200),
        status_text: status_text(example.status.unwrap_or(200)).to_owned(),
        headers,
        body: example.body.clone().unwrap_or_default().into_bytes(),
        delay_ms: example.delay_ms,
    }
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

async fn run_mock_server(path: &Path, host: &str, port: u16, once: bool) -> Result<()> {
    let routes = load_mock_routes(path)?;
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
    request.auth = parse_auth_flags_with_oauth(
        options.bearer,
        options.basic_user,
        options.basic_password,
        OAuthCliArgs {
            oauth_token_url: options.oauth_token_url,
            oauth_client_id: options.oauth_client_id,
            oauth_client_secret: options.oauth_client_secret,
            oauth_scope: options.oauth_scope,
            oauth_authorization_url: options.oauth_authorization_url,
            oauth_redirect_uri: options.oauth_redirect_uri,
            oauth_code: options.oauth_code,
            oauth_code_verifier: options.oauth_code_verifier,
            oauth_refresh_token: options.oauth_refresh_token,
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
    request.auth = parse_auth_flags_with_oauth(
        options.bearer,
        options.basic_user,
        options.basic_password,
        OAuthCliArgs {
            oauth_token_url: options.oauth_token_url,
            oauth_client_id: options.oauth_client_id,
            oauth_client_secret: options.oauth_client_secret,
            oauth_scope: options.oauth_scope,
            oauth_authorization_url: options.oauth_authorization_url,
            oauth_redirect_uri: options.oauth_redirect_uri,
            oauth_code: options.oauth_code,
            oauth_code_verifier: options.oauth_code_verifier,
            oauth_refresh_token: options.oauth_refresh_token,
        },
    )?;
    request.body = parse_cli_body(options.data, options.json_body)?;
    let response = execute(
        &request,
        VariableContext::default(),
        ExecuteOptions {
            timeout: options.timeout,
            proxy: options.proxy.as_deref(),
            no_proxy: options.no_proxy.as_deref(),
            ca_cert: options.ca_cert.as_deref(),
            client_identity: options.client_identity.as_deref(),
            insecure: options.insecure,
            cookie_jar: None,
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
            proxy: options.proxy.as_deref(),
            no_proxy: options.no_proxy.as_deref(),
            ca_cert: options.ca_cert.as_deref(),
            client_identity: options.client_identity.as_deref(),
            insecure: options.insecure,
            cookie_jar: None,
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
            "protocol": response.protocol,
            "url": response.url,
            "graphql": graphql,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!(
            "{} {} · {} ms · {} bytes · {}",
            response.status,
            response.status_text,
            response.duration_ms,
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
            proxy: options.proxy.as_deref(),
            no_proxy: options.no_proxy.as_deref(),
            ca_cert: options.ca_cert.as_deref(),
            client_identity: options.client_identity.as_deref(),
            insecure: options.insecure,
            cookie_jar: None,
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
                "protocol": response.protocol,
                "url": response.url,
                "schema": schema,
            }))?
        );
        return Ok(());
    }

    println!(
        "GraphQL schema · {} {} · {} ms",
        response.status, response.status_text, response.duration_ms
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
    let endpoint = configure_grpc_endpoint(
        &options.endpoint,
        options.timeout,
        options.tls.ca_cert.as_deref(),
        options.tls.client_identity.as_deref(),
    )?;
    let channel = endpoint
        .connect()
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

    let endpoint = configure_grpc_endpoint(
        &options.endpoint,
        options.timeout,
        options.ca_cert.as_deref(),
        options.client_identity.as_deref(),
    )?;
    let channel = endpoint
        .connect()
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
        accept_invalid_certs: options.insecure,
        proxy: options.proxy.clone(),
        no_proxy: options.no_proxy.clone(),
        ca_cert: options.ca_cert.clone(),
        client_identity: options.client_identity.clone(),
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
    let mut reconnects_used = 0;
    loop {
        let websocket_request = build_websocket_request(&options)?;
        let connection = tokio::time::timeout(
            Duration::from_secs(options.timeout),
            connect_async(websocket_request),
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
            Err(error) => return Err(error.into()),
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
            proxy: options.proxy,
            no_proxy: options.no_proxy,
            ca_cert: options.ca_cert,
            client_identity: options.client_identity,
            insecure: options.insecure,
            cookie_jar: Some(&workspace.root().join(".postly/cookies.json")),
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
    print_response_with_tests(
        &response,
        options.output_json,
        post_script.as_ref().map(|script| script.tests.as_slice()),
    )?;
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
        ImportKind::Environment { input, output } => {
            let report = import_environment(input, output)?;
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
        return Ok(postly_core::import_openapi(input, output)?);
    }

    let response = reqwest::Client::new()
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
    let bytes = response
        .bytes()
        .await
        .context("could not read OpenAPI URL response")?;
    if bytes.len() > MAX_OPENAPI_DOWNLOAD_BYTES {
        bail!("OpenAPI URL response exceeds {MAX_OPENAPI_DOWNLOAD_BYTES} bytes");
    }
    let text = String::from_utf8(bytes.to_vec()).context("OpenAPI URL response is not UTF-8")?;
    Ok(postly_core::import_openapi_text(input, &text, output)?)
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
        proxy: options.proxy.map(ToOwned::to_owned),
        no_proxy: options.no_proxy.map(ToOwned::to_owned),
        ca_cert: options.ca_cert.map(Path::to_path_buf),
        client_identity: options.client_identity.map(Path::to_path_buf),
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
                scripts: options.scripts,
                iterations: iterations.clone(),
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
                } else {
                    eprintln!(
                        "FAIL {}: {}",
                        result.name,
                        result.error.as_deref().unwrap_or("unknown error")
                    );
                }
            }
        }
        let should_stop = options.fail_fast && summary.failed > 0;
        summaries.push(summary);
        if should_stop {
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
            output.push_str(&format!("<failure message=\"{}\"/>", xml_escape(message)));
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
        accept_invalid_certs: options.insecure,
        proxy: options.proxy.map(ToOwned::to_owned),
        no_proxy: options.no_proxy.map(ToOwned::to_owned),
        ca_cert: options.ca_cert.map(Path::to_path_buf),
        client_identity: options.client_identity.map(Path::to_path_buf),
        cookie_jar: options.cookie_jar.map(Path::to_path_buf),
        ..EngineOptions::default()
    })?;
    Ok(engine.execute(request, &context).await?)
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
    let OAuthCliArgs {
        oauth_token_url,
        oauth_client_id,
        oauth_client_secret,
        oauth_scope,
        oauth_authorization_url,
        oauth_redirect_uri,
        oauth_code,
        oauth_code_verifier,
        oauth_refresh_token,
    } = oauth;
    if oauth_token_url.is_some()
        || oauth_client_id.is_some()
        || oauth_client_secret.is_some()
        || oauth_scope.is_some()
        || oauth_authorization_url.is_some()
        || oauth_redirect_uri.is_some()
        || oauth_code.is_some()
        || oauth_code_verifier.is_some()
        || oauth_refresh_token.is_some()
    {
        if bearer.is_some() || basic_user.is_some() || basic_password.is_some() {
            bail!("choose either bearer/basic authentication or OAuth 2.0");
        }
        let token_url = oauth_token_url.context("--oauth-token-url is required for OAuth 2.0")?;
        let client_id = oauth_client_id.context("--oauth-client-id is required for OAuth 2.0")?;
        let is_pkce = oauth_authorization_url.is_some()
            || oauth_redirect_uri.is_some()
            || oauth_code.is_some()
            || oauth_code_verifier.is_some();
        if is_pkce {
            if oauth_refresh_token.is_some() {
                bail!("choose either OAuth 2.0 PKCE or a refresh token");
            }
            let authorization_url = oauth_authorization_url
                .context("--oauth-authorization-url is required for OAuth 2.0 PKCE")?;
            let redirect_uri = oauth_redirect_uri
                .context("--oauth-redirect-uri is required for OAuth 2.0 PKCE")?;
            let code = oauth_code.context("--oauth-code is required for OAuth 2.0 PKCE")?;
            let code_verifier = oauth_code_verifier
                .context("--oauth-code-verifier is required for OAuth 2.0 PKCE")?;
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
    print_response_with_tests(response, output_json, None)
}

fn print_response_with_tests(
    response: &postly_core::HttpResponse,
    output_json: bool,
    tests: Option<&[ScriptTestResult]>,
) -> Result<()> {
    if output_json {
        let mut payload = json!({
            "status": response.status,
            "status_text": response.status_text,
            "headers": response.headers,
            "content_type": response.content_type,
            "response_size": response.response_size,
            "duration_ms": response.duration_ms,
            "protocol": response.protocol,
            "url": response.url,
            "body": response.formatted_body(postly_core::ResponseView::Pretty),
        });
        if let Some(tests) = tests {
            payload["tests"] = serde_json::to_value(tests)?;
        }
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!(
            "{} {} · {} ms · {}",
            response.status, response.status_text, response.duration_ms, response.protocol
        );
        println!(
            "{}",
            response.formatted_body(postly_core::ResponseView::Pretty)
        );
        if let Some(tests) = tests {
            for test in tests {
                if test.passed {
                    println!("PASS test: {}", test.name);
                } else {
                    println!(
                        "FAIL test: {} — {}",
                        test.name,
                        test.error.as_deref().unwrap_or("assertion failed")
                    );
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
        pin::Pin,
        task::{Context, Poll},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const TEST_CA_PEM: &str = include_str!("../../postly-core/testdata/tls/ca.pem");
    const TEST_SERVER_CERT_PEM: &str = include_str!("../../postly-core/testdata/tls/server.pem");
    const TEST_SERVER_KEY_PEM: &str = include_str!("../../postly-core/testdata/tls/server-key.pem");
    const TEST_CLIENT_CERT_PEM: &str = include_str!("../../postly-core/testdata/tls/client.pem");
    const TEST_CLIENT_KEY_PEM: &str = include_str!("../../postly-core/testdata/tls/client-key.pem");

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
                headers: vec![HeaderEntry::enabled("content-type", "application/json")],
                body: Some(r#"{"ok":true}"#.to_owned()),
                delay_ms: 7,
            },
        }];

        let response = mock_response_for(&routes, "get", "/health?token=secret");

        assert_eq!(response.status, 201);
        assert_eq!(response.status_text, "Created");
        assert_eq!(response.body, br#"{"ok":true}"#);
        assert_eq!(response.delay_ms, 7);
        assert_eq!(response.headers.len(), 1);
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
            ca_cert: None,
            client_identity: None,
            output_json: true,
        })
        .await
        .expect("gRPC bidirectional-streaming command");

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
            proto,
            includes: Vec::new(),
            method: "/demo.Echo/Echo".to_owned(),
            message: Some(r#"{"message":"secure"}"#.to_owned()),
            message_file: None,
            metadata: vec!["x-test=local".to_owned()],
            bearer: None,
            basic_user: None,
            basic_password: None,
            timeout: 10,
            ca_cert: Some(ca_path),
            client_identity: Some(client_identity_path),
            output_json: true,
        })
        .await
        .expect("gRPC mTLS command");

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
            output_json: true,
        })
        .await
        .expect("WebSocket command");
        server.await.expect("server");
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
            timeout: 10,
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
