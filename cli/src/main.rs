use std::io::Write;
use std::path::PathBuf;

use alchemists_core::game::{display_path, Game};
use alchemists_core::{datatable, extract, json};
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about = "Tools for modding Alchemy Factory")]
struct Cli {
    /// Game install directory [default: found in your Steam libraries]
    #[arg(long, global = true)]
    game: Option<PathBuf>,
    /// .usmap mappings file with the game's struct layouts
    /// [default: the newest in the game's ue4ss folder]
    #[arg(long, global = true)]
    usmap: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Extract the game's data tables to JSON
    Extract {
        /// Output directory
        #[arg(long, default_value = "data")]
        out: PathBuf,
    },
    /// Print one of the game's DataTables as JSON, keeping text as localisation keys
    Decode {
        /// Asset name, e.g. DT_Enemies
        name: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let game = match cli.game {
        Some(path) => Game::new(path),
        None => {
            Game::find().context("couldn't find the game in your Steam libraries; pass --game")?
        }
    };
    if !game.paks().is_dir() {
        bail!(
            "{} isn't the game folder: {} doesn't exist",
            display_path(&game.root),
            display_path(&game.paks())
        );
    }
    let usmap = match cli.usmap {
        Some(path) => path,
        None => game.find_usmap().context(
            "no .usmap mappings found in the game's ue4ss folder; \
             dump one with UE4SS (see README) or pass --usmap",
        )?,
    };
    match cli.command {
        Command::Extract { out } => {
            println!("game: {}", display_path(&game.root));
            println!("mappings: {}", display_path(&usmap));
            println!("out: {}", display_path(&out));
            extract::extract(&game, &usmap, &out)
        }
        Command::Decode { name } => {
            let assets = extract::named_tables(&game, &name)?;
            let path = match assets.find(&name).as_slice() {
                [path] => path.to_string(),
                [] => bail!("no DataTable named {name} in the game"),
                many => bail!("{name} is ambiguous: {}", many.join(", ")),
            };
            let mappings = alchemists_core::usmap::read_usmap(&usmap)?;
            let table = datatable::read_datatable(assets.asset(&path)?, &mappings, None)?;
            match std::io::stdout().write_all(json::to_string(&table)?.as_bytes()) {
                // A closed pipe is fine (e.g. piped to `head`).
                Err(error) if error.kind() != std::io::ErrorKind::BrokenPipe => Err(error.into()),
                _ => Ok(()),
            }
        }
    }
}
