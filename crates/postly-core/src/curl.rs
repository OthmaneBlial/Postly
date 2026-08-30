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
}
