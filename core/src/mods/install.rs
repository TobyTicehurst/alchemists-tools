//! Installing and uninstalling one mod, scanning the game folder, and making a new mod folder.

use super::*;

impl Workspace {
    fn check_installable(&self, info: &Entry, installed: &IndexMap<String, Entry>) -> Result<()> {
        let mut problems = vec![];
        let own_version = info.version()?;
        for dependency in info.requires()? {
            let other = installed.get(&dependency.name);
            match (dependency.kind, other) {
                (Kind::Incompatible, Some(_)) => problems.push(format!(
                    "{} doesn't work with {}, which is installed",
                    info.name, dependency.name
                )),
                (Kind::Incompatible, None) => {}
                (kind, None) if kind.is_required() => problems.push(format!(
                    "{} needs {}: install it first, or use --with-dependencies",
                    info.name,
                    requirement_text(&dependency)
                )),
                (Kind::Recommended, None) => {
                    note!(
                        "note: {} recommends {}",
                        info.name,
                        requirement_text(&dependency)
                    )
                }
                (_, None) => {}
                (_, Some(other)) => {
                    if !satisfies(other.version()?, &dependency) {
                        problems.push(format!(
                            "{} needs {}, but {} is installed",
                            info.name,
                            requirement_text(&dependency),
                            other.version
                        ));
                    }
                }
            }
        }
        for (name, other) in installed {
            if *name == info.name {
                continue;
            }
            for dependency in other.requires()? {
                if dependency.name != info.name {
                    continue;
                }
                if dependency.kind == Kind::Incompatible {
                    problems.push(format!(
                        "{name} (installed) doesn't work with {}",
                        info.name
                    ));
                } else if !satisfies(own_version, &dependency) {
                    problems.push(format!(
                        "{name} (installed) needs {}, not {}",
                        requirement_text(&dependency),
                        info.version
                    ));
                }
            }
        }
        let mut numbers: HashMap<u8, String> = HashMap::new();
        for entry in installed
            .values()
            .filter(|entry| entry.name != info.name)
            .chain([info])
        {
            for (key, number) in &entry.craft_type_numbers {
                let first = numbers
                    .entry(*number)
                    .or_insert_with(|| key.clone())
                    .clone();
                if first != *key {
                    problems.push(format!(
                        "craft type {number} is both {first} and {key} (mods made with different \
                        allocations.json \
                         files; one of them needs a new number)"
                    ));
                }
            }
        }
        if !problems.is_empty() {
            bail!("can't install:\n   {}", problems.join("\n   "));
        }
        Ok(())
    }

    /// The files to install for a mod, as records (path, size, hash) of its build.
    pub(super) fn file_records(
        build_folder: &Path,
        relative: &[String],
    ) -> Result<Vec<FileRecord>> {
        relative
            .iter()
            .map(|path| {
                let (size, sha256) = hash(&build_folder.join(path))?;
                Ok(FileRecord {
                    path: path.clone(),
                    size,
                    sha256,
                })
            })
            .collect()
    }

    /// Stop unless every file the mod would write is free for it: no other mod's manifest lists
    /// it, the tool didn't generate it, and if it exists it's this mod's own (as its manifest, or
    /// for a mod installed before manifests, the install record, says) and unchanged.
    fn check_files_free(
        &self,
        files: &GameFiles,
        name: &str,
        new_files: &[FileRecord],
        record: &Record,
        manifests: &[(String, Manifest)],
    ) -> Result<()> {
        let problems =
            Self::files_not_free(files, name, new_files, record, manifests, &HashSet::new())?;
        if !problems.is_empty() {
            bail!(
                "can't install {name}: its files would replace files that aren't its own:\n   {}",
                problems.join("\n   ")
            );
        }
        Ok(())
    }

    /// The files a mod would write that aren't free for it (see [`Self::check_files_free`]),
    /// leaving out `freed`: files that are deleted first, in the same transaction.
    pub(super) fn files_not_free(
        files: &GameFiles,
        name: &str,
        new_files: &[FileRecord],
        record: &Record,
        manifests: &[(String, Manifest)],
        freed: &HashSet<String>,
    ) -> Result<Vec<String>> {
        let previous = manifests
            .iter()
            .find(|(owner, _)| owner == name)
            .map(|(_, manifest)| manifest);
        let mut problems = vec![];
        for file in new_files.iter().filter(|file| !freed.contains(&file.path)) {
            let full = files.resolve(&file.path)?;
            if let Some((owner, _)) = manifests.iter().find(|(owner, manifest)| {
                owner != name && manifest.files.iter().any(|listed| listed.path == file.path)
            }) {
                problems.push(format!("{} belongs to the mod {owner}", file.path));
                continue;
            }
            if record.generated.contains_key(&file.path) {
                problems.push(format!("{} is one of the tool's merged files", file.path));
                continue;
            }
            if !full.exists() {
                continue;
            }
            match previous.and_then(|manifest| {
                manifest
                    .files
                    .iter()
                    .find(|listed| listed.path == file.path)
            }) {
                Some(old) if files.state(old)? == State::Unchanged => {}
                Some(_) => problems.push(format!(
                    "{} has changed since {name} installed it (move it away, or uninstall {name} \
                     with --force, first)",
                    file.path
                )),
                None => {
                    let recorded = record
                        .mods
                        .get(name)
                        .is_some_and(|entry| entry.files.contains(&file.path));
                    if !recorded {
                        problems.push(format!(
                            "{} already exists, and no manifest says it's {name}'s \
                            (alchemists-tools \
                             mod scan shows what it is; move it away first)",
                            file.path
                        ));
                    }
                }
            }
        }
        Ok(problems)
    }

    pub(super) fn write_manifest(
        &self,
        transaction: &mut Transaction,
        manifest: &Manifest,
    ) -> Result<()> {
        transaction.write(
            &GameFiles::manifest_path(&manifest.name),
            (serde_json::to_string_pretty(manifest)? + "\n").as_bytes(),
        )
    }

    pub(crate) fn manifest_for(
        info: &Entry,
        installed_by: &str,
        files: Vec<FileRecord>,
    ) -> Manifest {
        Manifest {
            name: info.name.clone(),
            title: info.title.clone(),
            version: info.version.clone(),
            game_build: info.game_build.clone(),
            dependencies: info.dependencies.clone(),
            installed_by: installed_by.to_string(),
            tables: info.tables.clone(),
            new_buildings: info.new_buildings.clone(),
            detours: info.detours.clone(),
            hooks: info.hooks.clone(),
            cautions: info.cautions.clone(),
            files,
        }
    }

    pub fn install(
        &self,
        mod_: &Mod,
        with_dependencies: bool,
        force: bool,
        installing: &[String],
    ) -> Result<()> {
        if installing.contains(&mod_.name) {
            bail!(
                "dependency loop: {} -> {}",
                installing.join(" -> "),
                mod_.name
            );
        }
        if installing.is_empty() {
            check_game_closed()?;
        }
        if with_dependencies {
            for dependency in mod_
                .requires
                .iter()
                .filter(|dependency| dependency.kind.is_required())
            {
                let current = self.record()?.mods.get(&dependency.name).cloned();
                // the tool carries its own Lua library, for mods that depend on it: written
                // when missing, and updated when the copy is older than the tool's
                if dependency.name == "AlchemistsLibrary" {
                    crate::probe::write_library(&self.library_home())?;
                }
                let local = self.local_folder(&dependency.name);
                let newer = match (&current, &local) {
                    (Some(current), Some(local)) => Mod::load(local)
                        .ok()
                        .and_then(|local| parse_version(&local.version, &dependency.name).ok())
                        .is_some_and(|local| {
                            current.version().is_ok_and(|installed| local > installed)
                        }),
                    _ => false,
                };
                let unmet = current.as_ref().is_none_or(|current| {
                    current
                        .version()
                        .is_ok_and(|version| !satisfies(version, dependency))
                });
                if unmet || newer {
                    let Some(local) = local else {
                        bail!(
                            "{} needs {}, which isn't in the mods folder",
                            mod_.name,
                            requirement_text(dependency)
                        );
                    };
                    let mut chain = installing.to_vec();
                    chain.push(mod_.name.clone());
                    self.install(&Mod::load(&local)?, true, force, &chain)?;
                }
            }
        }
        let (build_folder, info) = self.build_mod(mod_)?;
        if info.needs_ue4ss && !join(&self.game.root, UE4SS_FOLDER).is_dir() {
            bail!(
                "{} needs UE4SS, and it isn't installed in {}",
                info.name,
                display_path(&self.game.root)
            );
        }
        let files = GameFiles::new(&self.game.root)?;
        let mut record = self.record()?;
        self.check_installable(&info, &record.mods)?;
        let new_files = Self::file_records(&build_folder, &info.files)?;
        let manifests = files.manifests()?;
        self.check_files_free(&files, &info.name, &new_files, &record, &manifests)?;
        // this mod's old files that the new version doesn't have, deleted once it's in
        let stale: Vec<FileRecord> = manifests
            .iter()
            .find(|(owner, _)| *owner == info.name)
            .map(|(_, manifest)| {
                manifest
                    .files
                    .iter()
                    .filter(|old| !new_files.iter().any(|new| new.path == old.path))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        // from here on, a failure puts the game folder back as it was
        let mut transaction = files.begin()?;
        record.mods.insert(info.name.clone(), info.clone());
        self.merge(&mut record, force, &mut transaction)?;
        for file in &new_files {
            transaction.copy(&build_folder.join(&file.path), &file.path)?;
        }
        let gone = transaction.delete_listed(&stale, false)?;
        for kept in &gone.kept {
            note!(
                "   kept {kept}: no longer part of {}, but changed since it was installed",
                info.name
            );
        }
        self.write_manifest(
            &mut transaction,
            &Self::manifest_for(&info, "alchemists-tools", new_files),
        )?;
        self.save_record(&mut transaction, &record)?;
        transaction.commit()?;
        note!(
            "installed {} {} (load order: {}); restart the game to load it",
            info.name,
            info.version,
            record.load_order.join(", ")
        );
        if !info.new_buildings.is_empty() {
            note!(
                "   before uninstalling, deconstruct every placed {} in every save you keep: a \
                save with \
                 one placed crashes the game without the mod",
                info.new_buildings.join(", ")
            );
        }
        let unknown = self.unowned_containers(&files, &record)?;
        if !unknown.is_empty() {
            warning!(
                "~mods has containers no manifest lists ({}); if one replaces a table \
                that installed \
                 mods edit, only one takes effect (alchemists-tools mod scan shows what they \
                     change)",
                unknown.join(", ")
            );
        }
        Ok(())
    }

    /// Containers in `~mods` that no manifest or record lists and the tool didn't generate.
    pub(super) fn unowned_containers(
        &self,
        files: &GameFiles,
        record: &Record,
    ) -> Result<Vec<String>> {
        let owned: HashSet<String> = files
            .manifests()?
            .into_iter()
            .flat_map(|(_, manifest)| manifest.files.into_iter().map(|file| file.path))
            .chain(record.generated.keys().cloned())
            .chain(
                record
                    .mods
                    .values()
                    .flat_map(|entry| entry.files.iter().cloned()),
            )
            .collect();
        let prefix = PAK_FOLDER.join("/");
        let mut containers: Vec<String> = std::fs::read_dir(self.mods_paks_folder())
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".utoc") || name.ends_with(".pak"))
            .filter(|name| !owned.contains(&format!("{prefix}/{name}")))
            .map(|name| {
                name.rsplit_once('.')
                    .map(|(stem, _)| stem.to_string())
                    .unwrap_or(name)
            })
            .collect();
        containers.sort();
        containers.dedup();
        Ok(containers)
    }

    /// Files that look like a mod's by their names, for a mod no manifest or record describes.
    fn files_named_for(&self, name: &str) -> Vec<String> {
        let prefix = PAK_FOLDER.join("/");
        let mut files: Vec<String> = [
            data_container(name),
            content_container(name),
            save_container(name),
        ]
        .iter()
        .flat_map(|base| {
            ["pak", "utoc", "ucas"].map(|extension| format!("{prefix}/{base}.{extension}"))
        })
        .filter(|relative| self.game.root.join(relative).is_file())
        .collect();
        let ue4ss = UE4SS_FOLDER.join("/");
        files.extend(
            files_below(&join(&self.game.root, UE4SS_FOLDER).join(name))
                .into_iter()
                .map(|file| format!("{ue4ss}/{name}/{file}")),
        );
        files
    }

    pub fn uninstall(&self, name: &str, force: bool) -> Result<()> {
        // The name becomes part of the paths looked at: ".." or an absolute path would reach far
        // beyond a mod
        if !is_name(name) {
            bail!(
                "{name:?} isn't a mod name: a mod name is letters and digits, starting with a \
                letter"
            );
        }
        check_game_closed()?;
        let files = GameFiles::new(&self.game.root)?;
        let mut record = self.record()?;
        let manifest = files
            .manifests()?
            .into_iter()
            .find(|(owner, _)| owner == name)
            .map(|(_, manifest)| manifest);
        let entry = record.mods.get(name).cloned();
        if manifest.is_none() && entry.is_none() {
            let named_files = self.files_named_for(name);
            if named_files.is_empty() {
                note!("{name} is not installed");
                return Ok(());
            }
            if !force {
                bail!(
                    "{name} wasn't installed by this tool (no manifest or record), so there's \
                    nothing to \
                     confirm these are its files:\n   {}\npass --force to delete them",
                    named_files.join("\n   ")
                );
            }
            let mut transaction = files.begin()?;
            for path in transaction.delete_unlisted(&named_files)? {
                note!("removed {path}");
            }
            transaction.commit()?;
            return Ok(());
        }
        let mut needs = vec![];
        for (other, other_entry) in &record.mods {
            if other != name
                && other_entry
                    .requires()?
                    .iter()
                    .any(|dependency| dependency.name == name && dependency.kind.is_required())
            {
                needs.push(other.clone());
            }
        }
        if !needs.is_empty() && !force {
            let (verb, them) = if needs.len() == 1 {
                ("needs", "it")
            } else {
                ("need", "them")
            };
            bail!(
                "{} {verb} {name}: uninstall {them} first, or pass --force",
                needs.join(" and ")
            );
        }
        // Without a manifest (installed by mods.py or an older version), the record lists the files
        // but can't confirm they're unchanged
        let unverified: Vec<String> = match (&manifest, &entry) {
            (None, Some(entry)) => entry
                .files
                .iter()
                .filter(|file| self.game.root.join(file).is_file())
                .cloned()
                .collect(),
            _ => vec![],
        };
        if !unverified.is_empty() && !force {
            bail!(
                "{name} has no manifest (it was installed by mods.py or an older version), so \
                nothing \
                 confirms these are still its own files:\n   {}\npass --force to delete them (or \
                     reinstall \
                 it with this tool first, which writes its manifest)",
                unverified.join("\n   ")
            );
        }
        // a mod no folder of mods holds is kept as a zip there first, to install it again
        if self.local_folder(name).is_none() {
            let kept_entry = entry.clone().or_else(|| {
                manifest
                    .as_ref()
                    .map(|found| entry_from_manifest(name, found))
            });
            if let Some(kept_entry) = kept_entry {
                self.keep_copy(&files, &kept_entry, manifest.as_ref(), force)?;
            }
        }
        // from here on, a failure puts the game folder back as it was
        let mut transaction = files.begin()?;
        if entry.is_some() {
            record.mods.shift_remove(name);
            self.merge(&mut record, force, &mut transaction)?;
        }
        let mut kept = vec![];
        if let Some(manifest) = &manifest {
            let result = transaction.delete_listed(&manifest.files, force)?;
            for path in &result.deleted {
                note!("removed {path}");
            }
            for path in &result.missing {
                note!("already gone: {path}");
            }
            kept = result.kept;
            transaction.delete(&GameFiles::manifest_path(name))?;
        } else {
            for path in transaction.delete_unlisted(&unverified)? {
                note!("removed {path}");
            }
        }
        self.save_record(&mut transaction, &record)?;
        transaction.commit()?;
        note!("uninstalled {name}");
        if !kept.is_empty() {
            note!(
                "kept these files, which have changed since {name} installed them (another mod or \
                you may \
                 have replaced them; --force deletes them too):\n   {}",
                kept.join("\n   ")
            );
        }
        let buildings = manifest
            .map(|manifest| manifest.new_buildings)
            .or(entry.map(|recorded| recorded.new_buildings))
            .unwrap_or_default();
        if !buildings.is_empty() {
            note!(
                "reminder: saves with a placed {} will crash without the mod; reinstall it to \
                deconstruct them",
                buildings.join(", ")
            );
        }
        Ok(())
    }

    /// The merged files of a record from before the tool kept hashes (mods.py): they're the
    /// tool's, and the next install or uninstall records them.
    fn legacy_merged(&self, record: &Record) -> Vec<String> {
        if !record.generated.is_empty() || record.mods.is_empty() {
            return vec![];
        }
        let prefix = PAK_FOLDER.join("/");
        [data_container(MERGED), save_container(MERGED_SAVE)]
            .iter()
            .flat_map(|base| {
                ["pak", "ucas", "utoc"].map(|extension| format!("{prefix}/{base}.{extension}"))
            })
            .filter(|path| self.game.root.join(path).is_file())
            .collect()
    }

    /// Who each file in the game folder that a mod or the tool accounts for belongs to: path ->
    /// the mod's name (`<name> (recorded)` for a mod without a manifest), or `the tool`.
    pub(super) fn owners(
        &self,
        record: &Record,
        manifests: &[(String, Manifest)],
    ) -> HashMap<String, String> {
        let mut owned = HashMap::new();
        for (name, manifest) in manifests {
            for file in &manifest.files {
                owned.insert(file.path.clone(), name.clone());
            }
            owned.insert(GameFiles::manifest_path(name), name.clone());
        }
        for entry in record.mods.values() {
            if !manifests.iter().any(|(owner, _)| *owner == entry.name) {
                for file in &entry.files {
                    owned.insert(file.clone(), format!("{} (recorded)", entry.name));
                }
            }
        }
        for path in self
            .legacy_merged(record)
            .into_iter()
            .chain(record.generated.keys().cloned())
            .chain([format!("{}/{RECORD}", PAK_FOLDER.join("/"))])
        {
            owned.insert(path, "the tool".into());
        }
        owned
    }

    /// Everything else that could be a mod: the files below Paks (not the game's own) that no mod
    /// accounts for, and UE4SS mod folders (not those UE4SS comes with: `Mods/<x>`, or
    /// `Mods/shared/<x>`) -> the files in them no mod accounts for.
    pub(super) fn unaccounted(
        &self,
        owned: &HashMap<String, String>,
    ) -> (Vec<String>, BTreeMap<String, Vec<String>>) {
        let paks = join(&self.game.root, &["AlchemyFactory", "Content", "Paks"]);
        let mut unknown: Vec<String> = vec![];
        for file in files_below(&paks) {
            let relative = format!("AlchemyFactory/Content/Paks/{file}");
            let game_own = !file.contains('/')
                && (file.starts_with("AlchemyFactory-Windows.") || file.starts_with("global."));
            if !game_own && !owned.contains_key(&relative) {
                unknown.push(relative);
            }
        }
        let ue4ss_mods = join(&self.game.root, UE4SS_FOLDER);
        let ue4ss_prefix = UE4SS_FOLDER.join("/");
        let mut unknown_ue4ss: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for file in files_below(&ue4ss_mods) {
            let relative = format!("{ue4ss_prefix}/{file}");
            let top = if file.starts_with("shared/") {
                file.splitn(3, '/').take(2).collect::<Vec<_>>().join("/")
            } else {
                file.split('/').next().unwrap_or_default().to_string()
            };
            if UE4SS_OWN.contains(&top.as_str()) || owned.contains_key(&relative) {
                continue;
            }
            unknown_ue4ss
                .entry(top.clone())
                .or_default()
                .push(file[top.len()..].trim_start_matches('/').to_string());
        }
        (unknown, unknown_ue4ss)
    }

    /// What every mod in the game folder is, and whether its files are as installed: mods with
    /// manifests, mods recorded without one, the tool's merged files, and everything no mod
    /// accounts for (with what its containers replace), plus containers that replace the same
    /// asset. Mods installed from a zip are added to the install record.
    pub fn scan(&self) -> Result<()> {
        let files = GameFiles::new(&self.game.root)?;
        let mut record = self.record()?;
        let manifests = files.manifests()?;
        let owned = self.owners(&record, &manifests); // path -> who it belongs to

        note!("== mods with manifests");
        if manifests.is_empty() {
            note!("   none");
        }
        let mut added = vec![];
        for (name, manifest) in &manifests {
            let (mut unchanged, mut changed, mut missing) = (0, vec![], vec![]);
            for file in &manifest.files {
                match files.state(file)? {
                    State::Unchanged => unchanged += 1,
                    State::Changed => changed.push(file.path.clone()),
                    State::Missing => missing.push(file.path.clone()),
                }
            }
            note!(
                "   {name} {} (installed by {}): {}{}{}",
                manifest.version,
                manifest.installed_by,
                if manifest.files.is_empty() {
                    "no files of its own (its edits are in the merged files)".to_string()
                } else {
                    format!("{unchanged} file(s) as installed")
                },
                if changed.is_empty() {
                    String::new()
                } else {
                    format!("; CHANGED since: {}", changed.join(", "))
                },
                if missing.is_empty() {
                    String::new()
                } else {
                    format!("; MISSING: {}", missing.join(", "))
                }
            );
            if !record.mods.contains_key(name) {
                record
                    .mods
                    .insert(name.clone(), entry_from_manifest(name, manifest));
                added.push(name.clone());
            }
        }
        // saved now, before the rest of the report (whose output may be cut short)
        if !added.is_empty() {
            record.load_order = load_order(&record.mods)?;
            write_json(&self.record_path(), &record)?;
        }

        let unmanifested: Vec<&Entry> = record
            .mods
            .values()
            .filter(|entry| !manifests.iter().any(|(owner, _)| *owner == entry.name))
            .collect();
        if !unmanifested.is_empty() {
            note!("\n== recorded without a manifest (installed by mods.py or an older version)");
            for entry in unmanifested {
                let present = entry
                    .files
                    .iter()
                    .filter(|file| self.game.root.join(file).is_file())
                    .count();
                note!(
                    "   {} {}: {present} of {} recorded file(s) present; reinstall it with this \
                    tool to give it a manifest",
                    entry.name,
                    entry.version,
                    entry.files.len()
                );
            }
        }

        note!("\n== files the tool merges for all installed mods");
        let legacy = self.legacy_merged(&record);
        if record.generated.is_empty() && legacy.is_empty() {
            note!("   none recorded");
        }
        for path in legacy {
            note!("   {path}: written without a hash (by mods.py); the next install records it");
        }
        for (path, file) in &record.generated {
            let state = match files.state(file)? {
                State::Unchanged => "as written",
                State::Changed => "CHANGED since the tool wrote it",
                State::Missing => "MISSING",
            };
            note!("   {path}: {state}");
        }

        let (unknown, unknown_ue4ss) = self.unaccounted(&owned);
        let paks = join(&self.game.root, &["AlchemyFactory", "Content", "Paks"]);
        let ue4ss_prefix = UE4SS_FOLDER.join("/");
        let folder_owner = |top: &str| folder_owner(&owned, top);
        note!("\n== not from any mod this tool knows");
        if unknown.is_empty() && unknown_ue4ss.is_empty() {
            note!("   nothing");
        }
        let game_packages =
            crate::unpack::package_names(&self.game.paks().join("AlchemyFactory-Windows.utoc"))
                .unwrap_or_default();
        // a container's files together: <stem> -> its extensions
        let mut containers: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for path in &unknown {
            let (stem, extension) = path.rsplit_once('.').unwrap_or((path, ""));
            containers
                .entry(stem.to_string())
                .or_default()
                .push(extension.to_string());
        }
        for (stem, extensions) in &containers {
            // what it holds: the IoStore container's packages, else a .pak's files
            let source = ["utoc", "pak"]
                .into_iter()
                .find(|extension| extensions.iter().any(|listed| listed == extension));
            let contents = source
                .map(|extension| {
                    container_contents(&self.game.root.join(format!("{stem}.{extension}")))
                })
                .unwrap_or_default();
            let mut replaces: Vec<&String> = contents
                .iter()
                .filter(|package| game_packages.contains(*package))
                .collect();
            replaces.sort();
            let shown: Vec<&str> = replaces
                .iter()
                .take(6)
                .map(|package| package.as_str())
                .collect();
            let listed: Vec<String> = extensions
                .iter()
                .map(|extension| format!(".{extension}"))
                .collect();
            note!(
                "   {stem} ({}){}",
                listed.join(" "),
                if contents.is_empty() {
                    String::new()
                } else {
                    format!(
                        ": replaces {}{}; adds {} other file(s)",
                        if shown.is_empty() {
                            "nothing".to_string()
                        } else {
                            shown.join(", ")
                        },
                        if replaces.len() > 6 { ", ..." } else { "" },
                        contents.len() - replaces.len()
                    )
                }
            );
        }
        for (top, found) in &unknown_ue4ss {
            match folder_owner(top) {
                Some(owner) => note!(
                    "   {ue4ss_prefix}/{top}/: {} file(s) {owner}'s manifest doesn't list ({})",
                    found.len(),
                    found.join(", ")
                ),
                None => note!(
                    "   {ue4ss_prefix}/{top}/ (a UE4SS mod or module, {} file(s))",
                    found.len()
                ),
            }
        }
        // A mod's zip holds paths from the game folder; extracted anywhere below it, they nest
        let misplaced: Vec<String> = unknown
            .iter()
            .filter_map(|path| {
                let below = path.strip_prefix("AlchemyFactory/Content/Paks/")?;
                below
                    .find("AlchemyFactory/")
                    .map(|at| format!("AlchemyFactory/Content/Paks/{}", &below[..at]))
            })
            .chain(
                unknown_ue4ss
                    .keys()
                    .filter(|top| *top == "AlchemyFactory")
                    .map(|_| format!("{ue4ss_prefix}/")),
            )
            .collect::<BTreeSet<String>>()
            .into_iter()
            .collect();
        for folder in &misplaced {
            note!(
                "   note: {folder}AlchemyFactory/ looks like a mod's zip extracted into {folder} \
                 instead of the game folder. The game may still load its containers, but not \
                 merged with the other mods' edits. Delete {folder}AlchemyFactory/, then extract the zip into the game folder, the one that holds \
                 AlchemyFactory/"
            );
        }

        // Two containers with the same asset: only one copy takes effect
        let mut by_asset: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for file in files_below(&paks) {
            if file.ends_with(".utoc") && file.contains('/') {
                for asset in container_contents(&paks.join(&file)) {
                    by_asset.entry(asset).or_default().push(file.clone());
                }
            }
        }
        let clashes: Vec<(&String, &Vec<String>)> = by_asset
            .iter()
            .filter(|(_, containers)| containers.len() > 1)
            .collect();
        note!("\n== assets in more than one mod container (only one copy takes effect)");
        if clashes.is_empty() {
            note!("   none");
        }
        for (asset, containers) in clashes {
            note!("   {asset}: {}", containers.join(", "));
        }

        if !added.is_empty() {
            note!(
                "\nadded to {RECORD} (installed from their zips): {}",
                added.join(", ")
            );
        }
        Ok(())
    }

    pub fn new_mod(&self, name: &str) -> Result<()> {
        if !is_name(name) || name == MERGED || name == MERGED_SAVE {
            bail!("a mod name is letters and digits, starting with a letter");
        }
        if let Some(existing) = self.local_folder(name) {
            bail!("{} already exists", display_path(&existing));
        }
        let folder = self.first_mods().join(name);
        if folder.exists() {
            bail!("{} already exists", display_path(&folder));
        }
        std::fs::create_dir_all(folder.join("edits"))?;
        let mut fields = json!({"name": name, "title": name, "version": "0.1.0", "description": "",
                                "author": "", "homepage": "", "team": "", "dependencies": []});
        // made for the game as it is now
        if let Some(build) = game_build(&self.game) {
            fields["game-build"] = json!(build);
        }
        fields["content"] = json!([]);
        fields["craft-types"] = json!({});
        fields["new-craft-types"] = json!([]);
        write_json(&folder.join("mod.json"), &fields)?;
        // every table in it starts with `_`, so it changes nothing until one is renamed
        std::fs::write(folder.join("edits").join("example.json"), EXAMPLE_EDITS)?;
        note!(
            "made {}: put table edits in edits/ (edits/example.json shows how), a UE4SS mod in \
             ue4ss/, and list cooked content (in /Game/Mods/{name}/ in the stub project) in \
             mod.json",
            display_path(&folder)
        );
        Ok(())
    }
}
