use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    history::{HistoryEntry, HistoryFilter},
    model::{Collection, Environment, ProjectManifest, Request},
};

const MANIFEST_FILE: &str = "postly.toml";
const COLLECTION_FILE: &str = "postly.collection.toml";
const ENVIRONMENT_SUFFIX: &str = ".postly-env.toml";
const REQUEST_SUFFIX: &str = ".postly.toml";
const HISTORY_FILE: &str = ".postly/history.jsonl";
const MAX_HISTORY_ENTRIES: usize = 1_000;
const MAX_HISTORY_BYTES: usize = 1_048_576;
static ATOMIC_WRITE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?
        .to_string_lossy();
    let mut temporary = None;
    let mut temporary_file = None;
    for _ in 0..100 {
        let sequence = ATOMIC_WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{file_name}.{}-{sequence}.tmp",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                temporary = Some(candidate);
                temporary_file = Some(file);
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    let Some(temporary) = temporary else {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique atomic-write temporary file",
        ));
    };
    let mut temporary_file = temporary_file.expect("temporary path has a file");
    if let Err(error) = temporary_file
        .write_all(contents)
        .and_then(|_| temporary_file.sync_all())
    {
        drop(temporary_file);
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    drop(temporary_file);
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("filesystem error at {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("invalid TOML at {path}: {source}")]
    Toml {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("could not serialize TOML at {path}: {source}")]
    TomlSerialize {
        path: PathBuf,
        source: toml::ser::Error,
    },
    #[error("invalid JSON at {path}: {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("workspace manifest is missing at {0}")]
    MissingManifest(PathBuf),
    #[error("workspace manifest has an unsupported format: {0}")]
    UnsupportedFormat(String),
    #[error("request file has an unsafe or empty name: {0}")]
    InvalidName(String),
}

#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct CollectionFiles {
    pub directory: PathBuf,
    pub collection: Collection,
}

/// A canonical workspace file that could not be parsed during validation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceValidationIssue {
    /// Path relative to the workspace root so reports stay portable.
    pub path: PathBuf,
    pub message: String,
}

/// A non-destructive report over the canonical workspace files.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceValidationReport {
    pub collections: usize,
    pub requests: usize,
    pub environments: usize,
    pub issues: Vec<WorkspaceValidationIssue>,
}

impl WorkspaceValidationReport {
    pub fn is_valid(&self) -> bool {
        self.issues.is_empty()
    }
}

/// A secret-free index entry for a request found by workspace search.
///
/// Search deliberately covers navigational metadata only. Headers, cookies,
/// bodies, authentication and scripts are never loaded into the result so a
/// search command cannot accidentally turn sensitive request data into output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestSearchResult {
    pub collection_id: uuid::Uuid,
    pub collection: String,
    pub folder: Option<String>,
    /// Path relative to the workspace root, suitable for local CLI output.
    pub path: PathBuf,
    pub id: uuid::Uuid,
    pub name: String,
    pub method: String,
    pub url: String,
}

impl Workspace {
    pub fn init(root: impl AsRef<Path>, name: impl Into<String>) -> Result<Self, WorkspaceError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(root.join("collections")).map_err(|source| WorkspaceError::Io {
            path: root.join("collections"),
            source,
        })?;
        fs::create_dir_all(root.join("environments")).map_err(|source| WorkspaceError::Io {
            path: root.join("environments"),
            source,
        })?;
        let workspace = Self { root };
        if !workspace.manifest_path().exists() {
            workspace.write_toml(
                &workspace.manifest_path(),
                &ProjectManifest::new(name.into()),
            )?;
        }
        Ok(workspace)
    }

    pub fn open(root: impl AsRef<Path>) -> Result<Self, WorkspaceError> {
        let root = root.as_ref().to_path_buf();
        let manifest_path = root.join(MANIFEST_FILE);
        if !manifest_path.is_file() {
            return Err(WorkspaceError::MissingManifest(manifest_path));
        }
        let manifest: ProjectManifest = read_toml(&manifest_path)?;
        if manifest.format != "postly" {
            return Err(WorkspaceError::UnsupportedFormat(manifest.format));
        }
        Ok(Self { root })
    }

    pub fn open_or_init(
        root: impl AsRef<Path>,
        name: impl Into<String>,
    ) -> Result<Self, WorkspaceError> {
        let root = root.as_ref();
        if root.join(MANIFEST_FILE).is_file() {
            Self::open(root)
        } else {
            Self::init(root, name)
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn manifest(&self) -> Result<ProjectManifest, WorkspaceError> {
        read_toml(&self.manifest_path())
    }

    pub fn create_collection(
        &self,
        collection: &Collection,
    ) -> Result<CollectionFiles, WorkspaceError> {
        let directory = self
            .root
            .join("collections")
            .join(slugify(&collection.name)?);
        fs::create_dir_all(directory.join("requests")).map_err(|source| WorkspaceError::Io {
            path: directory.join("requests"),
            source,
        })?;
        self.write_toml(&directory.join(COLLECTION_FILE), collection)?;
        Ok(CollectionFiles {
            directory,
            collection: collection.clone(),
        })
    }

    pub fn save_collection(&self, files: &CollectionFiles) -> Result<(), WorkspaceError> {
        self.write_toml(&files.directory.join(COLLECTION_FILE), &files.collection)
    }

    pub fn collections(&self) -> Result<Vec<CollectionFiles>, WorkspaceError> {
        let directory = self.root.join("collections");
        if !directory.exists() {
            return Ok(Vec::new());
        }
        let mut collections = Vec::new();
        for entry in read_dir_sorted(&directory)? {
            if entry
                .file_type()
                .map_err(|source| WorkspaceError::Io {
                    path: entry.path(),
                    source,
                })?
                .is_dir()
            {
                let collection_path = entry.path().join(COLLECTION_FILE);
                if collection_path.is_file() {
                    collections.push(CollectionFiles {
                        directory: entry.path(),
                        collection: read_toml(&collection_path)?,
                    });
                }
            }
        }
        Ok(collections)
    }

    /// Validate every canonical collection, request and environment file.
    ///
    /// The scan is read-only and keeps going after a malformed file so a CLI
    /// or GUI can show all actionable issues in one pass. Ignored local state
    /// under `.postly/` is intentionally outside this report.
    pub fn validate(&self) -> Result<WorkspaceValidationReport, WorkspaceError> {
        let mut report = WorkspaceValidationReport {
            collections: 0,
            requests: 0,
            environments: 0,
            issues: Vec::new(),
        };
        let collections_directory = self.root.join("collections");
        if collections_directory.is_dir() {
            for entry in read_dir_sorted(&collections_directory)? {
                let directory = entry.path();
                if !entry
                    .file_type()
                    .map_err(|source| WorkspaceError::Io {
                        path: directory.clone(),
                        source,
                    })?
                    .is_dir()
                {
                    continue;
                }
                let collection_path = directory.join(COLLECTION_FILE);
                if !collection_path.is_file() {
                    report.issues.push(WorkspaceValidationIssue {
                        path: relative_path(&self.root, &collection_path),
                        message: "collection manifest is missing".to_owned(),
                    });
                } else {
                    match read_toml::<Collection>(&collection_path) {
                        Ok(_) => report.collections += 1,
                        Err(error) => report.issues.push(WorkspaceValidationIssue {
                            path: relative_path(&self.root, &collection_path),
                            message: error.to_string(),
                        }),
                    }
                }

                let requests_directory = directory.join("requests");
                let mut request_paths = Vec::new();
                collect_files(&requests_directory, &mut request_paths)?;
                for request_path in request_paths {
                    match read_toml::<Request>(&request_path) {
                        Ok(_) => report.requests += 1,
                        Err(error) => report.issues.push(WorkspaceValidationIssue {
                            path: relative_path(&self.root, &request_path),
                            message: error.to_string(),
                        }),
                    }
                }
            }
        }

        let environments_directory = self.root.join("environments");
        if environments_directory.is_dir() {
            for entry in read_dir_sorted(&environments_directory)? {
                let path = entry.path();
                if !entry
                    .file_type()
                    .map_err(|source| WorkspaceError::Io {
                        path: path.clone(),
                        source,
                    })?
                    .is_file()
                    || path
                        .extension()
                        .map_or(true, |extension| extension != "toml")
                {
                    continue;
                }
                match read_toml::<Environment>(&path) {
                    Ok(_) => report.environments += 1,
                    Err(error) => report.issues.push(WorkspaceValidationIssue {
                        path: relative_path(&self.root, &path),
                        message: error.to_string(),
                    }),
                }
            }
        }
        Ok(report)
    }

    pub fn save_request(
        &self,
        collection: &CollectionFiles,
        request: &Request,
    ) -> Result<PathBuf, WorkspaceError> {
        let path = self.request_path_for(collection, request, None)?;
        self.write_request_file(&path, request)?;
        Ok(path)
    }

    pub fn relocate_request(
        &self,
        path: impl AsRef<Path>,
        collection: &CollectionFiles,
        request: &Request,
    ) -> Result<PathBuf, WorkspaceError> {
        let old_path = path.as_ref();
        if !is_request_path(&self.root, old_path) {
            return Err(WorkspaceError::InvalidName(old_path.display().to_string()));
        }
        let new_path = self.request_path_for(collection, request, Some(old_path))?;
        self.write_request_file(&new_path, request)?;
        if new_path != old_path {
            if let Err(source) = fs::remove_file(old_path) {
                let _ = fs::remove_file(&new_path);
                return Err(WorkspaceError::Io {
                    path: old_path.to_path_buf(),
                    source,
                });
            }
        }
        Ok(new_path)
    }

    fn request_path_for(
        &self,
        collection: &CollectionFiles,
        request: &Request,
        current_path: Option<&Path>,
    ) -> Result<PathBuf, WorkspaceError> {
        let mut directory = collection.directory.join("requests");
        if let Some(folder) = request
            .folder
            .as_deref()
            .filter(|folder| !folder.trim().is_empty())
        {
            for segment in folder.split(['/', '\\']) {
                if segment.trim().is_empty() {
                    continue;
                }
                let slug = slugify(segment)?;
                directory.push(slug);
            }
        }
        let base = slugify(&request.name)?;
        let preferred = directory.join(format!("{base}{REQUEST_SUFFIX}"));
        if current_path.is_some_and(|path| path == preferred) || !preferred.exists() {
            return Ok(preferred);
        }
        let identity = &request.id.to_string()[..8];
        let identity_path = directory.join(format!("{base}-{identity}{REQUEST_SUFFIX}"));
        if current_path.is_some_and(|path| path == identity_path) || !identity_path.exists() {
            return Ok(identity_path);
        }
        let mut suffix = 2_u32;
        loop {
            let path = directory.join(format!("{base}-{identity}-{suffix}{REQUEST_SUFFIX}"));
            if current_path.is_some_and(|current| current == path) || !path.exists() {
                return Ok(path);
            }
            suffix = suffix.saturating_add(1);
        }
    }

    fn write_request_file(&self, path: &Path, request: &Request) -> Result<(), WorkspaceError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| WorkspaceError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        self.write_toml(path, request)
    }

    pub fn update_request(
        &self,
        path: impl AsRef<Path>,
        request: &Request,
    ) -> Result<(), WorkspaceError> {
        self.write_toml(path.as_ref(), request)
    }

    pub fn duplicate_request(
        &self,
        collection: &CollectionFiles,
        request: &Request,
    ) -> Result<PathBuf, WorkspaceError> {
        let mut duplicate = request.clone();
        duplicate.id = uuid::Uuid::new_v4();
        duplicate.name = format!("{} copy", request.name);
        self.save_request(collection, &duplicate)
    }

    pub fn delete_request(&self, path: impl AsRef<Path>) -> Result<(), WorkspaceError> {
        let path = path.as_ref();
        let relative = path
            .strip_prefix(&self.root)
            .map_err(|_| WorkspaceError::InvalidName(path.display().to_string()))?;
        let has_parent = relative
            .components()
            .any(|component| component == Component::ParentDir);
        if has_parent || !is_request_path(&self.root, path) {
            return Err(WorkspaceError::InvalidName(path.display().to_string()));
        }
        fs::remove_file(path).map_err(|source| WorkspaceError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn load_request(&self, path: impl AsRef<Path>) -> Result<Request, WorkspaceError> {
        read_toml(path.as_ref())
    }

    pub fn requests(
        &self,
        collection: &CollectionFiles,
    ) -> Result<Vec<(PathBuf, Request)>, WorkspaceError> {
        let requests_directory = collection.directory.join("requests");
        let mut paths = Vec::new();
        collect_files(&requests_directory, &mut paths)?;
        paths.sort();
        paths
            .into_iter()
            .map(|path| {
                let request = self.load_request(&path)?;
                Ok((path, request))
            })
            .collect()
    }

    /// Search all saved requests by collection, folder, name, method, URL or
    /// description, in deterministic filesystem order.
    pub fn search_requests(&self, query: &str) -> Result<Vec<RequestSearchResult>, WorkspaceError> {
        let query = query.trim().to_ascii_lowercase();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let mut results = Vec::new();
        for collection in self.collections()? {
            for (path, request) in self.requests(&collection)? {
                let folder = request.folder.clone();
                let fields = [
                    collection.collection.name.as_str(),
                    folder.as_deref().unwrap_or_default(),
                    request.name.as_str(),
                    request.method.as_str(),
                    request.url.as_str(),
                    request.description.as_deref().unwrap_or_default(),
                ];
                if fields
                    .iter()
                    .any(|field| field.to_ascii_lowercase().contains(&query))
                {
                    let relative_path = path
                        .strip_prefix(&self.root)
                        .map(Path::to_path_buf)
                        .unwrap_or(path);
                    results.push(RequestSearchResult {
                        collection_id: collection.collection.id,
                        collection: collection.collection.name.clone(),
                        folder,
                        path: relative_path,
                        id: request.id,
                        name: request.name,
                        method: request.method,
                        url: request.url,
                    });
                }
            }
        }
        Ok(results)
    }

    pub fn save_environment(&self, environment: &Environment) -> Result<PathBuf, WorkspaceError> {
        let path = self.root.join("environments").join(format!(
            "{}{}",
            slugify(&environment.name)?,
            ENVIRONMENT_SUFFIX
        ));
        self.write_toml(&path, environment)?;
        Ok(path)
    }

    pub fn environments(&self) -> Result<Vec<(PathBuf, Environment)>, WorkspaceError> {
        let directory = self.root.join("environments");
        if !directory.exists() {
            return Ok(Vec::new());
        }
        let mut paths = read_dir_sorted(&directory)?
            .into_iter()
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "toml")
            })
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        paths.sort();
        paths
            .into_iter()
            .map(|path| Ok((path.clone(), read_toml(&path)?)))
            .collect()
    }

    pub fn history_path(&self) -> PathBuf {
        self.root.join(HISTORY_FILE)
    }

    pub fn record_history(&self, entry: &HistoryEntry) -> Result<PathBuf, WorkspaceError> {
        let path = self.history_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| WorkspaceError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let line = serde_json::to_string(entry).map_err(|source| WorkspaceError::Json {
            path: path.clone(),
            source,
        })?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| WorkspaceError::Io {
                path: path.clone(),
                source,
            })?;
        writeln!(file, "{line}").map_err(|source| WorkspaceError::Io {
            path: path.clone(),
            source,
        })?;
        self.compact_history(&path)?;
        Ok(path)
    }

    pub fn history(&self, limit: usize) -> Result<Vec<HistoryEntry>, WorkspaceError> {
        self.history_filtered(limit, &HistoryFilter::default())
    }

    pub fn history_filtered(
        &self,
        limit: usize,
        filter: &HistoryFilter,
    ) -> Result<Vec<HistoryEntry>, WorkspaceError> {
        let path = self.history_path();
        if !path.is_file() || limit == 0 {
            return Ok(Vec::new());
        }
        let text = fs::read_to_string(&path).map_err(|source| WorkspaceError::Io {
            path: path.clone(),
            source,
        })?;
        let mut entries = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str::<HistoryEntry>(line).map_err(|source| WorkspaceError::Json {
                    path: path.clone(),
                    source,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        entries.reverse();
        Ok(entries
            .into_iter()
            .filter(|entry| filter.matches(entry))
            .take(limit)
            .collect())
    }

    pub fn clear_history(&self) -> Result<(), WorkspaceError> {
        let path = self.history_path();
        if !path.is_file() {
            return Ok(());
        }
        OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .map_err(|source| WorkspaceError::Io { path, source })?;
        Ok(())
    }

    fn manifest_path(&self) -> PathBuf {
        self.root.join(MANIFEST_FILE)
    }

    fn compact_history(&self, path: &Path) -> Result<(), WorkspaceError> {
        let metadata = fs::metadata(path).map_err(|source| WorkspaceError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let file = File::open(path).map_err(|source| WorkspaceError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mut retained = VecDeque::new();
        let mut total_entries = 0_usize;
        let mut retained_bytes = 0_usize;
        for line in io::BufReader::new(file).lines() {
            let line = line.map_err(|source| WorkspaceError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            if line.trim().is_empty() {
                continue;
            }
            total_entries += 1;
            retained_bytes = retained_bytes.saturating_add(line.len() + 1);
            retained.push_back(line);
            while retained.len() > MAX_HISTORY_ENTRIES
                || (retained_bytes > MAX_HISTORY_BYTES && retained.len() > 1)
            {
                if let Some(oldest) = retained.pop_front() {
                    retained_bytes = retained_bytes.saturating_sub(oldest.len() + 1);
                }
            }
        }
        if metadata.len() <= MAX_HISTORY_BYTES as u64 && total_entries <= MAX_HISTORY_ENTRIES {
            return Ok(());
        }

        let mut compacted = String::new();
        for line in retained {
            compacted.push_str(&line);
            compacted.push('\n');
        }
        atomic_write(path, compacted.as_bytes()).map_err(|source| WorkspaceError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    fn write_toml<T: serde::Serialize>(
        &self,
        path: &Path,
        value: &T,
    ) -> Result<(), WorkspaceError> {
        let text =
            toml::to_string_pretty(value).map_err(|source| WorkspaceError::TomlSerialize {
                path: path.to_path_buf(),
                source,
            })?;
        atomic_write(path, format!("{text}\n").as_bytes()).map_err(|source| WorkspaceError::Io {
            path: path.to_path_buf(),
            source,
        })
    }
}

fn read_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, WorkspaceError> {
    let text = fs::read_to_string(path).map_err(|source| WorkspaceError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| WorkspaceError::Toml {
        path: path.to_path_buf(),
        source,
    })
}

fn relative_path(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf())
}

fn read_dir_sorted(directory: &Path) -> Result<Vec<fs::DirEntry>, WorkspaceError> {
    let entries = fs::read_dir(directory).map_err(|source| WorkspaceError::Io {
        path: directory.to_path_buf(),
        source,
    })?;
    let mut entries = entries
        .map(|entry| {
            entry.map_err(|source| WorkspaceError::Io {
                path: directory.to_path_buf(),
                source,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

fn collect_files(directory: &Path, paths: &mut Vec<PathBuf>) -> Result<(), WorkspaceError> {
    if !directory.exists() {
        return Ok(());
    }
    for entry in read_dir_sorted(directory)? {
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| WorkspaceError::Io {
            path: path.clone(),
            source,
        })?;
        if file_type.is_dir() {
            collect_files(&path, paths)?;
        } else if file_type.is_file()
            && path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(REQUEST_SUFFIX))
        {
            paths.push(path);
        }
    }
    Ok(())
}

fn is_request_path(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let components = relative.components().collect::<Vec<_>>();
    components.len() >= 4
        && components
            .iter()
            .all(|component| matches!(component, Component::Normal(_)))
        && components[0].as_os_str() == "collections"
        && components[2].as_os_str() == "requests"
        && path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(REQUEST_SUFFIX))
}

fn slugify(value: &str) -> Result<String, WorkspaceError> {
    let slug = value
        .trim()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() || slug == "." || slug == ".." {
        Err(WorkspaceError::InvalidName(value.to_owned()))
    } else {
        Ok(slug)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        history::{HistoryEntry, HistoryFilter},
        model::Request,
    };

    #[test]
    fn writes_and_reopens_git_friendly_request_files() {
        let directory = tempfile::tempdir().expect("tempdir");
        let workspace = Workspace::init(directory.path(), "Demo API").expect("init");
        let collection = workspace
            .create_collection(&Collection::new("Users"))
            .expect("collection");
        let mut request = Request::new("List users", "GET", "https://example.com/users");
        request.folder = Some("Users / Read".to_owned());
        let path = workspace
            .save_request(&collection, &request)
            .expect("request");

        let reopened = Workspace::open(directory.path()).expect("reopen");
        let requests = reopened
            .requests(&reopened.collections().expect("collections")[0])
            .expect("requests");

        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].1.url, request.url);
        assert!(path.to_string_lossy().contains("users/read"));

        request.name = "List all users".to_owned();
        request.folder = Some("Users / Write".to_owned());
        let reopened_collection = reopened.collections().expect("collections")[0].clone();
        let relocated = reopened
            .relocate_request(&path, &reopened_collection, &request)
            .expect("relocate request");
        let updated = reopened.load_request(&relocated).expect("updated request");
        assert_eq!(updated.name, "List all users");
        assert!(relocated.to_string_lossy().contains("users/write"));
        assert!(!path.exists());
    }

    #[test]
    fn canonical_file_writes_leave_only_the_committed_destination() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("request.postly.toml");
        atomic_write(&path, b"name = \"first\"\n").expect("first write");
        atomic_write(&path, b"name = \"second\"\n").expect("replacement write");

        assert_eq!(
            std::fs::read_to_string(&path).expect("destination"),
            "name = \"second\"\n"
        );
        let entries = std::fs::read_dir(directory.path())
            .expect("directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].file_name(), "request.postly.toml");
    }

    #[test]
    fn failed_request_relocation_rolls_back_the_new_destination() {
        let directory = tempfile::tempdir().expect("tempdir");
        let workspace = Workspace::init(directory.path(), "Demo API").expect("init");
        let collection = workspace
            .create_collection(&Collection::new("Users"))
            .expect("collection");
        let request = Request::new("List users", "GET", "https://example.com/users");
        let old_path = workspace
            .save_request(&collection, &request)
            .expect("request");

        std::fs::remove_file(&old_path).expect("remove request for failure setup");
        std::fs::create_dir(&old_path).expect("replace old path with directory");
        let mut renamed = request;
        renamed.name = "Renamed users".to_owned();

        assert!(workspace
            .relocate_request(&old_path, &collection, &renamed)
            .is_err());
        assert!(old_path.is_dir());
        assert!(!collection
            .directory
            .join("requests/renamed-users.postly.toml")
            .exists());
    }

    #[test]
    fn validates_canonical_files_and_reports_all_parse_failures() {
        let directory = tempfile::tempdir().expect("tempdir");
        let workspace = Workspace::init(directory.path(), "Demo API").expect("init");
        let collection = workspace
            .create_collection(&Collection::new("Users"))
            .expect("collection");
        let request = Request::new("List users", "GET", "https://example.com/users");
        let request_path = workspace
            .save_request(&collection, &request)
            .expect("request");
        let environment_path = workspace
            .save_environment(&Environment::new("Local"))
            .expect("environment");

        let valid = workspace.validate().expect("valid report");
        assert!(valid.is_valid());
        assert_eq!(valid.collections, 1);
        assert_eq!(valid.requests, 1);
        assert_eq!(valid.environments, 1);

        std::fs::write(&request_path, "not = [valid").expect("corrupt request");
        std::fs::write(&environment_path, "not = [valid").expect("corrupt environment");
        let invalid = workspace.validate().expect("invalid report");
        assert!(!invalid.is_valid());
        assert_eq!(invalid.collections, 1);
        assert_eq!(invalid.requests, 0);
        assert_eq!(invalid.environments, 0);
        assert_eq!(invalid.issues.len(), 2);
        assert!(invalid
            .issues
            .iter()
            .any(|issue| issue.path == request_path.strip_prefix(directory.path()).unwrap()));
        assert!(invalid.issues.iter().any(|issue| {
            issue.path == environment_path.strip_prefix(directory.path()).unwrap()
        }));
    }

    #[test]
    fn duplicates_and_deletes_only_request_files_inside_the_workspace() {
        let directory = tempfile::tempdir().expect("tempdir");
        let workspace = Workspace::init(directory.path(), "Demo API").expect("init");
        let collection = workspace
            .create_collection(&Collection::new("Users"))
            .expect("collection");
        let request = Request::new("List users", "GET", "https://example.com/users");
        let original_path = workspace
            .save_request(&collection, &request)
            .expect("request");
        let duplicate_path = workspace
            .duplicate_request(&collection, &request)
            .expect("duplicate");

        let requests = workspace.requests(&collection).expect("requests");
        assert_eq!(requests.len(), 2);
        assert!(requests
            .iter()
            .any(|(_, request)| request.name == "List users copy"));
        assert_ne!(requests[0].1.id, requests[1].1.id);

        workspace
            .delete_request(&duplicate_path)
            .expect("delete duplicate");
        assert_eq!(workspace.requests(&collection).expect("remaining").len(), 1);
        assert!(workspace
            .delete_request(directory.path().join("postly.toml"))
            .is_err());
        assert!(workspace
            .delete_request(directory.path().join("collections/../postly.toml"))
            .is_err());
        assert!(original_path.is_file());
    }

    #[test]
    fn stores_newest_history_entries_without_request_secrets() {
        let directory = tempfile::tempdir().expect("tempdir");
        let workspace = Workspace::init(directory.path(), "Demo API").expect("init");
        let request = Request::new(
            "List users",
            "GET",
            "https://user:password@example.com/users?token=secret",
        );
        workspace
            .record_history(&HistoryEntry::from_error(&request, 12))
            .expect("history");

        let entries = workspace.history(10).expect("read history");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].url, "https://[redacted]example.com/users");
        assert_eq!(entries[0].outcome, crate::HistoryOutcome::Error);
    }

    #[test]
    fn filters_and_clears_local_history() {
        let directory = tempfile::tempdir().expect("tempdir");
        let workspace = Workspace::init(directory.path(), "Demo API").expect("init");
        let success = Request::new("List users", "GET", "https://example.com/users");
        let failure = Request::new("Create user", "POST", "https://example.com/users");
        workspace
            .record_history(&HistoryEntry::from_response(
                &success,
                &crate::HttpResponse {
                    status: 200,
                    status_text: "OK".to_owned(),
                    headers: Vec::new(),
                    body: Vec::new(),
                    response_size: 0,
                    content_type: None,
                    duration_ms: 8,
                    protocol: "HTTP/1.1".to_owned(),
                    url: "https://example.com/users".to_owned(),
                    cookies: Vec::new(),
                },
            ))
            .expect("success history");
        workspace
            .record_history(&HistoryEntry::from_error(&failure, 13))
            .expect("failure history");

        let entries = workspace
            .history_filtered(
                10,
                &HistoryFilter {
                    method: Some("get".to_owned()),
                    status: Some(200),
                    ..HistoryFilter::default()
                },
            )
            .expect("filtered history");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].request_name, "List users");

        workspace.clear_history().expect("clear history");
        assert!(workspace.history(10).expect("empty history").is_empty());
    }

    #[test]
    fn bounds_history_to_the_newest_entries() {
        let directory = tempfile::tempdir().expect("tempdir");
        let workspace = Workspace::init(directory.path(), "Demo API").expect("init");
        for index in 0..(MAX_HISTORY_ENTRIES + 5) {
            let request = Request::new(
                format!("Request {index}"),
                "GET",
                "https://example.com/health",
            );
            workspace
                .record_history(&HistoryEntry::from_error(&request, index as u64))
                .expect("history");
        }

        let entries = workspace.history(usize::MAX).expect("bounded history");
        assert_eq!(entries.len(), MAX_HISTORY_ENTRIES);
        assert_eq!(entries[0].request_name, "Request 1004");
        assert!(!entries
            .iter()
            .any(|entry| entry.request_name == "Request 0"));
    }

    #[test]
    fn searches_request_metadata_across_collections_without_secret_values() {
        let directory = tempfile::tempdir().expect("tempdir");
        let workspace = Workspace::init(directory.path(), "Demo API").expect("init");
        let users = workspace
            .create_collection(&Collection::new("Users"))
            .expect("users collection");
        let billing = workspace
            .create_collection(&Collection::new("Billing"))
            .expect("billing collection");

        let mut users_request =
            Request::new("List administrators", "GET", "https://example.com/users");
        users_request.folder = Some("Admin / Read".to_owned());
        users_request.description = Some("Searchable operational endpoint".to_owned());
        users_request.headers.push(crate::HeaderEntry::enabled(
            "Authorization",
            "Bearer secret",
        ));
        workspace
            .save_request(&users, &users_request)
            .expect("users request");

        let billing_request = Request::new("Charge card", "POST", "https://example.com/charge");
        workspace
            .save_request(&billing, &billing_request)
            .expect("billing request");

        let results = workspace.search_requests("ADMIN / READ").expect("search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].collection, "Users");
        assert_eq!(results[0].folder.as_deref(), Some("Admin / Read"));
        assert!(results[0].path.ends_with("list-administrators.postly.toml"));
        assert!(workspace
            .search_requests("secret")
            .expect("secret search")
            .is_empty());
        assert!(workspace
            .search_requests(" ")
            .expect("empty search")
            .is_empty());
    }
}
