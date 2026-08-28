//! Project-local microsandbox runtime: `MSB_HOME`, the short link, the installer.

use std::path::{Path, PathBuf};

use microsandbox::{Sandbox, backend::LocalBackend, setup::Setup};

use crate::Res;

/// Per-sandbox socket paths are `home` plus a suffix; macOS caps
/// `sockaddr_un.sun_path` at 104 bytes. The longest suffix the runtime derives
/// is 52 bytes, so the home path has to stay under this length.
const MAX_HOME_LEN: usize = 104 - 52;

/// The directory holding the runtime data, inside the repo.
pub fn data_dir() -> PathBuf {
    // `microsandbox/wbox` -> `microsandbox`
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("wbox crate always sits under microsandbox/")
        .join(".runtime")
}

/// The `microsandbox/` payload directory (home flake, vm-files, scripts).
pub fn payload_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("wbox crate always sits under microsandbox/")
        .to_path_buf()
}

/// The short path handed to the runtime as `MSB_HOME`.
///
/// The bytes live under `microsandbox/.runtime`; this is only a symlink, kept
/// short because the agent sockets are derived from it (see README).
pub fn home() -> Res<PathBuf> {
    let project = data_dir();
    let label = project
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .ok_or("cannot determine the project name for the runtime link")?
        .to_string_lossy()
        .into_owned();
    let base = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(base).join(".wbox").join(label))
}

/// Create the runtime dir and the short link, and point the SDK at them.
///
/// Must run before the tokio runtime starts: it sets `MSB_HOME`, and
/// `set_var` is only sound while the process is single-threaded.
pub fn install_home() -> Res<PathBuf> {
    let data = data_dir();
    std::fs::create_dir_all(&data)?;
    let link = home()?;
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::read_link(&link) {
        Ok(current) if current == data => {}
        Ok(_) => {
            std::fs::remove_file(&link)?;
            std::os::unix::fs::symlink(&data, &link)?;
        }
        Err(_) if link.exists() => {
            return Err(format!(
                "{} exists and is not a symlink to {}",
                link.display(),
                data.display()
            )
            .into());
        }
        Err(_) => std::os::unix::fs::symlink(&data, &link)?,
    }

    let len = link.as_os_str().len();
    if len >= MAX_HOME_LEN {
        return Err(format!(
            "runtime home {} is {len} bytes; macOS unix sockets need it under {MAX_HOME_LEN}",
            link.display()
        )
        .into());
    }

    // SAFETY: called from `main` before any thread (and so the tokio runtime)
    // exists, so no other thread can be reading the environment.
    unsafe {
        std::env::set_var("MSB_HOME", &link);
    }
    microsandbox::backend::set_default_backend(LocalBackend::builder().home(&link).build_lazy());
    Ok(link)
}

/// Install the `msb` binary and `libkrunfw` under the runtime home if missing.
pub async fn ensure_runtime() -> Res<PathBuf> {
    let home = home()?;
    let msb = home.join("bin").join("msb");
    let libkrunfw = home.join("lib").join("libkrunfw.dylib");
    if !msb.is_file() || !libkrunfw.exists() {
        eprintln!(
            "wbox: installing the microsandbox runtime under {}",
            home.display()
        );
        Setup::builder()
            .base_dir(home.clone())
            .build()
            .install()
            .await?;
    }
    Ok(home)
}

/// Label every sandbox wbox owns carries, and the filter `list` uses.
pub const WBOX_LABEL: (&str, &str) = ("wbox", "1");

/// `wbox list`.
pub async fn list() -> Res<()> {
    ensure_runtime().await?;
    let page = Sandbox::list_with(|l| l.label(WBOX_LABEL.0, WBOX_LABEL.1)).await?;
    println!(
        "{:<24} {:<12} {:<12} {:<12}",
        "NAME", "STATUS", "AUTH KEY", "SIGN KEY"
    );
    if page.sandboxes.is_empty() {
        println!("no wbox sandboxes");
        return Ok(());
    }
    for handle in &page.sandboxes {
        let labels = handle
            .config()
            .map(|c| c.spec.labels.clone())
            .unwrap_or_default();
        let label = |key: &str| labels.get(key).cloned().unwrap_or_else(|| "-".to_string());
        println!(
            "{:<24} {:<12} {:<12} {:<12}",
            handle.name(),
            format!("{:?}", handle.status_snapshot()),
            label(crate::state::AUTH_KEY_LABEL),
            label(crate::state::SIGNING_KEY_LABEL),
        );
    }
    Ok(())
}
