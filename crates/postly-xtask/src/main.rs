use std::{
    env, fs,
    path::Path,
    process::{Command, ExitCode},
    time::Instant,
};

use postly_core::{
    import_environment, import_openapi, import_postman_collection, run_requests, Collection,
    EngineOptions, HttpEngine, Request, RunnerOptions, VariableContext, Workspace,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

const BENCHMARK_ITERATIONS: usize = 5;
const WORKSPACE_REQUESTS: usize = 1_000;
const LARGE_WORKSPACE_REQUESTS: usize = 10_000;
const RUNNER_REQUESTS: usize = 100;

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
        "compat" => run_compatibility(json_output),
        "fuzz" => run_fuzz_smoke(),
        "package" => package_release(),
        "help" | "--help" => {
            println!("cargo xtask check|fmt|lint|test|compat|bench|fuzz|package [--json]");
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
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let context = benchmark_context(&root);
    match collect_benchmarks() {
        Ok(results) => {
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "context": context,
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
                println!(
                    "revision: {}",
                    context.revision.as_deref().unwrap_or("unknown")
                );
                println!(
                    "hardware: {}",
                    context.hardware.as_deref().unwrap_or("unknown")
                );
                println!(
                    "os version: {}",
                    context.os_version.as_deref().unwrap_or("unknown")
                );
                println!("rustc: {}", context.rustc.as_deref().unwrap_or("unknown"));
                println!("profile: {}", context.profile);
                println!();
                println!(
                    "{:<46} {:>12} {:>12} {:>12} {:>16}",
                    "benchmark", "median ms", "min ms", "max ms", "peak rss KiB"
                );
                for result in results {
                    println!(
                        "{:<46} {:>12.3} {:>12.3} {:>12.3} {:>16}",
                        result.name,
                        result.median_ms,
                        result.min_ms,
                        result.max_ms,
                        result
                            .peak_rss_kib
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "—".to_owned())
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
struct BenchmarkContext {
    os: &'static str,
    arch: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hardware: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    os_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rustc: Option<String>,
    profile: &'static str,
}

fn benchmark_context(root: &Path) -> BenchmarkContext {
    let profile = if root.join("target/debug/postly").is_file() {
        "debug"
    } else if root.join("target/release/postly").is_file() {
        "release"
    } else {
        "unknown"
    };
    BenchmarkContext {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        revision: command_output(root, "git", &["rev-parse", "--short", "HEAD"]),
        hardware: (cfg!(target_os = "macos"))
            .then(|| command_output(root, "sysctl", &["-n", "hw.model"]))
            .flatten(),
        os_version: (cfg!(target_os = "macos"))
            .then(|| command_output(root, "sw_vers", &["-productVersion"]))
            .flatten(),
        rustc: command_output(root, "rustc", &["--version"]),
        profile,
    }
}

fn command_output(root: &Path, program: &str, arguments: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .current_dir(root)
        .args(arguments)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn run_fuzz_smoke() -> bool {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if !run_in(
        &root,
        "cargo",
        &["+nightly", "fuzz", "check", "--fuzz-dir", "fuzz"],
    ) {
        return false;
    }
    [
        "curl_command",
        "variables",
        "postman_import",
        "native_workspace",
    ]
    .into_iter()
    .all(|target| {
        run_in(
            &root,
            "cargo",
            &[
                "+nightly",
                "fuzz",
                "run",
                "--fuzz-dir",
                "fuzz",
                target,
                "--",
                "-runs=256",
            ],
        )
    })
}

#[derive(Debug, serde::Serialize)]
struct CompatibilityFixtureResult {
    kind: String,
    fixture: String,
    status: String,
    imported_requests: usize,
    fully_supported_requests: usize,
    manual_review_requests: usize,
    warnings: usize,
    error: Option<String>,
}

#[derive(Debug, serde::Serialize)]
struct CompatibilityReport {
    scope: &'static str,
    fixtures: Vec<CompatibilityFixtureResult>,
    fixture_execution: CompatibilityScore,
    request_mapping: CompatibilityScore,
}

#[derive(Debug, serde::Serialize)]
struct CompatibilityScore {
    passed: usize,
    total: usize,
    percent: f64,
}

fn run_compatibility(json_output: bool) -> bool {
    match collect_compatibility() {
        Ok(report) => {
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .unwrap_or_else(|error| format!("{{\"error\":\"{error}\"}}"))
                );
            } else {
                println!("Postly fixture-backed compatibility report");
                println!("scope: {}", report.scope);
                println!();
                println!(
                    "{:<10} {:<12} {:>10} {:>10} {:>10}",
                    "status", "fixture", "requests", "mapped", "review"
                );
                for fixture in &report.fixtures {
                    println!(
                        "{:<10} {:<12} {:>10} {:>10} {:>10}",
                        fixture.status,
                        fixture.fixture,
                        fixture.imported_requests,
                        fixture.fully_supported_requests,
                        fixture.manual_review_requests
                    );
                    if let Some(error) = &fixture.error {
                        println!("  error: {error}");
                    }
                }
                println!();
                println!(
                    "fixture execution: {:.1}% ({}/{})",
                    report.fixture_execution.percent,
                    report.fixture_execution.passed,
                    report.fixture_execution.total
                );
                println!(
                    "request mapping: {:.1}% ({}/{}) — manual-review cases remain visible",
                    report.request_mapping.percent,
                    report.request_mapping.passed,
                    report.request_mapping.total
                );
            }
            report.fixture_execution.passed == report.fixture_execution.total
        }
        Err(error) => {
            eprintln!("compatibility report failed: {error}");
            false
        }
    }
}

fn collect_compatibility() -> Result<CompatibilityReport, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut fixtures = Vec::new();
    let postman_dir = root.join("compat/postman-import");
    let mut postman_fixtures = json_files(&postman_dir)?;
    postman_fixtures.retain(|path| {
        path.file_name().and_then(|name| name.to_str()) != Some("basic-environment.json")
    });
    for fixture in postman_fixtures {
        let output = tempfile::tempdir().map_err(|error| error.to_string())?;
        let relative = display_relative(&root, &fixture);
        let result = import_postman_collection(&fixture, output.path());
        fixtures.push(match result {
            Ok(report) => CompatibilityFixtureResult {
                kind: "postman_collection".to_owned(),
                fixture: relative,
                status: "passed".to_owned(),
                imported_requests: report.imported_requests,
                fully_supported_requests: report.fully_supported_requests,
                manual_review_requests: report.manual_review_requests,
                warnings: report.warnings.len(),
                error: None,
            },
            Err(error) => CompatibilityFixtureResult {
                kind: "postman_collection".to_owned(),
                fixture: relative,
                status: "failed".to_owned(),
                imported_requests: 0,
                fully_supported_requests: 0,
                manual_review_requests: 0,
                warnings: 0,
                error: Some(error.to_string()),
            },
        });
    }

    let environment_fixture = postman_dir.join("basic-environment.json");
    if !environment_fixture.is_file() {
        return Err(format!(
            "compatibility fixture is missing: {}",
            environment_fixture.display()
        ));
    }
    let output = tempfile::tempdir().map_err(|error| error.to_string())?;
    let relative = display_relative(&root, &environment_fixture);
    fixtures.push(
        match import_environment(&environment_fixture, output.path()) {
            Ok(report) => CompatibilityFixtureResult {
                kind: "postman_environment".to_owned(),
                fixture: relative,
                status: "passed".to_owned(),
                imported_requests: 0,
                fully_supported_requests: 0,
                manual_review_requests: 0,
                warnings: report.warnings.len(),
                error: None,
            },
            Err(error) => CompatibilityFixtureResult {
                kind: "postman_environment".to_owned(),
                fixture: relative,
                status: "failed".to_owned(),
                imported_requests: 0,
                fully_supported_requests: 0,
                manual_review_requests: 0,
                warnings: 0,
                error: Some(error.to_string()),
            },
        },
    );

    for fixture in json_files(&root.join("compat/openapi"))?
        .into_iter()
        .chain(yaml_files(&root.join("compat/openapi"))?)
    {
        let output = tempfile::tempdir().map_err(|error| error.to_string())?;
        let relative = display_relative(&root, &fixture);
        fixtures.push(match import_openapi(&fixture, output.path()) {
            Ok(report) => CompatibilityFixtureResult {
                kind: "openapi".to_owned(),
                fixture: relative,
                status: "passed".to_owned(),
                imported_requests: report.imported_operations,
                fully_supported_requests: report.imported_operations,
                manual_review_requests: 0,
                warnings: report.warnings.len(),
                error: None,
            },
            Err(error) => CompatibilityFixtureResult {
                kind: "openapi".to_owned(),
                fixture: relative,
                status: "failed".to_owned(),
                imported_requests: 0,
                fully_supported_requests: 0,
                manual_review_requests: 0,
                warnings: 0,
                error: Some(error.to_string()),
            },
        });
    }

    let passed_fixtures = fixtures
        .iter()
        .filter(|fixture| fixture.status == "passed")
        .count();
    let mapped = fixtures
        .iter()
        .map(|fixture| fixture.fully_supported_requests)
        .sum();
    let requests = fixtures
        .iter()
        .map(|fixture| fixture.imported_requests)
        .sum();
    Ok(CompatibilityReport {
        scope: "checked-in Postman collection/environment and OpenAPI fixtures; not full Postman behavioral parity",
        fixture_execution: score(passed_fixtures, fixtures.len()),
        request_mapping: score(mapped, requests),
        fixtures,
    })
}

fn score(passed: usize, total: usize) -> CompatibilityScore {
    CompatibilityScore {
        passed,
        total,
        percent: if total == 0 {
            0.0
        } else {
            (passed as f64 / total as f64) * 100.0
        },
    }
}

fn json_files(directory: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    files_with_extensions(directory, &["json"])
}

fn yaml_files(directory: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    files_with_extensions(directory, &["yaml", "yml"])
}

fn files_with_extensions(
    directory: &Path,
    extensions: &[&str],
) -> Result<Vec<std::path::PathBuf>, String> {
    let mut files = fs::read_dir(directory)
        .map_err(|error| format!("{}: {error}", directory.display()))?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    files.retain(|path| {
        path.is_file()
            && path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extensions.contains(&extension))
    });
    files.sort();
    Ok(files)
}

fn display_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn package_release() -> bool {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if !run_in(
        &root,
        "cargo",
        &[
            "build",
            "--locked",
            "--release",
            "-p",
            "postly",
            "-p",
            "postly-app",
        ],
    ) {
        return false;
    }

    let target = env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        })
        .unwrap_or_else(|| root.join("target"));
    let dist = root.join("dist");
    let package_name = format!(
        "postly-v{}-{}-{}",
        env!("CARGO_PKG_VERSION"),
        env::consts::OS,
        env::consts::ARCH
    );
    let package_dir = dist.join(&package_name);
    if let Err(error) = fs::create_dir_all(&package_dir) {
        eprintln!(
            "could not create package directory {}: {error}",
            package_dir.display()
        );
        return false;
    }

    let files = [
        (target.join("release/postly"), package_dir.join("postly")),
        (
            target.join("release/postly-gui"),
            package_dir.join("postly-gui"),
        ),
        (root.join("README.md"), package_dir.join("README.md")),
        (root.join("LICENSE"), package_dir.join("LICENSE")),
    ];
    for (source, destination) in files {
        if let Err(error) = fs::copy(&source, &destination) {
            eprintln!(
                "could not copy package file {} to {}: {error}",
                source.display(),
                destination.display()
            );
            return false;
        }
    }

    let manifest = json!({
        "name": "Postly",
        "version": env!("CARGO_PKG_VERSION"),
        "platform": env::consts::OS,
        "architecture": env::consts::ARCH,
        "binaries": ["postly", "postly-gui"],
        "source": "local cargo release build",
    });
    let manifest_path = package_dir.join("postly-package.json");
    match serde_json::to_vec_pretty(&manifest)
        .map_err(|error| error.to_string())
        .and_then(|contents| fs::write(&manifest_path, contents).map_err(|error| error.to_string()))
    {
        Ok(()) => {}
        Err(error) => {
            eprintln!("could not write package manifest: {error}");
            return false;
        }
    }

    let checksums = [
        "postly",
        "postly-gui",
        "README.md",
        "LICENSE",
        "postly-package.json",
    ]
    .iter()
    .map(|name| {
        let path = package_dir.join(name);
        sha256_hex(&path).map(|hash| format!("{hash}  {name}"))
    })
    .collect::<Result<Vec<_>, _>>();
    let checksums = match checksums {
        Ok(checksums) => checksums.join("\n") + "\n",
        Err(error) => {
            eprintln!("could not hash package files: {error}");
            return false;
        }
    };
    if let Err(error) = fs::write(package_dir.join("SHA256SUMS"), checksums) {
        eprintln!("could not write package checksums: {error}");
        return false;
    }
    if !run_package_cli_smoke(&package_dir) {
        return false;
    }

    let archive = dist.join(format!("{package_name}.tar.gz"));
    let tar_status = Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(&dist)
        .arg(&package_name)
        .status();
    if !tar_status.map(|status| status.success()).unwrap_or(false) {
        eprintln!("could not create package archive {}", archive.display());
        return false;
    }
    let archive_listing = Command::new("tar").arg("-tzf").arg(&archive).output();
    let archive_listing = match archive_listing {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).into_owned()
        }
        Ok(output) => {
            eprintln!("could not list package archive: {}", output.status);
            return false;
        }
        Err(error) => {
            eprintln!("could not inspect package archive: {error}");
            return false;
        }
    };
    for expected in [
        format!("{package_name}/postly"),
        format!("{package_name}/postly-gui"),
        format!("{package_name}/SHA256SUMS"),
    ] {
        if !archive_listing.lines().any(|entry| entry == expected) {
            eprintln!("package archive is missing {expected}");
            return false;
        }
    }
    match sha256_hex(&archive) {
        Ok(hash) => {
            println!("package directory: {}", package_dir.display());
            println!("package archive: {}", archive.display());
            println!("archive sha256: {hash}");
            true
        }
        Err(error) => {
            eprintln!("could not hash package archive: {error}");
            false
        }
    }
}

fn run_package_cli_smoke(package_dir: &Path) -> bool {
    let cli = package_dir.join("postly");
    for argument in ["--version", "--help"] {
        let output = match Command::new(&cli).arg(argument).output() {
            Ok(output) => output,
            Err(error) => {
                eprintln!("packaged CLI smoke test could not start {argument}: {error}");
                return false;
            }
        };
        if !output.status.success() {
            eprintln!(
                "packaged CLI smoke test {argument} exited with {}",
                output.status
            );
            return false;
        }
    }
    true
}

fn sha256_hex(path: &Path) -> Result<String, String> {
    let contents = fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let digest = Sha256::digest(contents);
    Ok(format!("{digest:x}"))
}

#[derive(Debug, serde::Serialize)]
struct BenchmarkResult {
    name: String,
    iterations: usize,
    median_ms: f64,
    min_ms: f64,
    max_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    peak_rss_kib: Option<u64>,
}

fn collect_benchmarks() -> Result<Vec<BenchmarkResult>, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cli_binary = resolve_cli_binary(&root)?;
    let cli_startup = measure_process("cli_startup_help", &cli_binary, &["--help"])?;

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

    let large_workspace = tempfile::tempdir().map_err(|error| error.to_string())?;
    let large_model = Workspace::init(large_workspace.path(), "Large benchmark workspace")
        .map_err(|error| error.to_string())?;
    let large_collection = large_model
        .create_collection(&Collection::new("Large benchmark collection"))
        .map_err(|error| error.to_string())?;
    for index in 0..LARGE_WORKSPACE_REQUESTS {
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
        large_model
            .save_request(&large_collection, &request)
            .map_err(|error| error.to_string())?;
    }
    drop(large_model);

    let large_load = measure("workspace_open_10000_requests", || {
        let opened = Workspace::open(large_workspace.path()).map_err(|error| error.to_string())?;
        let collections = opened.collections().map_err(|error| error.to_string())?;
        if collections.len() != 1 {
            return Err(format!(
                "expected one large collection, got {}",
                collections.len()
            ));
        }
        let requests = opened
            .requests(&collections[0])
            .map_err(|error| error.to_string())?;
        if requests.len() != LARGE_WORKSPACE_REQUESTS {
            return Err(format!(
                "expected {LARGE_WORKSPACE_REQUESTS} requests, got {}",
                requests.len()
            ));
        }
        Ok(())
    })?;
    let large_search = measure("workspace_search_10000_requests", || {
        let opened = Workspace::open(large_workspace.path()).map_err(|error| error.to_string())?;
        let results = opened
            .search_requests("needle")
            .map_err(|error| error.to_string())?;
        if results.len() != LARGE_WORKSPACE_REQUESTS / 10 {
            return Err(format!(
                "expected {} large search results, got {}",
                LARGE_WORKSPACE_REQUESTS / 10,
                results.len()
            ));
        }
        Ok(())
    })?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not create benchmark runtime: {error}"))?;
    let runner = measure("runner_local_100_requests", || {
        runtime.block_on(run_local_runner_benchmark())
    })?;
    Ok(vec![
        cli_startup,
        import,
        load,
        search,
        large_load,
        large_search,
        runner,
    ])
}

async fn run_local_runner_benchmark() -> Result<(), String> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|error| format!("could not bind runner benchmark server: {error}"))?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("could not read runner benchmark address: {error}"))?;
    let server = tokio::spawn(async move {
        for _ in 0..RUNNER_REQUESTS {
            let (mut socket, _) = listener
                .accept()
                .await
                .map_err(|error| format!("runner benchmark accept failed: {error}"))?;
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = socket
                    .read(&mut chunk)
                    .await
                    .map_err(|error| format!("runner benchmark read failed: {error}"))?;
                if read == 0 {
                    return Err("runner benchmark client closed before headers".to_owned());
                }
                request.extend_from_slice(&chunk[..read]);
                if request.len() > 16 * 1024 {
                    return Err(
                        "runner benchmark request headers exceeded the safety limit".to_owned()
                    );
                }
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")
                .await
                .map_err(|error| format!("runner benchmark write failed: {error}"))?;
        }
        Ok::<(), String>(())
    });

    let engine = HttpEngine::new(&EngineOptions::default())
        .map_err(|error| format!("could not create runner benchmark engine: {error}"))?;
    let requests = (0..RUNNER_REQUESTS)
        .map(|index| {
            (
                std::path::PathBuf::from(format!("request-{index}.postly.toml")),
                Request::new(
                    format!("Runner request {index}"),
                    "GET",
                    format!("http://{address}/{index}"),
                ),
            )
        })
        .collect::<Vec<_>>();
    let summary = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        run_requests(
            &engine,
            &requests,
            &VariableContext::default(),
            &RunnerOptions::default(),
        ),
    )
    .await
    .map_err(|_| "runner benchmark timed out".to_owned())?;
    let server_result = server
        .await
        .map_err(|error| format!("runner benchmark server task failed: {error}"))?;
    server_result?;
    if summary.requests != RUNNER_REQUESTS || !summary.succeeded() {
        return Err(format!(
            "runner benchmark expected {RUNNER_REQUESTS} successful requests, got {}",
            summary.requests
        ));
    }
    Ok(())
}

fn resolve_cli_binary(root: &Path) -> Result<std::path::PathBuf, String> {
    for profile in ["debug", "release"] {
        let candidate = root
            .join("target")
            .join(profile)
            .join(format!("postly{}", env::consts::EXE_SUFFIX));
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "postly CLI binary not found under {}; run `cargo build -p postly` first",
        root.join("target").display()
    ))
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
        peak_rss_kib: None,
    })
}

fn measure_process(name: &str, program: &Path, args: &[&str]) -> Result<BenchmarkResult, String> {
    let mut durations = Vec::with_capacity(BENCHMARK_ITERATIONS);
    let mut memory = Vec::with_capacity(BENCHMARK_ITERATIONS);
    for _ in 0..BENCHMARK_ITERATIONS {
        let started = Instant::now();
        #[cfg(target_os = "macos")]
        let output = Command::new("/usr/bin/time")
            .args(["-l", program.to_string_lossy().as_ref()])
            .args(args)
            .output()
            .map_err(|error| format!("could not measure {}: {error}", program.display()))?;
        #[cfg(not(target_os = "macos"))]
        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|error| format!("could not start {}: {error}", program.display()))?;
        if !output.status.success() {
            return Err(format!(
                "{} {} exited with {}",
                program.display(),
                args.join(" "),
                output.status
            ));
        }
        durations.push(started.elapsed().as_secs_f64() * 1_000.0);
        #[cfg(target_os = "macos")]
        if let Some(rss_bytes) =
            parse_macos_peak_rss(&output.stderr).or_else(|| parse_macos_peak_rss(&output.stdout))
        {
            memory.push((rss_bytes.saturating_add(1023)) / 1024);
        }
    }
    durations.sort_by(f64::total_cmp);
    memory.sort_unstable();
    Ok(BenchmarkResult {
        name: name.to_owned(),
        iterations: BENCHMARK_ITERATIONS,
        median_ms: durations[durations.len() / 2],
        min_ms: durations[0],
        max_ms: durations[durations.len() - 1],
        peak_rss_kib: (!memory.is_empty()).then(|| memory[memory.len() / 2]),
    })
}

#[cfg(target_os = "macos")]
fn parse_macos_peak_rss(stderr: &[u8]) -> Option<u64> {
    String::from_utf8_lossy(stderr)
        .lines()
        .find(|line| line.contains("maximum resident set size"))
        .and_then(|line| line.split_whitespace().find_map(|value| value.parse().ok()))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::parse_macos_peak_rss;

    #[test]
    fn parses_macos_time_peak_rss() {
        assert_eq!(
            parse_macos_peak_rss(b"            12484608  maximum resident set size\n"),
            Some(12_484_608)
        );
    }
}

fn run(program: &str, args: &[&str]) -> bool {
    eprintln!("$ {program} {}", args.join(" "));
    Command::new(program)
        .args(args)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn run_in(root: &Path, program: &str, args: &[&str]) -> bool {
    eprintln!("$ {program} {}", args.join(" "));
    Command::new(program)
        .current_dir(root)
        .args(args)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
