//! JSON file output.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::game::display_path;

pub fn to_string<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    Ok(serde_json::to_string_pretty(value)? + "\n")
}

pub fn write<T: Serialize + ?Sized>(value: &T, path: &Path) -> Result<()> {
    std::fs::write(path, to_string(value)?)
        .with_context(|| format!("writing {}", display_path(path)))
}
