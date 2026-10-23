//! Downloading, with the curl that comes with Windows (and is on the PATH elsewhere), so the
//! tools need no TLS library of their own.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::game::display_path;

/// GET `url` over HTTPS (following redirects, which must stay HTTPS), with these extra headers,
/// refusing more than `limit` bytes. An HTTP error status is an error.
pub fn get(url: &str, headers: &[(&str, &str)], limit: u64) -> Result<Vec<u8>> {
    let curl = match std::env::var_os("SystemRoot") {
        Some(root) => Path::new(&root).join("System32").join("curl.exe"),
        None => PathBuf::from("curl"),
    };
    let mut command = std::process::Command::new(&curl);
    command
        .args(["--fail", "--location", "--silent", "--show-error"])
        .args(["--proto", "=https", "--proto-redir", "=https"])
        .args(["--max-filesize", &limit.to_string()]);
    for (name, value) in headers {
        command.arg("--header").arg(format!("{name}: {value}"));
    }
    command.arg(url);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let output = command
        .output()
        .with_context(|| format!("running {}", display_path(&curl)))?;
    if !output.status.success() {
        bail!(
            "couldn't download {url}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}
