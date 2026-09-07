//! Workspace-oriented CLI commands.
//!
//! These commands stay close to the shared workspace model while keeping the
//! command dispatcher in `main.rs` focused on argument routing.

use std::path::Path;

use anyhow::{bail, Result};
use postly_core::{Collection, Workspace};
use serde_json::json;

pub(crate) fn init_workspace(path: &Path, name: &str) -> Result<()> {
    let workspace = Workspace::init(path, name)?;
    let collection = workspace.create_collection(&Collection::new("My API"))?;
    println!(
        "Initialized Postly workspace at {}",
        workspace.root().display()
    );
    println!("Created collection at {}", collection.directory.display());
    println!("No account or cloud service is required.");
    Ok(())
}

pub(crate) fn list_workspace(path: &Path) -> Result<()> {
    let workspace = Workspace::open(path)?;
    let manifest = workspace.manifest()?;
    println!("{} ({})", manifest.name, workspace.root().display());
    for collection in workspace.collections()? {
        println!("\nCollection: {}", collection.collection.name);
        for (request_path, request) in workspace.requests(&collection)? {
            println!(
                "  {} {} — {}",
                request.method,
                request.name,
                request_path.display()
            );
        }
    }
    let environments = workspace.environments()?;
    if !environments.is_empty() {
        println!("\nEnvironments:");
        for (_, environment) in environments {
            println!("  {}", environment.name);
        }
    }
    Ok(())
}

pub(crate) fn validate_workspace(path: &Path, output_json: bool) -> Result<()> {
    let workspace = Workspace::open(path)?;
    let report = workspace.validate()?;
    if output_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "valid": report.is_valid(),
                "collections": report.collections,
                "requests": report.requests,
                "environments": report.environments,
                "issues": report.issues,
            }))?
        );
    } else if report.is_valid() {
        println!(
            "Workspace is valid: {} collection(s), {} request(s), {} environment(s).",
            report.collections, report.requests, report.environments
        );
    } else {
        println!(
            "Workspace has {} issue(s): {} valid collection(s), {} valid request(s), {} valid environment(s).",
            report.issues.len(), report.collections, report.requests, report.environments
        );
        for issue in &report.issues {
            println!("  {} — {}", issue.path.display(), issue.message);
        }
    }
    if report.is_valid() {
        Ok(())
    } else {
        bail!("workspace validation failed")
    }
}

pub(crate) fn search_workspace(path: &Path, query: &str, output_json: bool) -> Result<()> {
    if query.trim().is_empty() {
        bail!("search query cannot be empty");
    }
    let workspace = Workspace::open(path)?;
    let results = workspace.search_requests(query)?;
    if output_json {
        println!("{}", serde_json::to_string_pretty(&results)?);
        return Ok(());
    }
    if results.is_empty() {
        println!("No saved requests matched {query:?}.");
        return Ok(());
    }
    println!("{} saved request(s) matched {query:?}:", results.len());
    for result in results {
        let location = result
            .folder
            .as_deref()
            .map(|folder| format!("{} / {folder}", result.collection))
            .unwrap_or(result.collection);
        println!(
            "{} {} — {} — {} ({})",
            result.method,
            result.name,
            location,
            result.url,
            result.path.display()
        );
    }
    Ok(())
}
