use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::{HeaderEntry, Request, RequestBody};

#[derive(Debug, Error)]
pub enum GraphqlError {
    #[error("GraphQL query cannot be empty")]
    EmptyQuery,
    #[error("GraphQL query has unbalanced braces")]
    UnbalancedBraces,
    #[error("GraphQL variables must be a JSON object")]
    VariablesMustBeObject,
    #[error("invalid GraphQL variables JSON: {0}")]
    InvalidVariables(#[from] serde_json::Error),
    #[error("invalid GraphQL response JSON: {0}")]
    InvalidResponse(serde_json::Error),
    #[error("GraphQL response must be a JSON object")]
    InvalidResponseEnvelope,
    #[error("GraphQL response errors must be an array")]
    InvalidErrorsField,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphqlRequest {
    pub endpoint: String,
    pub query: String,
    #[serde(default)]
    pub variables: Value,
    #[serde(default)]
    pub operation_name: Option<String>,
}

impl GraphqlRequest {
    pub fn new(endpoint: impl Into<String>, query: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            query: query.into(),
            variables: Value::Object(Map::new()),
            operation_name: None,
        }
    }

    pub fn validate(&self) -> Result<(), GraphqlError> {
        validate_query(&self.query)
    }

    pub fn into_http_request(self, name: impl Into<String>) -> Result<Request, GraphqlError> {
        self.validate()?;
        if !self.variables.is_object() && !self.variables.is_null() {
            return Err(GraphqlError::VariablesMustBeObject);
        }
        let mut request = Request::new(name, "POST", self.endpoint);
        request
            .headers
            .push(HeaderEntry::enabled("content-type", "application/json"));
        request.body = RequestBody::Graphql {
            query: self.query,
            variables: if self.variables.is_null() {
                Value::Object(Map::new())
            } else {
                self.variables
            },
            operation_name: self.operation_name,
        };
        Ok(request)
    }
}

pub fn validate_query(query: &str) -> Result<(), GraphqlError> {
    if query.trim().is_empty() {
        return Err(GraphqlError::EmptyQuery);
    }
    let mut depth = 0_i32;
    let mut in_string = false;
    let mut escaped = false;
    let mut has_selection = false;
    for character in query.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '{' => {
                depth += 1;
                has_selection = true;
            }
            '}' => {
                depth -= 1;
                if depth < 0 {
                    return Err(GraphqlError::UnbalancedBraces);
                }
            }
            _ => {}
        }
    }
    if depth != 0 || in_string || !has_selection {
        Err(GraphqlError::UnbalancedBraces)
    } else {
        Ok(())
    }
}

pub fn parse_variables_json(input: &str) -> Result<Value, GraphqlError> {
    let value: Value = serde_json::from_str(input)?;
    if value.is_null() {
        Ok(Value::Object(Map::new()))
    } else if value.is_object() {
        Ok(value)
    } else {
        Err(GraphqlError::VariablesMustBeObject)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphqlResponse {
    #[serde(default)]
    pub data: Option<Value>,
    #[serde(default)]
    pub errors: Vec<Value>,
    #[serde(default)]
    pub extensions: Option<Value>,
}

impl GraphqlResponse {
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    pub fn error_messages(&self) -> Vec<String> {
        self.errors
            .iter()
            .filter_map(|error| {
                error
                    .get("message")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .collect()
    }
}

pub fn parse_response(body: &str) -> Result<GraphqlResponse, GraphqlError> {
    let value = serde_json::from_str::<Value>(body).map_err(GraphqlError::InvalidResponse)?;
    let object = value
        .as_object()
        .ok_or(GraphqlError::InvalidResponseEnvelope)?;
    let errors = match object.get("errors") {
        None => Vec::new(),
        Some(Value::Array(errors)) => errors.clone(),
        Some(_) => return Err(GraphqlError::InvalidErrorsField),
    };
    Ok(GraphqlResponse {
        data: object.get("data").cloned(),
        errors,
        extensions: object.get("extensions").cloned(),
    })
}

pub fn introspection_query() -> &'static str {
    "query PostlyIntrospection { __schema { queryType { name } mutationType { name } subscriptionType { name } } }"
}

pub fn variables_from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Value {
    Value::Object(
        pairs
            .into_iter()
            .collect::<BTreeMap<_, _>>()
            .into_iter()
            .map(|(key, value)| (key, Value::String(value)))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_first_class_graphql_http_request() {
        let mut request = GraphqlRequest::new(
            "https://api.example.test/graphql",
            "query User($id: ID!) { user(id: $id) { name } }",
        );
        request.variables = parse_variables_json(r#"{"id":"42"}"#).expect("variables");
        request.operation_name = Some("User".to_owned());
        let request = request.into_http_request("Get user").expect("request");
        assert_eq!(request.method, "POST");
        assert!(matches!(request.body, RequestBody::Graphql { .. }));
        assert_eq!(request.headers[0].key, "content-type");
    }

    #[test]
    fn parses_graphql_partial_data_and_errors() {
        let response = parse_response(
            r#"{"data":{"user":null},"errors":[{"message":"not found"}],"extensions":{"trace":"x"}}"#,
        )
        .expect("response");
        assert!(response.has_errors());
        assert_eq!(response.error_messages(), vec!["not found"]);
        assert_eq!(response.data.expect("data")["user"], Value::Null);
    }

    #[test]
    fn rejects_empty_unbalanced_and_non_object_variables() {
        assert!(matches!(validate_query(""), Err(GraphqlError::EmptyQuery)));
        assert!(matches!(
            validate_query("query { user "),
            Err(GraphqlError::UnbalancedBraces)
        ));
        assert!(matches!(
            parse_variables_json("[1, 2]"),
            Err(GraphqlError::VariablesMustBeObject)
        ));
    }
}
