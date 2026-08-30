use std::{
    io::Write,
    process::{Command, Stdio},
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
    pub runtime_updates: Variables,
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
        context.environment.extend(
            self.environment_updates
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        context.collection.extend(
            self.collection_updates
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        context.runtime.extend(
            self.runtime_updates
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        *request =
            serde_json::from_value(self.request.clone()).map_err(ScriptError::InvalidRequest)?;
        Ok(())
    }
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
    runtime: Variables,
}

pub fn run_script(
    script: &str,
    request: &Request,
    response: Option<&HttpResponse>,
    context: &VariableContext,
) -> Result<ScriptResult, ScriptError> {
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
    let mut child = Command::new("node")
        .args(["--input-type=commonjs", "-e", NODE_HARNESS])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(ScriptError::NodeUnavailable)?;
    child
        .stdin
        .take()
        .expect("script child stdin was requested")
        .write_all(&payload)
        .map_err(ScriptError::NodeUnavailable)?;
    let output = child
        .wait_with_output()
        .map_err(ScriptError::NodeUnavailable)?;
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
        runtime_updates: node_output.changes.runtime,
        tests: node_output.tests,
        logs: node_output.logs,
    })
}

const NODE_HARNESS: &str = r##"
const vm = require("node:vm");
const fs = require("node:fs");
const input = JSON.parse(fs.readFileSync(0, "utf8"));
const changes = { environment: {}, collection: {}, runtime: {} };
const tests = [];
const logs = [];
const values = input.variables || {};

function text(value) {
  return value === undefined || value === null ? "" : String(value);
}

function visibleGet(key) {
  for (const scope of ["runtime", "request", "environment", "collection", "project", "globals"]) {
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
    set: (key, value) => {
      values[name][key] = text(value);
      changes[name][key] = text(value);
    },
    unset: (key) => {
      delete values[name][key];
      delete changes[name][key];
    },
    replaceIn
  };
}

const environment = scope("environment");
const collectionVariables = scope("collection");
const runtime = {
  get: visibleGet,
  set: (key, value) => {
    values.runtime = values.runtime || {};
    values.runtime[key] = text(value);
    changes.runtime[key] = text(value);
  },
  unset: (key) => {
    if (values.runtime) delete values.runtime[key];
    delete changes.runtime[key];
  },
  replaceIn
};

const request = input.request || {};
const requestHeaders = request.headers || [];
requestHeaders.get = (name) => {
  const found = requestHeaders.find((header) => header.key.toLowerCase() === text(name).toLowerCase() && header.enabled !== false);
  return found ? found.value : undefined;
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
  log: (...args) => logs.push({ level: "log", message: args.map(text).join(" ") }),
  warn: (...args) => logs.push({ level: "warn", message: args.map(text).join(" ") }),
  error: (...args) => logs.push({ level: "error", message: args.map(text).join(" ") })
};

const pm = {
  environment,
  collectionVariables,
  variables: runtime,
  request,
  response,
  test: (name, callback) => {
    try {
      callback();
      tests.push({ name: text(name), passed: true });
    } catch (error) {
      tests.push({ name: text(name), passed: false, error: text(error && (error.stack || error.message || error)) });
    }
  },
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
process.stdout.write(JSON.stringify({ request, changes, tests, logs }));
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
}
