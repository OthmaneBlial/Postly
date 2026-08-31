use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use futures_util::future::join_all;
use serde::Serialize;
use tokio::sync::Notify;

use crate::{
    http::{HttpEngine, HttpResponse},
    model::{Assertion, JsonValueType, Request, Variables},
    scripting::{
        run_script_with_cancellation_and_info, ScriptExecutionInfo, ScriptResult, ScriptTestResult,
    },
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
    /// Maximum number of script-free requests to execute concurrently.
    /// Requests remain sequential when scripts or delays are enabled.
    pub concurrency: usize,
    pub cancellation: CancellationToken,
    pub iterations: Vec<Variables>,
    pub scripts: bool,
}

impl Default for RunnerOptions {
    fn default() -> Self {
        Self {
            fail_fast: false,
            delay: Duration::ZERO,
            concurrency: 1,
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
    /// Individual Postman-style test results from the optional post-response
    /// script. Native assertions remain represented in `assertions` and
    /// `assertion_failures` for compact compatibility with existing reports.
    #[serde(default)]
    pub script_tests: Vec<ScriptTestResult>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RunnerSummary {
    pub requests: usize,
    pub iterations: usize,
    pub passed: usize,
    pub failed: usize,
    pub assertions: usize,
    pub assertion_failures: usize,
    #[serde(default)]
    pub status_distribution: BTreeMap<u16, usize>,
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
        Assertion::StatusRange { min, max } => {
            if min <= max && response.status >= *min && response.status <= *max {
                Ok(())
            } else {
                Err(format!(
                    "expected status between {} and {}, received {}",
                    min, max, response.status
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
        Assertion::HeaderNotPresent { name } => {
            if response
                .headers
                .iter()
                .all(|header| !header.key.eq_ignore_ascii_case(name))
            {
                Ok(())
            } else {
                Err(format!("expected response header {name} to be absent"))
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
        Assertion::HeaderContains { name, value } => {
            if response
                .headers
                .iter()
                .any(|header| header.key.eq_ignore_ascii_case(name) && header.value.contains(value))
            {
                Ok(())
            } else {
                Err(format!(
                    "expected response header {name} to contain {value}"
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
        Assertion::BodyIsJson => serde_json::from_slice::<serde_json::Value>(&response.body)
            .map(|_| ())
            .map_err(|error| format!("response body is not JSON: {error}")),
        Assertion::CookiePresent { name } => {
            if response.cookies.iter().any(|cookie| cookie.name == *name) {
                Ok(())
            } else {
                Err(format!("expected response cookie {name}"))
            }
        }
        Assertion::CookieNotPresent { name } => {
            if response.cookies.iter().all(|cookie| cookie.name != *name) {
                Ok(())
            } else {
                Err(format!("expected response cookie {name} to be absent"))
            }
        }
        Assertion::CookieEquals { name, expected } => {
            if response
                .cookies
                .iter()
                .any(|cookie| cookie.name == *name && cookie.value == *expected)
            {
                Ok(())
            } else {
                Err(format!(
                    "expected response cookie {name} to equal {expected}"
                ))
            }
        }
        Assertion::ResponseTimeUnder { max_ms } => {
            if response.duration_ms <= u128::from(*max_ms) {
                Ok(())
            } else {
                Err(format!(
                    "expected response time to be at most {max_ms} ms, received {} ms",
                    response.duration_ms
                ))
            }
        }
        Assertion::JsonPointerPresent { pointer } => {
            let body = serde_json::from_slice::<serde_json::Value>(&response.body)
                .map_err(|error| format!("response body is not JSON: {error}"))?;
            if body.pointer(pointer).is_some() {
                Ok(())
            } else {
                Err(format!("JSON Pointer {pointer:?} was not found"))
            }
        }
        Assertion::JsonPointerNotPresent { pointer } => {
            let body = serde_json::from_slice::<serde_json::Value>(&response.body)
                .map_err(|error| format!("response body is not JSON: {error}"))?;
            if body.pointer(pointer).is_none() {
                Ok(())
            } else {
                Err(format!("JSON Pointer {pointer:?} unexpectedly exists"))
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
        Assertion::JsonPointerContains { pointer, expected } => {
            let body = serde_json::from_slice::<serde_json::Value>(&response.body)
                .map_err(|error| format!("response body is not JSON: {error}"))?;
            let actual = body
                .pointer(pointer)
                .ok_or_else(|| format!("JSON Pointer {pointer:?} was not found"))?;
            if json_value_contains(actual, expected) {
                Ok(())
            } else {
                Err(format!(
                    "expected JSON Pointer {pointer:?} to contain {expected}, received {actual}"
                ))
            }
        }
        Assertion::JsonPointerType { pointer, expected } => {
            let body = serde_json::from_slice::<serde_json::Value>(&response.body)
                .map_err(|error| format!("response body is not JSON: {error}"))?;
            let actual = body
                .pointer(pointer)
                .ok_or_else(|| format!("JSON Pointer {pointer:?} was not found"))?;
            if json_value_type_matches(actual, *expected) {
                Ok(())
            } else {
                Err(format!(
                    "expected JSON Pointer {pointer:?} to be a {}, received {actual}",
                    expected.label()
                ))
            }
        }
        Assertion::JsonSchema { pointer, schema } => {
            let body = serde_json::from_slice::<serde_json::Value>(&response.body)
                .map_err(|error| format!("response body is not JSON: {error}"))?;
            let actual = body
                .pointer(pointer)
                .ok_or_else(|| format!("JSON Pointer {pointer:?} was not found"))?;
            validate_json_schema(actual, schema, pointer)
        }
    }
}

/// Implements a predictable, JSON-native subset of Postman's deep inclusion
/// checks without bringing a JavaScript assertion runtime into the runner.
/// Objects require all expected fields, arrays require every expected item to
/// have a matching item, and strings use substring matching.
fn json_value_contains(actual: &serde_json::Value, expected: &serde_json::Value) -> bool {
    match (actual, expected) {
        (serde_json::Value::Object(actual), serde_json::Value::Object(expected)) => {
            expected.iter().all(|(key, expected)| {
                actual
                    .get(key)
                    .is_some_and(|actual| json_value_contains(actual, expected))
            })
        }
        (serde_json::Value::Array(actual), serde_json::Value::Array(expected)) => {
            expected.iter().all(|expected| {
                actual
                    .iter()
                    .any(|actual| json_value_contains(actual, expected))
            })
        }
        (serde_json::Value::Array(actual), expected) => actual
            .iter()
            .any(|actual| json_value_contains(actual, expected)),
        (serde_json::Value::String(actual), serde_json::Value::String(expected)) => {
            actual.contains(expected)
        }
        _ => actual == expected,
    }
}

fn json_value_type_matches(value: &serde_json::Value, expected: JsonValueType) -> bool {
    matches!(
        (value, expected),
        (serde_json::Value::Null, JsonValueType::Null)
            | (serde_json::Value::Bool(_), JsonValueType::Boolean)
            | (serde_json::Value::Number(_), JsonValueType::Number)
            | (serde_json::Value::String(_), JsonValueType::String)
            | (serde_json::Value::Array(_), JsonValueType::Array)
            | (serde_json::Value::Object(_), JsonValueType::Object)
    )
}

/// Validate the deliberately bounded JSON Schema subset used by persisted
/// response assertions. It is JSON-native, deterministic and dependency-free;
/// unsupported annotation keywords are ignored, while the structural and
/// composition keywords below are enforced.
fn validate_json_schema(
    value: &serde_json::Value,
    schema: &serde_json::Value,
    path: &str,
) -> Result<(), String> {
    match schema {
        serde_json::Value::Bool(true) => return Ok(()),
        serde_json::Value::Bool(false) => {
            return Err(format!("schema rejects value at {path:?}"));
        }
        serde_json::Value::Object(schema) => {
            if let Some(reference) = schema.get("$ref") {
                return Err(format!(
                    "JSON Schema $ref is not supported in native assertions: {reference}"
                ));
            }

            if let Some(expected) = schema.get("type") {
                let matches = match expected {
                    serde_json::Value::String(expected) => {
                        json_schema_type_matches(value, expected)
                    }
                    serde_json::Value::Array(expected) => expected
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .any(|expected| json_schema_type_matches(value, expected)),
                    _ => false,
                };
                if !matches {
                    return Err(format!(
                        "expected JSON Schema type at {path:?} to match {expected}, received {value}"
                    ));
                }
            }

            if let Some(expected) = schema.get("const") {
                if value != expected {
                    return Err(format!(
                        "expected JSON Schema const at {path:?} to equal {expected}, received {value}"
                    ));
                }
            }
            if let Some(expected) = schema.get("enum").and_then(serde_json::Value::as_array) {
                if !expected.iter().any(|candidate| candidate == value) {
                    return Err(format!(
                        "expected JSON Schema enum at {path:?} to contain {value}"
                    ));
                }
            }

            validate_json_schema_composition(value, schema, path)?;
            validate_json_schema_object(value, schema, path)?;
            validate_json_schema_array(value, schema, path)?;
            validate_json_schema_string(value, schema, path)?;
            validate_json_schema_number(value, schema, path)?;
        }
        _ => {
            return Err(format!(
                "JSON Schema must be a boolean or object at {path:?}"
            ));
        }
    }
    Ok(())
}

fn json_schema_type_matches(value: &serde_json::Value, expected: &str) -> bool {
    match expected {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "number" => value.is_number(),
        "integer" => {
            value.as_i64().is_some()
                || value.as_u64().is_some()
                || value.as_f64().is_some_and(|number| number.fract() == 0.0)
        }
        "string" => value.is_string(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

fn validate_json_schema_composition(
    value: &serde_json::Value,
    schema: &serde_json::Map<String, serde_json::Value>,
    path: &str,
) -> Result<(), String> {
    if let Some(all_of) = schema.get("allOf").and_then(serde_json::Value::as_array) {
        for (index, branch) in all_of.iter().enumerate() {
            validate_json_schema(value, branch, path)
                .map_err(|error| format!("allOf branch {index}: {error}"))?;
        }
    }
    if let Some(any_of) = schema.get("anyOf").and_then(serde_json::Value::as_array) {
        if !any_of
            .iter()
            .any(|branch| validate_json_schema(value, branch, path).is_ok())
        {
            return Err(format!("no anyOf schema matched value at {path:?}"));
        }
    }
    if let Some(one_of) = schema.get("oneOf").and_then(serde_json::Value::as_array) {
        let matches = one_of
            .iter()
            .filter(|branch| validate_json_schema(value, branch, path).is_ok())
            .count();
        if matches != 1 {
            return Err(format!(
                "expected exactly one oneOf schema to match at {path:?}, matched {matches}"
            ));
        }
    }
    if let Some(not) = schema.get("not") {
        if validate_json_schema(value, not, path).is_ok() {
            return Err(format!("not schema unexpectedly matched value at {path:?}"));
        }
    }
    Ok(())
}

fn validate_json_schema_object(
    value: &serde_json::Value,
    schema: &serde_json::Map<String, serde_json::Value>,
    path: &str,
) -> Result<(), String> {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    if let Some(required) = schema.get("required").and_then(serde_json::Value::as_array) {
        for property in required.iter().filter_map(serde_json::Value::as_str) {
            if !object.contains_key(property) {
                return Err(format!(
                    "required JSON Schema property {property:?} is missing at {path:?}"
                ));
            }
        }
    }
    let properties = schema
        .get("properties")
        .and_then(serde_json::Value::as_object);
    if let Some(properties) = properties {
        for (property, property_schema) in properties {
            if let Some(property_value) = object.get(property) {
                let property_path = format!("{path}/{property}");
                validate_json_schema(property_value, property_schema, &property_path)?;
            }
        }
    }
    if schema.get("additionalProperties") == Some(&serde_json::Value::Bool(false)) {
        if let Some((unknown, _)) = object.iter().find(|(property, _)| {
            properties
                .map(|properties| !properties.contains_key(*property))
                .unwrap_or(true)
        }) {
            return Err(format!(
                "additional JSON Schema property {unknown:?} is not allowed at {path:?}"
            ));
        }
    }
    validate_json_schema_bound(
        object.len(),
        schema.get("minProperties"),
        |actual, expected| actual >= expected,
        "minimum properties",
        path,
    )?;
    validate_json_schema_bound(
        object.len(),
        schema.get("maxProperties"),
        |actual, expected| actual <= expected,
        "maximum properties",
        path,
    )?;
    Ok(())
}

fn validate_json_schema_array(
    value: &serde_json::Value,
    schema: &serde_json::Map<String, serde_json::Value>,
    path: &str,
) -> Result<(), String> {
    let Some(array) = value.as_array() else {
        return Ok(());
    };
    validate_json_schema_bound(
        array.len(),
        schema.get("minItems"),
        |actual, expected| actual >= expected,
        "minimum items",
        path,
    )?;
    validate_json_schema_bound(
        array.len(),
        schema.get("maxItems"),
        |actual, expected| actual <= expected,
        "maximum items",
        path,
    )?;
    if schema.get("uniqueItems") == Some(&serde_json::Value::Bool(true))
        && (0..array.len()).any(|index| array[index + 1..].contains(&array[index]))
    {
        return Err(format!("array items are not unique at {path:?}"));
    }
    if let Some(item_schema) = schema.get("items") {
        for (index, item) in array.iter().enumerate() {
            validate_json_schema(item, item_schema, &format!("{path}/{index}"))?;
        }
    }
    Ok(())
}

fn validate_json_schema_string(
    value: &serde_json::Value,
    schema: &serde_json::Map<String, serde_json::Value>,
    path: &str,
) -> Result<(), String> {
    let Some(string) = value.as_str() else {
        return Ok(());
    };
    validate_json_schema_bound(
        string.chars().count(),
        schema.get("minLength"),
        |actual, expected| actual >= expected,
        "minimum length",
        path,
    )?;
    validate_json_schema_bound(
        string.chars().count(),
        schema.get("maxLength"),
        |actual, expected| actual <= expected,
        "maximum length",
        path,
    )?;
    Ok(())
}

fn validate_json_schema_number(
    value: &serde_json::Value,
    schema: &serde_json::Map<String, serde_json::Value>,
    path: &str,
) -> Result<(), String> {
    let Some(number) = value.as_f64() else {
        return Ok(());
    };
    if let Some(expected) = schema.get("minimum").and_then(serde_json::Value::as_f64) {
        if number < expected {
            return Err(format!(
                "number at {path:?} is below schema minimum {expected}"
            ));
        }
    }
    if let Some(expected) = schema.get("maximum").and_then(serde_json::Value::as_f64) {
        if number > expected {
            return Err(format!(
                "number at {path:?} is above schema maximum {expected}"
            ));
        }
    }
    Ok(())
}

fn validate_json_schema_bound(
    actual: usize,
    expected: Option<&serde_json::Value>,
    predicate: impl Fn(usize, usize) -> bool,
    label: &str,
    path: &str,
) -> Result<(), String> {
    let Some(expected) = expected.and_then(serde_json::Value::as_u64) else {
        return Ok(());
    };
    let expected = usize::try_from(expected).unwrap_or(usize::MAX);
    if predicate(actual, expected) {
        Ok(())
    } else {
        Err(format!(
            "expected {label} {expected} at {path:?}, received {actual}"
        ))
    }
}

/// Evaluate persisted native response assertions without invoking a script
/// runtime. The GUI uses this same function as the collection runner so an
/// interactive send and a headless run report identical failures.
pub fn evaluate_response_assertions(
    assertions: &[Assertion],
    response: &HttpResponse,
) -> Vec<String> {
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

async fn execute_concurrent_request(
    engine: HttpEngine,
    path: PathBuf,
    request: Request,
    context: VariableContext,
    iteration: usize,
    cancellation: CancellationToken,
) -> Option<RunnerItemResult> {
    let started = Instant::now();
    let response = tokio::select! {
        _ = cancellation.cancelled() => return None,
        response = engine.execute(&request, &context) => response,
    };
    let duration_ms = started.elapsed().as_millis();
    match response {
        Ok(response) => {
            let assertion_failures = evaluate_response_assertions(&request.assertions, &response);
            let error = (!assertion_failures.is_empty())
                .then(|| format!("{} explicit assertion(s) failed", assertion_failures.len()));
            Some(RunnerItemResult {
                path,
                iteration,
                name: request.name,
                method: request.method,
                status: Some(response.status),
                duration_ms,
                passed: response.status < 400 && error.is_none(),
                error,
                assertions: request.assertions.len(),
                assertion_failures,
                script_tests: Vec::new(),
            })
        }
        Err(error) => Some(RunnerItemResult {
            path,
            iteration,
            name: request.name,
            method: request.method,
            status: None,
            duration_ms,
            passed: false,
            error: Some(error.to_string()),
            assertions: 0,
            assertion_failures: Vec::new(),
            script_tests: Vec::new(),
        }),
    }
}

async fn execute_concurrent_batch(
    engine: &HttpEngine,
    requests: &[(PathBuf, Request)],
    context: &VariableContext,
    iteration: usize,
    cancellation: &CancellationToken,
) -> (Vec<RunnerItemResult>, bool) {
    let futures = requests.iter().map(|(path, request)| {
        let engine = engine.clone();
        let path = path.clone();
        let request = request.clone();
        let context = context.clone();
        let cancellation = cancellation.clone();
        async move {
            execute_concurrent_request(engine, path, request, context, iteration, cancellation)
                .await
        }
    });
    let results = join_all(futures).await;
    let mut cancelled = false;
    let mut items = Vec::with_capacity(results.len());
    for item in results {
        if let Some(item) = item {
            items.push(item);
        } else {
            cancelled = true;
        }
    }
    (items, cancelled)
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
    let iteration_count = iterations.len();
    summary.iterations = iteration_count;
    'iterations: for (iteration_index, iteration_data) in iterations.into_iter().enumerate() {
        let mut iteration_context = context.clone();
        iteration_context.iteration = iteration_data;
        if options.concurrency > 1 && options.delay.is_zero() && !options.scripts {
            let concurrency = options.concurrency.max(1);
            let mut start = 0;
            while start < requests.len() {
                if options.cancellation.is_cancelled() {
                    summary.cancelled = true;
                    break 'iterations;
                }
                let end = (start + concurrency).min(requests.len());
                let (items, cancelled) = execute_concurrent_batch(
                    engine,
                    &requests[start..end],
                    &iteration_context,
                    iteration_index + 1,
                    &options.cancellation,
                )
                .await;
                let mut batch_failed = false;
                for item in items {
                    if let Some(status) = item.status {
                        *summary.status_distribution.entry(status).or_default() += 1;
                    }
                    summary.requests += 1;
                    summary.assertions += item.assertions;
                    summary.assertion_failures += item.assertion_failures.len();
                    if item.passed {
                        summary.passed += 1;
                    } else {
                        summary.failed += 1;
                        batch_failed = true;
                    }
                    summary.results.push(item);
                }
                if batch_failed && options.fail_fast {
                    break 'iterations;
                }
                if cancelled {
                    summary.cancelled = true;
                    break 'iterations;
                }
                start = end;
            }
            continue;
        }
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
                        ScriptExecutionInfo {
                            event_name: "prerequest".to_owned(),
                            iteration: iteration_index,
                            iteration_count,
                        },
                        options.cancellation.clone(),
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
                        Err(_error) if options.cancellation.is_cancelled() => {
                            summary.cancelled = true;
                            break 'iterations;
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
            let mut script_tests = Vec::new();
            let item = match result {
                Ok(response) => {
                    *summary
                        .status_distribution
                        .entry(response.status)
                        .or_default() += 1;
                    let mut error = None;
                    assertions = request_to_run.assertions.len();
                    assertion_failures =
                        evaluate_response_assertions(&request_to_run.assertions, &response);
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
                                ScriptExecutionInfo {
                                    event_name: "test".to_owned(),
                                    iteration: iteration_index,
                                    iteration_count,
                                },
                                options.cancellation.clone(),
                            )
                            .await
                            {
                                Ok(script_result) => {
                                    script_tests = script_result.tests.clone();
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
                                    } else if !assertion_failures.is_empty() && error.is_none() {
                                        error = Some(format!(
                                            "{} assertion(s) failed",
                                            assertion_failures.len()
                                        ));
                                    }
                                }
                                Err(_script_error) if options.cancellation.is_cancelled() => {
                                    summary.cancelled = true;
                                    break 'iterations;
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
                        script_tests,
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
                    script_tests,
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
    info: ScriptExecutionInfo,
    cancellation: CancellationToken,
) -> Result<ScriptResult, String> {
    tokio::task::spawn_blocking(move || {
        run_script_with_cancellation_and_info(
            &script,
            &request,
            response.as_ref(),
            &context,
            info,
            || cancellation.is_cancelled(),
        )
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
    async fn cancellation_terminates_a_running_script_before_network_work() {
        if std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let cancellation = CancellationToken::default();
        let trigger = cancellation.clone();
        let trigger_thread = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            trigger.cancel();
        });
        let mut request = Request::new("Long script", "GET", "http://127.0.0.1:1/never");
        request.pre_request_script = Some("while (true) {}".to_owned());
        let engine = HttpEngine::new(&EngineOptions::default()).expect("engine");
        let summary = run_requests(
            &engine,
            &[(PathBuf::from("long-script.postly.toml"), request)],
            &VariableContext::default(),
            &RunnerOptions {
                scripts: true,
                cancellation,
                ..RunnerOptions::default()
            },
        )
        .await;
        trigger_thread.join().expect("cancellation trigger");

        assert!(summary.cancelled);
        assert_eq!(summary.requests, 0);
        assert!(summary.results.is_empty());
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
            let body = r#"{"ok":true,"count":3,"tags":["postly","rust"],"message":"hello postly"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nx-request-id: local\r\nset-cookie: session=abc; Path=/\r\ncontent-length: {}\r\n\r\n{}",
                body.len(), body
            );
            socket.write_all(response.as_bytes()).await.expect("write");
        });
        let mut request =
            Request::new("Asserted health", "GET", format!("http://{address}/health"));
        request.assertions = vec![
            Assertion::Status { expected: 200 },
            Assertion::StatusRange { min: 200, max: 299 },
            Assertion::HeaderEquals {
                name: "content-type".to_owned(),
                expected: "application/json".to_owned(),
            },
            Assertion::HeaderNotPresent {
                name: "x-missing".to_owned(),
            },
            Assertion::HeaderContains {
                name: "content-type".to_owned(),
                value: "json".to_owned(),
            },
            Assertion::BodyContains {
                value: "\"ok\":true".to_owned(),
            },
            Assertion::BodyIsJson,
            Assertion::CookiePresent {
                name: "session".to_owned(),
            },
            Assertion::CookieEquals {
                name: "session".to_owned(),
                expected: "abc".to_owned(),
            },
            Assertion::CookieNotPresent {
                name: "missing".to_owned(),
            },
            Assertion::ResponseTimeUnder { max_ms: 5_000 },
            Assertion::JsonPointerPresent {
                pointer: "/ok".to_owned(),
            },
            Assertion::JsonPointerNotPresent {
                pointer: "/missing".to_owned(),
            },
            Assertion::JsonPointerEquals {
                pointer: "/count".to_owned(),
                expected: serde_json::json!(3),
            },
            Assertion::JsonPointerContains {
                pointer: String::new(),
                expected: serde_json::json!({"ok": true}),
            },
            Assertion::JsonPointerContains {
                pointer: "/tags".to_owned(),
                expected: serde_json::json!("rust"),
            },
            Assertion::JsonPointerContains {
                pointer: "/message".to_owned(),
                expected: serde_json::json!("postly"),
            },
            Assertion::JsonPointerType {
                pointer: "/ok".to_owned(),
                expected: JsonValueType::Boolean,
            },
            Assertion::JsonSchema {
                pointer: String::new(),
                schema: serde_json::json!({
                    "type": "object",
                    "required": ["ok", "count", "tags", "message"],
                    "properties": {
                        "ok": { "const": true },
                        "count": { "type": "integer", "minimum": 1 },
                        "tags": { "type": "array", "minItems": 2, "items": { "type": "string" } },
                        "message": { "type": "string", "minLength": 1 }
                    }
                }),
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
        assert_eq!(summary.assertions, 19);
        assert_eq!(summary.assertion_failures, 0);
        assert_eq!(summary.status_distribution.get(&200), Some(&1));
        assert_eq!(summary.results[0].assertions, 19);
    }

    #[test]
    fn json_value_contains_supports_deep_objects_arrays_and_strings() {
        let actual = serde_json::json!({
            "profile": {"role": "admin", "flags": ["staff", "active"]},
            "message": "hello postly"
        });
        assert!(json_value_contains(
            &actual,
            &serde_json::json!({"profile": {"flags": ["active"]}})
        ));
        assert!(json_value_contains(
            actual.pointer("/profile/flags").expect("flags"),
            &serde_json::json!("staff")
        ));
        assert!(json_value_contains(
            actual.pointer("/message").expect("message"),
            &serde_json::json!("postly")
        ));
        assert!(!json_value_contains(
            &actual,
            &serde_json::json!({"profile": {"role": "owner"}})
        ));
    }

    #[test]
    fn json_value_type_matching_covers_all_native_json_kinds() {
        let cases = [
            (serde_json::Value::Null, JsonValueType::Null),
            (serde_json::json!(true), JsonValueType::Boolean),
            (serde_json::json!(3), JsonValueType::Number),
            (serde_json::json!("postly"), JsonValueType::String),
            (serde_json::json!([1]), JsonValueType::Array),
            (serde_json::json!({"ok": true}), JsonValueType::Object),
        ];
        for (value, expected) in cases {
            assert!(json_value_type_matches(&value, expected));
        }
        assert!(!json_value_type_matches(
            &serde_json::json!("3"),
            JsonValueType::Number
        ));
    }

    #[test]
    fn json_schema_assertions_cover_structure_and_composition() {
        let actual = serde_json::json!({
            "id": 7,
            "role": "admin",
            "tags": ["api", "rust"]
        });
        let schema = serde_json::json!({
            "allOf": [{
                "type": "object",
                "required": ["id", "role"],
                "properties": {
                    "id": { "type": "integer", "minimum": 1 },
                    "role": { "enum": ["admin", "maintainer"] }
                }
            }, {
                "properties": {
                    "tags": {
                        "type": "array",
                        "minItems": 2,
                        "uniqueItems": true,
                        "items": { "type": "string", "minLength": 2 }
                    }
                }
            }]
        });
        validate_json_schema(&actual, &schema, "").expect("schema should accept the response");

        let invalid = serde_json::json!({ "id": 0, "role": "viewer", "tags": ["x", "x"] });
        assert!(validate_json_schema(&invalid, &schema, "").is_err());
        assert!(validate_json_schema(
            &actual,
            &serde_json::json!({ "not": { "type": "object" } }),
            ""
        )
        .is_err());
        let closed = serde_json::json!({
            "type": "object",
            "properties": { "id": { "type": "integer" } },
            "additionalProperties": false
        });
        assert!(validate_json_schema(&serde_json::json!({ "id": 7 }), &closed, "").is_ok());
        assert!(validate_json_schema(&serde_json::json!({ "extra": true }), &closed, "").is_err());
        let closed_without_properties = serde_json::json!({ "additionalProperties": false });
        assert!(validate_json_schema(
            &serde_json::json!({ "extra": true }),
            &closed_without_properties,
            ""
        )
        .is_err());
    }

    #[tokio::test]
    async fn runs_script_free_requests_with_bounded_concurrency_in_stable_order() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (first, _) = listener.accept().await.expect("first connection");
            let (second, _) = listener.accept().await.expect("second connection");
            let respond = |mut socket: tokio::net::TcpStream| async move {
                let mut request = [0_u8; 4096];
                let _request_len = socket.read(&mut request).await.expect("request");
                socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                    )
                    .await
                    .expect("response");
                socket.shutdown().await.expect("shutdown");
            };
            let _ = tokio::join!(respond(first), respond(second));
        });
        let engine = HttpEngine::new(&EngineOptions {
            timeout: Duration::from_secs(2),
            ..EngineOptions::default()
        })
        .expect("engine");
        let requests = [
            (
                PathBuf::from("first.postly.toml"),
                Request::new("First", "GET", format!("http://{address}/first")),
            ),
            (
                PathBuf::from("second.postly.toml"),
                Request::new("Second", "GET", format!("http://{address}/second")),
            ),
        ];
        let summary = tokio::time::timeout(
            Duration::from_secs(3),
            run_requests(
                &engine,
                &requests,
                &VariableContext::default(),
                &RunnerOptions {
                    concurrency: 2,
                    ..RunnerOptions::default()
                },
            ),
        )
        .await
        .expect("concurrent runner should complete");
        server.await.expect("server");

        assert!(summary.succeeded());
        assert_eq!(summary.requests, 2);
        assert_eq!(summary.results[0].name, "First");
        assert_eq!(summary.results[1].name, "Second");
        assert_eq!(summary.status_distribution.get(&200), Some(&2));
    }

    #[tokio::test]
    async fn passes_iteration_data_to_pre_request_scripts() {
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
            use tokio::io::{AsyncReadExt, AsyncWriteExt};

            for (expected, iteration) in [("one", "0"), ("two", "1")] {
                let (mut socket, _) = listener.accept().await.expect("connection");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                loop {
                    let read = socket.read(&mut buffer).await.expect("read request");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
                assert!(request.contains(&format!("x-iteration: {expected}")));
                assert!(request.contains(&format!("x-info-iteration: {iteration}")));
                assert!(request.contains("x-info-count: 2"));
                assert!(request.contains("x-info-event: prerequest"));
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nconnection: close\r\ncontent-length: 0\r\n\r\n")
                    .await
                    .expect("write response");
            }
        });

        let request = {
            let mut request = Request::new(
                "Iteration script",
                "GET",
                format!("http://{address}/health"),
            );
            request.pre_request_script = Some(
                r#"
                    pm.request.headers.upsert({
                        key: "X-Iteration",
                        value: pm.iterationData.get("id")
                    });
                    pm.request.headers.upsert({
                        key: "X-Info-Iteration",
                        value: String(pm.info.iteration)
                    });
                    pm.request.headers.upsert({
                        key: "X-Info-Count",
                        value: String(pm.info.iterationCount)
                    });
                    pm.request.headers.upsert({
                        key: "X-Info-Event",
                        value: pm.info.eventName
                    });
                "#
                .to_owned(),
            );
            request
        };
        let mut first = Variables::new();
        first.insert("id".to_owned(), "one".to_owned());
        let mut second = Variables::new();
        second.insert("id".to_owned(), "two".to_owned());
        let engine = HttpEngine::new(&EngineOptions::default()).expect("engine");
        let summary = run_requests(
            &engine,
            &[(PathBuf::from("iteration.postly.toml"), request)],
            &VariableContext::default(),
            &RunnerOptions {
                iterations: vec![first, second],
                scripts: true,
                ..RunnerOptions::default()
            },
        )
        .await;
        server.await.expect("server");

        assert!(summary.succeeded());
        assert_eq!(summary.requests, 2);
        assert_eq!(summary.failed, 0);
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
                pm.test("runner metadata is available", function () {
                    pm.expect(pm.info.eventName).to.eql("test");
                    pm.expect(pm.info.iteration).to.eql(0);
                    pm.expect(pm.info.iterationCount).to.eql(1);
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
        assert_eq!(summary.assertions, 3);
        assert_eq!(summary.assertion_failures, 0);
        assert_eq!(summary.results[0].assertions, 3);
        assert_eq!(summary.results[0].script_tests.len(), 3);
        assert_eq!(summary.results[0].script_tests[0].name, "status is 200");
        assert!(summary.results[0].script_tests[0].passed);
        assert!(summary.results[0].script_tests[0].duration_ms < 2_000);
    }

    #[tokio::test]
    async fn runs_pm_send_request_callback_tests_in_the_runner() {
        if std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let helper_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("helper listener");
        let helper_address = helper_listener.local_addr().expect("helper address");
        let helper_server = tokio::spawn(async move {
            let (mut socket, _) = helper_listener.accept().await.expect("helper connection");
            use tokio::io::AsyncWriteExt;
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 14\r\nconnection: close\r\n\r\n{\"token\":\"ok\"}",
                )
                .await
                .expect("helper response");
        });

        let api_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("api listener");
        let api_address = api_listener.local_addr().expect("api address");
        let api_server = tokio::spawn(async move {
            let (mut socket, _) = api_listener.accept().await.expect("api connection");
            use tokio::io::AsyncWriteExt;
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{\"ok\":true}",
                )
                .await
                .expect("api response");
        });

        let mut request = Request::new(
            "Scripted workflow",
            "GET",
            format!("http://{api_address}/health"),
        );
        request.test_script = Some(format!(
            r#"
                pm.sendRequest("http://{helper_address}/token", function (error, response) {{
                    pm.test("helper request is available", function () {{
                        pm.expect(error).to.eql(null);
                        pm.expect(response.code).to.eql(200);
                        pm.expect(response.json()).to.have.property("token", "ok");
                    }});
                }});
            "#
        ));
        let engine = HttpEngine::new(&EngineOptions::default()).expect("engine");
        let summary = run_requests(
            &engine,
            &[(PathBuf::from("scripted-workflow.postly.toml"), request)],
            &VariableContext::default(),
            &RunnerOptions {
                scripts: true,
                ..RunnerOptions::default()
            },
        )
        .await;
        api_server.await.expect("api server");
        helper_server.await.expect("helper server");

        assert!(summary.succeeded());
        assert_eq!(summary.assertions, 1);
        assert_eq!(summary.assertion_failures, 0);
        assert_eq!(summary.status_distribution.get(&200), Some(&1));
    }
}
