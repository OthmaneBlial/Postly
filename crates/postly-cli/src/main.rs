use std::{
    fs,
    io::{self, IsTerminal, Read},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use postly_core::{
    import_curl_command, import_environment, import_postman_collection, run_requests, Auth,
    Collection, EngineOptions, Environment, EnvironmentVariable, HeaderEntry, HistoryEntry,
    HistoryOutcome, HttpEngine, Request, RequestBody, RunnerOptions, ScriptResult,
    ScriptTestResult, VariableContext, Workspace,
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
    query: Vec<String>,
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
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Reporter {
    Pretty,
    Json,
    Junit,
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
        #[arg(
            long,
            help = "Execute preserved pre-request and test scripts through Node.js"
        )]
        scripts: bool,
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
    /// Create or update a local environment without printing its values.
    Env {
        #[command(subcommand)]
        kind: EnvKind,
    },
    /// List collections and saved requests in a workspace.
    List {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Show recent metadata-only executions from the local workspace.
    History {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        output_json: bool,
    },
    /// Execute every saved request in a collection, sequentially.
    Run {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        environment: Option<String>,
        #[arg(long)]
        fail_fast: bool,
        #[arg(
            long,
            help = "Execute preserved pre-request and test scripts through Node.js"
        )]
        scripts: bool,
        #[arg(long, default_value_t = 30)]
        timeout: u64,
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
    },
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
            timeout,
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
                timeout,
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
            insecure,
        } => {
            send_saved_request(
                &file,
                environment.as_deref(),
                scripts,
                timeout,
                insecure,
                output_json,
            )
            .await
        }
        Command::Import { kind } => import_command(kind),
        Command::Env { kind } => match kind {
            EnvKind::Set {
                workspace,
                name,
                values,
                secrets,
            } => set_environment(&workspace, &name, &values, &secrets),
        },
        Command::List { path } => list_workspace(&path),
        Command::History {
            path,
            limit,
            output_json,
        } => list_history(&path, limit, output_json),
        Command::Run {
            path,
            environment,
            fail_fast,
            scripts,
            timeout,
            reporter,
            data_file,
            output_json,
        } => {
            run_workspace(
                &path,
                environment.as_deref(),
                fail_fast,
                scripts,
                timeout,
                if output_json {
                    Reporter::Json
                } else {
                    reporter
                },
                data_file.as_deref(),
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
    request.auth = parse_auth_flags(options.bearer, options.basic_user, options.basic_password)?;
    request.body = parse_cli_body(options.data, options.json_body)?;
    let path = workspace.save_request(&collection, &request)?;
    println!("Saved request at {}", path.display());
    Ok(())
}

async fn send_unsaved_request(options: ImmediateRequestOptions) -> Result<()> {
    let mut request = Request::new("CLI request", options.method, options.url);
    request.query = parse_pairs_flags(&options.query)?;
    request.headers = parse_headers(&options.headers)?;
    request.auth = parse_auth_flags(options.bearer, options.basic_user, options.basic_password)?;
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
    scripts: bool,
    timeout: u64,
    insecure: bool,
    output_json: bool,
) -> Result<()> {
    let workspace = find_workspace(file)?;
    let mut request = workspace.load_request(file)?;
    let collections = workspace.collections()?;
    let collection = collections
        .iter()
        .find(|collection| file.starts_with(&collection.directory));
    let mut context = context_for_collection(
        &workspace,
        collection.map(|collection| &collection.collection),
        environment_name,
    )?;
    if scripts {
        if let Some(script) = request.pre_request_script.clone() {
            let script_result =
                run_script_async(script, request.clone(), None, context.clone()).await?;
            script_result.apply(&mut request, &mut context)?;
        }
    }
    let started = Instant::now();
    let result = execute(&request, context.clone(), timeout, insecure).await;
    let history_entry = match &result {
        Ok(response) => HistoryEntry::from_response(&request, response),
        Err(_) => HistoryEntry::from_error(&request, started.elapsed().as_millis() as u64),
    };
    if let Err(error) = workspace.record_history(&history_entry) {
        tracing::warn!(error = %error, "could not write local request history");
    }
    let response = result?;
    let post_script = if scripts {
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
        output_json,
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

fn import_command(kind: ImportKind) -> Result<()> {
    let report = match kind {
        ImportKind::Collection { input, output } => import_postman_collection(input, output)?,
        ImportKind::Environment { input, output } => import_environment(input, output)?,
        ImportKind::Openapi { input, output } => {
            let report = postly_core::import_openapi(input, output)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            return Ok(());
        }
        ImportKind::Curl {
            command,
            output,
            collection,
            name,
        } => {
            let result = import_curl_command(&command, output, &collection, &name)?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            return Ok(());
        }
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn set_environment(
    workspace_path: &Path,
    name: &str,
    values: &[String],
    secrets: &[String],
) -> Result<()> {
    let workspace = Workspace::open_or_init(workspace_path, "Postly workspace")?;
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
        environment.variables.insert(
            key.to_owned(),
            EnvironmentVariable {
                value: value.to_owned(),
                enabled: true,
                secret: true,
            },
        );
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

fn list_history(path: &Path, limit: usize, output_json: bool) -> Result<()> {
    let workspace = Workspace::open(path)?;
    let entries = workspace.history(limit)?;
    if output_json {
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

async fn run_workspace(
    path: &Path,
    environment_name: Option<&str>,
    fail_fast: bool,
    scripts: bool,
    timeout: u64,
    reporter: Reporter,
    data_file: Option<&Path>,
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
    let engine = HttpEngine::new(&EngineOptions {
        timeout: Duration::from_secs(timeout),
        ..EngineOptions::default()
    })?;
    let iterations = load_iteration_data(data_file)?;
    let mut summaries = Vec::new();
    for collection in collections {
        let requests = workspace.requests(&collection)?;
        let context =
            context_for_collection(&workspace, Some(&collection.collection), environment_name)?;
        let summary = run_requests(
            &engine,
            &requests,
            &context,
            &RunnerOptions {
                fail_fast,
                scripts,
                iterations: iterations.clone(),
                ..RunnerOptions::default()
            },
        )
        .await;
        if matches!(reporter, Reporter::Pretty) {
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
        let should_stop = fail_fast && summary.failed > 0;
        summaries.push(summary);
        if should_stop {
            break;
        }
    }
    match reporter {
        Reporter::Pretty => {}
        Reporter::Json => println!("{}", serde_json::to_string_pretty(&summaries)?),
        Reporter::Junit => println!("{}", render_junit(&summaries)),
    }
    if summaries.iter().any(|summary| !summary.succeeded()) {
        bail!("collection run failed");
    }
    Ok(())
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
        context.environment = environment.enabled_values();
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

fn parse_auth_flags(
    bearer: Option<String>,
    basic_user: Option<String>,
    basic_password: Option<String>,
) -> Result<Auth> {
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
