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
    #[error(
        "script references unsupported host capability `{feature}`; Postly scripts expose only the local pm.* bridge"
    )]
    UnsupportedHostAccess { feature: String },
    #[error("script process exceeded the {timeout_seconds}-second execution limit")]
    Timeout { timeout_seconds: u64 },
    #[error("script execution cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScriptTestResult {
    pub name: String,
    pub passed: bool,
    /// Wall-clock duration of the test callback in milliseconds.
    /// Older serialized results may omit this field.
    #[serde(default)]
    pub duration_ms: u128,
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
    info: ScriptExecutionInfo,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScriptExecutionInfo {
    pub(crate) event_name: String,
    pub(crate) iteration: usize,
    pub(crate) iteration_count: usize,
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
    run_script_with_cancellation(script, request, response, context, || false)
}

/// Execute a script while allowing the caller to terminate its child process.
///
/// The closure is polled while the Node process is alive. This keeps the
/// regular `run_script` API unchanged for callers that do not need cancellation
/// while giving GUI and runner workers a real terminal cancellation path.
pub fn run_script_with_cancellation(
    script: &str,
    request: &Request,
    response: Option<&HttpResponse>,
    context: &VariableContext,
    is_cancelled: impl Fn() -> bool,
) -> Result<ScriptResult, ScriptError> {
    run_script_with_execution_info(
        script,
        request,
        response,
        context,
        ScriptExecutionInfo::default(),
        is_cancelled,
    )
}

pub(crate) fn run_script_with_cancellation_and_info(
    script: &str,
    request: &Request,
    response: Option<&HttpResponse>,
    context: &VariableContext,
    info: ScriptExecutionInfo,
    is_cancelled: impl Fn() -> bool,
) -> Result<ScriptResult, ScriptError> {
    run_script_with_execution_info(
        script,
        request,
        response,
        context,
        ScriptExecutionInfo {
            iteration_count: info.iteration_count.max(1),
            ..info
        },
        is_cancelled,
    )
}

fn run_script_with_execution_info(
    script: &str,
    request: &Request,
    response: Option<&HttpResponse>,
    context: &VariableContext,
    info: ScriptExecutionInfo,
    is_cancelled: impl Fn() -> bool,
) -> Result<ScriptResult, ScriptError> {
    if is_cancelled() {
        return Err(ScriptError::Cancelled);
    }
    if script.len() > MAX_SCRIPT_BYTES {
        return Err(ScriptError::TooLarge {
            maximum_bytes: MAX_SCRIPT_BYTES,
        });
    }
    validate_script_source(script)?;
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
        info,
    };
    let payload = serde_json::to_vec(&input)?;
    if payload.len() > MAX_SCRIPT_INPUT_BYTES {
        return Err(ScriptError::InputTooLarge {
            maximum_bytes: MAX_SCRIPT_INPUT_BYTES,
        });
    }
    let mut command = Command::new("node");
    command.env_clear().args(node_permission_flags()).args([
        "--input-type=commonjs",
        "-e",
        NODE_HARNESS,
    ]);
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
    let output = wait_for_child(child, is_cancelled)?;
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

const UNSUPPORTED_HOST_IDENTIFIERS: [(&str, &str); 13] = [
    ("require", "require"),
    ("process", "process"),
    ("globalThis", "globalThis"),
    ("global", "global"),
    ("module", "module"),
    ("__dirname", "__dirname"),
    ("__filename", "__filename"),
    ("Deno", "Deno"),
    ("Bun", "Bun"),
    ("Worker", "Worker"),
    ("WebAssembly", "WebAssembly"),
    ("eval", "eval"),
    ("Function", "Function"),
];

fn validate_script_source(script: &str) -> Result<(), ScriptError> {
    let bytes = script.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
                continue;
            }
            b'\'' | b'"' | b'`' => {
                let quote = bytes[index];
                index += 1;
                let mut escaped = false;
                while index < bytes.len() {
                    let byte = bytes[index];
                    index += 1;
                    if escaped {
                        escaped = false;
                    } else if byte == b'\\' {
                        escaped = true;
                    } else if byte == quote {
                        break;
                    }
                }
                continue;
            }
            _ => {}
        }

        for (identifier, feature) in UNSUPPORTED_HOST_IDENTIFIERS {
            let end = index.saturating_add(identifier.len());
            if end > bytes.len() || &bytes[index..end] != identifier.as_bytes() {
                continue;
            }
            let before_is_identifier = index > 0 && is_script_identifier_byte(bytes[index - 1]);
            let after_is_identifier = end < bytes.len() && is_script_identifier_byte(bytes[end]);
            if !before_is_identifier && !after_is_identifier {
                return Err(ScriptError::UnsupportedHostAccess {
                    feature: feature.to_owned(),
                });
            }
        }
        index += 1;
    }
    Ok(())
}

fn is_script_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')
}

fn node_permission_flags() -> Vec<&'static str> {
    let Ok(output) = Command::new("node").arg("--help").output() else {
        return Vec::new();
    };
    node_permission_flags_from_help(&String::from_utf8_lossy(&output.stdout))
}

fn node_permission_flags_from_help(help: &str) -> Vec<&'static str> {
    let has_permission = help.lines().any(|line| {
        let line = line.trim_start();
        line == "--permission" || line.starts_with("--permission ")
    });
    let has_network_permission = help.lines().any(|line| {
        let line = line.trim_start();
        line == "--allow-net" || line.starts_with("--allow-net ")
    });
    if has_permission && has_network_permission {
        vec!["--permission", "--allow-net"]
    } else {
        Vec::new()
    }
}

fn wait_for_child(
    mut child: Child,
    is_cancelled: impl Fn() -> bool,
) -> Result<Output, ScriptError> {
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
        if is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = join_pipe(stdout_reader);
            let _ = join_pipe(stderr_reader);
            return Err(ScriptError::Cancelled);
        }
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
const nodeCrypto = require("node:crypto");
const input = JSON.parse(fs.readFileSync(0, "utf8"));
async function main() {
const changes = { environment: {}, collection: {}, runtime: {} };
changes.globals = {};
const removals = { environment: [], collection: [], globals: [], runtime: [] };
const tests = [];
const logs = [];
const MAX_LOG_ENTRIES = 200;
const MAX_LOG_MESSAGE_BYTES = 4096;
const MAX_TEST_ENTRIES = 1000;
const MAX_SEND_REQUESTS = 8;
const MAX_SEND_RESPONSE_BYTES = 1024 * 1024;
const SEND_REQUEST_TIMEOUT_MS = 2000;
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

function rawLanguage(contentType) {
  const value = text(contentType).toLowerCase();
  if (value.includes("json")) return "json";
  if (value.includes("xml")) return "xml";
  if (value.includes("html")) return "html";
  if (value.includes("javascript")) return "javascript";
  return value ? value.split("/").pop().split(";")[0] : undefined;
}

function contentTypeForLanguage(language) {
  switch (text(language).toLowerCase()) {
    case "json": return "application/json";
    case "xml": return "application/xml";
    case "html": return "text/html";
    case "javascript": return "text/javascript";
    default: return text(language) ? "text/" + text(language) : undefined;
  }
}

function decorateKeyValueList(list, makeEntry, onChange = () => {}) {
  const keyOf = (entry) => text(entry && (entry.key || entry.name));
  list.get = (key) => {
    const normalized = text(key).toLowerCase();
    return list.find((entry) => keyOf(entry).toLowerCase() === normalized);
  };
  list.has = (key) => list.get(key) !== undefined;
  list.all = () => list.slice();
  list.count = () => list.length;
  list.each = (callback) => list.forEach(callback);
  list.toObject = () => list.reduce((object, entry) => {
    if (entry && entry.enabled !== false && entry.disabled !== true && keyOf(entry)) {
      object[keyOf(entry)] = text(entry.value);
    }
    return object;
  }, {});
  list.add = (entry) => {
    const normalized = makeEntry(entry);
    if (normalized && normalized.key) {
      list.push(normalized);
      onChange();
    }
  };
  list.remove = (key) => {
    const normalized = text(key).toLowerCase();
    let changed = false;
    for (let index = list.length - 1; index >= 0; index -= 1) {
      if (text(list[index] && list[index].key).toLowerCase() === normalized) {
        list.splice(index, 1);
        changed = true;
      }
    }
    if (changed) onChange();
  };
  list.clear = () => {
    if (list.length > 0) {
      list.splice(0, list.length);
      onChange();
    }
  };
  return list;
}

function authParameterValue(parameters, names) {
  const candidates = Array.isArray(names) ? names : [names];
  const found = parameters.find((parameter) => parameter
    && parameter.enabled !== false
    && candidates.some((name) => text(parameter.key).toLowerCase() === text(name).toLowerCase()));
  return found ? text(found.value) : undefined;
}

function nativeAuthToPostman(nativeAuth) {
  const source = nativeAuth && typeof nativeAuth === "object" ? nativeAuth : { type: "none" };
  let authType = source.type === "none" ? "noauth" : text(source.type).toLowerCase();
  const parameters = [];
  const add = (key, value) => parameters.push({ key, value: text(value), type: "string", enabled: true });
  switch (source.type) {
    case "basic":
      authType = "basic";
      add("username", source.username);
      add("password", source.password);
      break;
    case "digest":
      authType = "digest";
      add("username", source.username);
      add("password", source.password);
      break;
    case "bearer":
      authType = "bearer";
      add("token", source.token);
      break;
    case "api_key":
      authType = "apikey";
      add("key", source.key);
      add("value", source.value);
      add("in", source.location === "query" ? "query" : "header");
      break;
    case "oauth2_client_credentials":
      authType = "oauth2";
      add("grant_type", "client_credentials");
      add("accessTokenUrl", source.token_url);
      add("clientId", source.client_id);
      add("clientSecret", source.client_secret);
      if (source.scope) add("scope", source.scope);
      break;
    case "oauth2_authorization_code_pkce":
      authType = "oauth2";
      add("grant_type", "authorization_code");
      add("authorizationUrl", source.authorization_url);
      add("accessTokenUrl", source.token_url);
      add("clientId", source.client_id);
      add("redirectUri", source.redirect_uri);
      add("code", source.code);
      add("codeVerifier", source.code_verifier);
      if (source.client_secret) add("clientSecret", source.client_secret);
      if (source.scope) add("scope", source.scope);
      break;
    case "oauth2_refresh_token":
      authType = "oauth2";
      add("grant_type", "refresh_token");
      add("accessTokenUrl", source.token_url);
      add("clientId", source.client_id);
      add("refreshToken", source.refresh_token);
      if (source.client_secret) add("clientSecret", source.client_secret);
      if (source.scope) add("scope", source.scope);
      break;
    case "oauth2_device_code":
      authType = "oauth2";
      add("grant_type", "urn:ietf:params:oauth:grant-type:device_code");
      add("deviceAuthorizationUrl", source.device_authorization_url);
      add("accessTokenUrl", source.token_url);
      add("clientId", source.client_id);
      if (source.client_secret) add("clientSecret", source.client_secret);
      if (source.scope) add("scope", source.scope);
      break;
    default:
      break;
  }

  let dirty = false;
  const auth = { parameters };
  Object.defineProperty(auth, "type", {
    enumerable: true,
    get: () => authType,
    set: (value) => { authType = text(value).toLowerCase(); dirty = true; }
  });
  Object.defineProperty(auth, "_native", { value: source, enumerable: false });
  Object.defineProperty(auth, "_dirty", { get: () => dirty, enumerable: false });
  const markDirty = () => { dirty = true; };
  decorateKeyValueList(parameters, (entry) => ({
    key: text(entry && entry.key),
    value: text(entry && entry.value),
    type: text(entry && entry.type) || "string",
    enabled: entry && entry.disabled !== true && entry.enabled !== false
  }), markDirty);
  auth.get = (key) => authParameterValue(parameters, key);
  auth.has = (key) => auth.get(key) !== undefined;
  auth.upsert = (entry) => {
    const key = text(entry && entry.key).trim();
    if (!key) return;
    const found = parameters.find((parameter) => text(parameter.key).toLowerCase() === key.toLowerCase());
    if (found) {
      found.value = text(entry.value);
      found.type = text(entry.type) || found.type || "string";
      found.enabled = entry.disabled !== true && entry.enabled !== false;
      markDirty();
    } else {
      parameters.add(entry);
    }
  };
  auth.remove = (key) => {
    const normalized = text(key).toLowerCase();
    for (let index = parameters.length - 1; index >= 0; index -= 1) {
      if (text(parameters[index].key).toLowerCase() === normalized) {
        parameters.splice(index, 1);
        markDirty();
      }
    }
  };
  auth.clear = () => {
    if (parameters.length > 0) {
      parameters.splice(0, parameters.length);
      markDirty();
    }
  };
  auth.each = (callback) => parameters.forEach(callback);
  auth.forEach = auth.each;
  return auth;
}

function postmanAuthToNative(auth) {
  const type = text(auth && auth.type).toLowerCase();
  const parameters = auth && Array.isArray(auth.parameters) ? auth.parameters : [];
  const value = (names) => authParameterValue(parameters, names);
  if (type === "noauth" || type === "none" || !type) return { type: "none" };
  if (type === "basic") {
    return {
      type: "basic",
      username: value("username") || "",
      password: value("password") || ""
    };
  }
  if (type === "digest") {
    return {
      type: "digest",
      username: value("username") || "",
      password: value("password") || ""
    };
  }
  if (type === "bearer") return { type: "bearer", token: value(["token", "value"]) || "" };
  if (type === "apikey" || type === "api_key") {
    return {
      type: "api_key",
      key: value("key") || "",
      value: value("value") || "",
      location: (value("in") || "header").toLowerCase() === "query" ? "query" : "header"
    };
  }

  const native = auth && auth._native && typeof auth._native === "object"
    ? JSON.parse(JSON.stringify(auth._native))
    : { type: "none" };
  const set = (key, names, optional = false) => {
    const next = value(names);
    if (next !== undefined || !optional) native[key] = next || "";
  };
  if (type === "oauth2" && native.type === "oauth2_client_credentials") {
    set("token_url", ["token_url", "accessTokenUrl", "access_token_url"]);
    set("client_id", ["client_id", "clientId"]);
    set("client_secret", ["client_secret", "clientSecret"]);
    set("scope", "scope", true);
  } else if (type === "oauth2" && native.type === "oauth2_authorization_code_pkce") {
    set("authorization_url", ["authorization_url", "authorizationUrl"]);
    set("token_url", ["token_url", "accessTokenUrl", "access_token_url"]);
    set("client_id", ["client_id", "clientId"]);
    set("redirect_uri", ["redirect_uri", "redirectUri"]);
    set("code", "code");
    set("code_verifier", ["code_verifier", "codeVerifier"]);
    set("client_secret", ["client_secret", "clientSecret"], true);
    set("scope", "scope", true);
  } else if (type === "oauth2" && native.type === "oauth2_refresh_token") {
    set("token_url", ["token_url", "accessTokenUrl", "access_token_url"]);
    set("client_id", ["client_id", "clientId"]);
    set("refresh_token", ["refresh_token", "refreshToken"]);
    set("client_secret", ["client_secret", "clientSecret"], true);
    set("scope", "scope", true);
  } else if (type === "oauth2" && native.type === "oauth2_device_code") {
    set("device_authorization_url", ["device_authorization_url", "deviceAuthorizationUrl"]);
    set("token_url", ["token_url", "accessTokenUrl", "access_token_url"]);
    set("client_id", ["client_id", "clientId"]);
    set("client_secret", ["client_secret", "clientSecret"], true);
    set("scope", "scope", true);
  }
  return native;
}

function nativeBodyToPostman(nativeBody) {
  if (!nativeBody || nativeBody.type === "none") return null;
  switch (nativeBody.type) {
    case "raw": {
      const body = { mode: "raw", raw: text(nativeBody.text) };
      const language = rawLanguage(nativeBody.content_type);
      if (language) body.options = { raw: { language } };
      return body;
    }
    case "json":
      return {
        mode: "raw",
        raw: JSON.stringify(nativeBody.value),
        options: { raw: { language: "json" } }
      };
    case "graphql":
      return {
        mode: "graphql",
        graphql: {
          query: text(nativeBody.query),
          variables: JSON.stringify(nativeBody.variables || {}),
          ...(nativeBody.operation_name ? { operationName: nativeBody.operation_name } : {})
        }
      };
    case "form_url_encoded":
      return {
        mode: "urlencoded",
        urlencoded: decorateKeyValueList((nativeBody.fields || []).map((field) => ({
          key: text(field.key), value: text(field.value), disabled: field.enabled === false
        })), (entry) => ({
          key: text(entry && entry.key), value: text(entry && entry.value), disabled: entry && entry.disabled === true
        }))
      };
    case "multipart":
      return {
        mode: "formdata",
        formdata: decorateKeyValueList((nativeBody.parts || []).map((part) => ({
          key: text(part.name),
          value: text(part.value),
          src: part.file_path || undefined,
          contentType: part.content_type || undefined,
          disabled: part.enabled === false
        })), (entry) => ({
          key: text(entry && (entry.key || entry.name)),
          value: text(entry && entry.value),
          src: entry && entry.src ? text(entry.src) : undefined,
          contentType: entry && (entry.contentType || entry.content_type) ? text(entry.contentType || entry.content_type) : undefined,
          disabled: entry && entry.disabled === true
        }))
      };
    case "binary_file":
      return {
        mode: "file",
        file: { src: text(nativeBody.path) },
        ...(nativeBody.content_type ? { contentType: nativeBody.content_type } : {})
      };
    default:
      return null;
  }
}

function postmanBodyToNative(body) {
  if (!body || typeof body !== "object") return { type: "none" };
  const mode = text(body.mode).toLowerCase();
  if (mode === "none") return { type: "none" };
  if (mode === "raw") {
    const raw = text(body.raw);
    const language = body.options && body.options.raw && body.options.raw.language;
    if (text(language).toLowerCase() === "json") {
      try { return { type: "json", value: JSON.parse(raw) }; } catch (_) {}
    }
    return { type: "raw", text: raw, content_type: contentTypeForLanguage(language) };
  }
  if (mode === "graphql") {
    const graphql = body.graphql || {};
    let variables = graphql.variables;
    if (typeof variables === "string") {
      try { variables = JSON.parse(variables); } catch (_) { variables = {}; }
    }
    return {
      type: "graphql",
      query: text(graphql.query),
      variables: variables && typeof variables === "object" ? variables : {},
      ...(graphql.operationName || graphql.operation_name ? { operation_name: text(graphql.operationName || graphql.operation_name) } : {})
    };
  }
  if (mode === "urlencoded") {
    return {
      type: "form_url_encoded",
      fields: (Array.isArray(body.urlencoded) ? body.urlencoded : []).map((field) => ({
        key: text(field && field.key), value: text(field && field.value), enabled: field && field.disabled !== true
      }))
    };
  }
  if (mode === "formdata") {
    return {
      type: "multipart",
      parts: (Array.isArray(body.formdata) ? body.formdata : []).map((part) => ({
        name: text(part && (part.key || part.name)),
        value: text(part && part.value),
        file_path: part && part.src ? text(part.src) : null,
        content_type: part && (part.contentType || part.content_type) ? text(part.contentType || part.content_type) : null,
        enabled: part && part.disabled !== true
      }))
    };
  }
  if (mode === "file") {
    const file = body.file || {};
    return {
      type: "binary_file",
      path: text(file.src),
      ...(body.contentType ? { content_type: text(body.contentType) } : { content_type: null })
    };
  }
  return { type: "raw", text: JSON.stringify(body), content_type: "application/json" };
}

function bodyUpdateValue(next) {
  if (typeof next === "string") return { mode: "raw", raw: next };
  if (!next || typeof next !== "object" || Array.isArray(next)) {
    throw new Error("pm.request.body.update expects a Postman body object or raw string");
  }
  try {
    return JSON.parse(JSON.stringify(next));
  } catch (_) {
    throw new Error("pm.request.body.update received a non-serializable body");
  }
}

function decoratePostmanBody(body) {
  const decorateCollections = () => {
    if (Array.isArray(body.urlencoded)) {
      body.urlencoded = decorateKeyValueList(body.urlencoded.map((field) => ({
        key: text(field && (field.key || field.name)),
        value: text(field && field.value),
        disabled: field && field.disabled === true
      })), (entry) => ({
        key: text(entry && (entry.key || entry.name)),
        value: text(entry && entry.value),
        disabled: entry && entry.disabled === true
      }));
    }
    if (Array.isArray(body.formdata)) {
      body.formdata = decorateKeyValueList(body.formdata.map((part) => ({
        key: text(part && (part.key || part.name)),
        value: text(part && part.value),
        src: part && part.src ? text(part.src) : undefined,
        contentType: part && (part.contentType || part.content_type)
          ? text(part.contentType || part.content_type)
          : undefined,
        disabled: part && part.disabled === true
      })), (entry) => ({
        key: text(entry && (entry.key || entry.name)),
        value: text(entry && entry.value),
        src: entry && entry.src ? text(entry.src) : undefined,
        contentType: entry && (entry.contentType || entry.content_type)
          ? text(entry.contentType || entry.content_type)
          : undefined,
        disabled: entry && entry.disabled === true
      }));
    }
  };

  Object.defineProperty(body, "update", {
    enumerable: false,
    configurable: true,
    value: (next) => {
      const replacement = bodyUpdateValue(next);
      Object.keys(body).forEach((key) => delete body[key]);
      Object.assign(body, replacement);
      decorateCollections();
    }
  });
  decorateCollections();
  return body;
}

function makeUrlFacade(rawValue, structuredQuery) {
  const raw = text(rawValue);
  const variablePattern = /\{\{\s*([^}]+?)\s*\}\}/g;
  const variableNames = [];
  const seenVariables = new Set();
  let variableMatch;
  while ((variableMatch = variablePattern.exec(raw)) !== null) {
    const key = text(variableMatch[1]).trim();
    if (key && !seenVariables.has(key)) {
      seenVariables.add(key);
      variableNames.push(key);
    }
  }
  let variableDirty = false;
  const variables = decorateKeyValueList(variableNames.map((key) => ({
    key,
    value: text(visibleGet(key)),
    enabled: true
  })), (entry) => ({
    key: text(entry && (entry.key || entry.name)),
    value: text(entry && entry.value),
    enabled: entry && entry.disabled !== true && entry.enabled !== false
  }), () => { variableDirty = true; });
  const initialVariableState = variables.map((entry) => ({
    key: text(entry.key), value: text(entry.value), enabled: entry.enabled !== false
  }));
  const hasVariableChanges = () => variableDirty
    || variables.length !== initialVariableState.length
    || variables.some((entry, index) => {
      const initial = initialVariableState[index];
      return !initial
        || text(entry && entry.key) !== initial.key
        || text(entry && entry.value) !== initial.value
        || (entry && entry.enabled !== false) !== initial.enabled;
    });
  const variableEntry = (key) => variables.find((entry) =>
    entry && text(entry.key).toLowerCase() === text(key).toLowerCase());
  variables.upsert = (entry) => {
    const key = text(entry && (entry.key || entry.name)).trim();
    if (!key) return;
    const found = variableEntry(key);
    if (found) {
      found.value = text(entry.value);
      found.enabled = entry.disabled !== true && entry.enabled !== false;
      variableDirty = true;
    } else {
      variables.add(entry);
    }
  };
  variables.replace = (key, value) => variables.upsert({ key, value });
  const url = { raw, query: [], variables };
  const usesStructuredQuery = Array.isArray(structuredQuery) && structuredQuery.length > 0;
  let queryDirty = false;
  const resolvedUrl = () => text(url.raw).replace(variablePattern, (_, key) => {
    const found = variableEntry(key.trim());
    if (found && found.enabled !== false && found.disabled !== true) return text(found.value);
    return replaceIn("{{" + key + "}}");
  });
  const parsedUrl = () => {
    try { return new URL(resolvedUrl()); } catch (_) { return null; }
  };
  const refreshQuery = () => {
    if (usesStructuredQuery && structuredQuery.length > 0) {
      structuredQuery.forEach((entry) => url.query.push({
        key: text(entry && entry.key),
        value: text(entry && entry.value),
        disabled: entry && entry.enabled === false
      }));
      return;
    }
    const parsed = parsedUrl();
    if (!parsed || url.query.length > 0) return;
    parsed.searchParams.forEach((value, key) => url.query.push({ key, value, disabled: false }));
  };
  const serializeQuery = () => {
    const parsed = parsedUrl();
    const query = url.query
      .filter((entry) => entry && entry.disabled !== true && text(entry.key))
      .map((entry) => encodeURIComponent(text(entry.key)) + "=" + encodeURIComponent(text(entry.value)))
      .join("&");
    if (parsed) {
      parsed.search = query ? "?" + query : "";
      return parsed.toString();
    }
    const source = text(url.raw).split("#")[0].split("?")[0];
    return source + (query ? "?" + query : "");
  };
  const query = decorateKeyValueList(url.query, (entry) => ({
    key: text(entry && entry.key), value: text(entry && entry.value), disabled: entry && entry.disabled === true
  }), () => { queryDirty = true; });
  query.upsert = (entry) => {
    const normalized = text(entry && entry.key).toLowerCase();
    const found = query.find((candidate) => text(candidate && candidate.key).toLowerCase() === normalized);
    if (found) {
      found.value = text(entry.value);
      found.disabled = entry.disabled === true;
      queryDirty = true;
    } else {
      query.add(entry);
    }
  };
  const queryParamEntries = (params) => {
    if (Array.isArray(params)) return params;
    if (params && typeof params === "object") {
      return Object.entries(params).map(([key, value]) => ({ key, value }));
    }
    return [];
  };
  const queryParamKey = (entry) => text(entry && typeof entry === "object" ? entry.key : entry);
  refreshQuery();
  url.query = query;
  Object.defineProperties(url, {
    addQueryParams: { value: (params) => queryParamEntries(params).forEach((entry) => query.add(entry)) },
    removeQueryParams: { value: (params) => {
      const entries = Array.isArray(params) ? params : [params];
      entries.forEach((entry) => query.remove(queryParamKey(entry)));
    }},
    getQueryParams: { value: () => query },
    toString: { value: () => (usesStructuredQuery || queryDirty || hasVariableChanges()) ? serializeQuery() : text(url.raw) },
    toObject: { value: () => {
      const parsed = parsedUrl();
      const object = {
        raw: url.toString(),
        protocol: parsed ? parsed.protocol.replace(/:$/, "") : undefined,
        host: parsed ? parsed.hostname.split(".").filter(Boolean) : [],
        path: parsed ? parsed.pathname.split("/").filter(Boolean) : [],
        query: query.map((entry) => ({
          key: text(entry && entry.key),
          value: text(entry && entry.value),
          disabled: entry && entry.disabled === true
        })),
        variable: variables.toObject()
      };
      if (parsed && parsed.port) object.port = parsed.port;
      if (parsed && parsed.hash) object.hash = parsed.hash.replace(/^#/, "");
      return object;
    }},
    getPath: { value: () => { const parsed = parsedUrl(); return parsed ? parsed.pathname : text(url.raw).split("?")[0]; } },
    getHost: { value: () => { const parsed = parsedUrl(); return parsed ? parsed.hostname : undefined; } },
    getProtocol: { value: () => { const parsed = parsedUrl(); return parsed ? parsed.protocol.replace(/:$/, "") : undefined; } },
    getQueryString: { value: () => {
      if (url.query.length > 0) {
        const serialized = serializeQuery();
        return serialized.split("?")[1]?.split("#")[0] || "";
      }
      const parsed = parsedUrl();
      return parsed ? parsed.search.replace(/^\?/, "") : text(url.raw).split("?")[1] || "";
    } },
    host: { get: () => { const parsed = parsedUrl(); return parsed ? parsed.hostname : undefined; } },
    protocol: { get: () => { const parsed = parsedUrl(); return parsed ? parsed.protocol.replace(/:$/, "") : undefined; } },
    port: { get: () => { const parsed = parsedUrl(); return parsed ? parsed.port : undefined; } },
    hash: { get: () => { const parsed = parsedUrl(); return parsed ? parsed.hash : undefined; } },
    path: { get: () => { const parsed = parsedUrl(); return parsed ? parsed.pathname.split("/").filter(Boolean) : []; } },
    variable: { get: () => url.variables },
    _usesStructuredQuery: { value: usesStructuredQuery, enumerable: false },
    _queryDirty: { get: () => queryDirty, enumerable: false },
    _variableDirty: { get: () => hasVariableChanges(), enumerable: false }
  });
  return url;
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
  toObject: () => ({ ...(values.iteration || {}) }),
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

const request = { ...(input.request || {}) };
request.url = makeUrlFacade(request.url, request.query);
request.auth = nativeAuthToPostman(request.auth);
request.body = decoratePostmanBody(nativeBodyToPostman(request.body) || { mode: "none" });
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
requestHeaders.clear = () => { requestHeaders.splice(0, requestHeaders.length); };
requestHeaders.all = () => requestHeaders.slice();
requestHeaders.count = () => requestHeaders.length;
requestHeaders.each = (callback) => requestHeaders.forEach(callback);
requestHeaders.toObject = () => requestHeaders.reduce((object, header) => {
  if (header && header.key && header.enabled !== false) object[header.key] = text(header.value);
  return object;
}, {});
request.headers = requestHeaders;
request.cookies = decorateKeyValueList(request.cookies || [], (cookie) => ({
  key: text(cookie && (cookie.key || cookie.name)),
  value: text(cookie && cookie.value),
  enabled: cookie && cookie.disabled !== true && cookie.enabled !== false
}));
const pmCookies = makeRequestCookieSnapshot(request.cookies);
const pmInfo = Object.freeze({
  requestName: text(request.name),
  requestId: text(request.id),
  eventName: text(input.info && input.info.eventName),
  iteration: Number.isInteger(input.info && input.info.iteration) ? input.info.iteration : 0,
  iterationCount: Number.isInteger(input.info && input.info.iterationCount) && input.info.iterationCount > 0
    ? input.info.iterationCount
    : 1
});

function serializeRequest() {
  const serialized = { ...request };
  if (request.url && typeof request.url === "object" && Array.isArray(request.url.query)
      && (request.url._usesStructuredQuery || request.url._queryDirty)) {
    const rawUrl = request.url._variableDirty ? request.url.toString() : text(request.url.raw);
    const fragmentIndex = rawUrl.indexOf("#");
    const fragment = fragmentIndex >= 0 ? rawUrl.slice(fragmentIndex) : "";
    serialized.url = rawUrl.split("?")[0] + fragment;
    serialized.query = request.url.query.map((entry) => ({
      key: text(entry && entry.key),
      value: text(entry && entry.value),
      enabled: entry && entry.disabled !== true
    }));
  } else {
    serialized.url = typeof request.url === "string"
      ? request.url
      : request.url && typeof request.url.toString === "function"
        ? request.url.toString()
        : text(request.url);
  }
  serialized.auth = postmanAuthToNative(request.auth);
  serialized.body = postmanBodyToNative(request.body);
  serialized.headers = Array.from(request.headers || []).map((header) => ({
    key: text(header && header.key),
    value: text(header && header.value),
    enabled: header && header.disabled !== true && header.enabled !== false
  }));
  serialized.cookies = Array.from(request.cookies || []).map((cookie) => ({
    key: text(cookie && (cookie.key || cookie.name)),
    value: text(cookie && cookie.value),
    enabled: cookie && cookie.disabled !== true && cookie.enabled !== false
  }));
  return serialized;
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function deepEqual(left, right) {
  if (left === right) return true;
  if (left === null || right === null || typeof left !== typeof right) return false;
  if (Array.isArray(left) || Array.isArray(right)) {
    return Array.isArray(left) && Array.isArray(right)
      && left.length === right.length
      && left.every((item, index) => deepEqual(item, right[index]));
  }
  if (typeof left !== "object") return false;
  const leftKeys = Object.keys(left).sort();
  const rightKeys = Object.keys(right).sort();
  return deepEqual(leftKeys, rightKeys)
    && leftKeys.every((key) => deepEqual(left[key], right[key]));
}

function deepIncludes(actual, expected) {
  if (deepEqual(actual, expected)) return true;
  if (Array.isArray(actual)) return actual.some((item) => deepIncludes(item, expected));
  if (actual === null || expected === null || typeof actual !== "object" || typeof expected !== "object") return false;
  return Object.keys(expected).every((key) =>
    Object.prototype.hasOwnProperty.call(actual, key) && deepIncludes(actual[key], expected[key])
  );
}

function membersInclude(actual, expected) {
  if (!Array.isArray(actual) || !Array.isArray(expected)) return false;
  const remaining = actual.slice();
  return expected.every((candidate) => {
    const index = remaining.findIndex((item) => deepEqual(item, candidate));
    if (index < 0) return false;
    remaining.splice(index, 1);
    return true;
  });
}

function membersEqual(actual, expected) {
  return Array.isArray(actual) && Array.isArray(expected)
    && actual.length === expected.length
    && membersInclude(actual, expected);
}

function typeMatches(value, type) {
  const expected = text(type).toLowerCase();
  if (expected === "array") return Array.isArray(value);
  if (expected === "object") return value !== null && typeof value === "object" && !Array.isArray(value);
  if (expected === "null") return value === null;
  return typeof value === expected;
}

function expect(value) {
  function expectation(negated) {
    const prefix = negated ? " not" : "";
    const check = (condition, message) => assert(negated ? !condition : condition, message);
    const lengthOf = (expected) => check(
      value !== null && value !== undefined && value.length === expected,
      "expected " + JSON.stringify(value) + " to" + prefix + " have length " + expected
    );
    const oneOf = (expected) => check(
      Array.isArray(expected) && expected.some((candidate) => deepEqual(value, candidate)),
      "expected " + JSON.stringify(value) + " to" + prefix + " be one of " + JSON.stringify(expected)
    );
    const keys = function (...expected) {
      const expectedKeys = expected.length === 1 && Array.isArray(expected[0]) ? expected[0] : expected;
      const actualKeys = value !== null && value !== undefined && typeof value === "object"
        ? Object.keys(value).sort()
        : [];
      check(
        deepEqual(actualKeys, expectedKeys.map(text).sort()),
        "expected " + JSON.stringify(value) + " to" + prefix + " have keys " + JSON.stringify(expectedKeys)
      );
    };
    const anyKeys = function (...expected) {
      const expectedKeys = expected.length === 1 && Array.isArray(expected[0]) ? expected[0] : expected;
      const actualKeys = value !== null && value !== undefined && typeof value === "object"
        ? Object.keys(value)
        : [];
      check(
        expectedKeys.some((key) => actualKeys.includes(text(key))),
        "expected " + JSON.stringify(value) + " to" + prefix + " have any of keys " + JSON.stringify(expectedKeys)
      );
    };
    const typeCheck = (type) => check(
      typeMatches(value, type),
      "expected value to" + prefix + " be a " + type
    );
    const closeTo = (expected, delta) => check(
      typeof value === "number" && Number.isFinite(value)
        && typeof expected === "number" && Number.isFinite(expected)
        && typeof delta === "number" && Number.isFinite(delta) && delta >= 0
        && Math.abs(value - expected) <= delta,
      "expected " + JSON.stringify(value) + " to" + prefix + " be close to " + expected + " +/- " + delta
    );
    const satisfy = (predicate) => check(
      typeof predicate === "function" && Boolean(predicate(value)),
      "expected " + JSON.stringify(value) + " to" + prefix + " satisfy the predicate"
    );
    const include = (expected) => {
      const included = typeof value === "string"
        ? value.includes(expected)
        : deepIncludes(value, expected);
      check(included, "expected " + JSON.stringify(value) + " to" + prefix + " include " + JSON.stringify(expected));
    };
    include.members = (expected) => check(
      membersInclude(value, expected),
      "expected " + JSON.stringify(value) + " to" + prefix + " include members " + JSON.stringify(expected)
    );
    include.keys = function (...expected) {
      const expectedKeys = expected.length === 1 && Array.isArray(expected[0]) ? expected[0] : expected;
      const actualKeys = value !== null && value !== undefined && typeof value === "object"
        ? Object.keys(value)
        : [];
      check(
        expectedKeys.every((key) => actualKeys.includes(text(key))),
        "expected " + JSON.stringify(value) + " to" + prefix + " include keys " + JSON.stringify(expectedKeys)
      );
    };
    const to = {
      equal: (expected) => check(value === expected, "expected " + JSON.stringify(value) + " to" + prefix + " equal " + JSON.stringify(expected)),
      equals: (expected) => check(value === expected, "expected " + JSON.stringify(value) + " to" + prefix + " equal " + JSON.stringify(expected)),
      eql: (expected) => check(deepEqual(value, expected), "expected " + JSON.stringify(value) + " to" + prefix + " deeply equal " + JSON.stringify(expected)),
      include,
      contain: include,
      closeTo,
      satisfy,
      match: (pattern) => check(typeof value === "string" && pattern.test(value), "expected " + JSON.stringify(value) + " to" + prefix + " match the pattern"),
      have: {
        property: function (name, expected) {
          const present = value !== null && value !== undefined && Object.prototype.hasOwnProperty.call(value, name);
          check(present, "expected property " + name);
          if (arguments.length > 1 && present) check(deepEqual(value[name], expected), "expected property " + name + " to" + prefix + " equal " + JSON.stringify(expected));
          const chained = expect(present ? value[name] : undefined).to;
          return chained;
        },
        deep: {
          property: function (name, expected) {
            const result = readJsonPath(value, name);
            check(result.found, "expected nested property " + name);
            if (arguments.length > 1 && result.found) {
              check(
                deepEqual(result.value, expected),
                "expected nested property " + name + " to" + prefix + " deeply equal " + JSON.stringify(expected)
              );
            }
            return expect(result.found ? result.value : undefined).to;
          },
          include: (expected) => check(
            deepIncludes(value, expected),
            "expected " + JSON.stringify(value) + " to" + prefix + " deeply include " + JSON.stringify(expected)
          ),
          members: (expected) => check(
            membersEqual(value, expected),
            "expected " + JSON.stringify(value) + " to" + prefix + " deeply have members " + JSON.stringify(expected)
          )
        },
        nested: {
          property: function (name, expected) {
            const result = readJsonPath(value, name);
            check(result.found, "expected nested property " + name);
            if (arguments.length > 1 && result.found) check(deepEqual(result.value, expected), "expected nested property " + name + " to" + prefix + " equal " + JSON.stringify(expected));
          }
        },
        lengthOf,
        members: (expected) => check(
          membersEqual(value, expected),
          "expected " + JSON.stringify(value) + " to" + prefix + " have members " + JSON.stringify(expected)
        ),
        keys,
        all: { keys },
        any: { keys: anyKeys }
      }
    };
    Object.defineProperties(to, {
      that: { get: () => to },
      is: { get: () => to },
      a: { value: typeCheck },
      an: { value: typeCheck }
    });
    Object.defineProperty(to, "deep", {
      get: () => ({
        equal: (expected) => check(
          deepEqual(value, expected),
          "expected " + JSON.stringify(value) + " to" + prefix + " deeply equal " + JSON.stringify(expected)
        ),
        include: (expected) => check(
          deepIncludes(value, expected),
          "expected " + JSON.stringify(value) + " to" + prefix + " deeply include " + JSON.stringify(expected)
        ),
        members: (expected) => check(
          membersEqual(value, expected),
          "expected " + JSON.stringify(value) + " to" + prefix + " deeply have members " + JSON.stringify(expected)
        )
      })
    });
    Object.defineProperty(to, "be", {
      value: {
        get true() { check(value === true, "expected " + JSON.stringify(value) + " to" + prefix + " be true"); return true; },
        get false() { check(value === false, "expected " + JSON.stringify(value) + " to" + prefix + " be false"); return true; },
        get null() { check(value === null, "expected " + JSON.stringify(value) + " to" + prefix + " be null"); return true; },
        get undefined() { check(value === undefined, "expected " + JSON.stringify(value) + " to" + prefix + " be undefined"); return true; },
        get exist() { check(value !== null && value !== undefined, "expected value to" + prefix + " exist"); return true; },
        get ok() { check(Boolean(value), "expected " + JSON.stringify(value) + " to" + prefix + " be truthy"); return true; },
        above: (expected) => check(value > expected, "expected " + JSON.stringify(value) + " to" + prefix + " be above " + expected),
        below: (expected) => check(value < expected, "expected " + JSON.stringify(value) + " to" + prefix + " be below " + expected),
        greaterThan: (expected) => check(value > expected, "expected " + JSON.stringify(value) + " to" + prefix + " be greater than " + expected),
        lessThan: (expected) => check(value < expected, "expected " + JSON.stringify(value) + " to" + prefix + " be less than " + expected),
        closeTo,
        at: {
          least: (expected) => check(value >= expected, "expected " + JSON.stringify(value) + " to" + prefix + " be at least " + expected),
          most: (expected) => check(value <= expected, "expected " + JSON.stringify(value) + " to" + prefix + " be at most " + expected)
        },
        within: (minimum, maximum) => check(value >= minimum && value <= maximum, "expected " + JSON.stringify(value) + " to" + prefix + " be within " + minimum + ".." + maximum),
        oneOf,
        get empty() {
          const length = value !== null && value !== undefined && value.length;
          const empty = length !== undefined
            ? length === 0
            : value !== null && value !== undefined && typeof value === "object" && Object.keys(value).length === 0;
          check(empty, "expected " + JSON.stringify(value) + " to" + prefix + " be empty");
          return true;
        },
        a: typeCheck,
        an: typeCheck
      }
    });
    Object.defineProperty(to, "not", { get: () => expectation(!negated) });
    return to;
  }
  return { to: expectation(false) };
}

function decorateHeaders(responseHeaders) {
  responseHeaders.get = (name) => {
    const found = responseHeaders.find((header) => text(header && header.key).toLowerCase() === text(name).toLowerCase() && header.enabled !== false);
    return found ? found.value : undefined;
  };
  responseHeaders.has = (name) => responseHeaders.get(name) !== undefined;
  responseHeaders.toObject = () => responseHeaders.reduce((object, header) => {
    if (header && header.key && header.enabled !== false) object[header.key] = text(header.value);
    return object;
  }, {});
  responseHeaders.all = () => responseHeaders.slice();
  responseHeaders.count = () => responseHeaders.length;
  responseHeaders.each = (callback) => responseHeaders.forEach(callback);
  return responseHeaders;
}

function decorateCookies(responseCookies) {
  responseCookies.get = (name) => {
    const found = responseCookies.find((cookie) => text(cookie && cookie.name).toLowerCase() === text(name).toLowerCase());
    return found ? found.value : undefined;
  };
  responseCookies.has = (name) => responseCookies.get(name) !== undefined;
  responseCookies.toObject = () => responseCookies.reduce((object, cookie) => {
    if (cookie && cookie.name) object[cookie.name] = text(cookie.value);
    return object;
  }, {});
  responseCookies.all = () => responseCookies.slice();
  responseCookies.count = () => responseCookies.length;
  responseCookies.each = (callback) => responseCookies.forEach(callback);
  responseCookies.forEach = (callback) => Array.prototype.forEach.call(responseCookies, callback);
  return responseCookies;
}

function splitSetCookieHeader(value) {
  return text(value).split(/,\s*(?=[^;,=\s]+=[^;,]*)/);
}

function parseSetCookie(value) {
  const parts = text(value).split(";").map((part) => part.trim());
  const pair = parts.shift() || "";
  const separator = pair.indexOf("=");
  if (separator <= 0) return null;
  const cookie = {
    name: pair.slice(0, separator).trim(),
    value: pair.slice(separator + 1),
    domain: undefined,
    path: undefined,
    secure: false,
    httpOnly: false,
    sameSite: undefined,
    expires: undefined,
    maxAge: undefined
  };
  if (!cookie.name) return null;
  parts.forEach((attribute) => {
    const separator = attribute.indexOf("=");
    const key = (separator >= 0 ? attribute.slice(0, separator) : attribute).trim().toLowerCase();
    const attributeValue = separator >= 0 ? attribute.slice(separator + 1).trim() : "";
    if (key === "domain") cookie.domain = attributeValue;
    else if (key === "path") cookie.path = attributeValue;
    else if (key === "secure") cookie.secure = true;
    else if (key === "httponly") cookie.httpOnly = true;
    else if (key === "samesite") cookie.sameSite = attributeValue;
    else if (key === "expires") cookie.expires = attributeValue;
    else if (key === "max-age" && /^-?\d+$/.test(attributeValue)) cookie.maxAge = Number(attributeValue);
  });
  return cookie;
}

function responseCookiesFromHeaders(headers) {
  let values = [];
  if (headers && typeof headers.getSetCookie === "function") {
    values = headers.getSetCookie();
  } else if (headers && typeof headers.get === "function") {
    values = splitSetCookieHeader(headers.get("set-cookie"));
  }
  return values.map(parseSetCookie).filter(Boolean);
}

function makeRequestCookieSnapshot(requestCookies) {
  const cookies = (Array.isArray(requestCookies) ? requestCookies : [])
    .filter((cookie) => cookie && cookie.enabled !== false && cookie.disabled !== true)
    .map((cookie) => ({
      name: text(cookie.key || cookie.name),
      value: text(cookie.value)
    }))
    .filter((cookie) => cookie.name);
  return Object.freeze(decorateCookies(cookies));
}

function readJsonPath(value, path) {
  if (path === undefined || path === null || text(path) === "") return { found: true, value };
  const segments = text(path)
    .replace(/\[(\w+)\]/g, ".$1")
    .split(".")
    .filter(Boolean);
  let current = value;
  for (const segment of segments) {
    if (current === null || current === undefined || !Object.prototype.hasOwnProperty.call(current, segment)) {
      return { found: false, value: undefined };
    }
    current = current[segment];
  }
  return { found: true, value: current };
}

function makeScriptResponse(responseData) {
  const responseHeaders = responseData.headers || [];
  decorateHeaders(responseHeaders);
  const responseCookies = responseData.cookies || [];
  decorateCookies(responseCookies);
  const responseCategories = {
    ok: () => responseData.status >= 200 && responseData.status < 400,
    success: () => responseData.status >= 200 && responseData.status < 300,
    redirection: () => responseData.status >= 300 && responseData.status < 400,
    clientError: () => responseData.status >= 400 && responseData.status < 500,
    serverError: () => responseData.status >= 500 && responseData.status < 600,
    error: () => responseData.status >= 400,
    withBody: () => responseData.body_text.length > 0,
    json: () => {
      try { JSON.parse(responseData.body_text); return true; } catch (_) { return false; }
    }
  };
  const makeResponseCategories = (negated) => {
    const categories = {};
    Object.entries(responseCategories).forEach(([name, predicate]) => {
      Object.defineProperty(categories, name, {
        get: () => {
          const matches = predicate();
          assert(
            negated ? !matches : matches,
            "expected response to" + (negated ? " not" : "") + " be " + name
          );
          return true;
        }
      });
    });
    return categories;
  };
  const responseTo = {
    have: {
      body: {
        get: () => {
          assert(responseData.body_text.length > 0, "expected response to have a body");
          return true;
        }
      },
      status: (expected) => assert(responseData.status === expected, "expected status " + responseData.status + " to equal " + expected),
      header: function (name, expected) {
        const actual = responseHeaders.get(name);
        assert(actual !== undefined, "expected response header " + name);
        if (arguments.length > 1) {
          const matches = expected && typeof expected.test === "function"
            ? expected.test(actual)
            : actual === text(expected);
          assert(matches, "expected response header " + name + " to equal " + text(expected));
        }
      },
      jsonBody: function (path, expected) {
        let parsed;
        try { parsed = JSON.parse(responseData.body_text); } catch (_) { throw new Error("expected a JSON response body"); }
        const result = readJsonPath(parsed, path);
        assert(result.found, "expected JSON body property " + text(path));
        if (arguments.length > 1) assert(deepEqual(result.value, expected), "expected JSON body property " + text(path) + " to equal " + JSON.stringify(expected));
        return result.value;
      },
      cookie: (name) => {
        const found = responseCookies.some((cookie) => text(cookie && cookie.name).toLowerCase() === text(name).toLowerCase());
        assert(found, "expected response cookie " + text(name));
      }
    }
  };
  Object.defineProperty(responseTo, "be", { value: makeResponseCategories(false) });
  Object.defineProperty(responseTo, "not", {
    value: {
      have: {
        get body() {
          assert(responseData.body_text.length === 0, "expected response not to have a body");
          return true;
        },
        status: (expected) => assert(responseData.status !== expected, "expected status " + responseData.status + " not to equal " + expected),
        header: function (name, expected) {
          const actual = responseHeaders.get(name);
          if (arguments.length > 1) {
            const matches = expected && typeof expected.test === "function"
              ? expected.test(actual || "")
              : actual === text(expected);
            assert(actual === undefined || !matches, "expected response not to have header " + text(name) + " matching " + text(expected));
          } else {
            assert(actual === undefined, "expected response not to have header " + text(name));
          }
        },
        cookie: (name) => {
          const found = responseCookies.some((cookie) => text(cookie && cookie.name).toLowerCase() === text(name).toLowerCase());
          assert(!found, "expected response not to have cookie " + text(name));
        }
      },
      be: makeResponseCategories(true)
    }
  });
  return {
    code: responseData.status,
    status: responseData.status_text,
    responseTime: responseData.duration_ms,
    body: responseData.body_text,
    headers: responseHeaders,
    cookies: responseCookies,
    text: () => responseData.body_text,
    json: () => JSON.parse(responseData.body_text),
    to: responseTo
  };
}

const responseData = input.response;
let response = responseData ? makeScriptResponse(responseData) : null;

const nativeFetch = globalThis.fetch;
const pendingRequests = new Set();
const asyncErrors = [];
let sendRequestCount = 0;
const MAX_DIGEST_CHALLENGE_BYTES = 16 * 1024;

function normalizeHeaders(inputHeaders) {
  const headers = {};
  if (Array.isArray(inputHeaders)) {
    inputHeaders.forEach((header) => {
      if (!header || header.disabled === true || header.enabled === false) return;
      const key = text(header.key || header.name).trim();
      if (key) headers[key] = text(header.value);
    });
  } else if (inputHeaders && typeof inputHeaders === "object") {
    Object.entries(inputHeaders).forEach(([key, value]) => {
      headers[key] = text(value);
    });
  }
  return headers;
}

function appendQueryParameters(rawUrl, query) {
  if (!query || (typeof query !== "object" && !Array.isArray(query))) return rawUrl;
  const parsed = new URL(rawUrl);
  const entries = Array.isArray(query)
    ? query.map((entry) => [entry && (entry.key || entry.name), entry && entry.value, entry && (entry.disabled === true || entry.enabled === false)])
    : Object.entries(query).map(([key, value]) => [key, value, false]);
  entries.forEach(([key, value, disabled]) => {
    if (!disabled && text(key)) parsed.searchParams.append(replaceIn(key), replaceIn(value));
  });
  return parsed.toString();
}

function hasHeader(headers, name) {
  return Object.keys(headers).some((key) => key.toLowerCase() === name.toLowerCase());
}

function sendRequestAuthParameters(auth, section) {
  const value = auth && auth[section];
  if (Array.isArray(value)) return value;
  if (value && typeof value === "object") {
    return Object.entries(value).map(([key, value]) => ({ key, value }));
  }
  return [];
}

function applySendRequestAuth(auth, headers) {
  if (!auth || typeof auth !== "object") return null;
  const type = text(auth.type).toLowerCase();
  if (!type || type === "none" || type === "noauth") return null;
  const parameter = (section, names) => authParameterValue(
    sendRequestAuthParameters(auth, section), names
  );
  if (type === "bearer") {
    const token = parameter("bearer", ["token", "value"]);
    if (token === undefined) throw new Error("pm.sendRequest bearer auth requires a token");
    if (!hasHeader(headers, "authorization")) headers.authorization = "Bearer " + replaceIn(token);
    return null;
  }
  if (type === "basic") {
    const username = parameter("basic", "username");
    const password = parameter("basic", "password");
    if (username === undefined || password === undefined) {
      throw new Error("pm.sendRequest basic auth requires username and password");
    }
    if (!hasHeader(headers, "authorization")) {
      const credentials = Buffer.from(replaceIn(username) + ":" + replaceIn(password)).toString("base64");
      headers.authorization = "Basic " + credentials;
    }
    return null;
  }
  if (type === "digest") {
    const username = parameter("digest", ["username", "user"]);
    const password = parameter("digest", ["password", "passwd"]);
    if (username === undefined || password === undefined) {
      throw new Error("pm.sendRequest digest auth requires username and password");
    }
    return {
      type: "digest",
      username: replaceIn(username),
      password: replaceIn(password)
    };
  }
  if (type === "apikey" || type === "api_key") {
    const key = parameter("apikey", "key");
    const value = parameter("apikey", "value");
    const location = text(parameter("apikey", "in") || "header").toLowerCase();
    if (key === undefined || value === undefined) {
      throw new Error("pm.sendRequest API-key auth requires key and value");
    }
    const resolvedKey = replaceIn(key);
    const resolvedValue = replaceIn(value);
    if (location === "query") return { key: resolvedKey, value: resolvedValue };
    if (!hasHeader(headers, resolvedKey)) headers[resolvedKey] = resolvedValue;
    return null;
  }
  throw new Error("pm.sendRequest auth type is not supported: " + type);
}

function parseDigestChallenge(value) {
  const source = text(value).trim();
  if (Buffer.byteLength(source, "utf8") > MAX_DIGEST_CHALLENGE_BYTES) {
    throw new Error("pm.sendRequest Digest challenge exceeded the " + MAX_DIGEST_CHALLENGE_BYTES + "-byte limit");
  }
  const scheme = source.match(/^Digest\s+/i);
  if (!scheme) return null;
  const parameters = {};
  let index = scheme[0].length;
  while (index < source.length) {
    while (index < source.length && (source[index] === "," || /\s/.test(source[index]))) index += 1;
    if (index >= source.length) break;
    const keyStart = index;
    while (index < source.length && /[A-Za-z0-9_-]/.test(source[index])) index += 1;
    if (keyStart === index) return null;
    const key = source.slice(keyStart, index).toLowerCase();
    while (index < source.length && /\s/.test(source[index])) index += 1;
    if (source[index] !== "=") return null;
    index += 1;
    while (index < source.length && /\s/.test(source[index])) index += 1;
    let parsed;
    if (source[index] === '"') {
      index += 1;
      let result = "";
      let closed = false;
      while (index < source.length) {
        const character = source[index++];
        if (character === "\\" && index < source.length) {
          result += source[index++];
        } else if (character === '"') {
          closed = true;
          break;
        } else {
          result += character;
        }
      }
      if (!closed) return null;
      parsed = result;
    } else {
      const valueStart = index;
      while (index < source.length && source[index] !== ",") index += 1;
      parsed = source.slice(valueStart, index).trim();
    }
    parameters[key] = parsed;
    while (index < source.length && /\s/.test(source[index])) index += 1;
    if (index < source.length && source[index] !== ",") return null;
  }
  if (!parameters.realm || !parameters.nonce) return null;
  return parameters;
}

function digestAlgorithm(value) {
  const normalized = text(value).trim().toUpperCase() || "MD5";
  if (!["MD5", "MD5-SESS", "SHA-256", "SHA-256-SESS"].includes(normalized)) {
    throw new Error("pm.sendRequest Digest algorithm is not supported: " + normalized);
  }
  return normalized;
}

function digestHash(algorithm, value) {
  const hashName = algorithm.startsWith("SHA-256") ? "sha256" : "md5";
  return nodeCrypto.createHash(hashName).update(value, "utf8").digest("hex");
}

function digestEntityHash(algorithm, body) {
  if (body === undefined || body === null) return digestHash(algorithm, "");
  if (typeof body === "string") return digestHash(algorithm, body);
  if (Buffer.isBuffer(body)) return nodeCrypto.createHash(algorithm.startsWith("SHA-256") ? "sha256" : "md5").update(body).digest("hex");
  throw new Error("pm.sendRequest Digest auth-int requires a replayable text or Buffer body");
}

function quoteDigest(value) {
  return '"' + text(value).replace(/\\/g, "\\\\").replace(/"/g, '\\"') + '"';
}

function digestAuthorization(request, credentials, challenge) {
  const algorithm = digestAlgorithm(challenge.algorithm);
  const qops = text(challenge.qop).split(",").map((value) => value.trim().toLowerCase()).filter(Boolean);
  const qop = qops.includes("auth") ? "auth" : qops.includes("auth-int") ? "auth-int" : null;
  if (text(challenge.qop) && !qop) {
    throw new Error("pm.sendRequest Digest challenge has no supported qop");
  }
  const parsed = new URL(request.url);
  const uri = parsed.pathname + parsed.search;
  const cnonce = nodeCrypto.randomBytes(12).toString("hex");
  const ha1Value = credentials.username + ":" + challenge.realm + ":" + credentials.password;
  let ha1 = digestHash(algorithm, ha1Value);
  if (algorithm.endsWith("-SESS")) ha1 = digestHash(algorithm, ha1 + ":" + challenge.nonce + ":" + cnonce);
  const entityHash = qop === "auth-int" ? digestEntityHash(algorithm, request.body) : null;
  const ha2 = digestHash(
    algorithm,
    request.method + ":" + uri + (qop === "auth-int" ? ":" + entityHash : "")
  );
  const nonceCount = "00000001";
  const response = qop
    ? digestHash(algorithm, ha1 + ":" + challenge.nonce + ":" + nonceCount + ":" + cnonce + ":" + qop + ":" + ha2)
    : digestHash(algorithm, ha1 + ":" + challenge.nonce + ":" + ha2);
  const fields = [
    "username=" + quoteDigest(credentials.username),
    "realm=" + quoteDigest(challenge.realm),
    "nonce=" + quoteDigest(challenge.nonce),
    "uri=" + quoteDigest(uri),
    "response=" + quoteDigest(response)
  ];
  if (challenge.algorithm) fields.push("algorithm=" + algorithm);
  if (qop) fields.push("qop=" + qop, "nc=" + nonceCount, "cnonce=" + quoteDigest(cnonce));
  else if (algorithm.endsWith("-SESS")) fields.push("cnonce=" + quoteDigest(cnonce));
  if (challenge.opaque) fields.push("opaque=" + quoteDigest(challenge.opaque));
  return "Digest " + fields.join(", ");
}

function headersWithAuthorization(headers, authorization) {
  const next = {};
  Object.entries(headers).forEach(([key, value]) => {
    if (key.toLowerCase() !== "authorization") next[key] = value;
  });
  next.authorization = authorization;
  return next;
}

function normalizeSendRequestBody(inputBody, headers) {
  if (inputBody === undefined || inputBody === null) return undefined;
  if (typeof inputBody !== "object") return replaceIn(inputBody);
  const mode = text(inputBody.mode).toLowerCase();
  if (!mode || mode === "raw") return replaceIn(inputBody.raw);
  if (mode === "urlencoded") {
    const params = new URLSearchParams();
    (Array.isArray(inputBody.urlencoded) ? inputBody.urlencoded : []).forEach((field) => {
      if (!field || field.disabled === true || field.enabled === false || !text(field.key)) return;
      params.append(replaceIn(field.key), replaceIn(field.value));
    });
    if (!hasHeader(headers, "content-type")) headers["content-type"] = "application/x-www-form-urlencoded";
    return params.toString();
  }
  if (mode === "formdata") {
    if (typeof FormData !== "function") throw new Error("pm.sendRequest formdata requires Node.js FormData");
    const form = new FormData();
    (Array.isArray(inputBody.formdata) ? inputBody.formdata : []).forEach((field) => {
      if (!field || field.disabled === true || field.enabled === false || !text(field.key)) return;
      if (field.src) throw new Error("pm.sendRequest file formdata parts are not supported");
      form.append(replaceIn(field.key), replaceIn(field.value));
    });
    return form;
  }
  if (mode === "graphql") {
    const graphql = inputBody.graphql || {};
    let variables = graphql.variables;
    if (typeof variables === "string") {
      try { variables = JSON.parse(replaceIn(variables)); } catch (_) { variables = {}; }
    }
    if (!hasHeader(headers, "content-type")) headers["content-type"] = "application/json";
    return JSON.stringify({ query: replaceIn(graphql.query), variables: variables || {} });
  }
  if (mode === "file") throw new Error("pm.sendRequest file bodies are not supported");
  throw new Error("pm.sendRequest body mode is not supported: " + mode);
}

function normalizeSendRequest(inputRequest) {
  const options = typeof inputRequest === "string" ? { url: inputRequest } : (inputRequest || {});
  const urlOptions = options.url && typeof options.url === "object" ? options.url : null;
  let rawUrl = urlOptions ? urlOptions.raw || urlOptions.toString() : options.url;
  let url = appendQueryParameters(replaceIn(rawUrl), urlOptions && urlOptions.query);
  if (!url) throw new Error("pm.sendRequest requires a URL");
  const method = text(options.method || "GET").toUpperCase();
  const headers = normalizeHeaders(options.header || options.headers);
  const authResult = applySendRequestAuth(options.auth, headers);
  const digestAuth = authResult && authResult.type === "digest" ? authResult : null;
  if (authResult && !digestAuth) url = appendQueryParameters(url, [authResult]);
  const parsed = new URL(url);
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
    throw new Error("pm.sendRequest only permits http and https URLs");
  }
  const body = normalizeSendRequestBody(options.body, headers);
  return { url, method, headers, body, digestAuth };
}

async function performSendRequest(inputRequest) {
  if (typeof nativeFetch !== "function") throw new Error("Node.js fetch is unavailable for pm.sendRequest");
  const request = normalizeSendRequest(inputRequest);
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), SEND_REQUEST_TIMEOUT_MS);
  const started = Date.now();
  try {
    let nativeResponse = await nativeFetch(request.url, {
      method: request.method,
      headers: request.headers,
      body: request.body,
      signal: controller.signal
    });
    if (request.digestAuth && nativeResponse.status === 401) {
      const challengeHeader = nativeResponse.headers.get("www-authenticate");
      const digestStart = text(challengeHeader).search(/Digest\s+/i);
      const challenge = digestStart >= 0
        ? parseDigestChallenge(text(challengeHeader).slice(digestStart))
        : null;
      if (challenge) {
        if (request.body !== undefined && typeof request.body !== "string" && !Buffer.isBuffer(request.body)) {
          throw new Error("pm.sendRequest Digest retry requires a replayable text or Buffer body");
        }
        if (nativeResponse.body && typeof nativeResponse.body.cancel === "function") await nativeResponse.body.cancel();
        nativeResponse = await nativeFetch(request.url, {
          method: request.method,
          headers: headersWithAuthorization(
            request.headers,
            digestAuthorization(request, request.digestAuth, challenge)
          ),
          body: request.body,
          signal: controller.signal
        });
      }
    }
    const bodyText = await nativeResponse.text();
    if (Buffer.byteLength(bodyText, "utf8") > MAX_SEND_RESPONSE_BYTES) {
      throw new Error("pm.sendRequest response exceeded the " + MAX_SEND_RESPONSE_BYTES + "-byte limit");
    }
    const headers = [];
    nativeResponse.headers.forEach((value, key) => {
      headers.push({ key, value, enabled: true });
    });
    return makeScriptResponse({
      status: nativeResponse.status,
      status_text: nativeResponse.statusText,
      headers,
      cookies: responseCookiesFromHeaders(nativeResponse.headers),
      body_text: bodyText,
      duration_ms: Date.now() - started
    });
  } catch (error) {
    if (error && error.name === "AbortError") throw new Error("pm.sendRequest timed out after " + SEND_REQUEST_TIMEOUT_MS + "ms");
    throw error;
  } finally {
    clearTimeout(timer);
  }
}

function sendRequest(inputRequest, callback) {
  const callbackFn = typeof callback === "function" ? callback : () => {};
  if (sendRequestCount >= MAX_SEND_REQUESTS) {
    const error = new Error("pm.sendRequest exceeded the maximum of " + MAX_SEND_REQUESTS + " requests");
    try { callbackFn(error, null); } catch (callbackError) { asyncErrors.push(callbackError); }
    return Promise.resolve();
  }
  sendRequestCount += 1;
  const operation = performSendRequest(inputRequest);
  const tracked = operation.then(
    (result) => {
      try { callbackFn(null, result); } catch (callbackError) { asyncErrors.push(callbackError); }
    },
    (error) => {
      try { callbackFn(error, null); } catch (callbackError) { asyncErrors.push(callbackError); }
    }
  );
  pendingRequests.add(tracked);
  tracked.then(() => pendingRequests.delete(tracked));
  return tracked;
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
      duration_ms: 0,
      error: "The script exceeded the maximum of " + MAX_TEST_ENTRIES + " tests."
    });
    return;
  }
  const started = Date.now();
  try {
    callback();
    tests.push({ name: text(name), passed: true, duration_ms: Date.now() - started });
  } catch (error) {
    tests.push({
      name: text(name),
      passed: false,
      duration_ms: Date.now() - started,
      error: text(error && (error.stack || error.message || error))
    });
  }
}

const pm = {
  info: pmInfo,
  cookies: pmCookies,
  environment,
  collectionVariables,
  globals,
  iterationData,
  variables: runtime,
  request,
  response,
  sendRequest,
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
let scriptError = null;
try {
  new vm.Script(input.script, { filename: "postly-script.js" }).runInContext(sandbox, { timeout: 2000 });
} catch (error) {
  scriptError = error;
}
while (pendingRequests.size > 0) {
  await Promise.all(Array.from(pendingRequests));
}
if (scriptError) throw scriptError;
if (asyncErrors.length > 0) throw asyncErrors[0];
process.stdout.write(JSON.stringify({
  request: serializeRequest(),
  changes: { ...changes, removed: removals },
  tests,
  logs
}));
}
main().catch((error) => {
  process.stderr.write(String(error && (error.stack || error.message || error)));
  process.exitCode = 1;
});
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Request;

    #[test]
    fn enables_node_permissions_only_when_network_access_can_be_retained() {
        assert_eq!(
            node_permission_flags_from_help(
                "  --permission  enable permissions\n  --allow-net  allow network"
            ),
            vec!["--permission", "--allow-net"]
        );
        assert!(node_permission_flags_from_help("  --permission  enable permissions").is_empty());
        assert!(node_permission_flags_from_help("  --permission-audit  audit only").is_empty());
    }

    #[test]
    fn rejects_explicit_host_access_before_starting_node() {
        let request = Request::new("Unsafe script", "GET", "https://example.test");
        for script in [
            "const fs = require('node:fs');",
            "process.exit(1);",
            "globalThis.process;",
            "eval('pm.test(\\\"escape\\\", () => {});');",
            "new Function('return 1')();",
        ] {
            let error = run_script(script, &request, None, &VariableContext::default())
                .expect_err("host access must be rejected before Node");
            assert!(
                matches!(error, ScriptError::UnsupportedHostAccess { .. }),
                "{error}"
            );
        }
    }

    #[test]
    fn ignores_host_like_words_inside_strings_and_comments() {
        assert!(validate_script_source(
            r#"
                // process and require are only documentation
                pm.test("globalThis Function", function () {
                    pm.expect("eval(module)").to.be.a("string");
                });
            "#
        )
        .is_ok());
    }

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
    fn supports_extended_postman_expect_matchers() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let request = Request::new("Matchers", "GET", "https://example.test");
        let result = run_script(
            r#"
                pm.test("extended matchers", function () {
                    pm.expect({ b: [2], a: 1 }).to.deep.equal({ a: 1, b: [2] });
                    pm.expect({ ready: true, meta: { id: 7, source: "fixture" } }).to.deep.include({ meta: { id: 7 } });
                    pm.expect([1, { ready: true }, 3]).to.deep.members([{ ready: true }, 3, 1]);
                    pm.expect({ ready: true, meta: { id: 7 } }).to.have.deep.include({ meta: { id: 7 } });
                    pm.expect([1, { ready: true }, 3]).to.have.deep.members([{ ready: true }, 3, 1]);
                    pm.expect({ ready: true }).to.not.deep.include({ ready: false });
                    pm.expect([1, 2]).to.not.deep.members([1, 2, 3]);
                    pm.expect([1, 2, 3]).to.have.lengthOf(3);
                    pm.expect([1, { ready: true }, 3]).to.have.members([3, { ready: true }, 1]);
                    pm.expect(["postly", "rust"]).to.include.members(["rust"]);
                    pm.expect([1, 1]).to.not.have.members([1]);
                    pm.expect({ ready: true }).to.have.keys(["ready"]);
                    pm.expect({ ready: true, pending: false }).to.have.all.keys("ready", "pending");
                    pm.expect({ ready: true }).to.have.any.keys("missing", "ready");
                    pm.expect({ ready: true, pending: false }).to.include.keys("ready");
                    pm.expect("ready").to.contain("ead");
                    pm.expect({ ready: true, meta: { id: 7 } }).to.include({ ready: true });
                    pm.expect([{ ready: true }]).to.contain({ ready: true });
                    pm.expect({ user: { name: "Ada" } }).to.have.nested.property("user.name", "Ada");
                    pm.expect({ user: { name: "Ada" } }).to.have.deep.property("user.name", "Ada");
                    pm.expect(null).to.be.null;
                    pm.expect(undefined).to.be.undefined;
                    pm.expect("present").to.be.exist;
                    pm.expect("ready").to.be.oneOf(["ready", "done"]);
                    pm.expect(3).to.be.at.least(2);
                    pm.expect(3).to.be.at.most(4);
                    pm.expect(3).to.be.within(3, 3);
                    pm.expect(3).to.be.greaterThan(2);
                    pm.expect(3).to.be.lessThan(4);
                    pm.expect(3.14).to.be.closeTo(3.1, 0.05);
                    pm.expect({ ready: true }).to.satisfy((value) => value.ready === true);
                    pm.expect(3.14).to.not.be.closeTo(3, 0.05);
                    pm.expect({ ready: true }).to.not.satisfy((value) => value.pending === true);
                    pm.expect([]).to.be.empty;
                    pm.expect({}).to.be.empty;
                    pm.expect([]).to.be.an("array");
                    pm.expect({ name: "Ada" }).to.have.property("name").that.is.a("string");
                    pm.expect({ count: 3 }).to.have.property("count").that.equals(3);
                    pm.expect({ ready: true }).to.not.have.property("missing");
                });
            "#,
            &request,
            None,
            &VariableContext::default(),
        )
        .expect("script");
        assert_eq!(result.tests.len(), 1);
        assert!(result.tests[0].passed, "{:?}", result.tests[0].error);
    }

    #[test]
    fn reports_individual_script_test_durations_and_errors() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let request = Request::new("Timed tests", "GET", "https://example.test");
        let result = run_script(
            r#"
                pm.test("slow pass", function () {
                    const started = Date.now();
                    while (Date.now() - started < 5) {}
                });
                pm.test("fast failure", function () {
                    pm.expect(false).to.be.true;
                });
            "#,
            &request,
            None,
            &VariableContext::default(),
        )
        .expect("timed script");

        assert_eq!(result.tests.len(), 2);
        assert!(result.tests[0].passed);
        assert!(result.tests[0].duration_ms >= 4);
        assert!(!result.tests[1].passed);
        assert!(result.tests[1].duration_ms < 2_000);
        assert!(result.tests[1]
            .error
            .as_deref()
            .is_some_and(|error| { error.contains("expected false to be true") }));
    }

    #[test]
    fn supports_bounded_pm_send_request_callbacks() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            use std::io::{Read, Write};

            let (mut stream, _) = listener.accept().expect("connection");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let read = stream.read(&mut buffer).expect("read request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
            assert!(request.contains("x-script: yes"));
            let body = r#"{"ok":true}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nx-request-id: script\r\nset-cookie: sid=script-cookie; Path=/; HttpOnly\r\nset-cookie: theme=dark; Path=/; SameSite=Lax\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        });

        let request = Request::new("Scripted", "GET", "https://example.test");
        let result = run_script(
            &format!(
                r#"
                    pm.sendRequest({{
                        url: "http://{address}/token",
                        method: "GET",
                        header: [{{ key: "X-Script", value: "yes" }}]
                    }}, function (error, response) {{
                        pm.test("callback received", function () {{
                            pm.expect(error).to.eql(null);
                            response.to.have.status(200);
                            pm.expect(response.headers.get("x-request-id")).to.eql("script");
                            pm.expect(response.json()).to.have.property("ok", true);
                            pm.expect(response.cookies.get("sid")).to.eql("script-cookie");
                            pm.expect(response.cookies.has("theme")).to.eql(true);
                            pm.expect(response.cookies.toObject()).to.deep.include({{ sid: "script-cookie" }});
                            pm.expect(response.cookies.all()).to.have.lengthOf(2);
                            const sid = response.cookies.all().find((cookie) => cookie.name === "sid");
                            const theme = response.cookies.all().find((cookie) => cookie.name === "theme");
                            pm.expect(sid.httpOnly).to.eql(true);
                            pm.expect(sid.path).to.eql("/");
                            pm.expect(theme.sameSite).to.eql("Lax");
                            const cookieNames = [];
                            response.cookies.each((cookie) => cookieNames.push(cookie.name));
                            pm.expect(cookieNames).to.include("theme");
                        }});
                    }});
                "#
            ),
            &request,
            None,
            &VariableContext::default(),
        )
        .expect("script");
        server.join().expect("server");

        assert_eq!(result.tests.len(), 1);
        assert!(result.tests[0].passed, "{:?}", result.tests[0].error);
    }

    #[test]
    fn supports_pm_send_request_authentication_shapes() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            use std::io::{Read, Write};

            for _ in 0..3 {
                let (mut stream, _) = listener.accept().expect("connection");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                loop {
                    let read = stream.read(&mut buffer).expect("read request");
                    assert!(read > 0, "request ended before headers");
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
                if request.starts_with("get /bearer") {
                    assert!(
                        request.contains("authorization: bearer script-token"),
                        "{request}"
                    );
                } else if request.starts_with("get /basic") {
                    assert!(
                        request.contains("authorization: basic dxnlcjpwyxnz"),
                        "{request}"
                    );
                } else if request.starts_with("get /query") {
                    assert!(
                        request.starts_with("get /query?api_key=query-value http/1.1"),
                        "{request}"
                    );
                } else {
                    panic!("unexpected pm.sendRequest path: {request}");
                }
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                    )
                    .expect("write response");
            }
        });

        let mut context = VariableContext::default();
        context
            .environment
            .insert("token".to_owned(), "script-token".to_owned());
        context
            .environment
            .insert("username".to_owned(), "user".to_owned());
        context
            .environment
            .insert("password".to_owned(), "pass".to_owned());
        context
            .environment
            .insert("queryKey".to_owned(), "api_key".to_owned());
        context
            .environment
            .insert("queryValue".to_owned(), "query-value".to_owned());
        let request = Request::new("Scripted auth", "GET", "https://example.test");
        let result = run_script(
            &format!(
                r#"
                    const base = "http://{address}";
                    pm.sendRequest({{
                        url: base + "/bearer",
                        auth: {{ type: "bearer", bearer: [{{ key: "token", value: "{{{{token}}}}" }}] }}
                    }}, function (error, response) {{
                        pm.test("bearer auth", function () {{
                            pm.expect(error).to.eql(null);
                            response.to.have.status(200);
                        }});
                    }});
                    pm.sendRequest({{
                        url: base + "/basic",
                        auth: {{ type: "basic", basic: [
                            {{ key: "username", value: "{{{{username}}}}" }},
                            {{ key: "password", value: "{{{{password}}}}" }}
                        ] }}
                    }}, function (error, response) {{
                        pm.test("basic auth", function () {{
                            pm.expect(error).to.eql(null);
                            response.to.have.status(200);
                        }});
                    }});
                    pm.sendRequest({{
                        url: base + "/query",
                        auth: {{ type: "apikey", apikey: [
                            {{ key: "key", value: "{{{{queryKey}}}}" }},
                            {{ key: "value", value: "{{{{queryValue}}}}" }},
                            {{ key: "in", value: "query" }}
                        ] }}
                    }}, function (error, response) {{
                        pm.test("query API key", function () {{
                            pm.expect(error).to.eql(null);
                            response.to.have.status(200);
                        }});
                    }});
                "#
            ),
            &request,
            None,
            &context,
        )
        .expect("script auth requests");
        server.join().expect("server");

        assert_eq!(result.tests.len(), 3);
        assert!(result.tests.iter().all(|test| test.passed), "{result:?}");
    }

    #[test]
    fn supports_pm_send_request_digest_challenge_retry() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            use std::io::{Read, Write};

            let read_headers = |stream: &mut std::net::TcpStream| {
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                loop {
                    let read = stream.read(&mut buffer).expect("read request");
                    assert!(read > 0, "request ended before headers");
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        return String::from_utf8_lossy(&request).to_ascii_lowercase();
                    }
                }
            };

            let (mut first, _) = listener.accept().expect("first connection");
            let first_request = read_headers(&mut first);
            assert!(
                first_request.starts_with("get /digest?source=script http/1.1"),
                "{first_request}"
            );
            assert!(!first_request.contains("authorization: digest"));
            first
                .write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nwww-authenticate: Digest realm=\"local\", nonce=\"nonce-123\", qop=\"auth\"\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .expect("write challenge");

            let (mut second, _) = listener.accept().expect("retry connection");
            let second_request = read_headers(&mut second);
            assert!(second_request.starts_with("get /digest?source=script http/1.1"));
            assert!(
                second_request.contains("authorization: digest"),
                "{second_request}"
            );
            assert!(
                second_request.contains("username=\"postly\""),
                "{second_request}"
            );
            assert!(
                second_request.contains("realm=\"local\""),
                "{second_request}"
            );
            assert!(
                second_request.contains("nonce=\"nonce-123\""),
                "{second_request}"
            );
            assert!(second_request.contains("qop=auth"), "{second_request}");
            assert!(second_request.contains("nc=00000001"), "{second_request}");
            let body = r#"{"ok":true}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            second
                .write_all(response.as_bytes())
                .expect("write response");
        });

        let request = Request::new("Scripted Digest", "GET", "https://example.test");
        let result = run_script(
            &format!(
                r#"
                    pm.sendRequest({{
                        url: "http://{address}/digest?source=script",
                        method: "GET",
                        auth: {{
                            type: "digest",
                            digest: [
                                {{ key: "username", value: "postly" }},
                                {{ key: "password", value: "secret" }}
                            ]
                        }}
                    }}, function (error, response) {{
                        pm.test("digest challenge retry", function () {{
                            pm.expect(error).to.eql(null);
                            response.to.have.status(200);
                            pm.expect(response.json()).to.have.property("ok", true);
                        }});
                    }});
                "#
            ),
            &request,
            None,
            &VariableContext::default(),
        )
        .expect("script Digest request");
        server.join().expect("server");

        assert_eq!(result.tests.len(), 1);
        assert!(result.tests[0].passed, "{result:?}");
    }

    #[test]
    fn supports_pm_send_request_sha256_auth_int_retry() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            use std::io::{Read, Write};

            let read_request = |stream: &mut std::net::TcpStream| {
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                let header_end;
                loop {
                    let read = stream.read(&mut buffer).expect("read request");
                    assert!(read > 0, "request ended before headers");
                    request.extend_from_slice(&buffer[..read]);
                    if let Some(position) =
                        request.windows(4).position(|window| window == b"\r\n\r\n")
                    {
                        header_end = position + 4;
                        break;
                    }
                }
                let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
                let content_length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                while request.len() < header_end + content_length {
                    let read = stream.read(&mut buffer).expect("read body");
                    assert!(read > 0, "body ended early");
                    request.extend_from_slice(&buffer[..read]);
                }
                (
                    headers,
                    request[header_end..header_end + content_length].to_vec(),
                )
            };

            let (mut first, _) = listener.accept().expect("first connection");
            let (first_headers, first_body) = read_request(&mut first);
            assert!(first_headers.starts_with("post /digest-body http/1.1"));
            assert_eq!(first_body, b"alpha=body");
            first
                .write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nwww-authenticate: Digest realm=\"local\", nonce=\"sha-nonce\", algorithm=SHA-256, qop=\"auth-int\"\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .expect("write challenge");

            let (mut second, _) = listener.accept().expect("retry connection");
            let (second_headers, second_body) = read_request(&mut second);
            assert!(second_headers.starts_with("post /digest-body http/1.1"));
            assert_eq!(second_body, b"alpha=body");
            assert!(
                second_headers.contains("authorization: digest"),
                "{second_headers}"
            );
            assert!(
                second_headers.contains("algorithm=sha-256"),
                "{second_headers}"
            );
            assert!(second_headers.contains("qop=auth-int"), "{second_headers}");
            let authorization = second_headers
                .lines()
                .find_map(|line| line.strip_prefix("authorization: digest "))
                .expect("Digest authorization");
            let response = authorization
                .split(", ")
                .find_map(|field| field.strip_prefix("response=\""))
                .and_then(|value| value.strip_suffix('"'))
                .expect("Digest response");
            assert_eq!(response.len(), 64, "{authorization}");
            second
                .write_all(
                    b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .expect("write response");
        });

        let request = Request::new("Scripted Digest auth-int", "GET", "https://example.test");
        let result = run_script(
            &format!(
                r#"
                    pm.sendRequest({{
                        url: "http://{address}/digest-body",
                        method: "POST",
                        auth: {{
                            type: "digest",
                            digest: [
                                {{ key: "username", value: "postly" }},
                                {{ key: "password", value: "secret" }}
                            ]
                        }},
                        body: {{ mode: "raw", raw: "alpha=body" }}
                    }}, function (error, response) {{
                        pm.test("sha256 auth-int retry", function () {{
                            pm.expect(error).to.eql(null);
                            response.to.have.status(204);
                        }});
                    }});
                "#
            ),
            &request,
            None,
            &VariableContext::default(),
        )
        .expect("script SHA-256 Digest request");
        server.join().expect("server");

        assert_eq!(result.tests.len(), 1);
        assert!(result.tests[0].passed, "{result:?}");
    }

    #[test]
    fn supports_pm_send_request_query_and_urlencoded_body() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        let address = listener.local_addr().expect("address");
        let server = std::thread::spawn(move || {
            use std::io::{Read, Write};

            let (mut stream, _) = listener.accept().expect("connection");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            let header_end;
            loop {
                let read = stream.read(&mut buffer).expect("read request");
                assert!(read > 0, "request ended before headers");
                request.extend_from_slice(&buffer[..read]);
                if let Some(position) = request.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    header_end = position + 4;
                    break;
                }
            }
            let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
            assert!(headers.starts_with("post /token?source=script&space=hello+world http/1.1"));
            assert!(headers.contains("content-type: application/x-www-form-urlencoded"));
            let content_length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .expect("content length");
            while request.len() < header_end + content_length {
                let read = stream.read(&mut buffer).expect("read body");
                assert!(read > 0, "body ended early");
                request.extend_from_slice(&buffer[..read]);
            }
            assert_eq!(
                &request[header_end..header_end + content_length],
                b"alpha=one&beta=two+words"
            );
            let body = r#"{"ok":true}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        });

        let request = Request::new("Scripted", "GET", "https://example.test");
        let result = run_script(
            &format!(
                r#"
                    pm.sendRequest({{
                        url: {{
                            raw: "http://{address}/token",
                            query: [
                                {{ key: "source", value: "script" }},
                                {{ key: "space", value: "hello world" }}
                            ]
                        }},
                        method: "POST",
                        body: {{
                            mode: "urlencoded",
                            urlencoded: [
                                {{ key: "alpha", value: "one" }},
                                {{ key: "beta", value: "two words" }}
                            ]
                        }}
                    }}, function (error, response) {{
                        pm.test("url and body are sent", function () {{
                            pm.expect(error).to.eql(null);
                            response.to.have.status(200);
                            pm.expect(response.json()).to.have.property("ok", true);
                        }});
                    }});
                "#
            ),
            &request,
            None,
            &VariableContext::default(),
        )
        .expect("script request");
        server.join().expect("server");

        assert_eq!(result.tests.len(), 1);
        assert!(result.tests[0].passed, "{:?}", result.tests[0].error);
    }

    #[test]
    fn supports_postman_request_url_body_and_cookie_facades() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let mut request = Request::new("Facade request", "POST", "https://api.example.test/users");
        request
            .query
            .push(crate::model::KeyValue::enabled("page", "1"));
        request.body = crate::model::RequestBody::Json {
            value: serde_json::json!({ "name": "Ada" }),
        };

        let result = run_script(
            r#"
                pm.test("Postman request facade", function () {
                    pm.expect(pm.request.url.host).to.eql("api.example.test");
                    pm.expect(pm.request.url.getPath()).to.eql("/users");
                    pm.expect(pm.request.url.getQueryString()).to.eql("page=1");
                    pm.expect(pm.request.body.mode).to.eql("raw");
                    pm.expect(JSON.parse(pm.request.body.raw).name).to.eql("Ada");
                });
                pm.request.url.query.add({ key: "filter", value: "active users" });
                pm.request.url.addQueryParams([
                    { key: "tag", value: "rust" },
                    { key: "tag", value: "api", disabled: true }
                ]);
                pm.test("URL query helpers are available", function () {
                    pm.expect(pm.request.url.getQueryParams().count()).to.eql(4);
                    pm.expect(pm.request.url.getQueryParams().get("tag").value).to.eql("rust");
                });
                pm.request.url.removeQueryParams("page");
                pm.request.body.update({
                    mode: "raw",
                    raw: JSON.stringify({ name: "Grace", role: "admin" }),
                    options: { raw: { language: "json" } }
                });
                pm.test("body update is visible", function () {
                    pm.expect(pm.request.body.mode).to.eql("raw");
                    pm.expect(JSON.parse(pm.request.body.raw).role).to.eql("admin");
                });
                pm.request.cookies.add({ key: "session", value: "local" });
            "#,
            &request,
            None,
            &VariableContext::default(),
        )
        .expect("script");

        result
            .apply(&mut request, &mut VariableContext::default())
            .expect("apply script changes");
        assert_eq!(
            request.query,
            vec![
                crate::model::KeyValue::enabled("filter", "active users"),
                crate::model::KeyValue::enabled("tag", "rust"),
                crate::model::KeyValue {
                    key: "tag".to_owned(),
                    value: "api".to_owned(),
                    enabled: false,
                },
            ]
        );
        assert_eq!(
            request.body,
            crate::model::RequestBody::Json {
                value: serde_json::json!({ "name": "Grace", "role": "admin" }),
            }
        );
        assert_eq!(
            request.cookies,
            vec![crate::model::KeyValue::enabled("session", "local")]
        );
        assert!(result.tests[0].passed, "{:?}", result.tests[0].error);
    }

    #[test]
    fn supports_postman_request_body_update_modes_and_lists() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let mut request = Request::new(
            "Body update modes",
            "POST",
            "https://api.example.test/submit",
        );
        let result = run_script(
            r#"
                pm.request.body.update("plain text");
                pm.test("raw body update", function () {
                    pm.expect(pm.request.body.mode).to.eql("raw");
                    pm.expect(pm.request.body.raw).to.eql("plain text");
                });
                pm.request.body.update({
                    mode: "urlencoded",
                    urlencoded: [
                        { key: "query", value: "Ada Lovelace" },
                        { key: "skip", value: "ignored", disabled: true }
                    ]
                });
                pm.request.body.urlencoded.add({ key: "role", value: "admin" });
                pm.test("urlencoded body list", function () {
                    pm.expect(pm.request.body.urlencoded.count()).to.eql(3);
                    pm.expect(pm.request.body.urlencoded.toObject()).to.eql({
                        query: "Ada Lovelace",
                        role: "admin"
                    });
                });
            "#,
            &request,
            None,
            &VariableContext::default(),
        )
        .expect("body update modes script");

        result
            .apply(&mut request, &mut VariableContext::default())
            .expect("apply body update modes");
        assert_eq!(
            request.body,
            crate::model::RequestBody::FormUrlEncoded {
                fields: vec![
                    crate::model::KeyValue::enabled("query", "Ada Lovelace"),
                    crate::model::KeyValue {
                        key: "skip".to_owned(),
                        value: "ignored".to_owned(),
                        enabled: false,
                    },
                    crate::model::KeyValue::enabled("role", "admin"),
                ],
            }
        );
        assert!(result.tests.iter().all(|test| test.passed));
    }

    #[test]
    fn exposes_postman_info_metadata() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let request = Request::new("Info facade", "GET", "https://api.example.test");
        let result = run_script(
            r#"
                pm.test("request metadata", function () {
                    pm.expect(pm.info.requestName).to.eql("Info facade");
                    pm.expect(pm.info.requestId).to.be.a("string");
                    pm.expect(pm.info.iteration).to.eql(0);
                    pm.expect(pm.info.iterationCount).to.eql(1);
                });
            "#,
            &request,
            None,
            &VariableContext::default(),
        )
        .expect("pm.info facade");

        assert_eq!(result.tests.len(), 1);
        assert!(result.tests[0].passed, "{result:?}");
    }

    #[test]
    fn exposes_postman_url_metadata_and_mutable_path_variables() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let request = Request::new(
            "URL metadata",
            "GET",
            "https://api.example.test/users/{{userId}}?include=profile#details",
        );
        let mut context = VariableContext::default();
        context
            .environment
            .insert("userId".to_owned(), "42".to_owned());
        let result = run_script(
            r##"
                pm.test("URL metadata", function () {
                    pm.expect(pm.request.url.protocol).to.eql("https");
                    pm.expect(pm.request.url.getProtocol()).to.eql("https");
                    pm.expect(pm.request.url.port).to.eql("");
                    pm.expect(pm.request.url.getHost()).to.eql("api.example.test");
                    pm.expect(pm.request.url.getPath()).to.eql("/users/42");
                    pm.expect(pm.request.url.hash).to.eql("#details");
                    pm.expect(pm.request.url.variables.get("userId").value).to.eql("42");
                    pm.expect(pm.request.url.variable.toObject()).to.eql({ userId: "42" });
                    pm.expect(pm.request.url.toObject()).to.have.deep.include({
                        protocol: "https",
                        host: ["api", "example", "test"],
                        path: ["users", "42"],
                        query: [{ key: "include", value: "profile", disabled: false }],
                        variable: { userId: "42" },
                        hash: "details"
                    });
                });
                pm.request.url.variables.replace("userId", "99");
                pm.request.url.variables.get("userId").value = "100";
                pm.test("path variable mutations materialize", function () {
                    pm.expect(pm.request.url.variables.get("userId").value).to.eql("100");
                    pm.expect(pm.request.url.toString()).to.eql("https://api.example.test/users/100?include=profile#details");
                });
            "##,
            &request,
            None,
            &context,
        )
        .expect("URL metadata script");

        assert!(result.tests[0].passed, "{:?}", result.tests[0].error);
        assert!(result.tests[1].passed, "{:?}", result.tests[1].error);
        let mut applied = request.clone();
        let mut applied_context = context.clone();
        result
            .apply(&mut applied, &mut applied_context)
            .expect("apply URL variable mutation");
        assert_eq!(
            applied.url,
            "https://api.example.test/users/100?include=profile#details"
        );
    }

    #[test]
    fn supports_postman_property_list_helpers_for_request_data() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let mut request = Request::new("Property lists", "GET", "https://api.example.test/users");
        request
            .headers
            .push(crate::model::HeaderEntry::enabled("X-Trace", "before"));
        request
            .headers
            .push(crate::model::HeaderEntry::enabled("X-Mode", "local"));
        request
            .cookies
            .push(crate::model::KeyValue::enabled("session", "cookie-value"));
        request
            .query
            .push(crate::model::KeyValue::enabled("page", "1"));

        let result = run_script(
            r#"
                pm.test("request property lists", function () {
                    pm.expect(pm.request.headers.get("x-trace")).to.eql("before");
                    pm.expect(pm.request.headers.has("x-mode")).to.eql(true);
                    pm.expect(pm.request.headers.count()).to.eql(2);
                    pm.expect(pm.request.headers.toObject()).to.eql({ "X-Trace": "before", "X-Mode": "local" });
                    pm.expect(pm.request.headers.all()).to.have.lengthOf(2);
                    let headerNames = [];
                    pm.request.headers.each((header) => headerNames.push(header.key));
                    pm.expect(headerNames).to.eql(["X-Trace", "X-Mode"]);
                    pm.expect(pm.request.cookies.get("session").value).to.eql("cookie-value");
                    pm.expect(pm.request.cookies.has("session")).to.eql(true);
                    pm.expect(pm.request.url.query.get("page").value).to.eql("1");
                    pm.expect(pm.request.url.query.toObject()).to.eql({ page: "1" });
                    pm.expect(pm.request.url.query.count()).to.eql(1);
                });
                pm.request.headers.clear();
                pm.test("header list can be cleared", function () {
                    pm.expect(pm.request.headers).to.be.empty;
                });
            "#,
            &request,
            None,
            &VariableContext::default(),
        )
        .expect("property list facade");

        result
            .apply(&mut request, &mut VariableContext::default())
            .expect("apply property list facade");
        assert!(request.headers.is_empty());
        assert_eq!(request.cookies[0].value, "cookie-value");
        assert!(result.tests.iter().all(|test| test.passed), "{result:?}");
    }

    #[test]
    fn supports_postman_request_auth_facade() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let mut request = Request::new("Auth facade", "GET", "https://api.example.test");
        request.auth = crate::model::Auth::Bearer {
            token: "old-token".to_owned(),
        };
        let result = run_script(
            r#"
                pm.test("bearer auth is visible", function () {
                    pm.expect(pm.request.auth.type).to.eql("bearer");
                    pm.expect(pm.request.auth.get("token")).to.eql("old-token");
                    pm.expect(pm.request.auth.has("token")).to.eql(true);
                });
                pm.request.auth.upsert({ key: "token", value: "new-token" });
            "#,
            &request,
            None,
            &VariableContext::default(),
        )
        .expect("auth facade");
        result
            .apply(&mut request, &mut VariableContext::default())
            .expect("apply auth facade");
        assert_eq!(
            request.auth,
            crate::model::Auth::Bearer {
                token: "new-token".to_owned()
            }
        );
        assert!(result.tests[0].passed, "{:?}", result.tests[0].error);

        let mut api_key_request = Request::new("API key facade", "GET", "https://api.example.test");
        api_key_request.auth = crate::model::Auth::ApiKey {
            key: "X-API-Key".to_owned(),
            value: "old-key".to_owned(),
            location: crate::model::ApiKeyLocation::Header,
        };
        let api_key_result = run_script(
            r#"
                pm.request.auth.upsert({ key: "value", value: "new-key" });
                pm.request.auth.upsert({ key: "in", value: "query" });
            "#,
            &api_key_request,
            None,
            &VariableContext::default(),
        )
        .expect("API key facade");
        api_key_result
            .apply(&mut api_key_request, &mut VariableContext::default())
            .expect("apply API key facade");
        assert_eq!(
            api_key_request.auth,
            crate::model::Auth::ApiKey {
                key: "X-API-Key".to_owned(),
                value: "new-key".to_owned(),
                location: crate::model::ApiKeyLocation::Query,
            }
        );

        let mut aws_request = Request::new("AWS auth facade", "GET", "https://api.example.test");
        aws_request.auth = crate::model::Auth::AwsSignatureV4 {
            access_key_id: "AKIDEXAMPLE".to_owned(),
            secret_access_key: "{{awsSecret}}".to_owned(),
            region: "us-east-1".to_owned(),
            service: "execute-api".to_owned(),
            session_token: None,
        };
        let aws_result = run_script(
            "pm.test('unsupported auth is preserved', function () { pm.expect(pm.request.auth.type).to.eql('aws_signature_v4'); });",
            &aws_request,
            None,
            &VariableContext::default(),
        )
        .expect("AWS auth facade");
        aws_result
            .apply(&mut aws_request, &mut VariableContext::default())
            .expect("apply AWS auth facade");
        assert_eq!(
            aws_request.auth,
            crate::model::Auth::AwsSignatureV4 {
                access_key_id: "AKIDEXAMPLE".to_owned(),
                secret_access_key: "{{awsSecret}}".to_owned(),
                region: "us-east-1".to_owned(),
                service: "execute-api".to_owned(),
                session_token: None,
            }
        );
        assert!(
            aws_result.tests[0].passed,
            "{:?}",
            aws_result.tests[0].error
        );

        let mut digest_request =
            Request::new("Digest auth facade", "GET", "https://api.example.test");
        digest_request.auth = crate::model::Auth::Digest {
            username: "Mufasa".to_owned(),
            password: "Circle Of Life".to_owned(),
        };
        let digest_result = run_script(
            r#"
                pm.test("digest auth is visible", function () {
                    pm.expect(pm.request.auth.type).to.eql("digest");
                    pm.expect(pm.request.auth.get("username")).to.eql("Mufasa");
                });
                pm.request.auth.upsert({ key: "password", value: "new-password" });
            "#,
            &digest_request,
            None,
            &VariableContext::default(),
        )
        .expect("Digest auth facade");
        digest_result
            .apply(&mut digest_request, &mut VariableContext::default())
            .expect("apply Digest auth facade");
        assert_eq!(
            digest_request.auth,
            crate::model::Auth::Digest {
                username: "Mufasa".to_owned(),
                password: "new-password".to_owned(),
            }
        );
        assert!(
            digest_result.tests[0].passed,
            "{:?}",
            digest_result.tests[0].error
        );
    }

    #[test]
    fn preserves_raw_url_queries_until_a_script_mutates_them() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let request = Request::new(
            "Raw URL query",
            "GET",
            "https://api.example.test/search?term={{term}}",
        );
        let mut context = VariableContext::default();
        context
            .environment
            .insert("term".to_owned(), "Ada Lovelace".to_owned());
        let result = run_script(
            r#"
                pm.test("read raw URL query", function () {
                    pm.expect(pm.request.url.getQueryString()).to.eql("term=Ada%20Lovelace");
                });
            "#,
            &request,
            None,
            &context,
        )
        .expect("script");

        let mut applied = request.clone();
        result
            .apply(&mut applied, &mut context)
            .expect("apply script changes");
        assert_eq!(applied.url, request.url);
        assert!(applied.query.is_empty());
        assert!(result.tests[0].passed, "{:?}", result.tests[0].error);
    }

    #[test]
    fn captures_response_assertion_failures_without_losing_the_request() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let mut request = Request::new("Scripted", "GET", "https://example.test");
        request
            .cookies
            .push(crate::model::KeyValue::enabled("session", "abc"));
        request.cookies.push(crate::model::KeyValue {
            key: "disabled".to_owned(),
            value: "ignored".to_owned(),
            enabled: false,
        });
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
            ttfb_ms: 0,
            download_ms: 0,
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
                    pm.response.to.be.success;
                    pm.response.to.not.be.redirection;
                    pm.response.to.not.be.clientError;
                    pm.response.to.not.be.serverError;
                    pm.response.to.not.be.error;
                    pm.response.to.be.withBody;
                    pm.response.to.be.json;
                    pm.response.to.have.body;
                    pm.response.to.have.cookie("session");
                    pm.response.to.not.have.cookie("missing");
                    pm.response.to.have.header("content-type");
                    pm.response.to.have.header("content-type", /json/);
                    pm.response.to.not.have.header("x-missing");
                    pm.response.to.not.have.header("content-type", "text/plain");
                    pm.response.to.have.jsonBody("ok", true);
                    pm.expect(pm.response.headers.get("content-type")).to.include("json");
                    pm.expect(pm.response.headers.toObject()).to.have.property("Content-Type", "application/json");
                    pm.expect(pm.response.cookies.get("SESSION")).to.eql("abc");
                    pm.expect(pm.response.cookies.toObject()).to.have.property("session", "abc");
                    pm.expect(pm.cookies.get("SESSION")).to.eql("abc");
                    pm.expect(pm.cookies.has("missing")).to.be.false;
                    pm.expect(pm.cookies.count()).to.eql(1);
                    pm.expect(pm.cookies.toObject()).to.have.property("session", "abc");
                    pm.expect(Object.isFrozen(pm.cookies)).to.be.true;
                    const requestCookieNames = [];
                    pm.cookies.forEach((cookie) => requestCookieNames.push(cookie.name));
                    pm.expect(requestCookieNames).to.eql(["session"]);
                    const responseCookieNames = [];
                    pm.response.cookies.forEach((cookie) => responseCookieNames.push(cookie.name));
                    pm.expect(responseCookieNames).to.eql(["session"]);
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
        assert!(result.tests[3].passed, "{:?}", result.tests[3].error);
        assert_eq!(result.request["name"], "Scripted");
    }

    #[test]
    fn supports_postman_response_status_categories() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let request = Request::new("Status categories", "GET", "https://example.test");
        let response = HttpResponse {
            status: 302,
            status_text: "Found".to_owned(),
            headers: Vec::new(),
            cookies: Vec::new(),
            body: Vec::new(),
            response_size: 0,
            content_type: None,
            duration_ms: 1,
            ttfb_ms: 0,
            download_ms: 0,
            protocol: "HTTP/1.1".to_owned(),
            url: "https://example.test".to_owned(),
        };
        let result = run_script(
            r#"
                pm.test("redirection", function () {
                    pm.response.to.be.redirection;
                    pm.response.to.not.be.json;
                    pm.response.to.not.be.success;
                    pm.response.to.not.be.error;
                });
            "#,
            &request,
            Some(&response),
            &VariableContext::default(),
        )
        .expect("redirection script");
        assert!(result.tests[0].passed, "{:?}", result.tests[0].error);

        let response = HttpResponse {
            status: 503,
            status_text: "Service Unavailable".to_owned(),
            ..response
        };
        let result = run_script(
            r#"
                pm.test("server error", function () {
                    pm.response.to.be.serverError;
                    pm.response.to.be.error;
                    pm.response.to.not.be.clientError;
                });
            "#,
            &request,
            Some(&response),
            &VariableContext::default(),
        )
        .expect("server error script");
        assert!(result.tests[0].passed, "{:?}", result.tests[0].error);

        let response = HttpResponse {
            status: 404,
            status_text: "Not Found".to_owned(),
            ..response
        };
        let result = run_script(
            r#"
                pm.test("client error", function () {
                    pm.response.to.be.clientError;
                    pm.response.to.be.error;
                    pm.response.to.not.be.serverError;
                });
            "#,
            &request,
            Some(&response),
            &VariableContext::default(),
        )
        .expect("client error script");
        assert!(result.tests[0].passed, "{:?}", result.tests[0].error);
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
                    pm.expect(pm.iterationData.toObject()).to.have.property("trace", "iteration-42");
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
        let error =
            wait_for_child(child, || false).expect_err("long-lived child should be terminated");
        assert!(matches!(error, ScriptError::Timeout { .. }));
    }

    #[test]
    fn cancels_a_running_node_process() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };

        let cancelled = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancelled);
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            trigger.store(true, Ordering::Release);
        });
        let request = Request::new("Cancellable script", "GET", "https://example.test");
        let error = run_script_with_cancellation(
            "while (true) {}",
            &request,
            None,
            &VariableContext::default(),
            || cancelled.load(Ordering::Acquire),
        )
        .expect_err("cancelled script should terminate");
        thread.join().expect("cancellation trigger");
        assert!(matches!(error, ScriptError::Cancelled));
    }
}
