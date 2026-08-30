use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use serde::Serialize;
use tokio::sync::Notify;

use crate::{
    http::{HttpEngine, HttpResponse},
    model::{Assertion, Request, Variables},
    scripting::{run_script, ScriptResult},
    variables::VariableContext,
};

#[derive(Debug, Clone)]
pub struct CancellationToken {
    inner: Arc<CancellationState>,
}

#[derive(Debug)]
struct CancellationState {
    cancelled: AtomicBool,
    notify: Notify,
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self {
            inner: Arc::new(CancellationState {
                cancelled: AtomicBool::new(false),
                notify: Notify::new(),
            }),
        }
    }
}

impl CancellationToken {
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::Release);
        self.inner.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    pub async fn cancelled(&self) {
        while !self.is_cancelled() {
            self.inner.notify.notified().await;
        }
    }
}

#[derive(Debug, Clone)]
pub struct RunnerOptions {
    pub fail_fast: bool,
    pub delay: Duration,
    pub cancellation: CancellationToken,
    pub iterations: Vec<Variables>,
    pub scripts: bool,
}

impl Default for RunnerOptions {
    fn default() -> Self {
        Self {
            fail_fast: false,
            delay: Duration::ZERO,
            cancellation: CancellationToken::default(),
            iterations: Vec::new(),
            scripts: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerItemResult {
    pub path: PathBuf,
    pub iteration: usize,
    pub name: String,
    pub method: String,
    pub status: Option<u16>,
    pub duration_ms: u128,
    pub error: Option<String>,
    pub passed: bool,
    #[serde(default)]
    pub assertions: usize,
    #[serde(default)]
    pub assertion_failures: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RunnerSummary {
    pub requests: usize,
    pub iterations: usize,
    pub passed: usize,
    pub failed: usize,
    pub assertions: usize,
    pub assertion_failures: usize,
    pub cancelled: bool,
    pub results: Vec<RunnerItemResult>,
}

fn evaluate_assertion(assertion: &Assertion, response: &HttpResponse) -> Result<(), String> {
    match assertion {
        Assertion::Status { expected } => {
            if response.status == *expected {
                Ok(())
            } else {
                Err(format!(
                    "expected status {}, received {}",
                    expected, response.status
                ))
            }
        }
        Assertion::HeaderPresent { name } => {
            if response
                .headers
                .iter()
                .any(|header| header.key.eq_ignore_ascii_case(name))
            {
                Ok(())
            } else {
                Err(format!("expected response header {name}"))
            }
        }
        Assertion::HeaderEquals { name, expected } => {
            if response
                .headers
                .iter()
                .any(|header| header.key.eq_ignore_ascii_case(name) && header.value == *expected)
            {
                Ok(())
            } else {
                Err(format!(
                    "expected response header {name} to equal {expected}"
                ))
            }
        }
        Assertion::BodyContains { value } => {
            if response.body_text().contains(value) {
                Ok(())
            } else {
                Err(format!("expected response body to contain {value:?}"))
            }
        }
        Assertion::JsonPointerEquals { pointer, expected } => {
            let body = serde_json::from_slice::<serde_json::Value>(&response.body)
                .map_err(|error| format!("response body is not JSON: {error}"))?;
            let actual = body
                .pointer(pointer)
                .ok_or_else(|| format!("JSON Pointer {pointer:?} was not found"))?;
            if actual == expected {
                Ok(())
            } else {
                Err(format!(
                    "expected JSON Pointer {pointer:?} to equal {expected}, received {actual}"
                ))
            }
        }
    }
}

fn evaluate_assertions(assertions: &[Assertion], response: &HttpResponse) -> Vec<String> {
    assertions
        .iter()
        .enumerate()
        .filter_map(|(index, assertion)| {
            evaluate_assertion(assertion, response)
                .err()
                .map(|error| format!("assertion {}: {error}", index + 1))
        })
        .collect()
}

impl RunnerSummary {
    pub fn succeeded(&self) -> bool {
        !self.cancelled && self.failed == 0
    }
}

pub async fn run_requests(
    engine: &HttpEngine,
    requests: &[(PathBuf, Request)],
    context: &VariableContext,
    options: &RunnerOptions,
) -> RunnerSummary {
    let mut summary = RunnerSummary::default();
    let iterations = if options.iterations.is_empty() {
        vec![Variables::new()]
    } else {
        options.iterations.clone()
    };
    summary.iterations = iterations.len();
    'iterations: for (iteration_index, iteration_data) in iterations.into_iter().enumerate() {
        let mut iteration_context = context.clone();
        iteration_context.runtime.extend(iteration_data);
        for (index, (path, request)) in requests.iter().enumerate() {
            if options.cancellation.is_cancelled() {
                summary.cancelled = true;
                break 'iterations;
            }
            if index > 0 && !options.delay.is_zero() {
                tokio::select! {
                    _ = options.cancellation.cancelled() => {
                        summary.cancelled = true;
                        break 'iterations;
                    }
                    _ = tokio::time::sleep(options.delay) => {}
                }
            }

            let started = Instant::now();
            let mut request_context = iteration_context.clone();
            let mut request_to_run = request.clone();
            let mut script_error = None;
            if options.scripts {
                if let Some(script) = request_to_run.pre_request_script.clone() {
                    match run_script_async(
                        script,
                        request_to_run.clone(),
                        None,
                        request_context.clone(),
                    )
                    .await
                    {
                        Ok(script_result) => {
                            if let Err(error) =
                                script_result.apply(&mut request_to_run, &mut request_context)
                            {
                                script_error = Some(error.to_string());
                            }
                        }
                        Err(error) => script_error = Some(error),
                    }
                }
            }
            let result = if let Some(error) = script_error {
                Err(error)
            } else {
                tokio::select! {
                    _ = options.cancellation.cancelled() => {
                        summary.cancelled = true;
                        break 'iterations;
                    }
                    response = engine.execute(&request_to_run, &request_context) => response.map_err(|error| error.to_string()),
                }
            };
            summary.requests += 1;
            let duration_ms = started.elapsed().as_millis();
            let mut assertions = 0;
            let mut assertion_failures = Vec::new();
            let item = match result {
                Ok(response) => {
                    let mut error = None;
                    assertions = request_to_run.assertions.len();
                    assertion_failures = evaluate_assertions(&request_to_run.assertions, &response);
                    if !assertion_failures.is_empty() {
                        error = Some(format!(
                            "{} explicit assertion(s) failed",
                            assertion_failures.len()
                        ));
                    }
                    if options.scripts {
                        if let Some(script) = request_to_run.test_script.clone() {
                            match run_script_async(
                                script,
                                request_to_run.clone(),
                                Some(response.clone()),
                                request_context.clone(),
                            )
                            .await
                            {
                                Ok(script_result) => {
                                    assertions += script_result.tests.len();
                                    assertion_failures.extend(
                                        script_result
                                            .failed_tests()
                                            .map(|test| {
                                                format!(
                                                    "{}: {}",
                                                    test.name,
                                                    test.error
                                                        .as_deref()
                                                        .unwrap_or("assertion failed")
                                                )
                                            })
                                            .collect::<Vec<_>>(),
                                    );
                                    if let Err(script_error) = script_result
                                        .apply(&mut request_to_run, &mut request_context)
                                    {
                                        error = Some(script_error.to_string());
                                    } else if !assertion_failures.is_empty() {
                                        if error.is_none() {
                                            error = Some(format!(
                                                "{} assertion(s) failed",
                                                assertion_failures.len()
                                            ));
                                        }
                                    }
                                }
                                Err(script_error) => error = Some(script_error),
                            }
                        }
                    }
                    RunnerItemResult {
                        path: path.clone(),
                        iteration: iteration_index + 1,
                        name: request_to_run.name.clone(),
                        method: request_to_run.method.clone(),
                        status: Some(response.status),
                        duration_ms,
                        passed: response.status < 400
                            && error.is_none()
                            && assertion_failures.is_empty(),
                        error,
                        assertions,
                        assertion_failures,
                    }
                }
                Err(error) => RunnerItemResult {
                    path: path.clone(),
                    iteration: iteration_index + 1,
                    name: request_to_run.name.clone(),
                    method: request_to_run.method.clone(),
                    status: None,
                    duration_ms,
                    error: Some(error),
                    passed: false,
                    assertions,
                    assertion_failures,
                },
            };
            iteration_context = request_context;
            summary.assertions += item.assertions;
            summary.assertion_failures += item.assertion_failures.len();
            if item.passed {
                summary.passed += 1;
            } else {
                summary.failed += 1;
            }
            let should_stop = !item.passed && options.fail_fast;
            summary.results.push(item);
            if should_stop {
                break 'iterations;
            }
        }
    }
    summary
}

async fn run_script_async(
    script: String,
    request: Request,
    response: Option<HttpResponse>,
    context: VariableContext,
) -> Result<ScriptResult, String> {
    tokio::task::spawn_blocking(move || {
        run_script(&script, &request, response.as_ref(), &context)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("script worker failed: {error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{http::EngineOptions, model::Request};

    #[tokio::test]
    async fn a_pre_cancelled_run_does_not_start_network_work() {
        let cancellation = CancellationToken::default();
        cancellation.cancel();
        let options = RunnerOptions {
            cancellation,
            ..RunnerOptions::default()
        };
        let engine = HttpEngine::new(&EngineOptions::default()).expect("engine");
        let request = Request::new("Never sent", "GET", "http://127.0.0.1:1");
        let summary = run_requests(
            &engine,
            &[(PathBuf::from("never.postly.toml"), request)],
            &VariableContext::default(),
            &options,
        )
        .await;

        assert!(summary.cancelled);
        assert_eq!(summary.requests, 0);
        assert!(!summary.succeeded());
    }

    #[tokio::test]
    async fn reports_requested_iterations_even_when_no_requests_are_present() {
        let mut first = Variables::new();
        first.insert("id".to_owned(), "one".to_owned());
        let mut second = Variables::new();
        second.insert("id".to_owned(), "two".to_owned());
        let engine = HttpEngine::new(&EngineOptions::default()).expect("engine");
        let summary = run_requests(
            &engine,
            &[],
            &VariableContext::default(),
            &RunnerOptions {
                iterations: vec![first, second],
                ..RunnerOptions::default()
            },
        )
        .await;

        assert_eq!(summary.iterations, 2);
        assert_eq!(summary.requests, 0);
        assert!(summary.succeeded());
    }

    #[tokio::test]
    async fn runs_explicit_response_assertions_without_node() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("connection");
            use tokio::io::AsyncWriteExt;
            let body = r#"{"ok":true,"count":3}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nx-request-id: local\r\ncontent-length: {}\r\n\r\n{}",
                body.len(), body
            );
            socket.write_all(response.as_bytes()).await.expect("write");
        });
        let mut request =
            Request::new("Asserted health", "GET", format!("http://{address}/health"));
        request.assertions = vec![
            Assertion::Status { expected: 200 },
            Assertion::HeaderEquals {
                name: "content-type".to_owned(),
                expected: "application/json".to_owned(),
            },
            Assertion::BodyContains {
                value: "\"ok\":true".to_owned(),
            },
            Assertion::JsonPointerEquals {
                pointer: "/count".to_owned(),
                expected: serde_json::json!(3),
            },
        ];
        let engine = HttpEngine::new(&EngineOptions::default()).expect("engine");
        let summary = run_requests(
            &engine,
            &[(PathBuf::from("asserted.postly.toml"), request)],
            &VariableContext::default(),
            &RunnerOptions::default(),
        )
        .await;
        server.await.expect("server");

        assert!(summary.succeeded());
        assert_eq!(summary.assertions, 4);
        assert_eq!(summary.assertion_failures, 0);
        assert_eq!(summary.results[0].assertions, 4);
    }

    #[tokio::test]
    async fn runs_post_response_tests_when_scripts_are_enabled() {
        if std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("connection");
            use tokio::io::AsyncWriteExt;
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\n\r\n{\"ok\":true}",
                )
                .await
                .expect("write");
        });
        let mut request =
            Request::new("Scripted health", "GET", format!("http://{address}/health"));
        request.test_script = Some(
            r#"
                pm.test("status is 200", function () {
                    pm.response.to.have.status(200);
                });
                pm.test("body is JSON", function () {
                    pm.expect(pm.response.json().ok).to.be.true;
                });
            "#
            .to_owned(),
        );
        let engine = HttpEngine::new(&EngineOptions::default()).expect("engine");
        let summary = run_requests(
            &engine,
            &[(PathBuf::from("scripted.postly.toml"), request)],
            &VariableContext::default(),
            &RunnerOptions {
                scripts: true,
                ..RunnerOptions::default()
            },
        )
        .await;
        server.await.expect("server");

        assert!(summary.succeeded());
        assert_eq!(summary.assertions, 2);
        assert_eq!(summary.assertion_failures, 0);
        assert_eq!(summary.results[0].assertions, 2);
    }
}
