use std::io::Write;
use std::path::{Path, PathBuf};

use alchemists_core::environment::{self, Status};
use alchemists_core::game::{display_path, Game};
use alchemists_core::settings::Settings;
use alchemists_core::{datatable, edits, extract, json, package, unpack};
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    version,
    about = "Tools for modding Alchemy Factory",
    after_help = "Options not given come from the settings (`alchemists-tools settings`), which \
                  the GUI shares"
)]
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
    /// Find the game, UE4SS, the .usmap and the other things the tools use, and say what's missing
    Check,
    /// Install UE4SS (the build mods' C++ is made for) into the game, or remove, disable or enable it
    Ue4ss {
        #[command(subcommand)]
        action: Ue4ssAction,
    },
    /// Show the saved settings, or change them: where the game, mods and builds are. The GUI uses
    /// the same settings
    Settings {
        /// The folders of mod folders (repeat for several; the first gets new mods)
        #[arg(long = "mods", value_name = "FOLDER")]
        mods: Vec<PathBuf>,
        /// Where builds go
        #[arg(long)]
        build: Option<PathBuf>,
        /// The stub project's .uproject
        #[arg(long)]
        project: Option<PathBuf>,
        /// The Unreal Engine install
        #[arg(long)]
        engine: Option<PathBuf>,
        /// Forget every saved setting (the defaults apply again)
        #[arg(long)]
        reset: bool,
    },
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
    /// Pack content cooked in the stub Unreal project (Blueprints, meshes, textures) as a mod
    /// container
    PackageContent {
        /// /Game/ folders or assets, e.g. /Game/Mods/MyMachine
        paths: Vec<String>,
        /// Mod name: the container is zzz_<name>_P
        #[arg(long)]
        name: String,
        /// Set a Blueprint's FactoryCraftType, by name (Max) or number (26): BP_MyMachine=26
        #[arg(long = "craft-type", value_name = "ASSET=VALUE")]
        craft_types: Vec<String>,
        /// The stub project's .uproject
        /// [default: Documents/Unreal Projects/AlchemyFactory/AlchemyFactory.uproject]
        #[arg(long)]
        project: Option<PathBuf>,
        /// Allow paths outside /Game/Mods, e.g. to override a game Blueprint for a test
        #[arg(long)]
        allow_game_paths: bool,
        /// Package even if assets were saved in the editor after the last cook
        #[arg(long)]
        allow_stale: bool,
        /// Output directory
        #[arg(long, default_value = "build")]
        out: PathBuf,
    },
    /// Build, install and share mods kept as folders (see `mod --help`)
    Mod {
        #[command(subcommand)]
        action: ModAction,
        /// A folder of mod folders; repeat for several (a mod in more than one is taken from the
        /// first) [default: the settings', else mods]
        #[arg(long = "mods", global = true, value_name = "FOLDER")]
        mods: Vec<PathBuf>,
        /// Where builds go [default: the settings', else build]
        #[arg(long, global = true)]
        build: Option<PathBuf>,
        /// The stub project's .uproject, for mods with cooked content
        /// [default: Documents/Unreal Projects/AlchemyFactory/AlchemyFactory.uproject]
        #[arg(long, global = true)]
        project: Option<PathBuf>,
    },
    /// After a game update: check the dumps, re-extract, rebuild the stub project, compare the
    /// Blueprints mods copy, and rebuild and re-merge the mods
    Update {
        /// Where `extract` writes the game's data
        #[arg(long, default_value = "data")]
        data: PathBuf,
        /// A folder of mod folders; repeat for several [default: the settings', else mods]
        #[arg(long = "mods", value_name = "FOLDER")]
        mods: Vec<PathBuf>,
        /// Where builds go (and the last update's record, in .update/)
        /// [default: the settings', else build]
        #[arg(long)]
        build: Option<PathBuf>,
        /// The stub project's .uproject
        /// [default: Documents/Unreal Projects/AlchemyFactory/AlchemyFactory.uproject]
        #[arg(long)]
        project: Option<PathBuf>,
        /// The Unreal Engine install, for rebuilding the stub project
        /// [default: the settings', else C:/Program Files/Epic Games/UE_5.7]
        #[arg(long)]
        engine: Option<PathBuf>,
        /// Don't rebuild the stub project's BeltTD module
        #[arg(long)]
        skip_stub: bool,
        /// Redo every step even if the game build is unchanged
        #[arg(long)]
        force: bool,
        /// Go on when the .usmap or header dump is older than the game
        #[arg(long)]
        allow_old_dumps: bool,
    },
    /// Set up the stub Unreal project, for cooked content: add the game's C++ module from the
    /// UE4SS header dump, its collision settings and the mod folders to cook, then build it.
    /// Start from a blank C++ project named AlchemyFactory, made in the Epic Games Launcher
    Stub {
        /// The stub project's .uproject
        /// [default: Documents/Unreal Projects/AlchemyFactory/AlchemyFactory.uproject]
        #[arg(long)]
        project: Option<PathBuf>,
        /// The Unreal Engine install
        /// [default: the settings', else C:/Program Files/Epic Games/UE_5.7]
        #[arg(long)]
        engine: Option<PathBuf>,
        /// Set it up but don't build it
        #[arg(long)]
        no_build: bool,
    },
    /// Write a UE4SS probe mod: logging hooks for game functions, e.g.
    /// probe VaultProbe "BeltTDInventoryComponent:Server_*"
    Probe {
        /// The probe mod's name (its folder in the mods folder)
        name: String,
        /// <Class>:<Function> (wildcards allowed in the function), or a Blueprint function's path
        functions: Vec<String>,
        /// Also log each new object of this class (a /Script/ or /Game/ class path)
        #[arg(long = "new-object", value_name = "CLASS")]
        new_objects: Vec<String>,
        /// Calls per function logged in full
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Calls logged in full whose arguments look the same (numbers aside), per function; more
        /// are only counted (Ctrl + Numpad 3)
        #[arg(long, default_value_t = 3)]
        repeats: usize,
        /// Overwrite an earlier probe of the same name
        #[arg(long)]
        force: bool,
        /// The folder of mod folders [default: the settings' first, else mods]
        #[arg(long)]
        mods: Option<PathBuf>,
    },
    /// Apply a folder of edit files to the game's tables, as a mod container
    BuildTables {
        /// Folder with the edit files (*.json, read in alphabetical order)
        folder: PathBuf,
        /// Mod name: the container is zzz_<name>_P [default: the folder's name]
        #[arg(long)]
        name: Option<String>,
        /// Output directory
        #[arg(long, default_value = "build")]
        out: PathBuf,
    },
}

#[derive(Subcommand)]
enum Ue4ssAction {
    /// Download the pinned build (checked against its SHA-256) and install it, if no UE4SS is
    /// there; one installed by hand is left alone, unless it's that very build
    Install {
        /// The build's zip, if you have it, instead of downloading it
        #[arg(long)]
        zip: Option<PathBuf>,
    },
    /// Remove the UE4SS the tools installed: only its files, and only those unchanged
    Remove {
        /// Also delete its files that have changed since
        #[arg(long)]
        force: bool,
    },
    /// Start the game without UE4SS (its loader, dwmapi.dll, is renamed), as after a game update
    /// it can't handle
    Disable,
    /// Start the game with UE4SS again
    Enable,
}

#[derive(Subcommand)]
enum ModAction {
    /// Start a mod folder with a mod.json
    New { name: String },
    /// Build everything a mod has into <build>/<name>/
    Build { name: String },
    /// Build a mod, then install it into the game (merging every installed mod's tables)
    Install {
        name: String,
        /// Also build and install the mods it requires, from the mods folder
        #[arg(long)]
        with_dependencies: bool,
        /// Replace the tool's merged files (zzz_ModTables_P, zzz_ModSave_P) even if they've
        /// changed since it wrote them
        #[arg(long)]
        force: bool,
    },
    /// Make the installed mods exactly those named, at once or not at all: build and install each
    /// (again, if it's installed), and uninstall every other installed mod. Everything is built and
    /// checked first: dependencies, incompatibilities, files, and clashes such as two mods
    /// detouring one game function
    Apply {
        /// The mods to have installed
        #[arg(required_unless_present = "none")]
        names: Vec<String>,
        /// Uninstall every mod
        #[arg(long, conflicts_with = "names")]
        none: bool,
        /// Also install the mods these require, from the mods folders
        #[arg(long)]
        with_dependencies: bool,
        /// Delete files changed since they were installed, and the files of mods without a
        /// manifest; replace the tool's merged files even if they've changed since it wrote them
        #[arg(long)]
        force: bool,
    },
    /// Remove a mod from the game: only the files its manifest lists, and only while they're as it
    /// installed them
    Uninstall {
        name: String,
        /// Also delete its files that have changed since it installed them; uninstall even if
        /// installed mods require it; delete the files of a mod this tool didn't install; replace
        /// the tool's merged files even if they've changed since it wrote them
        #[arg(long)]
        force: bool,
    },
    /// Build a mod, then zip it for players: <build>/<name>_<version>.zip
    Dist { name: String },
    /// Clone a git repository of mods (or one mod) into the first mods folder, so its mods are
    /// listed; then install them as any other. Needs git
    Add {
        /// The repository's URL, e.g. https://github.com/someone/MyMod
        url: String,
        /// A branch or tag to take instead of the default branch
        #[arg(long)]
        branch: Option<String>,
    },
    /// Pull the git repository a mod was added from (only a fast-forward); install it again to
    /// use the new version
    Update { name: String },
    /// Import a mod made without this tool (a zip from a mod site), as best it can be, into the
    /// first mods folder; then install it as any other. Refuses zips it can't place safely, and
    /// notes what it can't check
    Import {
        /// The mod's zip
        zip: PathBuf,
        /// The name to give it [default: from its folder or file names]
        #[arg(long)]
        name: Option<String>,
        /// Its version [default: from the zip's name, else 1.0.0]
        #[arg(long)]
        version: Option<String>,
    },
    /// The mods in the mods folders, what is installed, and what needs attention
    List,
    /// Every mod in the game folder, whether its files are as installed, and what else is there
    Scan,
}

/// Where things are: the command line's options, else the settings, else the defaults.
struct Places {
    settings: Settings,
    game: Option<PathBuf>,
    usmap: Option<PathBuf>,
}

impl Places {
    fn game(&self) -> Result<Game> {
        let game = match &self.game {
            Some(path) => Game::new(path),
            None => self.settings.game().context(
                "couldn't find the game in your Steam libraries; pass --game, or save it with \
                 `alchemists-tools settings --game <folder>`",
            )?,
        };
        if !game.paks().is_dir() {
            bail!(
                "{} isn't the game folder: {} doesn't exist",
                display_path(&game.root),
                display_path(&game.paks())
            );
        }
        Ok(game)
    }

    fn usmap(&self, game: &Game) -> Result<PathBuf> {
        self.usmap
            .clone()
            .or_else(|| self.settings.usmap(Some(game)))
            .context(
                "no .usmap mappings found in the game's ue4ss folder; dump one with UE4SS (see \
                 README) or pass --usmap",
            )
    }

    /// For mod commands, which only need the .usmap to build: an empty path until then.
    fn usmap_if_any(&self, game: &Game) -> PathBuf {
        self.usmap(game).unwrap_or_default()
    }

    fn mods(&self, given: Vec<PathBuf>) -> Vec<PathBuf> {
        if given.is_empty() {
            self.settings.mods()
        } else {
            given
        }
    }
}

fn mod_command(
    places: &Places,
    action: ModAction,
    mods: Vec<PathBuf>,
    build: Option<PathBuf>,
    project: Option<PathBuf>,
) -> Result<()> {
    let game = places.game()?;
    let usmap = places.usmap_if_any(&game);
    let workspace = alchemists_core::mods::Workspace::new(
        places.mods(mods),
        build.unwrap_or_else(|| places.settings.build()),
        game,
        usmap,
        project.unwrap_or_else(|| places.settings.project()),
    );
    match action {
        ModAction::New { name } => workspace.new_mod(&name),
        ModAction::List => workspace.list(),
        ModAction::Add { url, branch } => {
            workspace.add_from_git(&url, branch.as_deref()).map(|_| ())
        }
        ModAction::Update { name } => workspace.update_from_git(&name),
        ModAction::Import { zip, name, version } => workspace
            .import(&zip, name.as_deref(), version.as_deref())
            .map(|_| ()),
        ModAction::Apply {
            names,
            none: _,
            with_dependencies,
            force,
        } => {
            let mut wanted: std::collections::BTreeSet<String> = names.iter().cloned().collect();
            if with_dependencies {
                let overview = workspace.overview()?;
                for name in &names {
                    overview.tick(&mut wanted, name);
                }
            }
            let wanted: Vec<String> = wanted.into_iter().collect();
            workspace.apply(&wanted, force).map(|_| ())
        }
        ModAction::Scan => workspace.scan(),
        ModAction::Uninstall { name, force } => workspace.uninstall(&name, force),
        ModAction::Build { name } => workspace.build_mod(&workspace.load(&name)?).map(|_| ()),
        ModAction::Install {
            name,
            with_dependencies,
            force,
        } => workspace.install(&workspace.load(&name)?, with_dependencies, force, &[]),
        ModAction::Dist { name } => {
            let found = workspace.load(&name)?;
            let (out, info) = workspace.build_mod(&found)?;
            workspace.dist(&found, &out, &info).map(|_| ())
        }
    }
}

fn check(places: &Places) -> Result<()> {
    let mut settings = places.settings.clone();
    settings.game = places.game.clone().or(settings.game);
    settings.usmap = places.usmap.clone().or(settings.usmap);
    let checks = environment::checks(&settings);
    for check in &checks {
        let mark = match check.status {
            Status::Found => "ok",
            Status::Warning => "WARNING",
            Status::Missing => "MISSING",
            Status::Optional => "-",
        };
        println!("{:<20} {mark:<8} {}", check.name, check.summary);
        if let Some(path) = &check.path {
            println!("{:<29} {}", "", display_path(path));
        }
        if matches!(check.status, Status::Missing | Status::Warning) {
            println!("{:<29} {}", "", check.help);
        }
    }
    match Settings::path() {
        Some(path) if path.exists() => println!("\nsettings: {}", display_path(&path)),
        _ => println!("\nno saved settings (alchemists-tools settings --help)"),
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn settings_command(
    places: &Places,
    mods: Vec<PathBuf>,
    build: Option<PathBuf>,
    project: Option<PathBuf>,
    engine: Option<PathBuf>,
    reset: bool,
) -> Result<()> {
    let absolute = |path: PathBuf| std::path::absolute(&path).unwrap_or(path);
    let mut settings = if reset {
        Settings::default()
    } else {
        places.settings.clone()
    };
    let before = settings.clone();
    if let Some(game) = &places.game {
        settings.game = Some(absolute(game.clone()));
    }
    if let Some(usmap) = &places.usmap {
        settings.usmap = Some(absolute(usmap.clone()));
    }
    if !mods.is_empty() {
        settings.mods = mods.into_iter().map(absolute).collect();
    }
    settings.build = build.map(absolute).or(settings.build);
    settings.project = project.map(absolute).or(settings.project);
    settings.engine = engine.map(absolute).or(settings.engine);
    if settings != before || reset {
        settings.save()?;
        println!("saved");
    }
    let shown = |path: &Option<PathBuf>, default: &str| match path {
        Some(path) => display_path(path),
        None => format!("({default})"),
    };
    println!(
        "game:    {}",
        shown(&settings.game, "found in the Steam libraries")
    );
    println!(
        "usmap:   {}",
        shown(&settings.usmap, "the newest in the game's ue4ss folder")
    );
    if settings.mods.is_empty() {
        println!("mods:    (mods)");
    }
    for folder in &settings.mods {
        println!("mods:    {}", display_path(folder));
    }
    println!("build:   {}", shown(&settings.build, "build"));
    println!(
        "project: {}",
        shown(&settings.project, "Documents/Unreal Projects/...")
    );
    println!(
        "engine:  {}",
        shown(&settings.engine, alchemists_core::settings::DEFAULT_ENGINE)
    );
    if let Some(path) = Settings::path() {
        println!("(in {})", display_path(&path));
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let places = Places {
        settings: Settings::load()?,
        game: cli.game,
        usmap: cli.usmap,
    };
    match cli.command {
        Command::Check => return check(&places),
        Command::Ue4ss { action } => {
            let game = places.game()?;
            return match action {
                Ue4ssAction::Install { zip } => {
                    alchemists_core::ue4ss::install(&game, zip.as_deref())
                }
                Ue4ssAction::Remove { force } => alchemists_core::ue4ss::remove(&game, force),
                Ue4ssAction::Disable => alchemists_core::ue4ss::set_enabled(&game, false),
                Ue4ssAction::Enable => alchemists_core::ue4ss::set_enabled(&game, true),
            };
        }
        Command::Settings {
            mods,
            build,
            project,
            engine,
            reset,
        } => return settings_command(&places, mods, build, project, engine, reset),
        Command::Mod {
            action,
            mods,
            build,
            project,
        } => return mod_command(&places, action, mods, build, project),
        _ => {}
    }
    let game = places.game()?;
    let usmap = places.usmap(&game)?;
    match cli.command {
        Command::Check | Command::Ue4ss { .. } | Command::Settings { .. } | Command::Mod { .. } => {
            unreachable!()
        }
        Command::Extract { out } => {
            println!("game: {}", display_path(&game.root));
            println!("mappings: {}", display_path(&usmap));
            println!("out: {}", display_path(&out));
            extract::extract(&game, &usmap, &out)
        }
        Command::PackageContent {
            paths,
            name,
            craft_types,
            project,
            allow_game_paths,
            allow_stale,
            out,
        } => package_content(
            &game,
            &usmap,
            &paths,
            &name,
            &craft_types,
            project,
            allow_game_paths,
            allow_stale,
            &out,
        ),
        Command::Update {
            data,
            mods,
            build,
            project,
            engine,
            skip_stub,
            force,
            allow_old_dumps,
        } => {
            let project = project.unwrap_or_else(|| places.settings.project());
            let engine = engine.unwrap_or_else(|| places.settings.engine());
            let build = build.unwrap_or_else(|| places.settings.build());
            let workspace = alchemists_core::mods::Workspace::new(
                places.mods(mods),
                build,
                game,
                usmap,
                project,
            );
            let options = alchemists_core::update::Options {
                data,
                engine,
                skip_stub,
                force,
                allow_old_dumps,
            };
            alchemists_core::update::update(&workspace, &options)
        }
        Command::Stub {
            project,
            engine,
            no_build,
        } => alchemists_core::stub::setup(
            &game,
            &project.unwrap_or_else(|| places.settings.project()),
            &engine.unwrap_or_else(|| places.settings.engine()),
            !no_build,
        ),
        Command::Probe {
            name,
            functions,
            new_objects,
            limit,
            repeats,
            force,
            mods,
        } => {
            let schemas = alchemists_core::usmap::read_usmap(&usmap)?;
            let mods = mods.unwrap_or_else(|| places.settings.mods().remove(0));
            let probe = alchemists_core::probe::write_probe;
            probe(
                &game,
                &schemas,
                &mods,
                &name,
                &functions,
                &new_objects,
                limit,
                repeats,
                force,
            )
            .map(|_| ())
        }
        Command::BuildTables { folder, name, out } => {
            build_tables(&game, &usmap, &folder, name, &out)
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

fn build_tables(
    game: &Game,
    usmap: &Path,
    folder: &Path,
    name: Option<String>,
    out: &Path,
) -> Result<()> {
    let name = match name {
        Some(name) => name,
        None => folder
            .file_name()
            .and_then(|name| name.to_str())
            .context("give the mod a --name")?
            .to_string(),
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(folder)
        .with_context(|| format!("reading {}", display_path(folder)))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    files.sort();
    let mut sources = vec![];
    for file in &files {
        let text = std::fs::read_to_string(file)
            .with_context(|| format!("reading {}", display_path(file)))?;
        let contents = serde_json::from_str(&text)
            .with_context(|| format!("{} isn't valid JSON", display_path(file)))?;
        let label = file.file_name().unwrap().to_string_lossy().into_owned();
        sources.push(edits::Source { label, contents });
    }
    let labels: Vec<&str> = sources.iter().map(|source| source.label.as_str()).collect();
    println!(
        "edit files: {}",
        if labels.is_empty() {
            "(none)".to_string()
        } else {
            labels.join(", ")
        }
    );

    let schemas = alchemists_core::usmap::read_usmap(usmap)?;
    let assets = extract::content_assets(game)?;
    let localizer = extract::english(game, &assets)?;
    let packages = unpack::package_names(&game.paks())?;
    let input = edits::Game {
        assets: &assets,
        schemas: &schemas,
        localizer: Some(&localizer),
        packages: Some(&packages),
    };
    let built = match edits::build(&input, &sources) {
        Ok(built) => built,
        Err(error) => {
            eprintln!("errors:");
            for line in format!("{error:#}").lines() {
                eprintln!("   {line}");
            }
            std::process::exit(1);
        }
    };
    let utoc = out.join(&name).join(format!("zzz_{name}_P.utoc"));
    if let Some(folder) = utoc.parent() {
        let _ = std::fs::remove_dir_all(folder);
    }
    if !built.tables.is_empty() {
        package::write_container(
            &utoc,
            &built.tables,
            retoc::version::EngineVersion::UE5_6,
            package::script_objects(game)?,
        )?;
        package::check_container(&utoc, &built.tables, game)?;
    }
    let rows: usize = built.changed.values().map(|rows| rows.len()).sum();
    println!(
        ".pak mod: {rows} row(s) in {} table(s), checked -> {}",
        built.changed.len(),
        display_path(&utoc)
    );
    for line in &built.report {
        println!("   {line}");
    }
    for warning in &built.warnings {
        println!("   warning: {warning}");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn package_content(
    game: &Game,
    usmap: &Path,
    paths: &[String],
    name: &str,
    craft_types: &[String],
    project: Option<PathBuf>,
    allow_game_paths: bool,
    allow_stale: bool,
    out: &Path,
) -> Result<()> {
    use alchemists_core::content;
    // Git Bash turns /Game/... into C:/Program Files/Git/Game/...; take the /Game/ part back
    let paths: Vec<String> = paths
        .iter()
        .map(|path| {
            let path = path.replace('\\', "/");
            match path.find("/Game/") {
                Some(at) => path[at..].to_string(),
                None => path,
            }
        })
        .collect();
    content::check_paths(&paths, allow_game_paths)?;
    let project = project.unwrap_or_else(alchemists_core::settings::default_project);
    if !project.exists() {
        bail!(
            "{} doesn't exist; give the stub project with --project",
            display_path(&project)
        );
    }
    let schemas = alchemists_core::usmap::read_usmap(usmap)?;
    let mut wanted = indexmap::IndexMap::new();
    for item in craft_types {
        let (asset, value) = item
            .split_once('=')
            .context("--craft-type is ASSET=VALUE")?;
        wanted.insert(
            asset.to_string(),
            content::craft_type_number(value, &schemas)?,
        );
    }
    let stale: Vec<String> = paths
        .iter()
        .flat_map(|path| content::stale_cooks(&project, path))
        .collect();
    if !stale.is_empty() {
        let message = format!("the cook is out of date for:\n   {}", stale.join("\n   "));
        if !allow_stale {
            bail!(
                "{message}\ncook again (Platforms -> Windows -> Cook Content), or pass \
                --allow-stale"
            );
        }
        println!("warning: {message}");
    }
    let mut staged = content::stage(&project, &paths)?;
    let listed: Vec<&str> = staged.iter().map(|asset| asset.path.as_str()).collect();
    println!("staged {} asset(s): {}", staged.len(), listed.join(", "));
    for (asset, &value) in &wanted {
        let found = staged
            .iter_mut()
            .find(|staged| staged.path.rsplit('/').next() == Some(asset.as_str()))
            .with_context(|| {
                format!("craft type for {asset}: no {asset} among the staged assets")
            })?;
        let old = content::patch_craft_type(found, value)?;
        println!("   craft type {old} -> {value} in {asset}");
    }
    let utoc = out.join(name).join(format!("zzz_{name}_P.utoc"));
    if let Some(folder) = utoc.parent() {
        let _ = std::fs::remove_dir_all(folder);
    }
    let packaged = content::package(&utoc, &staged, game, &schemas, &wanted)?;
    println!("packed -> {}; checked:", display_path(&utoc));
    for (package, components) in &packaged.components {
        println!("   {package}:");
        for component in components {
            println!("      {}", content::describe(component));
        }
    }
    Ok(())
}
