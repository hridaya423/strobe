#[cfg(target_os = "macos")]
mod mac;
#[cfg(windows)]
mod sweep;
#[cfg(windows)]
mod win;

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    mac::main()
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    win::main()
}

#[cfg(not(any(target_os = "macos", windows)))]
fn main() {}
