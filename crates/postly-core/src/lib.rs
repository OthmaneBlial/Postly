//! The local-first, transport-agnostic core of Postly.
//!
//! The core deliberately owns the durable request model, variable resolution,
//! filesystem storage, Postman migration, and HTTP execution. UI clients and
//! the CLI can therefore share the same behavior without a cloud service.

pub mod curl;
pub mod export;
pub mod graphql;
pub mod history;
pub mod http;
pub mod import;
pub mod model;
pub mod openapi;
pub mod runner;
pub mod scripting;
pub mod sse;
pub mod storage;
pub mod variables;

pub use curl::{import_curl_command, parse_curl_command, CurlImportResult, CurlParseError};
pub use export::{
    export_postman_collection, export_postman_environment, ExportError, ExportReport,
};
pub use graphql::{
    introspection_query, parse_response as parse_graphql_response, parse_variables_json,
    validate_query as validate_graphql_query, GraphqlError, GraphqlRequest, GraphqlResponse,
};
pub use history::{HistoryEntry, HistoryFilter, HistoryOutcome};
pub use http::{
    EngineOptions, HttpEngine, HttpError, HttpResponse, HttpStreamResponse, ResponseCookie,
    ResponseView,
};
pub use import::{import_environment, import_postman_collection, ImportReport};
pub use model::*;
pub use openapi::{import_openapi, OpenApiImportError, OpenApiImportReport};
pub use runner::{run_requests, CancellationToken, RunnerItemResult, RunnerOptions, RunnerSummary};
pub use scripting::{run_script, ScriptError, ScriptLog, ScriptResult, ScriptTestResult};
pub use sse::{parse_sse, SseError, SseEvent, SseParser};
pub use storage::{CollectionFiles, Workspace, WorkspaceError};
pub use variables::{ResolvedText, VariableContext, VariableDiagnostic, VariableDiagnosticKind};
