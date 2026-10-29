#[cfg(target_os = "macos")]
mod mac;

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    mac::main()
}

#[cfg(not(target_os = "macos"))]
fn main() {}
