use std::{
    io::{self, Read, Write},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    http::{HttpResponse, ResponseCookie},
    model::{Request, Variables},
    variables::VariableContext,
};

#[derive(Debug, Error)]
pub enum ScriptError {
    #[error("Node.js is required to execute Postly scripts: {0}")]
    NodeUnavailable(#[source] std::io::Error),
    #[error("could not serialize script input: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("script execution failed: {0}")]
    Execution(String),
    #[error("script returned invalid output: {0}")]
    InvalidOutput(#[source] serde_json::Error),
    #[error("script returned an invalid request object: {0}")]
    InvalidRequest(#[source] serde_json::Error),
    #[error("script is too large to execute safely (maximum {maximum_bytes} bytes)")]
    TooLarge { maximum_bytes: usize },
    #[error("serialized script input is too large (maximum {maximum_bytes} bytes)")]
    InputTooLarge { maximum_bytes: usize },
    #[error("script process output exceeded the {maximum_bytes}-byte pipe limit")]
    OutputTooLarge { maximum_bytes: usize },
    #[error("script process exceeded the {timeout_seconds}-second execution limit")]
    Timeout { timeout_seconds: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScriptTestResult {
    pub name: String,
    pub passed: bool,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScriptLog {
    pub level: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScriptResult {
    pub request: Value,
    #[serde(default)]
    pub environment_updates: Variables,
    #[serde(default)]
    pub collection_updates: Variables,
    #[serde(default)]
    pub globals_updates: Variables,
    #[serde(default)]
    pub runtime_updates: Variables,
    #[serde(default)]
    pub environment_unsets: Vec<String>,
    #[serde(default)]
    pub collection_unsets: Vec<String>,
    #[serde(default)]
    pub globals_unsets: Vec<String>,
    #[serde(default)]
    pub runtime_unsets: Vec<String>,
    #[serde(default)]
    pub tests: Vec<ScriptTestResult>,
    #[serde(default)]
    pub logs: Vec<ScriptLog>,
}

impl ScriptResult {
    pub fn failed_tests(&self) -> impl Iterator<Item = &ScriptTestResult> {
        self.tests.iter().filter(|test| !test.passed)
    }

    pub fn apply(
        &self,
        request: &mut Request,
        context: &mut VariableContext,
    ) -> Result<(), ScriptError> {
        apply_variable_changes(
            &mut context.environment,
            &self.environment_updates,
            &self.environment_unsets,
        );
        apply_variable_changes(
            &mut context.collection,
            &self.collection_updates,
            &self.collection_unsets,
        );
        apply_variable_changes(
            &mut context.globals,
            &self.globals_updates,
            &self.globals_unsets,
        );
        apply_variable_changes(
            &mut context.runtime,
            &self.runtime_updates,
            &self.runtime_unsets,
        );
        *request =
            serde_json::from_value(self.request.clone()).map_err(ScriptError::InvalidRequest)?;
        Ok(())
    }
}

fn apply_variable_changes(target: &mut Variables, updates: &Variables, unsets: &[String]) {
    for key in unsets {
        target.remove(key);
    }
    target.extend(
        updates
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
}

#[derive(Debug, Clone, Serialize)]
struct ScriptInput {
    script: String,
    variables: VariableContext,
    request: Value,
    response: Option<ResponseInput>,
}

#[derive(Debug, Clone, Serialize)]
struct ResponseInput {
    status: u16,
    status_text: String,
    headers: Vec<crate::model::HeaderEntry>,
    cookies: Vec<ResponseCookie>,
    body_text: String,
    duration_ms: u128,
}

#[derive(Debug, Deserialize)]
struct NodeOutput {
    request: Value,
    #[serde(default)]
    changes: VariableChanges,
    #[serde(default)]
    tests: Vec<ScriptTestResult>,
    #[serde(default)]
    logs: Vec<ScriptLog>,
}

#[derive(Debug, Default, Deserialize)]
struct VariableChanges {
    #[serde(default)]
    environment: Variables,
    #[serde(default)]
    collection: Variables,
    #[serde(default)]
    globals: Variables,
    #[serde(default)]
    runtime: Variables,
    #[serde(default)]
    removed: VariableRemovals,
}

#[derive(Debug, Default, Deserialize)]
struct VariableRemovals {
    #[serde(default)]
    environment: Vec<String>,
    #[serde(default)]
    collection: Vec<String>,
    #[serde(default)]
    globals: Vec<String>,
    #[serde(default)]
    runtime: Vec<String>,
}

pub fn run_script(
    script: &str,
    request: &Request,
    response: Option<&HttpResponse>,
    context: &VariableContext,
) -> Result<ScriptResult, ScriptError> {
    if script.len() > MAX_SCRIPT_BYTES {
        return Err(ScriptError::TooLarge {
            maximum_bytes: MAX_SCRIPT_BYTES,
        });
    }
    let input = ScriptInput {
        script: script.to_owned(),
        variables: context.clone(),
        request: serde_json::to_value(request)?,
        response: response.map(|response| ResponseInput {
            status: response.status,
            status_text: response.status_text.clone(),
            headers: response.headers.clone(),
            cookies: response.cookies.clone(),
            body_text: response.body_text(),
            duration_ms: response.duration_ms,
        }),
    };
    let payload = serde_json::to_vec(&input)?;
    if payload.len() > MAX_SCRIPT_INPUT_BYTES {
        return Err(ScriptError::InputTooLarge {
            maximum_bytes: MAX_SCRIPT_INPUT_BYTES,
        });
    }
    let mut command = Command::new("node");
    command
        .env_clear()
        .args(["--input-type=commonjs", "-e", NODE_HARNESS]);
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(ScriptError::NodeUnavailable)?;
    let mut stdin = child.stdin.take().ok_or_else(|| {
        ScriptError::Execution("Node script process did not expose stdin.".to_owned())
    })?;
    stdin
        .write_all(&payload)
        .map_err(ScriptError::NodeUnavailable)?;
    drop(stdin);
    let output = wait_for_child(child)?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(ScriptError::Execution(if message.is_empty() {
            "Node.js exited with a failure status.".to_owned()
        } else {
            message
        }));
    }
    let node_output =
        serde_json::from_slice::<NodeOutput>(&output.stdout).map_err(ScriptError::InvalidOutput)?;
    Ok(ScriptResult {
        request: node_output.request,
        environment_updates: node_output.changes.environment,
        collection_updates: node_output.changes.collection,
        globals_updates: node_output.changes.globals,
        runtime_updates: node_output.changes.runtime,
        environment_unsets: node_output.changes.removed.environment,
        collection_unsets: node_output.changes.removed.collection,
        globals_unsets: node_output.changes.removed.globals,
        runtime_unsets: node_output.changes.removed.runtime,
        tests: node_output.tests,
        logs: node_output.logs,
    })
}

const MAX_SCRIPT_BYTES: usize = 512 * 1024;
const MAX_SCRIPT_INPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_SCRIPT_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const SCRIPT_TIMEOUT: Duration = Duration::from_secs(3);
#[cfg(test)]
const MAX_LOG_ENTRIES: usize = 200;
#[cfg(test)]
const MAX_LOG_MESSAGE_BYTES: usize = 4096;
#[cfg(test)]
const MAX_TEST_ENTRIES: usize = 1000;

fn wait_for_child(mut child: Child) -> Result<Output, ScriptError> {
    let stdout = child.stdout.take().ok_or_else(|| {
        ScriptError::Execution("Node script process did not expose stdout.".to_owned())
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        ScriptError::Execution("Node script process did not expose stderr.".to_owned())
    })?;
    let stdout_reader = thread::spawn(move || read_pipe(stdout));
    let stderr_reader = thread::spawn(move || read_pipe(stderr));
    let deadline = Instant::now() + SCRIPT_TIMEOUT;
    loop {
        match child.try_wait().map_err(ScriptError::NodeUnavailable)? {
            Some(status) => {
                let stdout = join_pipe(stdout_reader)?;
                let stderr = join_pipe(stderr_reader)?;
                return Ok(Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = join_pipe(stdout_reader);
                let _ = join_pipe(stderr_reader);
                return Err(ScriptError::Timeout {
                    timeout_seconds: SCRIPT_TIMEOUT.as_secs(),
                });
            }
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn read_pipe(mut pipe: impl Read) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = pipe.read(&mut buffer)?;
        if count == 0 {
            return Ok(output);
        }
        if output.len().saturating_add(count) > MAX_SCRIPT_OUTPUT_BYTES {
            return Err(io::Error::other("script output limit exceeded"));
        }
        output.extend_from_slice(&buffer[..count]);
    }
}

fn join_pipe(reader: thread::JoinHandle<io::Result<Vec<u8>>>) -> Result<Vec<u8>, ScriptError> {
    reader
        .join()
        .map_err(|_| ScriptError::Execution("script output reader panicked.".to_owned()))?
        .map_err(|error| {
            if error.kind() == io::ErrorKind::Other
                && error.to_string() == "script output limit exceeded"
            {
                ScriptError::OutputTooLarge {
                    maximum_bytes: MAX_SCRIPT_OUTPUT_BYTES,
                }
            } else {
                ScriptError::NodeUnavailable(error)
            }
        })
}

const NODE_HARNESS: &str = r##"
const vm = require("node:vm");
const fs = require("node:fs");
const input = JSON.parse(fs.readFileSync(0, "utf8"));
const changes = { environment: {}, collection: {}, runtime: {} };
changes.globals = {};
const removals = { environment: [], collection: [], globals: [], runtime: [] };
const tests = [];
const logs = [];
const MAX_LOG_ENTRIES = 200;
const MAX_LOG_MESSAGE_BYTES = 4096;
const MAX_TEST_ENTRIES = 1000;
const values = input.variables || {};

function text(value) {
  return value === undefined || value === null ? "" : String(value);
}

function visibleGet(key) {
  for (const scope of ["iteration", "runtime", "request", "environment", "collection", "project", "globals"]) {
    if (Object.prototype.hasOwnProperty.call(values[scope] || {}, key)) {
      return values[scope][key];
    }
  }
  return undefined;
}

function replaceIn(value) {
  return text(value).replace(/\{\{\s*([^}]+?)\s*\}\}/g, (_, key) => {
    const found = visibleGet(key.trim());
    return found === undefined ? "{{" + key + "}}" : text(found);
  });
}

function scope(name) {
  values[name] = values[name] || {};
  return {
    get: (key) => values[name][key],
    has: (key) => Object.prototype.hasOwnProperty.call(values[name], key),
    set: (key, value) => {
      values[name][key] = text(value);
      changes[name][key] = text(value);
      const removalIndex = removals[name].indexOf(key);
      if (removalIndex !== -1) removals[name].splice(removalIndex, 1);
    },
    unset: (key) => {
      delete values[name][key];
      delete changes[name][key];
      if (!removals[name].includes(key)) removals[name].push(key);
    },
    clear: () => {
      Object.keys(values[name]).forEach((key) => {
        delete values[name][key];
        delete changes[name][key];
        if (!removals[name].includes(key)) removals[name].push(key);
      });
    },
    replaceIn
  };
}

const environment = scope("environment");
const collectionVariables = scope("collection");
const globals = scope("globals");
const iterationData = {
  get: (key) => (values.iteration || {})[key],
  has: (key) => Object.prototype.hasOwnProperty.call(values.iteration || {}, key),
  replaceIn
};
const runtime = {
  get: visibleGet,
  has: (key) => visibleGet(key) !== undefined,
  set: (key, value) => {
    values.runtime = values.runtime || {};
    values.runtime[key] = text(value);
    changes.runtime[key] = text(value);
    const removalIndex = removals.runtime.indexOf(key);
    if (removalIndex !== -1) removals.runtime.splice(removalIndex, 1);
  },
  unset: (key) => {
    if (values.runtime) delete values.runtime[key];
    delete changes.runtime[key];
    if (!removals.runtime.includes(key)) removals.runtime.push(key);
  },
  clear: () => {
    Object.keys(values.runtime || {}).forEach((key) => runtime.unset(key));
  },
  replaceIn
};

const request = input.request || {};
const requestHeaders = request.headers || [];
requestHeaders.get = (name) => {
  const found = requestHeaders.find((header) => header.key && header.key.toLowerCase() === text(name).toLowerCase() && header.enabled !== false);
  return found ? found.value : undefined;
};
requestHeaders.has = (name) => requestHeaders.get(name) !== undefined;
requestHeaders.add = (header) => {
  const key = text(header && (header.key || header.name)).trim();
  if (!key) return;
  requestHeaders.push({ key, value: text(header.value), enabled: header.disabled !== true && header.enabled !== false });
};
requestHeaders.upsert = (header) => {
  const key = text(header && (header.key || header.name)).trim();
  if (!key) return;
  const found = requestHeaders.find((entry) => entry.key && entry.key.toLowerCase() === key.toLowerCase());
  if (found) {
    found.value = text(header.value);
    found.enabled = header.disabled !== true && header.enabled !== false;
  } else {
    requestHeaders.add(header);
  }
};
requestHeaders.remove = (name) => {
  const normalized = text(name).toLowerCase();
  for (let index = requestHeaders.length - 1; index >= 0; index -= 1) {
    if (requestHeaders[index].key && requestHeaders[index].key.toLowerCase() === normalized) {
      requestHeaders.splice(index, 1);
    }
  }
};
request.headers = requestHeaders;

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function expect(value) {
  function expectation(negated) {
    const prefix = negated ? " not" : "";
    const check = (condition, message) => assert(negated ? !condition : condition, message);
    const to = {
      equal: (expected) => check(value === expected, "expected " + JSON.stringify(value) + " to" + prefix + " equal " + JSON.stringify(expected)),
      eql: (expected) => check(JSON.stringify(value) === JSON.stringify(expected), "expected " + JSON.stringify(value) + " to" + prefix + " deeply equal " + JSON.stringify(expected)),
      include: (expected) => {
        const included = typeof value === "string" || Array.isArray(value)
          ? value.includes(expected)
          : value !== null && value !== undefined && Object.prototype.hasOwnProperty.call(value, expected);
        check(included, "expected " + JSON.stringify(value) + " to" + prefix + " include " + JSON.stringify(expected));
      },
      match: (pattern) => check(typeof value === "string" && pattern.test(value), "expected " + JSON.stringify(value) + " to" + prefix + " match the pattern"),
      have: {
        property: function (name, expected) {
          const present = value !== null && value !== undefined && Object.prototype.hasOwnProperty.call(value, name);
          check(present, "expected property " + name);
          if (arguments.length > 1 && present) check(value[name] === expected, "expected property " + name + " to" + prefix + " equal " + JSON.stringify(expected));
        }
      }
    };
    Object.defineProperty(to, "be", {
      value: {
        get true() { check(value === true, "expected " + JSON.stringify(value) + " to" + prefix + " be true"); return true; },
        get false() { check(value === false, "expected " + JSON.stringify(value) + " to" + prefix + " be false"); return true; },
        get ok() { check(Boolean(value), "expected " + JSON.stringify(value) + " to" + prefix + " be truthy"); return true; },
        above: (expected) => check(value > expected, "expected " + JSON.stringify(value) + " to" + prefix + " be above " + expected),
        below: (expected) => check(value < expected, "expected " + JSON.stringify(value) + " to" + prefix + " be below " + expected),
        a: (type) => check(typeof value === text(type), "expected value to" + prefix + " be a " + type)
      }
    });
    Object.defineProperty(to, "not", { get: () => expectation(!negated) });
    return to;
  }
  return { to: expectation(false) };
}

const responseData = input.response;
let response = null;
if (responseData) {
  const responseHeaders = responseData.headers || [];
  responseHeaders.get = (name) => {
    const found = responseHeaders.find((header) => header.key.toLowerCase() === text(name).toLowerCase() && header.enabled !== false);
    return found ? found.value : undefined;
  };
  const responseCookies = responseData.cookies || [];
  responseCookies.get = (name) => {
    const found = responseCookies.find((cookie) => cookie.name.toLowerCase() === text(name).toLowerCase());
    return found ? found.value : undefined;
  };
  const responseTo = {
    have: {
      status: (expected) => assert(responseData.status === expected, "expected status " + responseData.status + " to equal " + expected),
      header: (name) => assert(responseHeaders.get(name) !== undefined, "expected response header " + name)
    }
  };
  Object.defineProperty(responseTo, "be", {
    value: {
      get ok() {
        assert(responseData.status >= 200 && responseData.status < 400, "expected response to be ok");
        return true;
      }
    }
  });
  response = {
    code: responseData.status,
    status: responseData.status_text,
    responseTime: responseData.duration_ms,
    headers: responseHeaders,
    cookies: responseCookies,
    text: () => responseData.body_text,
    json: () => JSON.parse(responseData.body_text),
    to: responseTo
  };
}

const scriptConsole = {
  log: (...args) => captureLog("log", args),
  warn: (...args) => captureLog("warn", args),
  error: (...args) => captureLog("error", args)
};

function captureLog(level, args) {
  if (logs.length >= MAX_LOG_ENTRIES) return;
  const message = args.map((value) => text(value).slice(0, MAX_LOG_MESSAGE_BYTES)).join(" ");
  logs.push({ level, message: message.slice(0, MAX_LOG_MESSAGE_BYTES) });
}

function recordTest(name, callback) {
  if (tests.length >= MAX_TEST_ENTRIES) {
    return;
  }
  if (tests.length === MAX_TEST_ENTRIES - 1) {
    tests.push({
      name: "Postly script test limit",
      passed: false,
      error: "The script exceeded the maximum of " + MAX_TEST_ENTRIES + " tests."
    });
    return;
  }
  try {
    callback();
    tests.push({ name: text(name), passed: true });
  } catch (error) {
    tests.push({ name: text(name), passed: false, error: text(error && (error.stack || error.message || error)) });
  }
}

const pm = {
  environment,
  collectionVariables,
  globals,
  iterationData,
  variables: runtime,
  request,
  response,
  test: recordTest,
  expect
};

const sandbox = {
  pm,
  console: scriptConsole,
  JSON,
  Math,
  Date,
  URL,
  String,
  Number,
  Boolean,
  Object,
  Array
};
vm.createContext(sandbox);
new vm.Script(input.script, { filename: "postly-script.js" }).runInContext(sandbox, { timeout: 2000 });
process.stdout.write(JSON.stringify({
  request,
  changes: { ...changes, removed: removals },
  tests,
  logs
}));
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Request;

    #[test]
    fn executes_basic_pm_variables_and_assertions() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let mut context = VariableContext::default();
        context
            .environment
            .insert("token".to_owned(), "old".to_owned());
        let request = Request::new("Scripted", "GET", "{{baseUrl}}/users");
        let result = run_script(
            r#"
                pm.environment.set("token", "new");
                pm.test("token changed", function () {
                    pm.expect(pm.environment.get("token")).to.eql("new");
                });
            "#,
            &request,
            None,
            &context,
        )
        .expect("script");
        assert_eq!(result.environment_updates["token"], "new");
        assert_eq!(result.tests.len(), 1);
        assert!(result.tests[0].passed);
    }

    #[test]
    fn captures_response_assertion_failures_without_losing_the_request() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let request = Request::new("Scripted", "GET", "https://example.test");
        let response = HttpResponse {
            status: 201,
            status_text: "Created".to_owned(),
            headers: vec![crate::model::HeaderEntry::enabled(
                "Content-Type",
                "application/json",
            )],
            body: br#"{"ok":true}"#.to_vec(),
            response_size: 11,
            content_type: Some("application/json".to_owned()),
            duration_ms: 4,
            protocol: "HTTP/1.1".to_owned(),
            url: "https://example.test".to_owned(),
            cookies: vec![ResponseCookie {
                name: "session".to_owned(),
                value: "abc".to_owned(),
                domain: None,
                path: Some("/".to_owned()),
                secure: false,
                http_only: true,
                same_site: Some("Lax".to_owned()),
                expires: None,
                max_age_seconds: None,
            }],
        };
        let result = run_script(
            r#"
                pm.test("status", function () {
                    pm.response.to.have.status(200);
                });
                pm.test("json", function () {
                    pm.expect(pm.response.json().ok).to.be.true;
                });
                pm.test("cookie", function () {
                    pm.expect(pm.response.cookies[0].name).to.eql("session");
                });
                pm.test("common matchers", function () {
                    pm.response.to.be.ok;
                    pm.response.to.have.header("content-type");
                    pm.expect(pm.response.headers.get("content-type")).to.include("json");
                    pm.expect(pm.response.cookies.get("SESSION")).to.eql("abc");
                    pm.expect(pm.response.responseTime).to.be.below(10);
                    pm.expect(pm.response.status).to.match(/Created/);
                    pm.expect(pm.response.code).to.not.equal(200);
                    pm.expect(pm.response.json()).to.be.a("object");
                    pm.expect(pm.response.json()).to.have.property("ok", true);
                });
            "#,
            &request,
            Some(&response),
            &VariableContext::default(),
        )
        .expect("script");
        assert_eq!(result.tests.len(), 4);
        assert!(!result.tests[0].passed);
        assert!(result.tests[1].passed);
        assert!(result.tests[2].passed);
        assert!(result.tests[3].passed);
        assert_eq!(result.request["name"], "Scripted");
    }

    #[test]
    fn supports_iteration_data_request_headers_globals_and_unsets() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let mut request = Request::new("Scripted request", "GET", "https://example.test");
        request
            .headers
            .push(crate::model::HeaderEntry::enabled("X-Remove", "old"));
        request
            .headers
            .push(crate::model::HeaderEntry::enabled("X-Trace", "before"));
        let mut context = VariableContext::default();
        context
            .environment
            .insert("obsolete".to_owned(), "remove-me".to_owned());
        context
            .globals
            .insert("suite".to_owned(), "before".to_owned());
        context
            .iteration
            .insert("trace".to_owned(), "iteration-42".to_owned());

        let result = run_script(
            r#"
                pm.request.headers.upsert({ key: "X-Trace", value: pm.iterationData.get("trace") });
                pm.request.headers.add({ key: "X-Added", value: "yes" });
                pm.request.headers.remove("X-Remove");
                pm.environment.unset("obsolete");
                pm.globals.set("suite", "after");
                pm.test("iteration data is visible", function () {
                    pm.expect(pm.variables.get("trace")).to.eql("iteration-42");
                });
            "#,
            &request,
            None,
            &context,
        )
        .expect("script");

        result
            .apply(&mut request, &mut context)
            .expect("apply script changes");
        assert_eq!(
            request
                .headers
                .iter()
                .find(|header| header.key == "X-Trace")
                .map(|header| header.value.as_str()),
            Some("iteration-42")
        );
        assert!(request
            .headers
            .iter()
            .any(|header| { header.key == "X-Added" && header.value == "yes" }));
        assert!(!request
            .headers
            .iter()
            .any(|header| header.key == "X-Remove"));
        assert!(!context.environment.contains_key("obsolete"));
        assert_eq!(context.globals.get("suite"), Some(&"after".to_owned()));
        assert!(result.tests[0].passed);
    }

    #[test]
    fn rejects_oversized_scripts_before_starting_node() {
        let request = Request::new("Oversized", "GET", "https://example.test");
        let error = run_script(
            &"x".repeat(MAX_SCRIPT_BYTES + 1),
            &request,
            None,
            &VariableContext::default(),
        )
        .expect_err("oversized script should be rejected");
        assert!(matches!(error, ScriptError::TooLarge { .. }));
    }

    #[test]
    fn rejects_oversized_serialized_input_before_starting_node() {
        let mut request = Request::new("Oversized input", "POST", "https://example.test");
        request.body = crate::model::RequestBody::Raw {
            text: "x".repeat(MAX_SCRIPT_INPUT_BYTES),
            content_type: None,
        };
        let error = run_script(
            "pm.test('input is bounded', function () {});",
            &request,
            None,
            &VariableContext::default(),
        )
        .expect_err("oversized serialized input should be rejected");
        assert!(matches!(error, ScriptError::InputTooLarge { .. }));
    }

    #[test]
    fn bounds_script_logs_and_test_results() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let request = Request::new("Bounded output", "GET", "https://example.test");
        let result = run_script(
            r#"
                for (let index = 0; index < 250; index += 1) {
                    console.log("x".repeat(10000));
                }
                for (let index = 0; index < 1005; index += 1) {
                    pm.test("test " + index, function () {});
                }
            "#,
            &request,
            None,
            &VariableContext::default(),
        )
        .expect("bounded script");

        assert_eq!(result.logs.len(), MAX_LOG_ENTRIES);
        assert!(result
            .logs
            .iter()
            .all(|log| log.message.len() <= MAX_LOG_MESSAGE_BYTES));
        assert_eq!(result.tests.len(), MAX_TEST_ENTRIES);
        assert!(!result.tests.last().expect("test limit marker").passed);
    }

    #[test]
    fn rejects_oversized_child_output() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let request = Request::new("Oversized child output", "GET", "https://example.test");
        let error = run_script(
            &format!(
                "pm.request.headers.add({{ key: 'X-Large', value: 'x'.repeat({}) }});",
                MAX_SCRIPT_OUTPUT_BYTES
            ),
            &request,
            None,
            &VariableContext::default(),
        )
        .expect_err("oversized child output should be rejected");
        assert!(matches!(error, ScriptError::OutputTooLarge { .. }));
    }

    #[test]
    fn kills_a_long_lived_node_process() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let child = Command::new("node")
            .args(["-e", "setInterval(() => {}, 1000);"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("long-lived node process");
        let error = wait_for_child(child).expect_err("long-lived child should be terminated");
        assert!(matches!(error, ScriptError::Timeout { .. }));
    }
}
