use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};

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
    fs::write(
        app.join("Contents/Info.plist"),
        plist(executable, identifier),
    )?;
    Ok(installed)
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

    let framework = frameworks.join(FRAMEWORK);
    if !framework.exists() {
        let source = cef_path().join(FRAMEWORK);
        let status = Command::new("cp")
            .arg("-c")
            .arg("-R")
            .arg(&source)
            .arg(&frameworks)
            .status()
            .context("run cp")?;
        ensure!(status.success(), "cannot copy {}", source.display());
    }
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
