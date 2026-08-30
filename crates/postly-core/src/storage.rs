use std::{
    fs, io,
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::model::{Collection, Environment, ProjectManifest, Request};

const MANIFEST_FILE: &str = "postly.toml";
const COLLECTION_FILE: &str = "postly.collection.toml";
const ENVIRONMENT_SUFFIX: &str = ".postly-env.toml";
const REQUEST_SUFFIX: &str = ".postly.toml";

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

    pub fn save_request(
        &self,
        collection: &CollectionFiles,
        request: &Request,
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
        fs::create_dir_all(&directory).map_err(|source| WorkspaceError::Io {
            path: directory.clone(),
            source,
        })?;
        let base = slugify(&request.name)?;
        let mut path = directory.join(format!("{base}{REQUEST_SUFFIX}"));
        if path.exists() {
            path = directory.join(format!(
                "{}-{}{}",
                base,
                &request.id.to_string()[..8],
                REQUEST_SUFFIX
            ));
        }
        self.write_toml(&path, request)?;
        Ok(path)
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

    fn manifest_path(&self) -> PathBuf {
        self.root.join(MANIFEST_FILE)
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
        fs::write(path, format!("{text}\n")).map_err(|source| WorkspaceError::Io {
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
    use crate::model::Request;

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
    }
}