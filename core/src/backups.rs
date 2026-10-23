//! Copies of the game's saves, made before mods are installed or uninstalled: a save that uses
//! a mod's buildings crashes the game without it, and one that uses its items or recipes can lose
//! them when the game loads it and saves again. Kept in `save-backups/` beside the settings, one
//! folder per backup, named for when it was made; the newest [`KEEP`] are kept.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::game::{display_path, Game};
use crate::settings::Settings;

/// How many backups are kept.
pub const KEEP: usize = 10;

/// The game's saves (its `SaveGames` folder): in the local application data folder on Windows;
/// under Proton, in the game's prefix in the Steam library the game is in.
pub fn save_folder(game: &Game) -> Option<PathBuf> {
    let local = if cfg!(windows) {
        PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
    } else {
        let steamapps = game.root.parent()?.parent()?;
        steamapps.join("compatdata/3669570/pfx/drive_c/users/steamuser/AppData/Local")
    };
    Some(local.join("AlchemyFactory").join("Saved").join("SaveGames"))
}

/// Where backups go.
pub fn backups_folder() -> Option<PathBuf> {
    Settings::folder().map(|folder| folder.join("save-backups"))
}

/// Copy the game's saves into a new backup, and remove all but the newest [`KEEP`]. Returns the
/// new backup's folder, or None if the game has no saves.
pub fn back_up(game: &Game) -> Result<Option<PathBuf>> {
    let Some(saves) = save_folder(game).filter(|folder| folder.is_dir()) else {
        return Ok(None);
    };
    let backups = backups_folder().context("no APPDATA or HOME folder to keep backups in")?;
    let mut target = backups.join(timestamp(SystemTime::now()));
    // two in one second: the second gets a suffix
    let mut copy = 1;
    while target.exists() {
        copy += 1;
        target = backups.join(format!("{} ({copy})", timestamp(SystemTime::now())));
    }
    crate::mods::copy_folder(&saves, &target).with_context(|| {
        format!(
            "backing up the saves in {} to {}",
            display_path(&saves),
            display_path(&target)
        )
    })?;
    // the oldest first; only folders named as this names them
    let mut made: Vec<PathBuf> = std::fs::read_dir(&backups)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(is_backup_name)
        })
        .collect();
    made.sort();
    let excess = made.len().saturating_sub(KEEP);
    for old in &made[..excess] {
        std::fs::remove_dir_all(old)
            .with_context(|| format!("removing the old backup {}", display_path(old)))?;
    }
    Ok(Some(target))
}

/// `2026-10-23 21-05-09 UTC`: sorts by time, and is a valid folder name everywhere.
fn timestamp(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let (year, month, day) = civil_date(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}-{:02}-{:02} UTC",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

fn is_backup_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() >= 23
        && bytes[..19]
            .iter()
            .enumerate()
            .all(|(index, byte)| match index {
                4 | 7 | 13 | 16 => *byte == b'-',
                10 => *byte == b' ',
                _ => byte.is_ascii_digit(),
            })
        && bytes[19..].starts_with(b" UTC")
}

/// The (year, month, day) of a day counted from 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_date(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn timestamps() {
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01 00-00-00 UTC");
        let time = UNIX_EPOCH + Duration::from_secs(1_792_789_509);
        assert_eq!(timestamp(time), "2026-10-23 21-05-09 UTC");
        // a leap day
        let time = UNIX_EPOCH + Duration::from_secs(1_709_164_800);
        assert_eq!(timestamp(time), "2024-02-29 00-00-00 UTC");
        assert!(is_backup_name("2026-10-23 21-05-09 UTC"));
        assert!(is_backup_name("2026-10-23 21-05-09 UTC (2)"));
        assert!(!is_backup_name("my saves"));
    }
}
