use std::{
    io::{self, IsTerminal, Read},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use postly_core::{
    import_environment, import_postman_collection, Auth, Collection, EngineOptions, HeaderEntry,
    HttpEngine, Request, RequestBody, VariableContext, Workspace,
};
use serde_json::json;
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
    headers: Vec<String>,
    data: Option<String>,
    json_body: Option<String>,
    bearer: Option<String>,
    basic_user: Option<String>,
    basic_password: Option<String>,
    timeout: u64,
    insecure: bool,
    output_json: bool,
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
    /// Send an unsaved HTTP request immediately.
    Request {
        url: String,
        #[arg(short, long, default_value = "GET")]
        method: String,
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
        #[arg(long, default_value_t = 30)]
        timeout: u64,
        #[arg(long)]
        insecure: bool,
        #[arg(long)]
        output_json: bool,
    },
    /// Send a saved .postly.toml request file.
    Send {
        file: PathBuf,
        #[arg(long)]
        environment: Option<String>,
        #[arg(long)]
        output_json: bool,
        #[arg(long, default_value_t = 30)]
        timeout: u64,
        #[arg(long)]
        insecure: bool,
    },
    /// Import a Postman collection or environment into a local workspace.
    Import {
        #[command(subcommand)]
        kind: ImportKind,
    },
    /// List collections and saved requests in a workspace.
    List {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Execute every saved request in a collection, sequentially.
    Run {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        environment: Option<String>,
        #[arg(long)]
        fail_fast: bool,
        #[arg(long, default_value_t = 30)]
        timeout: u64,
        #[arg(long)]
        output_json: bool,
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
        Command::Request {
            url,
            method,
            headers,
            data,
            json,
            bearer,
            basic_user,
            basic_password,
            timeout,
            insecure,
            output_json,
        } => {
            send_unsaved_request(ImmediateRequestOptions {
                url,
                method,
                headers,
                data,
                json_body: json,
                bearer,
                basic_user,
                basic_password,
                timeout,
                insecure,
                output_json,
            })
            .await
        }
        Command::Send {
            file,
            environment,
            output_json,
            timeout,
            insecure,
        } => {
            send_saved_request(
                &file,
                environment.as_deref(),
                timeout,
                insecure,
                output_json,
            )
            .await
        }
        Command::Import { kind } => import_command(kind),
        Command::List { path } => list_workspace(&path),
        Command::Run {
            path,
            environment,
            fail_fast,
            timeout,
            output_json,
        } => {
            run_workspace(
                &path,
                environment.as_deref(),
                fail_fast,
                timeout,
                output_json,
            )
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

async fn send_unsaved_request(options: ImmediateRequestOptions) -> Result<()> {
    let mut request = Request::new("CLI request", options.method, options.url);
    request.headers = parse_headers(&options.headers)?;
    request.auth = match (options.bearer, options.basic_user, options.basic_password) {
        (Some(token), None, None) => Auth::Bearer { token },
        (None, Some(username), password) => Auth::Basic {
            username,
            password: password.unwrap_or_default(),
        },
        (None, None, None) => Auth::None,
        _ => bail!("choose either --bearer or --basic-user/--basic-password"),
    };
    request.body = parse_cli_body(options.data, options.json_body)?;
    let response = execute(
        &request,
        VariableContext::default(),
        options.timeout,
        options.insecure,
    )
    .await?;
    print_response(&response, options.output_json)?;
    Ok(())
}

async fn send_saved_request(
    file: &Path,
    environment_name: Option<&str>,
    timeout: u64,
    insecure: bool,
    output_json: bool,
) -> Result<()> {
    let workspace = find_workspace(file)?;
    let request = workspace.load_request(file)?;
    let context = context_for_request(&workspace, &request, environment_name)?;
    let response = execute(&request, context, timeout, insecure).await?;
    print_response(&response, output_json)?;
    Ok(())
}

fn import_command(kind: ImportKind) -> Result<()> {
    let report = match kind {
        ImportKind::Collection { input, output } => import_postman_collection(input, output)?,
        ImportKind::Environment { input, output } => import_environment(input, output)?,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
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

async fn run_workspace(
    path: &Path,
    environment_name: Option<&str>,
    fail_fast: bool,
    timeout: u64,
    output_json: bool,
) -> Result<()> {
    let workspace = if path.join("postly.toml").is_file() {
        Workspace::open(path)?
    } else {
        find_workspace(path)?
    };
    let collections = workspace.collections()?;
    if collections.is_empty() {
        bail!("no collections found in {}", workspace.root().display());
    }
    let mut results = Vec::new();
    for collection in collections {
        let requests = workspace.requests(&collection)?;
        for (_, request) in requests {
            let context = context_for_request(&workspace, &request, environment_name)
                .with_context(|| format!("building variables for {}", request.name))?;
            let result = execute(&request, context, timeout, false).await;
            match result {
                Ok(response) => {
                    let passed = response.status < 400;
                    if !output_json {
                        println!(
                            "{} {} {} ({} ms)",
                            if passed { "PASS" } else { "FAIL" },
                            response.status,
                            request.name,
                            response.duration_ms
                        );
                    }
                    results.push(json!({
                        "name": request.name,
                        "method": request.method,
                        "status": response.status,
                        "duration_ms": response.duration_ms,
                        "passed": passed
                    }));
                    if fail_fast && !passed {
                        break;
                    }
                }
                Err(error) => {
                    if !output_json {
                        eprintln!("FAIL {}: {error}", request.name);
                    }
                    results.push(json!({
                        "name": request.name,
                        "method": request.method,
                        "error": error.to_string(),
                        "passed": false
                    }));
                    if fail_fast {
                        break;
                    }
                }
            }
        }
    }
    if output_json {
        println!("{}", serde_json::to_string_pretty(&results)?);
    }
    if results
        .iter()
        .any(|result| result.get("passed") == Some(&serde_json::Value::Bool(false)))
    {
        bail!("collection run failed");
    }
    Ok(())
}

async fn execute(
    request: &Request,
    context: VariableContext,
    timeout: u64,
    insecure: bool,
) -> Result<postly_core::HttpResponse> {
    let engine = HttpEngine::new(&EngineOptions {
        timeout: Duration::from_secs(timeout),
        accept_invalid_certs: insecure,
        ..EngineOptions::default()
    })?;
    Ok(engine.execute(request, &context).await?)
}

fn context_for_request(
    workspace: &Workspace,
    request: &Request,
    environment_name: Option<&str>,
) -> Result<VariableContext> {
    let mut context = VariableContext::default();
    if let Some(name) = environment_name {
        let (_, environment) = workspace
            .environments()?
            .into_iter()
            .find(|(_, environment)| {
                environment.name == name || environment.name.eq_ignore_ascii_case(name)
            })
            .with_context(|| format!("environment not found: {name}"))?;
        context.environment = environment.enabled_values();
    }
    for pair in &request.query {
        if let Some(key) = pair.key.strip_prefix("x-postly-var-") {
            context.request.insert(key.to_owned(), pair.value.clone());
        }
    }
    for header in &request.headers {
        if let Some(key) = header.key.strip_prefix("x-postly-var-") {
            context.request.insert(key.to_owned(), header.value.clone());
        }
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
    if output_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "status": response.status,
                "status_text": response.status_text,
                "headers": response.headers,
                "content_type": response.content_type,
                "duration_ms": response.duration_ms,
                "protocol": response.protocol,
                "url": response.url,
                "body": response.formatted_body(postly_core::ResponseView::Pretty),
            }))?
        );
    } else {
        println!(
            "{} {} · {} ms · {}",
            response.status, response.status_text, response.duration_ms, response.protocol
        );
        println!(
            "{}",
            response.formatted_body(postly_core::ResponseView::Pretty)
        );
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