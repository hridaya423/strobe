use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

const FRAMEWORK: &str = "Chromium Embedded Framework.framework";
const HELPER_KINDS: [&str; 5] = ["", " (GPU)", " (Renderer)", " (Plugin)", " (Alerts)"];

pub fn cef_path() -> PathBuf {
    std::env::var_os("CEF_PATH").map_or_else(
        || PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share/cef"),
        PathBuf::from,
    )
}

pub fn is_bundled(exe: &Path) -> bool {
    let name = |path: Option<&Path>| path.and_then(Path::file_name).map(|n| n.to_owned());
    name(exe.parent()).is_some_and(|n| n == "MacOS")
        && name(exe.parent().and_then(Path::parent)).is_some_and(|n| n == "Contents")
}

fn plist(executable: &str, identifier: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>{executable}</string>
<key>CFBundleIdentifier</key><string>{identifier}</string>
<key>CFBundleName</key><string>{executable}</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>1.0</string>
<key>LSUIElement</key><string>1</string>
<key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict></plist>
"#
    )
}

fn up_to_date(source: &Path, copy: &Path) -> bool {
    let (Ok(source), Ok(copy)) = (fs::metadata(source), fs::metadata(copy)) else {
        return false;
    };
    source.len() == copy.len() && copy.modified().ok() >= source.modified().ok()
}

/// A signed binary must be removed before it is replaced. Overwriting it in place gets the
/// next launch killed with SIGKILL.
fn install_executable(source: &Path, copy: &Path) -> Result<()> {
    if up_to_date(source, copy) {
        return Ok(());
    }
    fs::create_dir_all(copy.parent().context("executable has no parent")?)?;
    match fs::remove_file(copy) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    fs::copy(source, copy).with_context(|| format!("copy to {}", copy.display()))?;
    Ok(())
}

fn bundle_app(app: &Path, executable: &str, identifier: &str, source: &Path) -> Result<PathBuf> {
    let installed = app.join("Contents/MacOS").join(executable);
    install_executable(source, &installed)?;
    write_if_changed(
        &app.join("Contents/Info.plist"),
        plist(executable, identifier).as_bytes(),
    )?;
    Ok(installed)
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(());
    }
    fs::write(path, bytes)
}

/// Copies the framework to a sibling name and renames it into place, so a copy that was
/// interrupted never leaves a partial framework under its real name.
fn install_framework(source: &Path, frameworks: &Path) -> Result<()> {
    let framework = frameworks.join(FRAMEWORK);
    if framework.exists() {
        return Ok(());
    }
    let partial = frameworks.join(format!("{FRAMEWORK}.partial"));
    match fs::remove_dir_all(&partial) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    let status = Command::new("cp")
        .arg("-c")
        .arg("-R")
        .arg(source)
        .arg(&partial)
        .status()
        .context("run cp")?;
    if !status.success() {
        let _ = fs::remove_dir_all(&partial);
        bail!("cannot copy {}", source.display());
    }
    fs::rename(&partial, &framework)
        .with_context(|| format!("move the copy to {}", framework.display()))
}

/// Builds `<profile dir>/bundle/<name>.app` around `exe` with the CEF framework and the five
/// helper apps, and returns the path of the bundled copy of `exe`.
pub fn build(exe: &Path) -> Result<PathBuf> {
    let dir = exe.parent().context("executable has no parent")?;
    let dir = if dir.file_name().is_some_and(|n| n == "deps") {
        dir.parent().context("deps has no parent")?
    } else {
        dir
    };
    let name = exe
        .file_name()
        .and_then(|n| n.to_str())
        .context("executable name is not UTF-8")?;
    let app = dir.join("bundle").join(format!("{name}.app"));
    let identifier = format!(
        "com.vrf.blink.{}",
        name.replace(|c: char| !c.is_ascii_alphanumeric(), "-")
    );
    let frameworks = app.join("Contents/Frameworks");
    fs::create_dir_all(&frameworks)?;
    let main = bundle_app(&app, name, &identifier, exe)?;

    install_framework(&cef_path().join(FRAMEWORK), &frameworks)?;
    for kind in HELPER_KINDS {
        let helper = format!("{name} Helper{kind}");
        let suffix: String = kind
            .chars()
            .filter(char::is_ascii_alphabetic)
            .collect::<String>()
            .to_lowercase();
        let helper_identifier = if suffix.is_empty() {
            format!("{identifier}.helper")
        } else {
            format!("{identifier}.helper.{suffix}")
        };
        bundle_app(
            &frameworks.join(format!("{helper}.app")),
            &helper,
            &helper_identifier,
            exe,
        )?;
    }
    Ok(main)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vrf-bundle-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_plist_is_written_only_when_it_changes() {
        let dir = scratch("plist");
        let path = dir.join("Info.plist");
        write_if_changed(&path, b"one").unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).unwrap();
        write_if_changed(&path, b"one").unwrap();
        assert!(write_if_changed(&path, b"two").is_err());
        fs::remove_dir_all(&dir).unwrap_or_default();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_interrupted_framework_copy_is_replaced_not_used() {
        let dir = scratch("framework");
        let source = dir.join("cef").join(FRAMEWORK);
        fs::create_dir_all(source.join("Resources")).unwrap();
        fs::write(source.join("Resources/a"), "a").unwrap();
        fs::write(source.join("Resources/b"), "b").unwrap();
        let frameworks = dir.join("Frameworks");
        let partial = frameworks.join(format!("{FRAMEWORK}.partial"));
        fs::create_dir_all(partial.join("Resources")).unwrap();
        fs::write(partial.join("Resources/a"), "half").unwrap();
        install_framework(&source, &frameworks).unwrap();
        let framework = frameworks.join(FRAMEWORK);
        assert_eq!(
            fs::read_to_string(framework.join("Resources/a")).unwrap(),
            "a"
        );
        assert_eq!(
            fs::read_to_string(framework.join("Resources/b")).unwrap(),
            "b"
        );
        assert!(!partial.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_failed_framework_copy_leaves_nothing_under_the_real_name() {
        let dir = scratch("missing");
        let frameworks = dir.join("Frameworks");
        fs::create_dir_all(&frameworks).unwrap();
        assert!(install_framework(&dir.join("absent"), &frameworks).is_err());
        assert!(!frameworks.join(FRAMEWORK).exists());
        assert!(!frameworks.join(format!("{FRAMEWORK}.partial")).exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
