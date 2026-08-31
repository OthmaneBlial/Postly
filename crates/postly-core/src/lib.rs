//! The local-first, transport-agnostic core of Postly.
//!
//! The core deliberately owns the durable request model, variable resolution,
//! filesystem storage, Postman migration, and HTTP execution. UI clients and
//! the CLI can therefore share the same behavior without a cloud service.

pub mod codegen;
pub mod curl;
pub mod documentation;
pub mod export;
pub mod graphql;
pub mod grpc;
pub mod history;
pub mod http;
pub mod import;
pub mod model;
pub mod openapi;
pub mod runner;
pub mod scripting;
pub mod secrets;
pub mod sse;
pub mod storage;
pub mod variables;

pub use codegen::{generate_code_snippet, CodeSnippet, SnippetLanguage};
pub use curl::{
    export_curl_command, import_curl_command, parse_curl_command, CurlExportResult,
    CurlImportResult, CurlParseError,
};
pub use documentation::generate_markdown_docs;
pub use export::{
    export_postman_collection, export_postman_environment, export_postman_environment_with_store,
    ExportError, ExportReport,
};
pub use graphql::{
    introspection_query, parse_response as parse_graphql_response,
    parse_schema as parse_graphql_schema, parse_variables_json, schema_introspection_query,
    validate_query as validate_graphql_query, GraphqlArgument, GraphqlEnumValue, GraphqlError,
    GraphqlField, GraphqlInputField, GraphqlRequest, GraphqlResponse, GraphqlSchema, GraphqlType,
};
pub use grpc::{
    message_from_json, message_to_json, GrpcError, GrpcMethodDescription, GrpcSchema,
    GrpcServiceDescription,
};
pub use history::{HistoryEntry, HistoryFilter, HistoryOutcome};
pub use http::{
    EngineOptions, HttpEngine, HttpError, HttpResponse, HttpStreamResponse,
    OAuthAuthorizationRequest, OAuthDeviceCodePrompt, ResponseCookie, ResponseView,
};
pub use import::{import_dotenv, import_environment, import_postman_collection, ImportReport};
pub use model::*;
pub use openapi::{
    export_openapi_collection, import_openapi, import_openapi_text, OpenApiExportError,
    OpenApiExportReport, OpenApiImportError, OpenApiImportReport,
};
pub use runner::{run_requests, CancellationToken, RunnerItemResult, RunnerOptions, RunnerSummary};
pub use scripting::{run_script, ScriptError, ScriptLog, ScriptResult, ScriptTestResult};
pub use secrets::{SecretReference, SecretStore, SecretStoreError};
pub use sse::{parse_sse, SseError, SseEvent, SseParser};
pub use storage::{CollectionFiles, RequestSearchResult, Workspace, WorkspaceError};
pub use variables::{ResolvedText, VariableContext, VariableDiagnostic, VariableDiagnosticKind};
