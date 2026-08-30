//! The local-first, transport-agnostic core of Postly.
//!
//! The core deliberately owns the durable request model, variable resolution,
//! filesystem storage, Postman migration, and HTTP execution. UI clients and
//! the CLI can therefore share the same behavior without a cloud service.

pub mod curl;
pub mod history;
pub mod http;
pub mod import;
pub mod model;
pub mod runner;
pub mod storage;
pub mod variables;

pub use curl::{import_curl_command, parse_curl_command, CurlImportResult, CurlParseError};
pub use history::{HistoryEntry, HistoryOutcome};
pub use http::{EngineOptions, HttpEngine, HttpError, HttpResponse, ResponseView};
pub use import::{import_environment, import_postman_collection, ImportReport};
pub use model::*;
pub use runner::{run_requests, CancellationToken, RunnerItemResult, RunnerOptions, RunnerSummary};
pub use storage::{CollectionFiles, Workspace, WorkspaceError};
pub use variables::{ResolvedText, VariableContext, VariableDiagnostic, VariableDiagnosticKind};
