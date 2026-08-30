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

use crate::{http::HttpEngine, model::Request, variables::VariableContext};

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
}

impl Default for RunnerOptions {
    fn default() -> Self {
        Self {
            fail_fast: false,
            delay: Duration::ZERO,
            cancellation: CancellationToken::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerItemResult {
    pub path: PathBuf,
    pub name: String,
    pub method: String,
    pub status: Option<u16>,
    pub duration_ms: u128,
    pub error: Option<String>,
    pub passed: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RunnerSummary {
    pub requests: usize,
    pub passed: usize,
    pub failed: usize,
    pub cancelled: bool,
    pub results: Vec<RunnerItemResult>,
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
    for (index, (path, request)) in requests.iter().enumerate() {
        if options.cancellation.is_cancelled() {
            summary.cancelled = true;
            break;
        }
        if index > 0 && !options.delay.is_zero() {
            tokio::select! {
                _ = options.cancellation.cancelled() => {
                    summary.cancelled = true;
                    break;
                }
                _ = tokio::time::sleep(options.delay) => {}
            }
        }

        let started = Instant::now();
        let result = tokio::select! {
            _ = options.cancellation.cancelled() => {
                summary.cancelled = true;
                break;
            }
            response = engine.execute(request, context) => response,
        };
        summary.requests += 1;
        let duration_ms = started.elapsed().as_millis();
        let item = match result {
            Ok(response) => RunnerItemResult {
                path: path.clone(),
                name: request.name.clone(),
                method: request.method.clone(),
                status: Some(response.status),
                duration_ms,
                error: None,
                passed: response.status < 400,
            },
            Err(error) => RunnerItemResult {
                path: path.clone(),
                name: request.name.clone(),
                method: request.method.clone(),
                status: None,
                duration_ms,
                error: Some(error.to_string()),
                passed: false,
            },
        };
        if item.passed {
            summary.passed += 1;
        } else {
            summary.failed += 1;
        }
        let should_stop = !item.passed && options.fail_fast;
        summary.results.push(item);
        if should_stop {
            break;
        }
    }
    summary
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
}
