#![no_main]

use std::fs;

use libfuzzer_sys::fuzz_target;
use postly_core::storage::Workspace;

const MAX_FUZZ_INPUT_BYTES: usize = 2 * 1024 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_FUZZ_INPUT_BYTES {
        return;
    }

    let Ok(manifest_directory) = tempfile::tempdir() else {
        return;
    };
    let manifest_path = manifest_directory.path().join("postly.toml");
    if fs::write(&manifest_path, data).is_ok() {
        let _ = Workspace::open(manifest_directory.path());
    }

    let Ok(workspace_directory) = tempfile::tempdir() else {
        return;
    };
    let Ok(workspace) = Workspace::init(workspace_directory.path(), "fuzz") else {
        return;
    };
    let collection_directory = workspace
        .root()
        .join("collections")
        .join("fuzz")
        .join("requests");
    let environment_directory = workspace.root().join("environments");
    if fs::create_dir_all(&collection_directory).is_err() {
        return;
    }
    if fs::write(
        collection_directory
            .parent()
            .expect("collection directory has a parent")
            .join("postly.collection.toml"),
        data,
    )
    .is_err()
    {
        return;
    }
    if fs::write(
        collection_directory.join("request.postly.toml"),
        data,
    )
    .is_err()
    {
        return;
    }
    if fs::write(environment_directory.join("fuzz.postly-env.toml"), data).is_err() {
        return;
    }
    let _ = workspace.validate();
});
