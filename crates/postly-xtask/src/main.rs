use std::{
    env,
    path::Path,
    process::{Command, ExitCode},
    time::Instant,
};

use postly_core::{import_postman_collection, Collection, Request, Workspace};
use serde_json::json;

const BENCHMARK_ITERATIONS: usize = 5;
const WORKSPACE_REQUESTS: usize = 1_000;

fn main() -> ExitCode {
    let mut arguments = env::args().skip(1);
    let command = arguments.next().unwrap_or_else(|| "check".to_owned());
    let json_output = arguments.any(|argument| argument == "--json");
    let result = match command.as_str() {
        "fmt" => run("cargo", &["fmt", "--all", "--", "--check"]),
        "lint" => run(
            "cargo",
            &[
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
        ),
        "test" => run("cargo", &["test", "--workspace", "--all-targets"]),
        "check" => {
            run("cargo", &["fmt", "--all", "--", "--check"])
                && run(
                    "cargo",
                    &[
                        "clippy",
                        "--workspace",
                        "--all-targets",
                        "--",
                        "-D",
                        "warnings",
                    ],
                )
                && run("cargo", &["test", "--workspace", "--all-targets"])
        }
        "bench" => run_benchmarks(json_output),
        "help" | "--help" => {
            println!("cargo xtask check|fmt|lint|test|bench [--json]");
            true
        }
        other => {
            eprintln!("unknown xtask command: {other}");
            false
        }
    };
    if result {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn run_benchmarks(json_output: bool) -> bool {
    match collect_benchmarks() {
        Ok(results) => {
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "platform": {
                            "os": std::env::consts::OS,
                            "arch": std::env::consts::ARCH,
                        },
                        "iterations": BENCHMARK_ITERATIONS,
                        "results": results,
                    }))
                    .unwrap_or_else(|error| format!("{{\"error\":\"{error}\"}}"))
                );
            } else {
                println!("Postly local benchmarks ({BENCHMARK_ITERATIONS} samples each)");
                println!(
                    "platform: {} / {}",
                    std::env::consts::OS,
                    std::env::consts::ARCH
                );
                println!();
                println!(
                    "{:<46} {:>12} {:>12} {:>12}",
                    "benchmark", "median ms", "min ms", "max ms"
                );
                for result in results {
                    println!(
                        "{:<46} {:>12.3} {:>12.3} {:>12.3}",
                        result.name, result.median_ms, result.min_ms, result.max_ms
                    );
                }
            }
            true
        }
        Err(error) => {
            eprintln!("benchmark failed: {error}");
            false
        }
    }
}

#[derive(Debug, serde::Serialize)]
struct BenchmarkResult {
    name: String,
    iterations: usize,
    median_ms: f64,
    min_ms: f64,
    max_ms: f64,
}

fn collect_benchmarks() -> Result<Vec<BenchmarkResult>, String> {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../compat/postman-import/variants-v2.1.json");
    if !fixture.is_file() {
        return Err(format!(
            "benchmark fixture is missing: {}",
            fixture.display()
        ));
    }
    let import = measure("postman_import_variants", || {
        let output = tempfile::tempdir().map_err(|error| error.to_string())?;
        import_postman_collection(&fixture, output.path()).map_err(|error| error.to_string())?;
        Ok(())
    })?;

    let workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
    let model = Workspace::init(workspace.path(), "Benchmark workspace")
        .map_err(|error| error.to_string())?;
    let collection = model
        .create_collection(&Collection::new("Benchmark collection"))
        .map_err(|error| error.to_string())?;
    for index in 0..WORKSPACE_REQUESTS {
        let marker = if index % 10 == 0 {
            "needle"
        } else {
            "ordinary"
        };
        let request = Request::new(
            format!("Request {index} {marker}"),
            "GET",
            format!("https://api.example.test/{marker}/{index}"),
        );
        model
            .save_request(&collection, &request)
            .map_err(|error| error.to_string())?;
    }
    drop(model);

    let load = measure("workspace_open_1000_requests", || {
        let opened = Workspace::open(workspace.path()).map_err(|error| error.to_string())?;
        let collections = opened.collections().map_err(|error| error.to_string())?;
        if collections.len() != 1 {
            return Err(format!(
                "expected one collection, got {}",
                collections.len()
            ));
        }
        let requests = opened
            .requests(&collections[0])
            .map_err(|error| error.to_string())?;
        if requests.len() != WORKSPACE_REQUESTS {
            return Err(format!(
                "expected {WORKSPACE_REQUESTS} requests, got {}",
                requests.len()
            ));
        }
        Ok(())
    })?;
    let search = measure("workspace_search_1000_requests", || {
        let opened = Workspace::open(workspace.path()).map_err(|error| error.to_string())?;
        let results = opened
            .search_requests("needle")
            .map_err(|error| error.to_string())?;
        if results.len() != WORKSPACE_REQUESTS / 10 {
            return Err(format!(
                "expected 100 search results, got {}",
                results.len()
            ));
        }
        Ok(())
    })?;
    Ok(vec![import, load, search])
}

fn measure<F>(name: &str, mut operation: F) -> Result<BenchmarkResult, String>
where
    F: FnMut() -> Result<(), String>,
{
    let mut samples = Vec::with_capacity(BENCHMARK_ITERATIONS);
    for _ in 0..BENCHMARK_ITERATIONS {
        let started = Instant::now();
        operation()?;
        samples.push(started.elapsed().as_secs_f64() * 1_000.0);
    }
    samples.sort_by(f64::total_cmp);
    let median_ms = samples[samples.len() / 2];
    Ok(BenchmarkResult {
        name: name.to_owned(),
        iterations: BENCHMARK_ITERATIONS,
        median_ms,
        min_ms: samples[0],
        max_ms: samples[samples.len() - 1],
    })
}

fn run(program: &str, args: &[&str]) -> bool {
    eprintln!("$ {program} {}", args.join(" "));
    Command::new(program)
        .args(args)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
