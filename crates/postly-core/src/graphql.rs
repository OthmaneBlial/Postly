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
    #[error("GraphQL introspection response contains errors: {0}")]
    IntrospectionErrors(String),
    #[error("GraphQL introspection response does not contain __schema")]
    SchemaMissing,
    #[error("GraphQL introspection schema is malformed: {0}")]
    InvalidSchema(String),
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

/// A compact, UI- and CLI-friendly representation of a GraphQL schema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphqlSchema {
    pub query_type: Option<String>,
    pub mutation_type: Option<String>,
    pub subscription_type: Option<String>,
    pub types: Vec<GraphqlType>,
}

impl GraphqlSchema {
    pub fn named_type(&self, name: &str) -> Option<&GraphqlType> {
        self.types
            .iter()
            .find(|graphql_type| graphql_type.name == name)
    }

    pub fn object_types(&self) -> impl Iterator<Item = &GraphqlType> {
        self.types
            .iter()
            .filter(|graphql_type| graphql_type.kind == "OBJECT")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphqlType {
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub fields: Vec<GraphqlField>,
    #[serde(default)]
    pub input_fields: Vec<GraphqlInputField>,
    #[serde(default)]
    pub enum_values: Vec<GraphqlEnumValue>,
    #[serde(default)]
    pub possible_types: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphqlField {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub type_name: String,
    #[serde(default)]
    pub arguments: Vec<GraphqlArgument>,
    #[serde(default)]
    pub deprecated: bool,
    #[serde(default)]
    pub deprecation_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphqlArgument {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub type_name: String,
    #[serde(default)]
    pub default_value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphqlInputField {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub type_name: String,
    #[serde(default)]
    pub default_value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphqlEnumValue {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub deprecated: bool,
    #[serde(default)]
    pub deprecation_reason: Option<String>,
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

/// Parse the __schema object returned by a GraphQL introspection query.
pub fn parse_schema(response: &GraphqlResponse) -> Result<GraphqlSchema, GraphqlError> {
    if response.has_errors() {
        let detail = response.error_messages().join("; ");
        let detail = if detail.is_empty() {
            format!("{} GraphQL error(s)", response.errors.len())
        } else {
            detail
        };
        return Err(GraphqlError::IntrospectionErrors(detail));
    }
    let schema = response
        .data
        .as_ref()
        .and_then(|data| data.get("__schema"))
        .ok_or(GraphqlError::SchemaMissing)?;
    let types = schema
        .get("types")
        .and_then(Value::as_array)
        .ok_or_else(|| GraphqlError::InvalidSchema("__schema.types must be an array".to_owned()))?;
    let mut parsed_types = types
        .iter()
        .filter_map(|value| parse_type(value).transpose())
        .collect::<Result<Vec<_>, _>>()?;
    parsed_types.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(GraphqlSchema {
        query_type: root_type_name(schema, "queryType")?,
        mutation_type: root_type_name(schema, "mutationType")?,
        subscription_type: root_type_name(schema, "subscriptionType")?,
        types: parsed_types,
    })
}

fn root_type_name(schema: &Value, key: &str) -> Result<Option<String>, GraphqlError> {
    let value = schema
        .get(key)
        .ok_or_else(|| GraphqlError::InvalidSchema(format!("__schema is missing {key}")))?;
    if value.is_null() {
        return Ok(None);
    }
    value
        .get("name")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| GraphqlError::InvalidSchema(format!("{key}.name must be a string")))
        .map(Some)
}

fn parse_type(value: &Value) -> Result<Option<GraphqlType>, GraphqlError> {
    let Some(name) = value.get("name").and_then(Value::as_str) else {
        return Ok(None);
    };
    let kind = match value.get("kind").and_then(Value::as_str) {
        Some(kind) => kind.to_owned(),
        None => {
            return Err(GraphqlError::InvalidSchema(format!(
                "type {name} is missing kind"
            )))
        }
    };
    let fields = parse_fields(value.get("fields"))
        .transpose()?
        .unwrap_or_default();
    let input_fields = parse_input_fields(value.get("inputFields"))
        .transpose()?
        .unwrap_or_default();
    let enum_values = parse_enum_values(value.get("enumValues"))
        .transpose()?
        .unwrap_or_default();
    let possible_types = parse_named_type_names(value.get("possibleTypes"))
        .transpose()?
        .unwrap_or_default();
    Ok(Some(GraphqlType {
        kind,
        name: name.to_owned(),
        description: optional_string(value.get("description")),
        fields,
        input_fields,
        enum_values,
        possible_types,
    }))
}

fn parse_fields(value: Option<&Value>) -> Option<Result<Vec<GraphqlField>, GraphqlError>> {
    let Some(value) = value else {
        return Some(Ok(Vec::new()));
    };
    if value.is_null() {
        return Some(Ok(Vec::new()));
    }
    let Some(values) = value.as_array() else {
        return Some(Err(GraphqlError::InvalidSchema(
            "type.fields must be an array or null".to_owned(),
        )));
    };
    Some(
        values
            .iter()
            .map(parse_field)
            .collect::<Result<Vec<_>, _>>(),
    )
}

fn parse_field(value: &Value) -> Result<GraphqlField, GraphqlError> {
    let name = required_string(value, "name", "field")?;
    let type_name =
        type_ref_display(value.get("type").ok_or_else(|| {
            GraphqlError::InvalidSchema(format!("field {name} is missing type"))
        })?)?;
    let arguments = value
        .get("args")
        .map(parse_arguments)
        .transpose()?
        .unwrap_or_default();
    Ok(GraphqlField {
        name,
        description: optional_string(value.get("description")),
        type_name,
        arguments,
        deprecated: value
            .get("isDeprecated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        deprecation_reason: optional_string(value.get("deprecationReason")),
    })
}

fn parse_arguments(value: &Value) -> Result<Vec<GraphqlArgument>, GraphqlError> {
    let values = value
        .as_array()
        .ok_or_else(|| GraphqlError::InvalidSchema("field.args must be an array".to_owned()))?;
    values
        .iter()
        .map(|value| {
            let name = required_string(value, "name", "argument")?;
            let type_name = type_ref_display(value.get("type").ok_or_else(|| {
                GraphqlError::InvalidSchema(format!("argument {name} is missing type"))
            })?)?;
            Ok(GraphqlArgument {
                name,
                description: optional_string(value.get("description")),
                type_name,
                default_value: optional_string(value.get("defaultValue")),
            })
        })
        .collect()
}

fn parse_input_fields(
    value: Option<&Value>,
) -> Option<Result<Vec<GraphqlInputField>, GraphqlError>> {
    let Some(value) = value else {
        return Some(Ok(Vec::new()));
    };
    if value.is_null() {
        return Some(Ok(Vec::new()));
    }
    let Some(values) = value.as_array() else {
        return Some(Err(GraphqlError::InvalidSchema(
            "type.inputFields must be an array or null".to_owned(),
        )));
    };
    Some(
        values
            .iter()
            .map(|value| {
                let name = required_string(value, "name", "input field")?;
                let type_name = type_ref_display(value.get("type").ok_or_else(|| {
                    GraphqlError::InvalidSchema(format!("input field {name} is missing type"))
                })?)?;
                Ok(GraphqlInputField {
                    name,
                    description: optional_string(value.get("description")),
                    type_name,
                    default_value: optional_string(value.get("defaultValue")),
                })
            })
            .collect::<Result<Vec<_>, _>>(),
    )
}

fn parse_enum_values(value: Option<&Value>) -> Option<Result<Vec<GraphqlEnumValue>, GraphqlError>> {
    let Some(value) = value else {
        return Some(Ok(Vec::new()));
    };
    if value.is_null() {
        return Some(Ok(Vec::new()));
    }
    let Some(values) = value.as_array() else {
        return Some(Err(GraphqlError::InvalidSchema(
            "type.enumValues must be an array or null".to_owned(),
        )));
    };
    Some(
        values
            .iter()
            .map(|value| {
                Ok(GraphqlEnumValue {
                    name: required_string(value, "name", "enum value")?,
                    description: optional_string(value.get("description")),
                    deprecated: value
                        .get("isDeprecated")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    deprecation_reason: optional_string(value.get("deprecationReason")),
                })
            })
            .collect::<Result<Vec<_>, _>>(),
    )
}

fn parse_named_type_names(value: Option<&Value>) -> Option<Result<Vec<String>, GraphqlError>> {
    let Some(value) = value else {
        return Some(Ok(Vec::new()));
    };
    if value.is_null() {
        return Some(Ok(Vec::new()));
    }
    let Some(values) = value.as_array() else {
        return Some(Err(GraphqlError::InvalidSchema(
            "type.possibleTypes must be an array or null".to_owned(),
        )));
    };
    Some(
        values
            .iter()
            .map(|value| required_string(value, "name", "possible type"))
            .collect(),
    )
}

fn required_string(value: &Value, key: &str, subject: &str) -> Result<String, GraphqlError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| GraphqlError::InvalidSchema(format!("{subject} is missing {key}")))
}

fn optional_string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(ToOwned::to_owned)
}

fn type_ref_display(value: &Value) -> Result<String, GraphqlError> {
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| GraphqlError::InvalidSchema("type reference is missing kind".to_owned()))?;
    match kind {
        "NON_NULL" => Ok(format!(
            "{}!",
            type_ref_display(value.get("ofType").ok_or_else(|| {
                GraphqlError::InvalidSchema("NON_NULL type reference is missing ofType".to_owned())
            })?)?
        )),
        "LIST" => Ok(format!(
            "[{}]",
            type_ref_display(value.get("ofType").ok_or_else(|| {
                GraphqlError::InvalidSchema("LIST type reference is missing ofType".to_owned())
            })?)?
        )),
        "SCALAR" | "OBJECT" | "INTERFACE" | "UNION" | "ENUM" | "INPUT_OBJECT" => value
            .get("name")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                GraphqlError::InvalidSchema(format!("{kind} type reference is missing name"))
            }),
        other => Err(GraphqlError::InvalidSchema(format!(
            "unknown GraphQL type reference kind {other}"
        ))),
    }
}

pub fn introspection_query() -> &'static str {
    "query PostlyIntrospection { __schema { queryType { name } mutationType { name } subscriptionType { name } } }"
}

/// Query the complete schema shape needed by the local explorer.
pub fn schema_introspection_query() -> &'static str {
    "query PostlySchemaIntrospection { __schema { queryType { name } mutationType { name } subscriptionType { name } types { kind name description fields(includeDeprecated: true) { name description args { name description type { ...PostlyTypeRef } defaultValue } type { ...PostlyTypeRef } isDeprecated deprecationReason } inputFields { name description type { ...PostlyTypeRef } defaultValue } enumValues(includeDeprecated: true) { name description isDeprecated deprecationReason } possibleTypes { name } } } } fragment PostlyTypeRef on __Type { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } } }"
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

    #[test]
    fn parses_schema_roots_fields_arguments_and_nested_type_references() {
        let response = parse_response(
            r#"{
                "data": {
                    "__schema": {
                        "queryType": {"name": "Query"},
                        "mutationType": null,
                        "subscriptionType": {"name": "Subscription"},
                        "types": [
                            {"kind":"SCALAR","name":"String","description":null,"fields":null,"inputFields":null,"enumValues":null,"possibleTypes":null},
                            {"kind":"OBJECT","name":"Query","description":"Root query","fields":[{"name":"users","description":"List users","args":[{"name":"limit","description":null,"type":{"kind":"NON_NULL","name":null,"ofType":{"kind":"SCALAR","name":"Int","ofType":null}},"defaultValue":"10"}],"type":{"kind":"NON_NULL","name":null,"ofType":{"kind":"LIST","name":null,"ofType":{"kind":"NON_NULL","name":null,"ofType":{"kind":"OBJECT","name":"User","ofType":null}}}},"isDeprecated":false,"deprecationReason":null}],"inputFields":null,"enumValues":null,"possibleTypes":null},
                            {"kind":"OBJECT","name":"User","description":null,"fields":[],"inputFields":null,"enumValues":null,"possibleTypes":null}
                        ]
                    }
                }
            }"#,
        )
        .expect("response");
        let schema = parse_schema(&response).expect("schema");
        assert_eq!(schema.query_type.as_deref(), Some("Query"));
        assert_eq!(schema.subscription_type.as_deref(), Some("Subscription"));
        assert!(schema.mutation_type.is_none());
        let query = schema.named_type("Query").expect("query type");
        assert_eq!(query.fields[0].type_name, "[User!]!");
        assert_eq!(query.fields[0].arguments[0].type_name, "Int!");
        assert_eq!(
            query.fields[0].arguments[0].default_value.as_deref(),
            Some("10")
        );
        assert_eq!(schema.object_types().count(), 2);
    }

    #[test]
    fn rejects_incomplete_introspection_data() {
        let response = parse_response(r#"{"data":{"__schema":{"types":[]}}}"#).expect("response");
        assert!(matches!(
            parse_schema(&response),
            Err(GraphqlError::InvalidSchema(_))
        ));
    }
}
