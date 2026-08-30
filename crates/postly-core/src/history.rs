use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{http::HttpResponse, model::Request};

/// A local, metadata-only record of a saved request execution.
///
/// History deliberately excludes query parameters, headers, cookies, body,
/// authentication and response content. The URL is reduced to its path and
/// redacts credentials so the local convenience feature does not become a
/// second secret store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryEntry {
    pub timestamp_unix_ms: u64,
    pub request_name: String,
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub status: Option<u16>,
    pub duration_ms: u64,
    pub outcome: HistoryOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HistoryOutcome {
    Completed,
    Error,
}

impl HistoryEntry {
    pub fn from_response(request: &Request, response: &HttpResponse) -> Self {
        Self {
            timestamp_unix_ms: now_unix_ms(),
            request_name: request.name.clone(),
            method: request.method.clone(),
            url: sanitize_url(&request.url),
            status: Some(response.status),
            duration_ms: response.duration_ms.min(u64::MAX as u128) as u64,
            outcome: HistoryOutcome::Completed,
        }
    }

    pub fn from_error(request: &Request, duration_ms: u64) -> Self {
        Self {
            timestamp_unix_ms: now_unix_ms(),
            request_name: request.name.clone(),
            method: request.method.clone(),
            url: sanitize_url(&request.url),
            status: None,
            duration_ms,
            outcome: HistoryOutcome::Error,
        }
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn sanitize_url(url: &str) -> String {
    let without_query = url.split(['?', '#']).next().unwrap_or(url);
    let Some(scheme_end) = without_query.find("://") else {
        return without_query.to_owned();
    };
    let authority_start = scheme_end + 3;
    let authority_end = without_query[authority_start..]
        .find('/')
        .map(|offset| authority_start + offset)
        .unwrap_or(without_query.len());
    let authority = &without_query[authority_start..authority_end];
    let Some(credentials_end) = authority.rfind('@') else {
        return without_query.to_owned();
    };
    format!(
        "{}[redacted]{}",
        &without_query[..authority_start],
        &without_query[authority_start + credentials_end + 1..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_query_and_credentials_from_history_urls() {
        assert_eq!(
            sanitize_url("https://user:password@example.com/users?token=secret"),
            "https://[redacted]example.com/users"
        );
        assert_eq!(
            sanitize_url("{{baseUrl}}/health?token=secret"),
            "{{baseUrl}}/health"
        );
    }
}
