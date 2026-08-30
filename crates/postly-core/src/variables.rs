use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::Variables;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VariableContext {
    #[serde(default)]
    pub iteration: Variables,
    #[serde(default)]
    pub runtime: Variables,
    #[serde(default)]
    pub request: Variables,
    #[serde(default)]
    pub environment: Variables,
    #[serde(default)]
    pub collection: Variables,
    #[serde(default)]
    pub project: Variables,
    #[serde(default)]
    pub globals: Variables,
}

impl VariableContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_environment(mut self, variables: Variables) -> Self {
        self.environment = variables;
        self
    }

    pub fn with_collection(mut self, variables: Variables) -> Self {
        self.collection = variables;
        self
    }

    pub fn with_runtime(mut self, variables: Variables) -> Self {
        self.runtime = variables;
        self
    }

    pub fn with_iteration(mut self, variables: Variables) -> Self {
        self.iteration = variables;
        self
    }

    pub fn set_runtime(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.runtime.insert(key.into(), value.into());
    }

    pub fn resolve(&self, input: &str) -> ResolvedText {
        resolve_text(input, self)
    }

    pub fn visible_values(&self) -> Variables {
        let mut merged = BTreeMap::new();
        for source in [
            &self.globals,
            &self.project,
            &self.collection,
            &self.environment,
            &self.request,
            &self.runtime,
            &self.iteration,
        ] {
            merged.extend(
                source
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
        }
        merged
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedText {
    pub value: String,
    pub diagnostics: Vec<VariableDiagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VariableDiagnostic {
    pub name: String,
    pub kind: VariableDiagnosticKind,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum VariableDiagnosticKind {
    Undefined,
    Cyclic,
    Malformed,
}

fn resolve_text(input: &str, context: &VariableContext) -> ResolvedText {
    let mut current = input.to_owned();
    let mut diagnostics = Vec::new();
    let mut seen = Vec::new();

    for _ in 0..12 {
        let (next, names, malformed) = substitute_once(&current, context);
        for name in malformed {
            diagnostics.push(VariableDiagnostic {
                name,
                kind: VariableDiagnosticKind::Malformed,
                message: "Variable expression is missing its closing `}}`.".to_owned(),
            });
        }
        for name in names {
            if lookup(context, &name).is_none()
                && !diagnostics.iter().any(|diagnostic| {
                    diagnostic.kind == VariableDiagnosticKind::Undefined && diagnostic.name == name
                })
            {
                diagnostics.push(VariableDiagnostic {
                    name: name.clone(),
                    kind: VariableDiagnosticKind::Undefined,
                    message: format!("Variable {name} is not defined in the active scopes."),
                });
            }
            if seen.iter().any(|previous| previous == &name)
                && next.contains(&format!("{{{{{name}}}}}"))
            {
                diagnostics.push(VariableDiagnostic {
                    name: name.clone(),
                    kind: VariableDiagnosticKind::Cyclic,
                    message: format!("Variable `{name}` refers to itself or a cycle."),
                });
            }
            seen.push(name);
        }
        if next == current {
            break;
        }
        current = next;
    }

    ResolvedText {
        value: current,
        diagnostics,
    }
}

fn substitute_once(input: &str, context: &VariableContext) -> (String, Vec<String>, Vec<String>) {
    let mut output = String::with_capacity(input.len());
    let mut names = Vec::new();
    let mut malformed = Vec::new();
    let mut cursor = 0;

    while let Some(relative_start) = input[cursor..].find("{{") {
        let start = cursor + relative_start;
        output.push_str(&input[cursor..start]);
        let expression_start = start + 2;
        let Some(relative_end) = input[expression_start..].find("}}") else {
            output.push_str(&input[start..]);
            malformed.push(input[expression_start..].trim().to_owned());
            cursor = input.len();
            break;
        };
        let end = expression_start + relative_end;
        let name = input[expression_start..end].trim().to_owned();
        if name.is_empty() {
            output.push_str(&input[start..end + 2]);
            malformed.push(name);
        } else if let Some(value) = lookup(context, &name) {
            output.push_str(value);
            names.push(name);
        } else {
            output.push_str(&input[start..end + 2]);
            names.push(name.clone());
        }
        cursor = end + 2;
    }

    if cursor < input.len() {
        output.push_str(&input[cursor..]);
    }
    (output, names, malformed)
}

fn lookup<'a>(context: &'a VariableContext, name: &str) -> Option<&'a str> {
    [
        &context.iteration,
        &context.runtime,
        &context.request,
        &context.environment,
        &context.collection,
        &context.project,
        &context.globals,
    ]
    .into_iter()
    .find_map(|source| source.get(name).map(String::as_str))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_with_postman_like_precedence() {
        let mut context = VariableContext::new();
        context
            .collection
            .insert("baseUrl".into(), "https://collection".into());
        context
            .environment
            .insert("baseUrl".into(), "https://environment".into());
        context
            .runtime
            .insert("baseUrl".into(), "https://runtime".into());

        let resolved = context.resolve("{{baseUrl}}/users");

        assert_eq!(resolved.value, "https://runtime/users");
        assert!(resolved.diagnostics.is_empty());
    }

    #[test]
    fn reports_undefined_variables_without_destroying_the_template() {
        let resolved = VariableContext::new().resolve("{{missing}}/users");

        assert_eq!(resolved.value, "{{missing}}/users");
        assert_eq!(
            resolved.diagnostics[0].kind,
            VariableDiagnosticKind::Undefined
        );
    }

    #[test]
    fn resolves_nested_variables() {
        let mut context = VariableContext::new();
        context
            .environment
            .insert("host".into(), "api.example.com".into());
        context
            .environment
            .insert("baseUrl".into(), "https://{{host}}".into());

        assert_eq!(
            context.resolve("{{baseUrl}}/health").value,
            "https://api.example.com/health"
        );
    }

    #[test]
    fn resolves_iteration_data_before_runtime_values() {
        let mut context = VariableContext::new();
        context.runtime.insert("id".into(), "runtime".into());
        context.iteration.insert("id".into(), "iteration".into());

        assert_eq!(context.resolve("{{id}}").value, "iteration");
    }
}
