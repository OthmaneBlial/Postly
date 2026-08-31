use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{http::HttpResponse, model::Request};

/// A local, metadata-only record of a saved request execution.
///
/// History deliberately excludes query parameters, headers, cookies, body,
/// authentication and response content. The URL is reduced to its path and
/// redacts credentials so the local convenience feature does not become a
/// second secret store. The optional request UUID permits a GUI to reopen the
/// canonical saved file without embedding a request snapshot in history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryEntry {
    pub timestamp_unix_ms: u64,
    #[serde(default)]
    pub request_id: Option<Uuid>,
    pub request_name: String,
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub status: Option<u16>,
    pub duration_ms: u64,
    pub outcome: HistoryOutcome,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryFilter {
    pub search: Option<String>,
    pub method: Option<String>,
    pub status: Option<u16>,
    pub errors_only: bool,
}

impl HistoryFilter {
    pub fn matches(&self, entry: &HistoryEntry) -> bool {
        if let Some(method) = self.method.as_deref().filter(|method| !method.is_empty()) {
            if !entry.method.eq_ignore_ascii_case(method) {
                return false;
            }
        }
        if let Some(status) = self.status {
            if entry.status != Some(status) {
                return false;
            }
        }
        if self.errors_only && entry.outcome != HistoryOutcome::Error {
            return false;
        }
        if let Some(search) = self.search.as_deref().filter(|search| !search.is_empty()) {
            let search = search.to_ascii_lowercase();
            let fields = [
                entry.request_name.as_str(),
                entry.method.as_str(),
                entry.url.as_str(),
            ];
            if !fields
                .iter()
                .any(|field| field.to_ascii_lowercase().contains(&search))
            {
                return false;
            }
        }
        true
    }
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
            request_id: Some(request.id),
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
            request_id: Some(request.id),
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

pub(crate) fn sanitize_url(url: &str) -> String {
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

    #[test]
    fn filters_history_by_search_method_status_and_error_state() {
        let success = HistoryEntry {
            timestamp_unix_ms: 2,
            request_id: None,
            request_name: "List users".to_owned(),
            method: "GET".to_owned(),
            url: "https://example.com/users".to_owned(),
            status: Some(200),
            duration_ms: 12,
            outcome: HistoryOutcome::Completed,
        };
        let failure = HistoryEntry {
            timestamp_unix_ms: 1,
            request_id: None,
            request_name: "Create user".to_owned(),
            method: "POST".to_owned(),
            url: "https://example.com/users".to_owned(),
            status: None,
            duration_ms: 7,
            outcome: HistoryOutcome::Error,
        };

        assert!(HistoryFilter {
            search: Some("LIST".to_owned()),
            method: Some("get".to_owned()),
            status: Some(200),
            ..HistoryFilter::default()
        }
        .matches(&success));
        assert!(!HistoryFilter {
            search: Some("LIST".to_owned()),
            ..HistoryFilter::default()
        }
        .matches(&failure));
        assert!(HistoryFilter {
            errors_only: true,
            ..HistoryFilter::default()
        }
        .matches(&failure));
    }
}
