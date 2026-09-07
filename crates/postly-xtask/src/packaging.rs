//! Native-host release packaging. Building for an OS is not validation on it.

use serde_json::json;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

type Result<T> = std::result::Result<T, String>;

pub fn executable(name: &str, os: &str) -> String {
    format!("{name}{}", if os == "windows" { ".exe" } else { "" })
}

fn output(command: &mut Command) -> Result<String> {
    let result = command.output().map_err(|e| format!("{command:?}: {e}"))?;
    if !result.status.success() {
        return Err(format!(
            "{command:?}: {}\n{}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&result.stdout).trim().to_owned())
}

fn copy(source: impl AsRef<Path>, destination: impl AsRef<Path>) -> Result<()> {
    let source = source.as_ref();
    let destination = destination.as_ref();
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::copy(source, destination)
        .map_err(|e| format!("{} -> {}: {e}", source.display(), destination.display()))?;
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(source).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        if file_type.is_dir() {
            copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
        } else if file_type.is_file() {
            copy(entry.path(), destination.join(entry.file_name()))?;
        } else {
            return Err(format!(
                "Unexpected non-regular package entry: {}",
                entry.path().display()
            ));
        }
    }
    Ok(())
}

fn files(root: &Path, directory: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_dir() {
            files(root, &entry.path(), paths)?;
        } else if kind.is_file() {
            paths.push(
                entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|e| e.to_string())?
                    .to_owned(),
            );
        } else {
            return Err("Package checksums require regular files, not symlinks.".into());
        }
    }
    Ok(())
}

fn checksums(root: &Path) -> Result<String> {
    let mut paths = Vec::new();
    files(root, root, &mut paths)?;
    paths.sort();
    let mut manifest = String::new();
    for path in paths {
        manifest.push_str(&format!(
            "{}  {}\n",
            crate::sha256_hex(&root.join(&path))?,
            path.to_string_lossy().replace('\\', "/")
        ));
    }
    Ok(manifest)
}

fn smoke_example(cli: &Path) -> Result<()> {
    let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
    let server = postly_core::demo::DemoServer::start(0).map_err(|e| e.to_string())?;
    postly_core::demo::create_workspace(directory.path(), &server.url())?;
    let report = output(
        Command::new(cli)
            .arg("run")
            .arg(directory.path())
            .args(["--reporter", "json"]),
    )?;
    let report: serde_json::Value = serde_json::from_str(&report).map_err(|e| e.to_string())?;
    if report[0]["passed"] != 2 || report[0]["failed"] != 0 || report[0]["assertions"] != 5 {
        return Err("Packaged CLI did not pass both demo requests and all five assertions.".into());
    }
    Ok(())
}

pub fn package() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .map_err(|e| e.to_string())?;
    // An explicit target in Cargo configuration must not be mislabeled as host.
    let metadata: serde_json::Value =
        serde_json::from_str(&output(Command::new("cargo").current_dir(&root).args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
        ]))?)
        .map_err(|e| e.to_string())?;
    let target = PathBuf::from(
        metadata["target_directory"]
            .as_str()
            .ok_or("Cargo omitted target_directory")?,
    );
    let rustc = output(Command::new("rustc").arg("-vV"))?;
    let triple = rustc
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or("rustc did not report its host target")?;
    let build = Command::new("cargo")
        .current_dir(&root)
        .args([
            "build",
            "--locked",
            "--release",
            "--target",
            triple,
            "-p",
            "postly",
            "-p",
            "postly-app",
        ])
        .env("CARGO_PROFILE_RELEASE_DEBUG", "0")
        .env("OPENSSL_NO_VENDOR", "0")
        .status()
        .map_err(|e| e.to_string())?;
    if !build.success() {
        return Err("The locked release build failed.".into());
    }

    let revision = output(
        Command::new("git")
            .current_dir(&root)
            .args(["rev-parse", "HEAD"]),
    )?;
    let dirty = !output(Command::new("git").current_dir(&root).args([
        "status",
        "--porcelain",
        "--untracked-files=normal",
    ]))?
    .is_empty();
    let os = env::consts::OS;
    let name = format!(
        "postly-v{}-{os}-{}",
        env!("CARGO_PKG_VERSION"),
        env::consts::ARCH
    );
    let dist = root.join("dist");
    fs::create_dir_all(&dist).map_err(|e| e.to_string())?;
    let staging = tempfile::tempdir_in(&dist).map_err(|e| e.to_string())?;
    let package = staging.path().join(&name);
    fs::create_dir_all(&package).map_err(|e| e.to_string())?;
    let cli = executable("postly", os);
    let gui = executable("postly-gui", os);
    for binary in [&cli, &gui] {
        copy(
            target.join(triple).join("release").join(binary),
            package.join(binary),
        )?;
    }
    for file in ["README.md", "LICENSE"] {
        copy(root.join(file), package.join(file))?;
    }
    copy(root.join("docs/install.md"), package.join("INSTALL.md"))?;
    copy_tree(&root.join("docs"), &package.join("docs"))?;
    copy(
        root.join("website/logo.svg"),
        package.join("website/logo.svg"),
    )?;
    copy(
        root.join("packaging/OPENSSL-LICENSE.txt"),
        package.join("OPENSSL-LICENSE.txt"),
    )?;
    copy_tree(
        &root.join("examples/orders"),
        &package.join("examples/orders"),
    )?;

    if os == "macos" {
        macos_bundle(&root, &package, &cli, &gui)?;
    }
    if os == "linux" {
        copy(
            root.join("packaging/postly.desktop"),
            package.join("postly.desktop"),
        )?;
        copy(root.join("website/logo.svg"), package.join("postly.svg"))?;
    }
    for argument in ["--version", "--help"] {
        output(Command::new(package.join(&cli)).arg(argument))?;
    }
    let provenance = json!({
        "name":"Postly", "version":env!("CARGO_PKG_VERSION"), "platform":os,
        "architecture":env::consts::ARCH, "target":triple, "source_commit":revision,
        "source_dirty":dirty, "rustc":rustc, "profile":"release", "locked":true,
        "binaries":[cli, gui], "signing":if os == "macos" { "ad-hoc; not notarized" } else { "unsigned" },
        "validation":"packaged CLI and extracted archive smoke checks including two local HTTP requests and five assertions; desktop and clean-machine validation are separate gates"
    });
    fs::write(
        package.join("postly-package.json"),
        serde_json::to_vec_pretty(&provenance).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    fs::write(package.join("SHA256SUMS"), checksums(&package)?).map_err(|e| e.to_string())?;

    let extension = if os == "windows" { "zip" } else { "tar.gz" };
    let archive_name = format!("{name}.{extension}");
    let archive = staging.path().join(&archive_name);
    if os == "windows" {
        output(Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", "Compress-Archive -LiteralPath $env:POSTLY_PACKAGE_DIR -DestinationPath $env:POSTLY_PACKAGE_ARCHIVE"])
            .env("POSTLY_PACKAGE_DIR", &package).env("POSTLY_PACKAGE_ARCHIVE", &archive))?;
    } else {
        output(
            Command::new("tar")
                .arg("-czf")
                .arg(&archive)
                .arg("-C")
                .arg(staging.path())
                .arg(&name),
        )?;
    }
    // Extract and inspect the final archive, not just the staging directory.
    let unpacked = staging.path().join("unpacked");
    fs::create_dir(&unpacked).map_err(|e| e.to_string())?;
    if os == "windows" {
        output(Command::new("powershell").args(["-NoProfile", "-NonInteractive", "-Command", "Expand-Archive -LiteralPath $env:POSTLY_PACKAGE_ARCHIVE -DestinationPath $env:POSTLY_UNPACK_DIR"])
            .env("POSTLY_PACKAGE_ARCHIVE", &archive).env("POSTLY_UNPACK_DIR", &unpacked))?;
    } else {
        output(
            Command::new("tar")
                .arg("-xzf")
                .arg(&archive)
                .arg("-C")
                .arg(&unpacked),
        )?;
    }
    let extracted = unpacked.join(&name);
    let original_sums =
        fs::read_to_string(extracted.join("SHA256SUMS")).map_err(|e| e.to_string())?;
    for line in original_sums.lines() {
        let (expected, file) = line.split_once("  ").ok_or("Malformed checksum line")?;
        if crate::sha256_hex(&extracted.join(file))? != expected {
            return Err(format!("Archive checksum mismatch: {file}"));
        }
    }
    output(Command::new(extracted.join(&cli)).arg("--version"))?;
    smoke_example(&extracted.join(&cli))?;
    let mut assets = vec![archive_name];
    if os == "macos" {
        let dmg_name = format!("{name}.dmg");
        let dmg_root = staging.path().join("disk-image");
        fs::create_dir(&dmg_root).map_err(|e| e.to_string())?;
        copy_tree(&package.join("Postly.app"), &dmg_root.join("Postly.app"))?;
        copy(package.join("INSTALL.md"), dmg_root.join("INSTALL.md"))?;
        #[cfg(unix)]
        std::os::unix::fs::symlink("/Applications", dmg_root.join("Applications"))
            .map_err(|e| e.to_string())?;
        output(
            Command::new("hdiutil")
                .args([
                    "create",
                    "-volname",
                    "Postly Preview",
                    "-format",
                    "UDZO",
                    "-srcfolder",
                ])
                .arg(&dmg_root)
                .arg(staging.path().join(&dmg_name)),
        )?;
        output(
            Command::new("hdiutil")
                .arg("verify")
                .arg(staging.path().join(&dmg_name)),
        )?;
        assets.push(dmg_name);
    }
    let mut outer_sums = String::new();
    for asset in &assets {
        outer_sums.push_str(&format!(
            "{}  {asset}\n",
            crate::sha256_hex(&staging.path().join(asset))?
        ));
        copy(staging.path().join(asset), dist.join(asset))?;
        println!("artifact: {}", dist.join(asset).display());
    }
    // Target-specific lists can be merged into release SHA256SUMS after all OS builds.
    fs::write(dist.join(format!("{name}-SHA256SUMS")), &outer_sums).map_err(|e| e.to_string())?;
    copy(
        package.join("postly-package.json"),
        dist.join(format!("{name}-manifest.json")),
    )?;
    println!("{outer_sums}");
    if dirty {
        eprintln!("Preview built with uncommitted changes. Rebuild from a clean commit before publishing.");
    }
    Ok(())
}

fn macos_bundle(root: &Path, package: &Path, cli: &str, gui: &str) -> Result<()> {
    let app = package.join("Postly.app");
    let contents = app.join("Contents");
    let executables = contents.join("MacOS");
    let resources = contents.join("Resources");
    fs::create_dir_all(&resources).map_err(|e| e.to_string())?;
    for binary in [cli, gui] {
        copy(package.join(binary), executables.join(binary))?;
        let libraries = output(
            Command::new("otool")
                .arg("-L")
                .arg(executables.join(binary)),
        )?;
        for dependency in libraries
            .lines()
            .skip(1)
            .filter_map(|line| line.split_whitespace().next())
        {
            if !dependency.starts_with("/System/Library/") && !dependency.starts_with("/usr/lib/") {
                return Err(format!("{binary} depends on non-system library {dependency}; do not ship a developer-machine dependency."));
            }
        }
        output(
            Command::new("codesign")
                .args(["--force", "--sign", "-"])
                .arg(executables.join(binary)),
        )?;
    }
    let version = env!("CARGO_PKG_VERSION");
    let plist = fs::read_to_string(root.join("packaging/Info.plist"))
        .map_err(|e| e.to_string())?
        .replace("@VERSION@", version);
    fs::write(contents.join("Info.plist"), plist).map_err(|e| e.to_string())?;
    copy(
        root.join("packaging/Postly.icns"),
        resources.join("Postly.icns"),
    )?;
    copy(root.join("LICENSE"), resources.join("LICENSE"))?;
    copy(
        root.join("packaging/OPENSSL-LICENSE.txt"),
        resources.join("OPENSSL-LICENSE.txt"),
    )?;
    output(
        Command::new("plutil")
            .arg("-lint")
            .arg(contents.join("Info.plist")),
    )?;
    output(
        Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(&app),
    )?;
    output(
        Command::new("codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(&app),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn executable_names_follow_target_platform() {
        assert_eq!(executable("postly", "windows"), "postly.exe");
        assert_eq!(executable("postly-gui", "windows"), "postly-gui.exe");
        assert_eq!(executable("postly", "macos"), "postly");
        assert_eq!(executable("postly-gui", "linux"), "postly-gui");
    }
    #[test]
    fn checksums_include_nested_app_resources_and_detect_changes() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("Postly.app/Contents")).unwrap();
        fs::write(
            directory.path().join("Postly.app/Contents/Info.plist"),
            "version one",
        )
        .unwrap();
        let before = checksums(directory.path()).unwrap();
        assert!(before.contains("  Postly.app/Contents/Info.plist\n"));
        fs::write(
            directory.path().join("Postly.app/Contents/Info.plist"),
            "version two",
        )
        .unwrap();
        assert_ne!(before, checksums(directory.path()).unwrap());
    }
}
