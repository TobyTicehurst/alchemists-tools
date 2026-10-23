//! Whether a newer release of the tools is out: GitHub's latest release of the repository, asked
//! at most once a day (what it said is kept in `release-check.json` beside the settings), so the
//! GUI can say so. Running a newer release's installer updates the tools in place.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::mods::parse_version;
use crate::settings::Settings;

const LATEST: &str = "https://api.github.com/repos/TobyTicehurst/alchemists-tools/releases/latest";
const DAY: u64 = 24 * 60 * 60;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Release {
    /// Without the tag's `v`: `0.2.0`.
    pub version: String,
    /// Its page on GitHub.
    pub page: String,
}

/// What the last check found, and when.
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
struct LastCheck {
    /// Seconds since 1970.
    checked: u64,
    latest: Option<Release>,
}

/// The latest release, if it's newer than these tools. Asks GitHub only if the last check was
/// more than a day ago (or there's nowhere to keep what it said).
pub fn newer_release() -> Result<Option<Release>> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let path = Settings::folder().map(|folder| folder.join("release-check.json"));
    let last: Option<LastCheck> = path
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok());
    let latest = match last {
        Some(last) if now.saturating_sub(last.checked) < DAY => last.latest,
        _ => {
            // a failure counts as a check, so a day passes before the next
            let latest = latest_release().ok();
            if let Some(path) = &path {
                let check = LastCheck {
                    checked: now,
                    latest: latest.clone(),
                };
                if let Some(folder) = path.parent() {
                    let _ = std::fs::create_dir_all(folder);
                }
                let _ = std::fs::write(path, serde_json::to_string_pretty(&check)? + "\n");
            }
            latest
        }
    };
    let ours = parse_version(env!("CARGO_PKG_VERSION"), "these tools")?;
    Ok(latest.filter(|release| {
        parse_version(&release.version, "the latest release").is_ok_and(|theirs| theirs > ours)
    }))
}

/// Ask GitHub for the latest release.
fn latest_release() -> Result<Release> {
    let bytes = crate::web::get(
        LATEST,
        &[
            ("Accept", "application/vnd.github+json"),
            ("User-Agent", "alchemists-tools"),
        ],
        1 << 20,
    )?;
    let value: Value = serde_json::from_slice(&bytes).context("GitHub's answer isn't JSON")?;
    let tag = value["tag_name"]
        .as_str()
        .context("GitHub's latest release has no tag")?;
    Ok(Release {
        version: tag.trim_start_matches('v').to_string(),
        page: value["html_url"]
            .as_str()
            .unwrap_or("https://github.com/TobyTicehurst/alchemists-tools/releases")
            .to_string(),
    })
}
