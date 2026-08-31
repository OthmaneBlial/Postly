//! Deterministic, local Markdown documentation for native collections.
//!
//! Generated documentation intentionally describes request shape without
//! copying header values, authentication material or response bodies by
//! default. Including example bodies is an explicit user choice at the CLI.

use std::fmt::Write;

use url::Url;

use crate::{
    model::{Auth, Request, RequestBody},
    storage::{CollectionFiles, Workspace, WorkspaceError},
};

const MAX_EXAMPLE_BODY_BYTES: usize = 64 * 1024;

/// Generate browsable Markdown for one collection or every collection in a
/// workspace. Collection selection is case-insensitive when provided.
pub fn generate_markdown_docs(
    workspace: &Workspace,
    collection_name: Option<&str>,
    include_example_bodies: bool,
) -> Result<String, WorkspaceError> {
    let collections = workspace.collections()?;
    let selected = collections
        .iter()
        .filter(|collection| {
            collection_name.map_or(true, |name| {
                collection.collection.name == name
                    || collection.collection.name.eq_ignore_ascii_case(name)
            })
        })
        .collect::<Vec<_>>();
    let mut markdown = String::from("# API documentation\n\n");
    markdown.push_str(
        "Generated locally by Postly. Authentication values, header values and response bodies are omitted by default.\n\n",
    );
    if include_example_bodies {
        markdown.push_str(
            "> Example bodies were included explicitly; review this file before sharing it.\n\n",
        );
    }
    for collection in selected {
        append_collection(&mut markdown, workspace, collection, include_example_bodies)?;
    }
    Ok(markdown)
}

fn append_collection(
    markdown: &mut String,
    workspace: &Workspace,
    collection: &CollectionFiles,
    include_example_bodies: bool,
) -> Result<(), WorkspaceError> {
    let name = &collection.collection.name;
    writeln!(markdown, "## {name}\n").expect("String write cannot fail");
    if let Some(description) = &collection.collection.description {
        markdown.push_str(description.trim());
        markdown.push_str("\n\n");
    }
    if !collection.collection.variables.is_empty() {
        markdown.push_str("### Collection variables\n\n");
        markdown.push_str("| Name |\n| --- |\n");
        for key in collection.collection.variables.keys() {
            writeln!(markdown, "| `{key}` |\n").expect("String write cannot fail");
        }
        markdown.push('\n');
    }

    let requests = workspace.requests(collection)?;
    for (_, request) in requests {
        append_request(markdown, &request, include_example_bodies);
    }
    Ok(())
}

fn append_request(markdown: &mut String, request: &Request, include_example_bodies: bool) {
    writeln!(
        markdown,
        "### {} {}\n",
        request.method.to_ascii_uppercase(),
        request.name
    )
    .expect("String write cannot fail");
    writeln!(markdown, "- **URL:** `{}`", redacted_url(&request.url))
        .expect("String write cannot fail");
    if let Some(folder) = &request.folder {
        writeln!(markdown, "- **Folder:** `{folder}`").expect("String write cannot fail");
    }
    writeln!(
        markdown,
        "- **Authentication:** {}",
        auth_label(&request.auth)
    )
    .expect("String write cannot fail");
    writeln!(markdown, "- **Body:** {}", body_label(&request.body))
        .expect("String write cannot fail");
    if let Some(description) = &request.description {
        markdown.push('\n');
        markdown.push_str(description.trim());
        markdown.push('\n');
    }

    append_pairs(markdown, "Parameters", &request.query);
    if !request.headers.is_empty() {
        markdown.push_str("\n#### Headers\n\n| Name | State |\n| --- | --- |\n");
        for header in &request.headers {
            let state = if header.enabled {
                "enabled"
            } else {
                "disabled"
            };
            writeln!(markdown, "| `{}` | {state} |", header.key).expect("String write cannot fail");
        }
        markdown.push('\n');
    }
    append_pairs(markdown, "Cookies", &request.cookies);
    if !request.assertions.is_empty() {
        writeln!(
            markdown,
            "\n#### Assertions\n\n- {} native assertion(s)\n",
            request.assertions.len()
        )
        .expect("String write cannot fail");
    }
    if !request.examples.is_empty() {
        markdown.push_str("\n#### Response examples\n\n");
        for example in &request.examples {
            let status = example
                .status
                .map(|status| status.to_string())
                .unwrap_or_else(|| "unspecified".to_owned());
            writeln!(markdown, "##### {} — {status}\n", example.name)
                .expect("String write cannot fail");
            if include_example_bodies {
                if let Some(body) = &example.body {
                    let body = truncate_example_body(body);
                    markdown.push_str("```text\n");
                    markdown.push_str(&body);
                    if !body.ends_with('\n') {
                        markdown.push('\n');
                    }
                    markdown.push_str("```\n\n");
                } else {
                    markdown.push_str("_No response body._\n\n");
                }
            } else {
                markdown.push_str("_Response body omitted; rerun with `--include-example-bodies` to include it._\n\n");
            }
        }
    }
}

fn append_pairs(markdown: &mut String, title: &str, pairs: &[crate::model::KeyValue]) {
    if pairs.is_empty() {
        return;
    }
    writeln!(
        markdown,
        "\n#### {title}\n\n| Name | State |\n| --- | --- |"
    )
    .expect("String write cannot fail");
    for pair in pairs {
        let state = if pair.enabled { "enabled" } else { "disabled" };
        writeln!(markdown, "| `{}` | {state} |", pair.key).expect("String write cannot fail");
    }
    markdown.push('\n');
}

fn redacted_url(value: &str) -> String {
    let Ok(mut url) = Url::parse(value) else {
        return redact_query_text(value);
    };
    let pairs = url
        .query_pairs()
        .map(|(key, value)| {
            let value = if looks_sensitive(&key) {
                "[redacted]".to_owned()
            } else {
                value.into_owned()
            };
            (key.into_owned(), value)
        })
        .collect::<Vec<_>>();
    if url.query().is_some() {
        url.set_query(None);
        if !pairs.is_empty() {
            let mut query = url::form_urlencoded::Serializer::new(String::new());
            for (key, value) in pairs {
                query.append_pair(&key, &value);
            }
            url.set_query(Some(&query.finish()));
        }
    }
    url.to_string()
}

fn redact_query_text(value: &str) -> String {
    let Some((prefix, query_and_fragment)) = value.split_once('?') else {
        return value.to_owned();
    };
    let (query, fragment) = query_and_fragment
        .split_once('#')
        .map_or((query_and_fragment, ""), |(query, fragment)| {
            (query, fragment)
        });
    let redacted = query
        .split('&')
        .map(|pair| {
            let Some((key, _)) = pair.split_once('=') else {
                return pair.to_owned();
            };
            if looks_sensitive(key.trim()) {
                format!("{key}=[redacted]")
            } else {
                pair.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("&");
    if fragment.is_empty() {
        format!("{prefix}?{redacted}")
    } else {
        format!("{prefix}?{redacted}#{fragment}")
    }
}

fn looks_sensitive(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();
    [
        "auth",
        "token",
        "secret",
        "password",
        "passwd",
        "api-key",
        "apikey",
        "credential",
    ]
    .iter()
    .any(|part| normalized.contains(part))
}

fn auth_label(auth: &Auth) -> &'static str {
    match auth {
        Auth::None => "none",
        Auth::Basic { .. } => "Basic",
        Auth::Bearer { .. } => "Bearer",
        Auth::ApiKey { .. } => "API key",
        Auth::OAuth2ClientCredentials { .. } => "OAuth 2.0 client credentials",
        Auth::OAuth2AuthorizationCodePkce { .. } => "OAuth 2.0 authorization code + PKCE",
        Auth::OAuth2RefreshToken { .. } => "OAuth 2.0 refresh token",
        Auth::OAuth2DeviceCode { .. } => "OAuth 2.0 device code",
    }
}

fn body_label(body: &RequestBody) -> &'static str {
    match body {
        RequestBody::None => "none",
        RequestBody::Raw { .. } => "raw text",
        RequestBody::Json { .. } => "JSON",
        RequestBody::Graphql { .. } => "GraphQL",
        RequestBody::FormUrlEncoded { .. } => "form URL encoded",
        RequestBody::Multipart { .. } => "multipart form-data",
        RequestBody::BinaryFile { .. } => "binary file",
    }
}

fn truncate_example_body(body: &str) -> String {
    if body.len() <= MAX_EXAMPLE_BODY_BYTES {
        return body.to_owned();
    }
    let mut truncated = body
        .char_indices()
        .take_while(|(index, _)| *index < MAX_EXAMPLE_BODY_BYTES)
        .map(|(_, character)| character)
        .collect::<String>();
    truncated.push_str("\n[example body truncated by Postly]");
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Collection, HeaderEntry, KeyValue, ResponseExample};

    #[test]
    fn generates_safe_markdown_without_secret_values_by_default() {
        let directory = tempfile::tempdir().expect("directory");
        let workspace = Workspace::init(directory.path(), "Docs").expect("workspace");
        let mut collection = Collection::new("Payments");
        collection.description = Some("Create and inspect payments.".to_owned());
        collection
            .variables
            .insert("baseUrl".to_owned(), "https://example.test".to_owned());
        let files = workspace
            .create_collection(&collection)
            .expect("collection");
        let mut request = Request::new(
            "Create payment",
            "POST",
            "{{baseUrl}}/payments?token=secret-value&limit=10",
        );
        request.description = Some("Creates one payment.".to_owned());
        request.headers = vec![HeaderEntry::enabled("Authorization", "Bearer secret-value")];
        request.query = vec![KeyValue::enabled("token", "secret-value")];
        request.auth = Auth::Bearer {
            token: "secret-value".to_owned(),
        };
        request.examples = vec![ResponseExample {
            name: "Created".to_owned(),
            status: Some(201),
            headers: Vec::new(),
            body: Some("{\"id\":\"pay_123\"}".to_owned()),
            delay_ms: 0,
        }];
        workspace.save_request(&files, &request).expect("request");

        let markdown = generate_markdown_docs(&workspace, None, false).expect("docs");
        assert!(markdown.contains("Create payment"));
        assert!(markdown.contains("token=[redacted]"));
        assert!(!markdown.contains("secret-value"));
        assert!(!markdown.contains("pay_123"));
        assert!(markdown.contains("--include-example-bodies"));

        let with_body = generate_markdown_docs(&workspace, Some("payments"), true).expect("docs");
        assert!(with_body.contains("pay_123"));
        assert!(with_body.contains("Example bodies were included explicitly"));
    }

    #[test]
    fn truncates_large_example_bodies() {
        let body = "x".repeat(MAX_EXAMPLE_BODY_BYTES + 100);
        let result = truncate_example_body(&body);
        assert!(result.len() < body.len());
        assert!(result.contains("example body truncated"));
    }
}
