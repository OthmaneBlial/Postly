use std::{
    env, fs,
    path::Path,
    process::{Command, ExitCode},
    time::Instant,
};

use postly_core::{
    import_environment, import_openapi, import_postman_collection, Collection, Request, Workspace,
};
use serde_json::json;
use sha2::{Digest, Sha256};

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

fn run_fuzz_smoke() -> bool {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if !run_in(
        &root,
        "cargo",
        &["+nightly", "fuzz", "check", "--fuzz-dir", "fuzz"],
    ) {
        return false;
    }
    ["curl_command", "variables", "postman_import"]
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
}

fn collect_benchmarks() -> Result<Vec<BenchmarkResult>, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cli_binary = resolve_cli_binary(&root)?;
    let cli_startup = measure("cli_startup_help", || {
        let output = Command::new(&cli_binary)
            .arg("--help")
            .output()
            .map_err(|error| format!("could not start {}: {error}", cli_binary.display()))?;
        if !output.status.success() {
            return Err(format!(
                "{} --help exited with {}",
                cli_binary.display(),
                output.status
            ));
        }
        Ok(())
    })?;

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
    Ok(vec![cli_startup, import, load, search])
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

fn run_in(root: &Path, program: &str, args: &[&str]) -> bool {
    eprintln!("$ {program} {}", args.join(" "));
    Command::new(program)
        .current_dir(root)
        .args(args)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
