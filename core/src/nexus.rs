//! Nexus Mods (<https://www.nexusmods.com/games/alchemyfactory>), where players find mods: the
//! game's mods and their files, from Nexus's public API (GraphQL, v2), which needs no account or
//! key. Downloading through Nexus's API needs a key, so players download from the mod's page.
//!
//! A mod whose mod.json `homepage` is its Nexus page is linked to it ([`mod_id_in`]).

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

/// The game on Nexus: its domain name and id.
pub const GAME: &str = "alchemyfactory";
const GAME_ID: u32 = 8547;
const API: &str = "https://api.nexusmods.com/v2/graphql";
/// Mods asked for at a time.
const PAGE: u64 = 50;
const LIMIT: u64 = 8 << 20;

#[derive(Clone, Debug, Default)]
pub struct NexusMod {
    pub id: u32,
    pub name: String,
    pub summary: String,
    pub author: String,
    pub version: String,
    /// Its pictures' addresses, small and full size.
    pub thumbnail: Option<String>,
    pub picture: Option<String>,
    pub endorsements: u64,
    pub downloads: u64,
    /// When it was last updated, and first published: `2026-10-18`.
    pub updated: String,
    pub created: String,
}

impl NexusMod {
    pub fn page(&self) -> String {
        page(self.id)
    }
}

/// A file of a mod on Nexus.
#[derive(Clone, Debug, Default)]
pub struct NexusFile {
    pub id: u32,
    pub name: String,
    pub version: String,
    /// `MAIN`, `UPDATE`, `OPTIONAL`, `OLD_VERSION`, `MISCELLANEOUS` or `ARCHIVED`.
    pub category: String,
    pub size: u64,
    /// When it was uploaded: `2026-10-18`.
    pub uploaded: String,
    pub description: String,
}

impl NexusFile {
    /// Whether it's a current file: not an old version, or archived.
    pub fn is_current(&self) -> bool {
        !matches!(
            self.category.as_str(),
            "OLD_VERSION" | "ARCHIVED" | "REMOVED"
        )
    }
}

/// A mod's page on Nexus.
pub fn page(id: u32) -> String {
    format!("https://www.nexusmods.com/{GAME}/mods/{id}")
}

/// The Nexus mod a link is the page of (`.../alchemyfactory/mods/15`, with or without `games/`
/// or a query), if it is one.
pub fn mod_id_in(link: &str) -> Option<u32> {
    let after = &link[link.find("nexusmods.com/")? + "nexusmods.com/".len()..];
    let after = after.strip_prefix("games/").unwrap_or(after);
    let digits = after.strip_prefix(&format!("{GAME}/mods/"))?;
    let end = digits
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(digits.len());
    digits[..end].parse().ok()
}

/// The game's published mods (not those marked adult), most endorsed first.
pub fn mods() -> Result<Vec<NexusMod>> {
    const QUERY: &str = "query Mods($offset: Int!, $count: Int!) {
        mods(
            filter: { gameDomainName: [{ value: \"alchemyfactory\", op: EQUALS }] },
            sort: [{ endorsements: { direction: DESC } }],
            offset: $offset,
            count: $count
        ) {
            totalCount
            nodes {
                modId name summary author version thumbnailUrl pictureUrl endorsements downloads
                updatedAt createdAt adultContent status
            }
        }
    }";
    let mut found = vec![];
    let mut offset = 0;
    loop {
        let data = query(QUERY, json!({ "offset": offset, "count": PAGE }))?;
        let page = &data["mods"];
        let nodes = page["nodes"]
            .as_array()
            .context("Nexus's answer has no mods")?;
        for node in nodes {
            if node["adultContent"].as_bool() == Some(true)
                || node["status"]
                    .as_str()
                    .is_some_and(|status| status != "published")
            {
                continue;
            }
            found.push(NexusMod {
                id: node["modId"].as_u64().unwrap_or_default() as u32,
                name: text(&node["name"]),
                summary: text(&node["summary"]),
                author: text(&node["author"]),
                version: text(&node["version"]),
                thumbnail: node["thumbnailUrl"].as_str().map(str::to_string),
                picture: node["pictureUrl"].as_str().map(str::to_string),
                endorsements: node["endorsements"].as_u64().unwrap_or_default(),
                downloads: node["downloads"].as_u64().unwrap_or_default(),
                updated: date(&node["updatedAt"]),
                created: date(&node["createdAt"]),
            });
        }
        offset += PAGE;
        let total = page["totalCount"].as_u64().unwrap_or_default();
        if nodes.len() < PAGE as usize || offset >= total {
            return Ok(found);
        }
    }
}

/// A mod's files, newest first.
pub fn files(mod_id: u32) -> Result<Vec<NexusFile>> {
    const QUERY: &str = "query Files($mod: ID!, $game: ID!) {
        modFiles(modId: $mod, gameId: $game) {
            fileId name version category sizeInBytes date description
        }
    }";
    let data = query(QUERY, json!({ "mod": mod_id, "game": GAME_ID }))?;
    let nodes = data["modFiles"]
        .as_array()
        .context("Nexus's answer has no files")?;
    let mut found: Vec<(u64, NexusFile)> = nodes
        .iter()
        .map(|node| {
            let uploaded = node["date"].as_u64().unwrap_or_default();
            let (year, month, day) = crate::backups::civil_date((uploaded / 86_400) as i64);
            let file = NexusFile {
                id: node["fileId"].as_u64().unwrap_or_default() as u32,
                name: text(&node["name"]),
                version: text(&node["version"]),
                category: text(&node["category"]),
                // a string, as it can be larger than GraphQL's Int
                size: node["sizeInBytes"]
                    .as_str()
                    .and_then(|size| size.parse().ok())
                    .unwrap_or_default(),
                uploaded: format!("{year:04}-{month:02}-{day:02}"),
                description: text(&node["description"]),
            };
            (uploaded, file)
        })
        .collect();
    found.sort_by_key(|(uploaded, _)| std::cmp::Reverse(*uploaded));
    Ok(found.into_iter().map(|(_, file)| file).collect())
}

/// Run a query, returning its `data`.
fn query(text: &str, variables: Value) -> Result<Value> {
    let mut answer = crate::web::post_json(
        API,
        &[("User-Agent", "alchemists-tools")],
        &json!({ "query": text, "variables": variables }),
        LIMIT,
    )?;
    if let Some(errors) = answer["errors"]
        .as_array()
        .filter(|errors| !errors.is_empty())
    {
        let messages: Vec<&str> = errors
            .iter()
            .filter_map(|error| error["message"].as_str())
            .collect();
        bail!("Nexus Mods refused the query: {}", messages.join("; "));
    }
    Ok(answer["data"].take())
}

/// A date and time (`2026-10-18T11:31:13Z`) as its date.
fn date(value: &Value) -> String {
    value
        .as_str()
        .unwrap_or_default()
        .chars()
        .take(10)
        .collect()
}

/// A text from Nexus, readable: HTML entities decoded, and UTF-8 that was read as Windows-1252
/// and encoded again (`â€”` for `—`) undone.
fn text(value: &Value) -> String {
    let text = value.as_str().unwrap_or_default();
    let text = if text.contains('â') || text.contains('Ã') {
        undo_double_encoding(text).unwrap_or_else(|| text.to_string())
    } else {
        text.to_string()
    };
    decode_entities(&text).trim().to_string()
}

/// Windows-1252's characters for the bytes 0x80 to 0x9F, with `\0` for the five it leaves out.
const WINDOWS_1252: &str = "€\0‚ƒ„…†‡ˆ‰Š‹Œ\0Ž\0\0‘’“”•–—˜™š›œ\0žŸ";

fn undo_double_encoding(text: &str) -> Option<String> {
    let mut bytes = vec![];
    for character in text.chars() {
        let byte = match u32::from(character) {
            code @ (0..0x80 | 0xA0..0x100) => code as u8,
            _ => {
                let index = WINDOWS_1252.chars().position(|known| known == character)?;
                0x80 + index as u8
            }
        };
        bytes.push(byte);
    }
    String::from_utf8(bytes).ok()
}

fn decode_entities(text: &str) -> String {
    let mut decoded = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        decoded.push_str(&rest[..start]);
        rest = &rest[start..];
        let entity = rest[1..]
            .find(';')
            .filter(|&end| end <= 8)
            .map(|end| &rest[1..end + 1]);
        let character = entity.and_then(|entity| match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => {
                let number = entity.strip_prefix('#')?;
                let code = match number.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                    None => number.parse().ok()?,
                };
                char::from_u32(code)
            }
        });
        match (entity, character) {
            (Some(entity), Some(character)) => {
                decoded.push(character);
                rest = &rest[entity.len() + 2..];
            }
            _ => {
                decoded.push('&');
                rest = &rest[1..];
            }
        }
    }
    decoded.push_str(rest);
    decoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links() {
        assert_eq!(
            mod_id_in("https://www.nexusmods.com/alchemyfactory/mods/15"),
            Some(15)
        );
        assert_eq!(
            mod_id_in("https://www.nexusmods.com/games/alchemyfactory/mods/7?tab=files"),
            Some(7)
        );
        assert_eq!(mod_id_in("https://www.nexusmods.com/skyrim/mods/15"), None);
        assert_eq!(mod_id_in("https://github.com/someone/mod"), None);
    }

    #[test]
    fn texts() {
        assert_eq!(
            text(&json!("with ease\u{e2}\u{20ac}\u{201d}even from a pillar")),
            "with ease—even from a pillar"
        );
        assert_eq!(
            text(&json!("Salt &amp; Pepper&#39;s &#x2014; &bogus;")),
            "Salt & Pepper's — &bogus;"
        );
        assert_eq!(text(&json!("Café")), "Café");
    }
}
