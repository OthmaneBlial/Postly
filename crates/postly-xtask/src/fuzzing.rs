use std::{fs, io::Write, path::Path};

use sha2::{Digest, Sha256};

const TARGETS: [&str; 4] = [
    "curl_command",
    "variables",
    "postman_import",
    "native_workspace",
];

/// Merge reviewed seeds without replacing any previously discovered corpus file.
fn prepare_corpus(root: &Path, target: &str) -> Result<usize, String> {
    let source = root.join("fuzz/seeds").join(target);
    let destination = root.join("fuzz/corpus").join(target);
    let mut seeds = fs::read_dir(&source)
        .map_err(|error| format!("cannot read {}: {error}", source.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    seeds.sort_by_key(|entry| entry.file_name());
    if seeds.is_empty() {
        return Err(format!("no reviewed seeds for {target}"));
    }
    fs::create_dir_all(&destination).map_err(|error| error.to_string())?;
    for seed in &seeds {
        if !seed
            .file_type()
            .map_err(|error| error.to_string())?
            .is_file()
        {
            return Err(format!(
                "seed must be a regular file: {}",
                seed.path().display()
            ));
        }
        let bytes = fs::read(seed.path()).map_err(|error| error.to_string())?;
        let name = format!("seed-{:x}", Sha256::digest(&bytes));
        let path = destination.join(name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => file.write_all(&bytes).map_err(|error| error.to_string())?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if fs::read(&path).map_err(|error| error.to_string())? != bytes {
                    return Err(format!(
                        "existing corpus seed differs; refusing overwrite: {}",
                        path.display()
                    ));
                }
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(seeds.len())
}

pub(super) fn run_fuzz_smoke() -> bool {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for target in TARGETS {
        match prepare_corpus(&root, target) {
            Ok(count) => eprintln!("{target}: merged {count} reviewed seeds into the local corpus"),
            Err(error) => {
                eprintln!("fuzz preparation failed: {error}");
                return false;
            }
        }
    }
    if !super::run_in(
        &root,
        "cargo",
        &["+nightly", "fuzz", "check", "--fuzz-dir", "fuzz"],
    ) {
        return false;
    }
    TARGETS.into_iter().all(|target| {
        super::run_in(
            &root,
            "cargo",
            &[
                "+nightly",
                "fuzz",
                "run",
                "--fuzz-dir",
                "fuzz",
                target,
                "--",
                "-runs=1024",
                "-seed=1",
                "-rss_limit_mb=512",
                "-timeout=5",
            ],
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeding_is_repeatable_and_preserves_discovered_inputs() {
        let root = tempfile::tempdir().unwrap();
        let seeds = root.path().join("fuzz/seeds/curl_command");
        let corpus = root.path().join("fuzz/corpus/curl_command");
        fs::create_dir_all(&seeds).unwrap();
        fs::create_dir_all(&corpus).unwrap();
        fs::write(seeds.join("request.txt"), b"curl http://127.0.0.1/").unwrap();
        fs::write(corpus.join("discovered"), b"keep me").unwrap();
        assert_eq!(prepare_corpus(root.path(), "curl_command").unwrap(), 1);
        assert_eq!(prepare_corpus(root.path(), "curl_command").unwrap(), 1);
        assert_eq!(fs::read_dir(&corpus).unwrap().count(), 2);
        assert_eq!(fs::read(corpus.join("discovered")).unwrap(), b"keep me");
    }

    #[test]
    fn missing_or_empty_seed_sets_fail_explicitly() {
        let root = tempfile::tempdir().unwrap();
        assert!(prepare_corpus(root.path(), "variables").is_err());
        fs::create_dir_all(root.path().join("fuzz/seeds/variables")).unwrap();
        assert!(prepare_corpus(root.path(), "variables")
            .unwrap_err()
            .contains("no reviewed seeds"));
    }

    #[test]
    fn mismatched_existing_seed_is_not_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let seeds = root.path().join("fuzz/seeds/variables");
        fs::create_dir_all(&seeds).unwrap();
        fs::write(seeds.join("input.txt"), b"{{baseUrl}}").unwrap();
        prepare_corpus(root.path(), "variables").unwrap();
        let corpus = root.path().join("fuzz/corpus/variables");
        let stored = fs::read_dir(corpus)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        fs::write(&stored, b"do not replace").unwrap();
        assert!(prepare_corpus(root.path(), "variables")
            .unwrap_err()
            .contains("refusing overwrite"));
        assert_eq!(fs::read(stored).unwrap(), b"do not replace");
    }

    #[test]
    fn all_targets_have_reviewed_seeds_in_the_repository() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for target in TARGETS {
            let seeds = fs::read_dir(root.join("fuzz/seeds").join(target)).unwrap();
            assert!(seeds.count() > 0, "{target}");
        }
    }

    #[test]
    fn native_seeds_form_a_valid_workspace_not_only_syntax_errors() {
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/native_workspace");
        let root = tempfile::tempdir().unwrap();
        let workspace = postly_core::Workspace::init(root.path(), "Seed validation").unwrap();
        let collection = workspace
            .create_collection(&postly_core::Collection::new("seed"))
            .unwrap();
        for (seed, destination) in [
            ("manifest.toml", root.path().join("postly.toml")),
            (
                "collection.toml",
                collection.directory.join("postly.collection.toml"),
            ),
            (
                "request.toml",
                collection.directory.join("requests/seed.postly.toml"),
            ),
            (
                "environment.toml",
                root.path().join("environments/seed.postly-env.toml"),
            ),
        ] {
            fs::copy(source.join(seed), destination).unwrap();
        }
        let report = postly_core::Workspace::open(root.path())
            .unwrap()
            .validate()
            .unwrap();
        assert!(report.issues.is_empty(), "{:?}", report.issues);
        assert_eq!(
            (report.collections, report.requests, report.environments),
            (1, 1, 1)
        );
    }

    #[test]
    fn curl_and_postman_seeds_reach_request_construction() {
        let seeds = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds");
        for entry in fs::read_dir(seeds.join("curl_command")).unwrap() {
            let command = fs::read_to_string(entry.unwrap().path()).unwrap();
            postly_core::parse_curl_command(&command).unwrap();
        }
        for entry in fs::read_dir(seeds.join("postman_import")).unwrap() {
            let output = tempfile::tempdir().unwrap();
            let report =
                postly_core::import_postman_collection(entry.unwrap().path(), output.path())
                    .unwrap();
            assert_eq!(report.imported_requests, 1);
        }
    }
}
