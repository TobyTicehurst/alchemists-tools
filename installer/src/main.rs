//! The installer players download from a release: `alchemists-tools-setup.exe`. It fetches the
//! release's files (the tools, and the game's mappings, which no exe carries), puts them in
//! `%LOCALAPPDATA%\Programs\AlchemistsTools`, adds that folder to the user's PATH (so
//! `alchemists-tools` works in any new terminal), adds a Start menu shortcut to the GUI, and
//! registers itself in Windows' Installed apps to uninstall. Everything is the user's own, so no
//! administrator rights are needed. Then, if the player agrees, it installs UE4SS into the game,
//! with the installed tools (`alchemists-tools ue4ss install`, which pins and checks the build).
//! Uninstalling the tools leaves UE4SS, which installed mods may still need.
//!
//! The release's files are one zip, `alchemists-tools-<version>-windows.zip`, downloaded from the
//! release of the installer's own version and checked against the SHA-256 built into the installer
//! when the release was made (`ALCHEMISTS_PAYLOAD_SHA256`). An installer built without one only
//! installs a zip it's given (`--payload`, or one beside it). Only the files a release has are
//! taken from the zip: the tools, `README.md`, `LICENSE`, and `mappings/<build>.usmap`.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Parser;

const NAME: &str = "Alchemist's Tools";
const REPOSITORY: &str = "https://github.com/TobyTicehurst/alchemists-tools";
/// The checksum of this version's release zip, set when the release is built.
const PAYLOAD_SHA256: Option<&str> = option_env!("ALCHEMISTS_PAYLOAD_SHA256");
/// The files a release zip holds, besides `mappings/<build>.usmap`.
const FILES: &[&str] = &[
    "alchemists-tools.exe",
    "alchemists-gui.exe",
    "README.md",
    "LICENSE",
];
/// What the tools download into their own folder the first time they need it.
const DOWNLOADED: &[&str] = &["oo2core_9_win64.dll"];
const SETUP: &str = "alchemists-tools-setup.exe";
/// Where Windows lists installed apps (the user's own).
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\AlchemistsTools";
const LIMIT: u64 = 512 << 20;

#[derive(Parser)]
#[command(
    version,
    about = "Install Alchemist's Tools: the GUI, the command line, and the game's mappings"
)]
struct Options {
    /// Remove them instead (your mods folder and settings are kept)
    #[arg(long)]
    uninstall: bool,
    /// Where to install [default: %LOCALAPPDATA%\Programs\AlchemistsTools]
    #[arg(long)]
    folder: Option<PathBuf>,
    /// The release zip to install, instead of downloading it
    #[arg(long)]
    payload: Option<PathBuf>,
    /// Don't add the folder to the PATH
    #[arg(long)]
    no_path: bool,
    /// Don't add a Start menu shortcut, or register in Installed apps
    #[arg(long)]
    no_shortcut: bool,
    /// Also install UE4SS into the game, without asking
    #[arg(long, conflicts_with = "no_ue4ss")]
    ue4ss: bool,
    /// Don't install UE4SS, or ask to
    #[arg(long)]
    no_ue4ss: bool,
    /// Don't wait for Enter at the end
    #[arg(long)]
    quiet: bool,
    /// (An uninstall running from a copy in the temporary folder, which can delete the original.)
    #[arg(long, hide = true)]
    from_temporary: bool,
}

fn main() -> std::process::ExitCode {
    let options = Options::parse();
    let result = if options.uninstall {
        uninstall(&options)
    } else {
        install(&options)
    };
    let failed = match &result {
        Ok(Finished::Done) => false,
        // a copy carries on (see `uninstall`)
        Ok(Finished::HandedOver) => return std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("\nerror: {error:#}");
            true
        }
    };
    if !options.quiet {
        println!("\nPress Enter to close.");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    if failed {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}

enum Finished {
    Done,
    /// A copy of the uninstaller carries on.
    HandedOver,
}

fn default_folder() -> Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").context("no LOCALAPPDATA folder")?;
    Ok(PathBuf::from(local)
        .join("Programs")
        .join("AlchemistsTools"))
}

fn payload_name() -> String {
    format!("alchemists-tools-{}-windows.zip", env!("CARGO_PKG_VERSION"))
}

fn system_program(path: &[&str]) -> Result<PathBuf> {
    let root = std::env::var_os("SystemRoot").context("no SystemRoot folder")?;
    let program = path
        .iter()
        .fold(PathBuf::from(root), |folder, part| folder.join(part));
    if !program.is_file() {
        bail!("{} isn't there", program.display());
    }
    Ok(program)
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The release zip: given, beside the installer, or downloaded (with HTTPS only, through Windows'
/// own curl) and checked against the checksum built in.
fn payload(options: &Options) -> Result<Vec<u8>> {
    let beside = std::env::current_exe()?
        .parent()
        .map(|folder| folder.join(payload_name()));
    let local = options
        .payload
        .clone()
        .or(beside.filter(|path| path.is_file()));
    let bytes = match local {
        Some(path) => {
            println!("installing from {}", path.display());
            std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?
        }
        None => {
            let Some(_) = PAYLOAD_SHA256 else {
                bail!(
                    "this installer was built without its release's checksum, so it doesn't \
                     download: give the release zip with --payload"
                );
            };
            let url = format!(
                "{REPOSITORY}/releases/download/v{}/{}",
                env!("CARGO_PKG_VERSION"),
                payload_name()
            );
            println!("downloading {url}");
            let file = std::env::temp_dir().join(payload_name());
            let curl = system_program(&["System32", "curl.exe"])
                .context("Windows' curl is needed to download (Windows 10 1803 or later)")?;
            let status = std::process::Command::new(curl)
                .args(["--fail", "--location", "--silent", "--show-error"])
                .args(["--proto", "=https", "--proto-redir", "=https"])
                .args(["--max-filesize", &LIMIT.to_string(), "--output"])
                .arg(&file)
                .arg(&url)
                .status()
                .context("running curl")?;
            if !status.success() {
                bail!("couldn't download {url}");
            }
            let bytes = std::fs::read(&file)?;
            let _ = std::fs::remove_file(&file);
            bytes
        }
    };
    if let Some(expected) = PAYLOAD_SHA256 {
        if sha256(&bytes) != expected.to_ascii_lowercase() {
            bail!(
                "the release zip isn't the one this installer was made for (its SHA-256 differs)"
            );
        }
    }
    Ok(bytes)
}

/// The release zip's files, each checked to be one a release has: (path in the folder, contents).
fn release_files(bytes: Vec<u8>) -> Result<Vec<(String, Vec<u8>)>> {
    use std::io::Read;
    let mut zip =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("reading the release zip")?;
    let mut files = vec![];
    for index in 0..zip.len() {
        let mut file = zip.by_index(index)?;
        if file.is_dir() {
            continue;
        }
        let name = file.name().replace('\\', "/");
        let mappings = name
            .strip_prefix("mappings/")
            .and_then(|file| file.strip_suffix(".usmap"))
            .is_some_and(|build| {
                !build.is_empty() && build.bytes().all(|byte| byte.is_ascii_digit())
            });
        if !FILES.contains(&name.as_str()) && !mappings {
            bail!("the release zip holds {name:?}, which a release doesn't");
        }
        if file.size() > LIMIT {
            bail!("{name} is too large");
        }
        let mut contents = vec![];
        (&mut file).take(LIMIT + 1).read_to_end(&mut contents)?;
        files.push((name, contents));
    }
    for required in ["alchemists-tools.exe", "alchemists-gui.exe"] {
        if !files.iter().any(|(name, _)| name == required) {
            bail!("the release zip has no {required}");
        }
    }
    Ok(files)
}

/// Stop if a tool from `folder` is running: its exe can't be replaced or deleted.
fn check_closed(folder: &Path) -> Result<()> {
    for exe in ["alchemists-gui.exe", "alchemists-tools.exe"] {
        let path = folder.join(exe);
        if path.is_file()
            && std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .is_err()
        {
            bail!("{NAME} is running ({exe}): close it first");
        }
    }
    Ok(())
}

fn install(options: &Options) -> Result<Finished> {
    let folder = match &options.folder {
        Some(folder) => folder.clone(),
        None => default_folder()?,
    };
    println!(
        "{NAME} {} -> {}",
        env!("CARGO_PKG_VERSION"),
        folder.display()
    );
    let files = release_files(payload(options)?)?;
    check_closed(&folder)?;
    std::fs::create_dir_all(folder.join("mappings"))?;
    // the old release's mappings go: the new release's are the ones to use
    for old in std::fs::read_dir(folder.join("mappings"))?.flatten() {
        if old
            .path()
            .extension()
            .is_some_and(|extension| extension == "usmap")
        {
            std::fs::remove_file(old.path())?;
        }
    }
    let mut size = 0;
    for (name, contents) in &files {
        let path = folder.join(name);
        std::fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))?;
        size += contents.len() as u64;
        println!("   {name}");
    }
    // itself, to uninstall (or run again)
    let exe = std::env::current_exe()?;
    let setup = folder.join(SETUP);
    if exe != setup {
        std::fs::copy(&exe, &setup).with_context(|| format!("writing {}", setup.display()))?;
    }
    if options.no_path {
        println!("not added to the PATH (--no-path)");
    } else if windows::add_to_path(&folder)? {
        println!("added {} to your PATH", folder.display());
    } else {
        println!("{} is on your PATH already", folder.display());
    }
    if !options.no_shortcut {
        windows::shortcut(&folder)?;
        windows::register(&folder, size)?;
        println!("added {NAME} to the Start menu and to Installed apps");
    }
    println!(
        "\nInstalled. Start {NAME} from the Start menu; in a new terminal, alchemists-tools \
         --help shows the command line."
    );
    offer_ue4ss(options, &folder);
    Ok(Finished::Done)
}

/// Install UE4SS into the game with the installed tools, if the player wants it. A failure (the
/// game not found, say) doesn't undo the tools: Setup in the GUI can install it later.
fn offer_ue4ss(options: &Options, folder: &Path) {
    let wanted = if options.no_ue4ss {
        false
    } else if options.ue4ss {
        true
    } else if options.quiet {
        println!("UE4SS wasn't installed: Setup in {NAME} installs it, for mods with Lua or C++");
        false
    } else {
        println!(
            "\nMods with Lua or C++ need UE4SS in the game. Install it now (the build these \
             tools are made for)? [Y/n]"
        );
        let mut answer = String::new();
        let _ = std::io::stdin().read_line(&mut answer);
        !answer.trim().to_ascii_lowercase().starts_with('n')
    };
    if !wanted {
        return;
    }
    let status = std::process::Command::new(folder.join("alchemists-tools.exe"))
        .args(["ue4ss", "install"])
        .status();
    if !status.is_ok_and(|status| status.success()) {
        println!(
            "UE4SS wasn't installed (see above). Setup in {NAME}, or alchemists-tools ue4ss \
             install, can install it later."
        );
    }
}

fn uninstall(options: &Options) -> Result<Finished> {
    let folder = match &options.folder {
        Some(folder) => folder.clone(),
        None => windows::registered_folder().map_or_else(default_folder, Ok)?,
    };
    // An exe can't delete itself: the one in the folder hands over to a copy elsewhere
    let exe = std::env::current_exe()?;
    if exe.starts_with(&folder) && !options.from_temporary {
        let copy = std::env::temp_dir().join("alchemists-tools-uninstall.exe");
        std::fs::copy(&exe, &copy)?;
        let mut command = std::process::Command::new(&copy);
        command
            .args(["--uninstall", "--from-temporary", "--folder"])
            .arg(&folder);
        if options.quiet {
            command.arg("--quiet");
        }
        command.spawn().context("starting the uninstaller")?;
        return Ok(Finished::HandedOver);
    }
    println!("removing {NAME} from {}", folder.display());
    check_closed(&folder)?;
    if windows::remove_from_path(&folder)? {
        println!("removed {} from your PATH", folder.display());
    }
    windows::remove_shortcut()?;
    windows::unregister()?;
    // only the files it installed (and those the tools fetched), then the folders they leave empty
    let mut names: Vec<String> = FILES
        .iter()
        .chain(DOWNLOADED)
        .chain(&[SETUP])
        .map(|name| name.to_string())
        .collect();
    for mappings in std::fs::read_dir(folder.join("mappings"))
        .into_iter()
        .flatten()
        .flatten()
    {
        if mappings
            .path()
            .extension()
            .is_some_and(|extension| extension == "usmap")
        {
            names.push(format!(
                "mappings/{}",
                mappings.file_name().to_string_lossy()
            ));
        }
    }
    for name in names {
        let path = folder.join(&name);
        if path.is_file() {
            std::fs::remove_file(&path).with_context(|| format!("deleting {}", path.display()))?;
        }
    }
    let _ = std::fs::remove_dir(folder.join("mappings"));
    let _ = std::fs::remove_dir(&folder);
    if folder.exists() {
        println!(
            "kept {}: it holds files the installer didn't put there",
            folder.display()
        );
    }
    if options.from_temporary {
        windows::delete_once_closed(&exe);
    }
    println!(
        "\nUninstalled. Your mods folder, your settings (%APPDATA%\\alchemists-tools), the mods \
         installed in the game and UE4SS are as they were: untick the mods, and remove UE4SS in \
         Setup, first to remove them."
    );
    Ok(Finished::Done)
}

#[cfg(windows)]
mod windows {
    //! The user's PATH, Start menu shortcut, and Installed apps entry.

    use std::path::{Path, PathBuf};

    use anyhow::{Context, Result};
    use winreg::enums::{RegType, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
    use winreg::{RegKey, RegValue};

    use super::{NAME, REPOSITORY, SETUP, UNINSTALL_KEY};

    fn environment() -> Result<RegKey> {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags("Environment", KEY_READ | KEY_WRITE)
            .context("opening your environment variables")
    }

    /// The user's PATH as stored, with its type (REG_EXPAND_SZ keeps `%VARIABLES%` unexpanded,
    /// and is written back as it was).
    fn read_path(environment: &RegKey) -> (String, RegType) {
        match environment.get_raw_value("Path") {
            Ok(value) => {
                let units: Vec<u16> = value
                    .bytes
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .take_while(|unit| *unit != 0)
                    .collect();
                (String::from_utf16_lossy(&units), value.vtype)
            }
            Err(_) => (String::new(), RegType::REG_EXPAND_SZ),
        }
    }

    fn write_path(environment: &RegKey, path: &str, kind: RegType) -> Result<()> {
        let bytes: Vec<u8> = path
            .encode_utf16()
            .chain([0])
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        environment
            .set_raw_value(
                "Path",
                &RegValue {
                    bytes: bytes.into(),
                    vtype: kind,
                },
            )
            .context("writing your PATH")?;
        announce();
        Ok(())
    }

    fn same_folder(entry: &str, folder: &Path) -> bool {
        let folder = folder.to_string_lossy();
        entry
            .trim()
            .trim_end_matches('\\')
            .eq_ignore_ascii_case(folder.trim_end_matches('\\'))
    }

    /// Add `folder` to the end of the user's PATH; false if it's there already.
    pub fn add_to_path(folder: &Path) -> Result<bool> {
        let environment = environment()?;
        let (path, kind) = read_path(&environment);
        if path.split(';').any(|entry| same_folder(entry, folder)) {
            return Ok(false);
        }
        let start = path.trim_end_matches(';');
        let new = if start.is_empty() {
            folder.display().to_string()
        } else {
            format!("{start};{}", folder.display())
        };
        write_path(&environment, &new, kind)?;
        Ok(true)
    }

    /// Take `folder` out of the user's PATH, leaving every other entry as it was; false if it
    /// wasn't there.
    pub fn remove_from_path(folder: &Path) -> Result<bool> {
        let environment = environment()?;
        let (path, kind) = read_path(&environment);
        let kept: Vec<&str> = path
            .split(';')
            .filter(|entry| !same_folder(entry, folder))
            .collect();
        let new = kept.join(";");
        if new == path {
            return Ok(false);
        }
        write_path(&environment, &new, kind)?;
        Ok(true)
    }

    /// Tell running programs (Explorer, so new terminals) that the environment changed.
    fn announce() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
        };
        let name: Vec<u16> = "Environment".encode_utf16().chain([0]).collect();
        let mut result = 0;
        // SAFETY: a broadcast with a nul-terminated string that outlives the call
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                name.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                5000,
                &mut result,
            );
        }
    }

    fn shortcut_path() -> Result<PathBuf> {
        let roaming = std::env::var_os("APPDATA").context("no APPDATA folder")?;
        Ok(PathBuf::from(roaming)
            .join(r"Microsoft\Windows\Start Menu\Programs")
            .join(format!("{NAME}.lnk")))
    }

    /// A Start menu shortcut to the GUI, made by Windows' own scripting (PowerShell).
    pub fn shortcut(folder: &Path) -> Result<()> {
        // in single quotes, where only ' itself needs doubling
        let quoted = |path: &Path| format!("'{}'", path.display().to_string().replace('\'', "''"));
        let script = format!(
            "$link = (New-Object -ComObject WScript.Shell).CreateShortcut({}); \
             $link.TargetPath = {}; $link.WorkingDirectory = {}; $link.Save()",
            quoted(&shortcut_path()?),
            quoted(&folder.join("alchemists-gui.exe")),
            quoted(folder)
        );
        let powershell =
            super::system_program(&["System32", "WindowsPowerShell", "v1.0", "powershell.exe"])?;
        let status = std::process::Command::new(powershell)
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .status()
            .context("running PowerShell to make the shortcut")?;
        if !status.success() {
            anyhow::bail!("couldn't make the Start menu shortcut");
        }
        Ok(())
    }

    pub fn remove_shortcut() -> Result<()> {
        let path = shortcut_path()?;
        if path.is_file() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// List it in Windows' Installed apps, to uninstall from there.
    pub fn register(folder: &Path, size: u64) -> Result<()> {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(UNINSTALL_KEY)
            .context("registering in Installed apps")?;
        let setup = folder.join(SETUP);
        key.set_value("DisplayName", &NAME)?;
        key.set_value("DisplayVersion", &env!("CARGO_PKG_VERSION"))?;
        key.set_value("InstallLocation", &folder.display().to_string())?;
        key.set_value(
            "DisplayIcon",
            &folder.join("alchemists-gui.exe").display().to_string(),
        )?;
        key.set_value(
            "UninstallString",
            &format!("\"{}\" --uninstall", setup.display()),
        )?;
        key.set_value(
            "QuietUninstallString",
            &format!("\"{}\" --uninstall --quiet", setup.display()),
        )?;
        key.set_value("URLInfoAbout", &REPOSITORY)?;
        key.set_value("NoModify", &1u32)?;
        key.set_value("NoRepair", &1u32)?;
        key.set_value("EstimatedSize", &((size / 1024) as u32))?;
        Ok(())
    }

    /// Where it's registered as installed, if it is.
    pub fn registered_folder() -> Option<PathBuf> {
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(UNINSTALL_KEY)
            .ok()?;
        let folder: String = key.get_value("InstallLocation").ok()?;
        Some(PathBuf::from(folder))
    }

    /// Delete `exe` (the uninstaller's copy in the temporary folder) a moment after it closes.
    pub fn delete_once_closed(exe: &Path) {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let Ok(command) = super::system_program(&["System32", "cmd.exe"]) else {
            return;
        };
        let script = format!("ping -n 3 127.0.0.1 >nul & del /q \"{}\"", exe.display());
        let _ = std::process::Command::new(command)
            .args(["/d", "/c"])
            .raw_arg(script)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }

    pub fn unregister() -> Result<()> {
        match RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(UNINSTALL_KEY) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                Err(error).context("removing it from Installed apps")
            }
            _ => Ok(()),
        }
    }
}

#[cfg(not(windows))]
mod windows {
    //! The installer is for Windows; elsewhere, copy the files by hand.
    use std::path::{Path, PathBuf};

    use anyhow::{bail, Result};

    pub fn add_to_path(_: &Path) -> Result<bool> {
        bail!("the installer is for Windows")
    }
    pub fn remove_from_path(_: &Path) -> Result<bool> {
        bail!("the installer is for Windows")
    }
    pub fn shortcut(_: &Path) -> Result<()> {
        bail!("the installer is for Windows")
    }
    pub fn remove_shortcut() -> Result<()> {
        Ok(())
    }
    pub fn register(_: &Path, _: u64) -> Result<()> {
        Ok(())
    }
    pub fn registered_folder() -> Option<PathBuf> {
        None
    }
    pub fn delete_once_closed(_: &Path) {}
    pub fn unregister() -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    fn zip(names: &[&str]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(vec![]));
        for name in names {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"x").unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn only_a_release_files_are_taken() {
        let good = zip(&[
            "alchemists-tools.exe",
            "alchemists-gui.exe",
            "README.md",
            "mappings/25991404.usmap",
        ]);
        assert_eq!(super::release_files(good).unwrap().len(), 4);
        for bad in [
            "../evil.exe",
            "mappings/../x.usmap",
            "mappings/new.usmap",
            "other.dll",
            "sub/alchemists-tools.exe",
        ] {
            let zip = zip(&["alchemists-tools.exe", "alchemists-gui.exe", bad]);
            assert!(super::release_files(zip).is_err(), "{bad} was taken");
        }
    }
}
