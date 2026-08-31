use std::path::PathBuf;

use serde::Serialize;
use thiserror::Error;

use crate::{
    model::{Auth, HeaderEntry, Request, RequestBody},
    storage::{Workspace, WorkspaceError},
};

#[derive(Debug, Error)]
pub enum CurlParseError {
    #[error("the command must start with curl")]
    MissingCurl,
    #[error("curl option {option} is missing its value")]
    MissingOptionValue { option: String },
    #[error("curl command contains an invalid header: {0}")]
    InvalidHeader(String),
    #[error("curl basic auth must use username:password syntax")]
    InvalidBasicAuth,
    #[error("curl command does not contain a URL")]
    MissingUrl,
    #[error("unsupported curl option {0}")]
    UnsupportedOption(String),
}

#[derive(Debug, Error)]
pub enum CurlImportError {
    #[error("could not parse curl command: {0}")]
    Parse(#[from] CurlParseError),
    #[error("workspace error: {0}")]
    Workspace(#[from] WorkspaceError),
    #[error("could not read request file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct CurlImportResult {
    pub path: PathBuf,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CurlExportResult {
    pub command: String,
    pub warnings: Vec<String>,
}

/// Convert a native request into a reviewable POSIX-shell cURL command.
///
/// Values are quoted for a shell, but variable placeholders remain visible so
/// the copied command can still be reviewed and adapted before execution.
pub fn export_curl_command(request: &Request) -> CurlExportResult {
    let mut arguments = vec!["curl".to_owned()];
    let mut warnings = Vec::new();
    if request.grpc.is_some() {
        warnings.push(
            "gRPC requests are exported as an HTTP-shaped cURL preview, not a protobuf call."
                .to_owned(),
        );
    }
    let url = append_query_parameters(&request.url, &request.query);
    arguments.push("--request".to_owned());
    arguments.push(shell_quote(&request.method));
    arguments.push(shell_quote(&url));

    for header in request.headers.iter().filter(|header| header.enabled) {
        arguments.push("--header".to_owned());
        arguments.push(shell_quote(&format!("{}: {}", header.key, header.value)));
    }
    if !has_header(&request.headers, "content-type") {
        if let Some(content_type) = body_content_type(&request.body) {
            arguments.push("--header".to_owned());
            arguments.push(shell_quote(&format!("Content-Type: {content_type}")));
        }
    }
    for cookie in request.cookies.iter().filter(|cookie| cookie.enabled) {
        arguments.push("--cookie".to_owned());
        arguments.push(shell_quote(&format!("{}={}", cookie.key, cookie.value)));
    }

    match &request.auth {
        Auth::None => {}
        Auth::Basic { username, password } => {
            arguments.push("--user".to_owned());
            arguments.push(shell_quote(&format!("{username}:{password}")));
        }
        Auth::Digest { username, password } => {
            arguments.push("--digest".to_owned());
            arguments.push("--user".to_owned());
            arguments.push(shell_quote(&format!("{username}:{password}")));
        }
        Auth::Bearer { token } => {
            arguments.push("--header".to_owned());
            arguments.push(shell_quote(&format!("Authorization: Bearer {token}")));
        }
        Auth::ApiKey {
            key,
            value,
            location: crate::model::ApiKeyLocation::Header,
        } => {
            arguments.push("--header".to_owned());
            arguments.push(shell_quote(&format!("{key}: {value}")));
        }
        Auth::ApiKey {
            key,
            value,
            location: crate::model::ApiKeyLocation::Query,
        } => {
            warnings.push(
                "API-key query auth is appended without resolving variable placeholders."
                    .to_owned(),
            );
            arguments[3] = shell_quote(&append_query_parameters(
                &url,
                &[crate::model::KeyValue::enabled(key, value)],
            ));
        }
        Auth::OAuth2ClientCredentials { .. } => {
            warnings.push(
                "OAuth 2.0 client credentials were not materialized; fetch a token before running the copied command."
                    .to_owned(),
            );
        }
        Auth::OAuth2AuthorizationCodePkce { .. } => {
            warnings.push(
                "OAuth 2.0 authorization code + PKCE was not materialized; exchange the code before running the copied command."
                    .to_owned(),
            );
        }
        Auth::OAuth2RefreshToken { .. } => {
            warnings.push(
                "OAuth 2.0 refresh-token auth was not materialized; fetch a token before running the copied command."
                    .to_owned(),
            );
        }
        Auth::OAuth2DeviceCode { .. } => {
            warnings.push(
                "OAuth 2.0 device-code auth was not materialized; complete device authorization in Postly before running the copied command."
                    .to_owned(),
            );
        }
        Auth::AwsSignatureV4 { .. } => {
            warnings.push(
                "AWS Signature V4 was not materialized; run the signed request through Postly or an AWS SDK."
                    .to_owned(),
            );
        }
    }

    match &request.body {
        RequestBody::None => {}
        RequestBody::Raw { text, .. } => {
            arguments.push("--data-raw".to_owned());
            arguments.push(shell_quote(text));
        }
        RequestBody::Json { value } => {
            arguments.push("--data-raw".to_owned());
            arguments.push(shell_quote(
                &serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned()),
            ));
        }
        RequestBody::Graphql {
            query,
            variables,
            operation_name,
        } => {
            let mut payload = serde_json::Map::new();
            payload.insert("query".to_owned(), serde_json::Value::String(query.clone()));
            payload.insert("variables".to_owned(), variables.clone());
            if let Some(operation_name) = operation_name {
                payload.insert(
                    "operationName".to_owned(),
                    serde_json::Value::String(operation_name.clone()),
                );
            }
            arguments.push("--data-raw".to_owned());
            arguments.push(shell_quote(
                &serde_json::to_string(&serde_json::Value::Object(payload))
                    .unwrap_or_else(|_| "{}".to_owned()),
            ));
        }
        RequestBody::FormUrlEncoded { fields } => {
            for field in fields.iter().filter(|field| field.enabled) {
                arguments.push("--data-urlencode".to_owned());
                arguments.push(shell_quote(&format!("{}={}", field.key, field.value)));
            }
        }
        RequestBody::Multipart { parts } => {
            for part in parts.iter().filter(|part| part.enabled) {
                let value = if let Some(path) = &part.file_path {
                    let content_type = part
                        .content_type
                        .as_deref()
                        .map(|content_type| format!(";type={content_type}"))
                        .unwrap_or_default();
                    format!("{}=@{}{}", part.name, path, content_type)
                } else {
                    format!("{}={}", part.name, part.value)
                };
                arguments.push("--form".to_owned());
                arguments.push(shell_quote(&value));
            }
        }
        RequestBody::BinaryFile { path, .. } => {
            arguments.push("--data-binary".to_owned());
            arguments.push(shell_quote(&format!("@{path}")));
        }
    }

    CurlExportResult {
        command: arguments.join(" "),
        warnings,
    }
}

fn append_query_parameters(url: &str, pairs: &[crate::model::KeyValue]) -> String {
    let query = pairs
        .iter()
        .filter(|pair| pair.enabled)
        .fold(
            url::form_urlencoded::Serializer::new(String::new()),
            |mut query, pair| {
                query.append_pair(&pair.key, &pair.value);
                query
            },
        )
        .finish();
    if query.is_empty() {
        return url.to_owned();
    }
    format!(
        "{url}{}{}",
        if url.contains('?') { '&' } else { '?' },
        query
    )
}

fn has_header(headers: &[HeaderEntry], name: &str) -> bool {
    headers
        .iter()
        .any(|header| header.enabled && header.key.eq_ignore_ascii_case(name))
}

fn body_content_type(body: &RequestBody) -> Option<&'static str> {
    match body {
        RequestBody::Json { .. } | RequestBody::Graphql { .. } => Some("application/json"),
        RequestBody::FormUrlEncoded { .. } => Some("application/x-www-form-urlencoded"),
        RequestBody::Multipart { .. } => None,
        RequestBody::Raw { .. } | RequestBody::BinaryFile { .. } | RequestBody::None => None,
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn parse_curl_command(command: &str) -> Result<(Request, Vec<String>), CurlParseError> {
    let tokens = shell_words(command)?;
    if tokens.first().map(String::as_str) != Some("curl") {
        return Err(CurlParseError::MissingCurl);
    }
    let mut method = None;
    let mut url = None;
    let mut headers = Vec::new();
    let mut body_values = Vec::new();
    let mut auth = Auth::None;
    let mut cookies = None;
    let mut warnings = Vec::new();
    let mut get_mode = false;
    let mut index = 1;

    while index < tokens.len() {
        let token = &tokens[index];
        if token == "--" {
            index += 1;
            if index < tokens.len() {
                url = Some(tokens[index].clone());
            }
            break;
        }
        if token == "-G" || token == "--get" {
            get_mode = true;
            index += 1;
            continue;
        }
        if token == "-k" || token == "--insecure" {
            warnings.push(
                "curl --insecure is preserved as a warning; review TLS settings before sending."
                    .to_owned(),
            );
            index += 1;
            continue;
        }
        if token == "-L" || token == "--location" || token == "--compressed" {
            warnings.push(format!(
                "curl option {token} is handled by Postly defaults or the HTTP engine."
            ));
            index += 1;
            continue;
        }
        if let Some(value) = inline_option_value(token, "--request", "-X") {
            method = Some(value.to_owned());
            index += 1;
            continue;
        }
        if let Some(value) = inline_option_value(token, "--header", "-H") {
            headers.push(parse_header(value)?);
            index += 1;
            continue;
        }
        if let Some(value) = inline_option_value(token, "--data", "-d")
            .or_else(|| inline_option_value(token, "--data-raw", "--data-raw"))
            .or_else(|| inline_option_value(token, "--data-binary", "--data-binary"))
        {
            body_values.push(value.to_owned());
            index += 1;
            continue;
        }
        if let Some(value) = inline_option_value(token, "--url", "--url") {
            url = Some(value.to_owned());
            index += 1;
            continue;
        }
        let needs_value = matches!(
            token.as_str(),
            "-X" | "--request"
                | "-H"
                | "--header"
                | "-d"
                | "--data"
                | "--data-raw"
                | "--data-binary"
                | "--data-urlencode"
                | "-u"
                | "--user"
                | "-b"
                | "--cookie"
                | "-A"
                | "--user-agent"
                | "-e"
                | "--referer"
                | "-F"
                | "--form"
        );
        if needs_value {
            let value =
                tokens
                    .get(index + 1)
                    .ok_or_else(|| CurlParseError::MissingOptionValue {
                        option: token.clone(),
                    })?;
            match token.as_str() {
                "-X" | "--request" => method = Some(value.clone()),
                "-H" | "--header" => headers.push(parse_header(value)?),
                "-d" | "--data" | "--data-raw" | "--data-binary" => body_values.push(value.clone()),
                "--data-urlencode" => {
                    body_values.push(value.clone());
                    warnings.push(
                        "curl --data-urlencode was imported as raw body text; review encoding."
                            .to_owned(),
                    );
                }
                "-u" | "--user" => {
                    let (username, password) = value
                        .split_once(':')
                        .ok_or(CurlParseError::InvalidBasicAuth)?;
                    auth = Auth::Basic {
                        username: username.to_owned(),
                        password: password.to_owned(),
                    };
                }
                "-b" | "--cookie" => cookies = Some(value.clone()),
                "-A" | "--user-agent" => headers.push(HeaderEntry::enabled("user-agent", value)),
                "-e" | "--referer" => headers.push(HeaderEntry::enabled("referer", value)),
                "-F" | "--form" => {
                    warnings.push("curl multipart form data needs manual review.".to_owned())
                }
                _ => {}
            }
            index += 2;
            continue;
        }
        if token.starts_with('-') {
            return Err(CurlParseError::UnsupportedOption(token.clone()));
        }
        if url.is_none() {
            url = Some(token.clone());
        } else {
            warnings.push(format!("Ignored extra positional curl token {token}."));
        }
        index += 1;
    }

    let url = url.ok_or(CurlParseError::MissingUrl)?;
    let mut request = Request::new(
        "Imported cURL request",
        method.unwrap_or_else(|| {
            if body_values.is_empty() && !get_mode {
                "GET".to_owned()
            } else {
                "POST".to_owned()
            }
        }),
        url,
    );
    request.headers = headers;
    request.auth = auth;
    if let Some(cookie) = cookies {
        request.headers.push(HeaderEntry::enabled("cookie", cookie));
    }
    if !body_values.is_empty() {
        let body = body_values.join("&");
        if get_mode {
            request.method = "GET".to_owned();
            request.url = format!(
                "{}{}{}",
                request.url,
                if request.url.contains('?') { "&" } else { "?" },
                body
            );
        } else if request.headers.iter().any(|header| {
            header.key.eq_ignore_ascii_case("content-type") && header.value.contains("json")
        }) {
            request.body = serde_json::from_str(&body)
                .map(|value| RequestBody::Json { value })
                .unwrap_or_else(|_| RequestBody::Raw {
                    text: body,
                    content_type: Some("application/json".to_owned()),
                });
        } else {
            request.body = RequestBody::Raw {
                text: body,
                content_type: None,
            };
        }
    }
    Ok((request, warnings))
}

pub fn import_curl_command(
    command: &str,
    output_directory: impl AsRef<std::path::Path>,
    collection_name: &str,
    request_name: &str,
) -> Result<CurlImportResult, CurlImportError> {
    let (mut request, warnings) = parse_curl_command(command)?;
    request.name = request_name.to_owned();
    let workspace = Workspace::open_or_init(output_directory, "Postly workspace")?;
    let collection = match workspace
        .collections()?
        .into_iter()
        .find(|collection| collection.collection.name == collection_name)
    {
        Some(collection) => collection,
        None => workspace.create_collection(&crate::model::Collection::new(collection_name))?,
    };
    let path = workspace.save_request(&collection, &request)?;
    Ok(CurlImportResult { path, warnings })
}

fn parse_header(value: &str) -> Result<HeaderEntry, CurlParseError> {
    let (key, value) = value
        .split_once(':')
        .ok_or_else(|| CurlParseError::InvalidHeader(value.to_owned()))?;
    if key.trim().is_empty() {
        return Err(CurlParseError::InvalidHeader(value.to_owned()));
    }
    Ok(HeaderEntry::enabled(key.trim(), value.trim()))
}

fn inline_option_value<'a>(token: &'a str, long: &str, short: &str) -> Option<&'a str> {
    token
        .strip_prefix(&format!("{long}="))
        .or_else(|| token.strip_prefix(&format!("{short}=")))
}

fn shell_words(command: &str) -> Result<Vec<String>, CurlParseError> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in command.chars() {
        if escaped {
            current.push(character);
            escaped = false;
        } else if character == '\\' && quote != Some('\'') {
            escaped = true;
        } else if let Some(active_quote) = quote {
            if character == active_quote {
                quote = None;
            } else {
                current.push(character);
            }
        } else if character == '\'' || character == '"' {
            quote = Some(character);
        } else if character.is_whitespace() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
    }
    if escaped {
        current.push('\\');
    }
    if let Some(quote) = quote {
        return Err(CurlParseError::InvalidHeader(format!(
            "unclosed quote {quote}"
        )));
    }
    if !current.is_empty() {
        words.push(current);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_curl_request_parts_without_shell_execution() {
        let (request, warnings) = parse_curl_command(
            r#"curl -X POST 'https://api.example.test/users' -H 'Content-Type: application/json' -H 'X-Trace: local' --data-raw '{"name":"Ada"}' -u 'user:pass'"#,
        )
        .expect("curl");

        assert!(warnings.is_empty());
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, "https://api.example.test/users");
        assert_eq!(request.headers.len(), 2);
        assert!(matches!(request.body, RequestBody::Json { .. }));
        assert!(matches!(request.auth, Auth::Basic { .. }));
    }

    #[test]
    fn preserves_get_data_as_query_text() {
        let (request, _) =
            parse_curl_command("curl -G https://api.example.test/search -d q=postly")
                .expect("curl");

        assert_eq!(request.method, "GET");
        assert_eq!(request.url, "https://api.example.test/search?q=postly");
        assert!(matches!(request.body, RequestBody::None));
    }

    #[test]
    fn exports_a_request_as_shell_safe_curl() {
        let mut request = Request::new("Create user", "POST", "https://api.example.test/users");
        request.query = vec![crate::model::KeyValue::enabled("filter", "Ada Lovelace")];
        request.headers = vec![HeaderEntry::enabled("X-Trace", "it's-local")];
        request.auth = Auth::Basic {
            username: "user".to_owned(),
            password: "p'ass".to_owned(),
        };
        request.body = RequestBody::Json {
            value: serde_json::json!({"name": "Ada"}),
        };

        let exported = export_curl_command(&request);
        assert!(exported
            .command
            .contains("--request 'POST' 'https://api.example.test/users?filter=Ada+Lovelace'"));
        assert!(exported.command.contains("'X-Trace: it'\\''s-local'"));
        assert!(exported.command.contains("'user:p'\\''ass'"));
        assert!(exported.command.contains("--data-raw '{\"name\":\"Ada\"}'"));
        assert!(exported.warnings.is_empty());
    }

    #[test]
    fn exports_digest_auth_with_curl_digest_negotiation() {
        let mut request = Request::new("Digest request", "GET", "https://api.example.test/users");
        request.auth = Auth::Digest {
            username: "Mufasa".to_owned(),
            password: "Circle Of Life".to_owned(),
        };
        let exported = export_curl_command(&request);
        assert!(exported.command.contains("--digest"));
        assert!(exported.command.contains("Mufasa:Circle Of Life"));
        assert!(exported.warnings.is_empty());
    }
}
