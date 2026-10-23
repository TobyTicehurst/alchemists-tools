//! Mods from git repositories: `mod add <url>` clones one into the first folder of mods, where the
//! mods it holds are found (see `Workspace::mod_folders`), and `mod update` pulls it again.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

/// Run git, without letting it ask for a password (there's no console to answer in the GUI), and
/// return what it printed.
fn git(arguments: &[&str], folder: Option<&Path>) -> Result<String> {
    let mut command = Command::new("git");
    if let Some(folder) = folder {
        command.arg("-C").arg(folder);
    }
    command.args(arguments).env("GIT_TERMINAL_PROMPT", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let output = match command.output() {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("git isn't installed (https://git-scm.com/downloads), or isn't on the PATH")
        }
        Err(error) => return Err(error).context("running git"),
    };
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            arguments.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Git's version, if it's installed.
pub fn version() -> Option<String> {
    git(&["--version"], None)
        .ok()
        .map(|text| text.trim().trim_start_matches("git version ").to_string())
}

/// The folder a repository is cloned into: the last part of its URL, without `.git`.
pub fn repository_name(url: &str) -> Result<String> {
    let last = url
        .trim()
        .trim_end_matches('/')
        .rsplit(['/', ':', '\\'])
        .next()
        .unwrap_or_default();
    let name = last.strip_suffix(".git").unwrap_or(last);
    let plain = name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character));
    if name.is_empty() || name.starts_with('.') || !plain {
        bail!("can't tell the repository's name from {url:?}");
    }
    Ok(name.to_string())
}

/// Clone `url` (only its latest commit) into `folder`, at a branch or tag if given.
pub fn clone(url: &str, reference: Option<&str>, folder: &Path) -> Result<()> {
    let folder_text = folder.to_string_lossy();
    let mut arguments = vec!["clone", "--depth", "1"];
    if let Some(reference) = reference {
        arguments.extend(["--branch", reference]);
    }
    // `--` so that a URL can't be taken for an option
    arguments.extend(["--", url, &folder_text]);
    git(&arguments, None).map(|_| ())
}

/// Bring a clone up to date with its branch (only if that's a fast-forward, so local changes are
/// never merged); returns git's account of it.
pub fn pull(checkout: &Path) -> Result<String> {
    git(&["pull", "--ff-only"], Some(checkout))
}

/// The URL a clone came from (its `origin`), read from its `.git/config`.
pub fn origin(checkout: &Path) -> Option<String> {
    let config = std::fs::read_to_string(checkout.join(".git").join("config")).ok()?;
    let mut in_origin = false;
    for line in config.lines().map(str::trim) {
        if line.starts_with('[') {
            in_origin = line == "[remote \"origin\"]";
        } else if in_origin {
            if let Some((key, value)) = line.split_once('=') {
                if key.trim() == "url" {
                    return Some(value.trim().to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn repository_names() {
        for (url, name) in [
            (
                "https://github.com/someone/af-cauldron-recipes.git",
                "af-cauldron-recipes",
            ),
            ("https://github.com/someone/MyMod/", "MyMod"),
            ("git@github.com:someone/MyMod.git", "MyMod"),
            ("C:\\repositories\\MyMod", "MyMod"),
        ] {
            assert_eq!(super::repository_name(url).unwrap(), name);
        }
        assert!(super::repository_name("https://github.com/someone/..").is_err());
    }
}
