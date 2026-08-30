use std::fmt;

use serde::Serialize;

use crate::{
    curl::export_curl_command,
    model::{ApiKeyLocation, Auth, KeyValue, Request, RequestBody},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SnippetLanguage {
    Curl,
    Javascript,
    Python,
    Rust,
    Go,
    Java,
    Csharp,
    Php,
}

impl fmt::Display for SnippetLanguage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Curl => "curl",
            Self::Javascript => "javascript",
            Self::Python => "python",
            Self::Rust => "rust",
            Self::Go => "go",
            Self::Java => "java",
            Self::Csharp => "csharp",
            Self::Php => "php",
        })
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CodeSnippet {
    pub language: SnippetLanguage,
    pub code: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct SnippetParts {
    url: String,
    method: String,
    headers: Vec<(String, String)>,
    body: Option<String>,
    body_content_type: Option<String>,
    warnings: Vec<String>,
}

pub fn generate_code_snippet(request: &Request, language: SnippetLanguage) -> CodeSnippet {
    if language == SnippetLanguage::Curl {
        let exported = export_curl_command(request);
        return CodeSnippet {
            language,
            code: exported.command,
            warnings: exported.warnings,
        };
    }

    let parts = snippet_parts(request);
    let code = match language {
        SnippetLanguage::Curl => unreachable!(),
        SnippetLanguage::Javascript => javascript(&parts),
        SnippetLanguage::Python => python(&parts),
        SnippetLanguage::Rust => rust(&parts),
        SnippetLanguage::Go => go(&parts),
        SnippetLanguage::Java => java(&parts),
        SnippetLanguage::Csharp => csharp(&parts),
        SnippetLanguage::Php => php(&parts),
    };
    CodeSnippet {
        language,
        code,
        warnings: parts.warnings,
    }
}

fn snippet_parts(request: &Request) -> SnippetParts {
    let mut warnings = Vec::new();
    let mut url = append_query_parameters(&request.url, &request.query);
    let mut headers = request
        .headers
        .iter()
        .filter(|header| header.enabled)
        .map(|header| (header.key.clone(), header.value.clone()))
        .collect::<Vec<_>>();
    let cookies = request
        .cookies
        .iter()
        .filter(|cookie| cookie.enabled)
        .map(|cookie| (cookie.key.clone(), cookie.value.clone()))
        .collect::<Vec<_>>();

    match &request.auth {
        Auth::None => {}
        Auth::Bearer { token } => headers.push((
            "Authorization".to_owned(),
            format!("Bearer {token}"),
        )),
        Auth::Basic { .. } => warnings.push(
            "Basic auth is not materialized in generated snippets; add credentials explicitly after reviewing the output."
                .to_owned(),
        ),
        Auth::ApiKey {
            key,
            value,
            location: ApiKeyLocation::Header,
        } => headers.push((key.clone(), value.clone())),
        Auth::ApiKey {
            key,
            value,
            location: ApiKeyLocation::Query,
        } => {
            url = append_query_parameters(&url, &[KeyValue::enabled(key, value)]);
        }
        Auth::OAuth2ClientCredentials { .. } => warnings.push(
            "OAuth 2.0 client credentials are not materialized; fetch a token before running the snippet."
                .to_owned(),
        ),
    }

    if !cookies.is_empty() && !has_header(&headers, "cookie") {
        headers.push((
            "Cookie".to_owned(),
            cookies
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }
    let (body, body_content_type) = body_parts(&request.body, &mut warnings);
    if !has_header(&headers, "content-type") {
        if let Some(content_type) = body_content_type.as_deref() {
            headers.push(("Content-Type".to_owned(), content_type.to_owned()));
        }
    }
    SnippetParts {
        url,
        method: request.method.clone(),
        headers,
        body,
        body_content_type,
        warnings,
    }
}

fn body_parts(body: &RequestBody, warnings: &mut Vec<String>) -> (Option<String>, Option<String>) {
    match body {
        RequestBody::None => (None, None),
        RequestBody::Raw { text, content_type } => (Some(text.clone()), content_type.clone()),
        RequestBody::Json { value } => (
            Some(serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned())),
            Some("application/json".to_owned()),
        ),
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
            (
                Some(
                    serde_json::to_string(&serde_json::Value::Object(payload))
                        .unwrap_or_else(|_| "{}".to_owned()),
                ),
                Some("application/json".to_owned()),
            )
        }
        RequestBody::FormUrlEncoded { fields } => (
            Some(
                fields
                    .iter()
                    .filter(|field| field.enabled)
                    .map(|field| format!("{}={}", field.key, field.value))
                    .collect::<Vec<_>>()
                    .join("&"),
            ),
            Some("application/x-www-form-urlencoded".to_owned()),
        ),
        RequestBody::Multipart { parts } => {
            warnings.push(
                "Multipart snippets preserve fields as a reviewable placeholder; file upload syntax may need local adaptation."
                    .to_owned(),
            );
            (
                Some(
                    parts
                        .iter()
                        .filter(|part| part.enabled)
                        .map(|part| {
                            part.file_path
                                .as_ref()
                                .map(|path| format!("{}=@{}", part.name, path))
                                .unwrap_or_else(|| format!("{}={}", part.name, part.value))
                        })
                        .collect::<Vec<_>>()
                        .join("&"),
                ),
                None,
            )
        }
        RequestBody::BinaryFile { path, .. } => {
            warnings.push(format!(
                "Binary file body is referenced by path ({path}); generated code keeps the path visible for review."
            ));
            (Some(format!("@{path}")), None)
        }
    }
}

fn append_query_parameters(url: &str, pairs: &[KeyValue]) -> String {
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

fn has_header(headers: &[(String, String)], name: &str) -> bool {
    headers
        .iter()
        .any(|(key, _)| key.eq_ignore_ascii_case(name))
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}

fn rust_byte_string(value: &str) -> String {
    format!("b{}", json_string(value))
}

fn js_headers(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .map(|(key, value)| format!("    {}: {}", json_string(key), json_string(value)))
        .collect::<Vec<_>>()
        .join(",\n")
}

fn python_headers(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .map(|(key, value)| format!("    {}: {}", json_string(key), json_string(value)))
        .collect::<Vec<_>>()
        .join(",\n")
}

fn rust_headers(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .map(|(key, value)| {
            format!(
                "        .header({}, {})",
                json_string(key),
                json_string(value)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn go_headers(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .map(|(key, value)| {
            format!(
                "\treq.Header.Set({}, {})",
                json_string(key),
                json_string(value)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn java_headers(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .map(|(key, value)| {
            format!(
                "        .header({}, {})",
                json_string(key),
                json_string(value)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn csharp_headers(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .map(|(key, value)| {
            format!(
                "request.Headers.TryAddWithoutValidation({}, {});",
                json_string(key),
                json_string(value)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn javascript(parts: &SnippetParts) -> String {
    let headers = if parts.headers.is_empty() {
        "{}".to_owned()
    } else {
        format!("{{\n{}\n  }}", js_headers(&parts.headers))
    };
    let body = parts
        .body
        .as_deref()
        .map(|body| format!("\n  body: {},", json_string(body)))
        .unwrap_or_default();
    format!(
        "const response = await fetch({}, {{\n  method: {},\n  headers: {}{},\n}});\n\nconsole.log(response.status, await response.text());",
        json_string(&parts.url),
        json_string(&parts.method),
        headers,
        body
    )
}

fn python(parts: &SnippetParts) -> String {
    let body = parts
        .body
        .as_deref()
        .map(|body| format!(",\n    data={}", json_string(body)))
        .unwrap_or_default();
    format!(
        "import requests\n\nresponse = requests.request(\n    {},\n    {},\n    headers={{\n{}\n    }}{}\n)\n\nprint(response.status_code)\nprint(response.text)",
        json_string(&parts.method),
        json_string(&parts.url),
        python_headers(&parts.headers),
        body
    )
}

fn rust(parts: &SnippetParts) -> String {
    let body = parts
        .body
        .as_deref()
        .map(|body| format!("\n        .body({})", json_string(body)))
        .unwrap_or_default();
    format!(
        "#[tokio::main]\nasync fn main() -> Result<(), reqwest::Error> {{\n    let client = reqwest::Client::new();\n    let response = client\n        .request(reqwest::Method::from_bytes({}).expect(\"valid method\"), {})\n{}{}\n        .send()\n        .await?;\n\n    println!(\"{{}}\", response.text().await?);\n    Ok(())\n}}",
        rust_byte_string(&parts.method),
        json_string(&parts.url),
        rust_headers(&parts.headers),
        body
    )
}

fn go(parts: &SnippetParts) -> String {
    let body = parts
        .body
        .as_deref()
        .map(|body| format!("strings.NewReader({})", json_string(body)))
        .unwrap_or_else(|| "nil".to_owned());
    let imports = if parts.body.is_some() {
        "\t\"strings\"\n"
    } else {
        ""
    };
    format!(
        "package main\n\nimport (\n\t\"fmt\"\n\t\"io\"\n\t\"net/http\"\n{}\n)\n\nfunc main() {{\n\treq, err := http.NewRequest({}, {}, {})\n\tif err != nil {{ panic(err) }}\n{}\n\tres, err := http.DefaultClient.Do(req)\n\tif err != nil {{ panic(err) }}\n\tdefer res.Body.Close()\n\tbody, _ := io.ReadAll(res.Body)\n\tfmt.Println(res.Status)\n\tfmt.Println(string(body))\n}}",
        imports,
        json_string(&parts.method),
        json_string(&parts.url),
        body,
        go_headers(&parts.headers)
    )
}

fn java(parts: &SnippetParts) -> String {
    let body = parts
        .body
        .as_deref()
        .map(|body| {
            format!(
                ", HttpRequest.BodyPublishers.ofString({})",
                json_string(body)
            )
        })
        .unwrap_or_else(|| ", HttpRequest.BodyPublishers.noBody()".to_owned());
    format!(
        "import java.net.URI;\nimport java.net.http.HttpClient;\nimport java.net.http.HttpRequest;\nimport java.net.http.HttpResponse;\n\nclass PostlyRequest {{\n  public static void main(String[] args) throws Exception {{\n    var request = HttpRequest.newBuilder()\n        .uri(URI.create({}))\n{}\n        .method({}, {})\n        .build();\n    var response = HttpClient.newHttpClient().send(request, HttpResponse.BodyHandlers.ofString());\n    System.out.println(response.statusCode());\n    System.out.println(response.body());\n  }}\n}}",
        json_string(&parts.url),
        java_headers(&parts.headers),
        json_string(&parts.method),
        body
    )
}

fn csharp(parts: &SnippetParts) -> String {
    let body = parts
        .body
        .as_deref()
        .map(|body| {
            format!(
                "\n        request.Content = new StringContent({}, Encoding.UTF8, {});",
                json_string(body),
                json_string(parts.body_content_type.as_deref().unwrap_or("text/plain"))
            )
        })
        .unwrap_or_default();
    format!(
        "using System;\nusing System.Net.Http;\nusing System.Text;\n\nusing var client = new HttpClient();\nusing var request = new HttpRequestMessage(new HttpMethod({}), {});\n{}{}\nusing var response = await client.SendAsync(request);\nConsole.WriteLine((int)response.StatusCode);\nConsole.WriteLine(await response.Content.ReadAsStringAsync());",
        json_string(&parts.method),
        json_string(&parts.url),
        csharp_headers(&parts.headers),
        body
    )
}

fn php(parts: &SnippetParts) -> String {
    let body = parts
        .body
        .as_deref()
        .map(|body| format!(",\n    CURLOPT_POSTFIELDS => {}", json_string(body)))
        .unwrap_or_default();
    let headers = parts
        .headers
        .iter()
        .map(|(key, value)| {
            format!(
                "        {} . ': ' . {}",
                json_string(key),
                json_string(value)
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    format!(
        "<?php\n$ch = curl_init({});\ncurl_setopt_array($ch, [\n    CURLOPT_CUSTOMREQUEST => {},\n    CURLOPT_RETURNTRANSFER => true,\n    CURLOPT_HTTPHEADER => [\n{}\n    ]{}\n]);\n$response = curl_exec($ch);\ncurl_close($ch);\necho $response;",
        json_string(&parts.url),
        json_string(&parts.method),
        headers,
        body
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Auth, HeaderEntry, RequestBody};

    #[test]
    fn generates_javascript_from_the_native_request_model() {
        let mut request = Request::new("Create user", "POST", "{{baseUrl}}/users");
        request
            .headers
            .push(HeaderEntry::enabled("Accept", "application/json"));
        request.auth = Auth::Bearer {
            token: "{{token}}".to_owned(),
        };
        request.body = RequestBody::Json {
            value: serde_json::json!({"name": "Ada"}),
        };

        let snippet = generate_code_snippet(&request, SnippetLanguage::Javascript);

        assert!(snippet.code.contains("fetch(\"{{baseUrl}}/users\""));
        assert!(snippet.code.contains("Authorization"));
        assert!(snippet.code.contains("\\\"name\\\":\\\"Ada\\\""));
        assert!(snippet.warnings.is_empty());
    }

    #[test]
    fn generates_python_with_query_cookies_and_oauth_warning() {
        let mut request = Request::new("List", "GET", "https://example.test/users");
        request.query.push(KeyValue::enabled("limit", "10"));
        request.cookies.push(KeyValue::enabled("session", "local"));
        request.auth = Auth::OAuth2ClientCredentials {
            token_url: "https://auth.example.test/token".to_owned(),
            client_id: "postly".to_owned(),
            client_secret: "secret".to_owned(),
            scope: None,
        };

        let snippet = generate_code_snippet(&request, SnippetLanguage::Python);

        assert!(snippet.code.contains("limit=10"));
        assert!(snippet.code.contains("session"));
        assert_eq!(snippet.warnings.len(), 1);
        assert!(snippet.warnings[0].contains("OAuth"));
    }

    #[test]
    fn every_supported_language_produces_source() {
        let request = Request::new("Health", "GET", "https://example.test/health");
        for language in [
            SnippetLanguage::Curl,
            SnippetLanguage::Javascript,
            SnippetLanguage::Python,
            SnippetLanguage::Rust,
            SnippetLanguage::Go,
            SnippetLanguage::Java,
            SnippetLanguage::Csharp,
            SnippetLanguage::Php,
        ] {
            let snippet = generate_code_snippet(&request, language);
            assert!(!snippet.code.trim().is_empty(), "{language}");
        }
    }
}
