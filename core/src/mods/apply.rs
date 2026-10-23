//! Making the installed mods exactly a given set ([`Workspace::apply`]).

use super::*;

impl Workspace {
    /// Make the installed mods exactly `wanted`, all at once or not at all:
    /// - every wanted mod in the folders of mods is built and installed (again, if it's installed);
    /// - a wanted mod installed from elsewhere (its zip) stays as it is;
    /// - every other installed mod is uninstalled.
    ///
    /// Everything is built and checked before the game folder is touched: the mods' dependencies
    /// and incompatibilities as a set, the files each writes, and the merge (rows two mods add,
    /// functions two mods detour). Then the game folder is changed in one transaction, undone if any
    /// step fails. `force` is as for `install` and `uninstall`: delete files changed since they were
    /// installed and those of mods without a manifest, and replace changed merged files.
    pub fn apply(&self, wanted: &[String], force: bool) -> Result<Applied> {
        check_game_closed()?;
        let files = GameFiles::new(&self.game.root)?;
        if files.recover()? {
            warning!("a change to the game folder was interrupted last time; it has been undone");
        }
        let manifests = files.manifests()?;
        let mut record = self.record()?;
        // mods installed from their zips, which the record may not list yet
        for (name, manifest) in &manifests {
            if !record.mods.contains_key(name) {
                record
                    .mods
                    .insert(name.clone(), entry_from_manifest(name, manifest));
            }
        }
        let wanted: BTreeSet<&String> = wanted.iter().collect();
        let mut problems: Vec<(Vec<String>, String)> = vec![];
        let archives = self.archives();
        let mut from_archives: Vec<&Archive> = vec![];
        let mut to_build: IndexMap<String, Mod> = IndexMap::new();
        for name in &wanted {
            match self.local_folder(name) {
                Some(folder) => match Mod::load(&folder) {
                    Ok(found) if found.name != **name => problems.push((
                        vec![name.to_string()],
                        format!(
                            "{}: its mod.json names it {}; name its folder the same",
                            display_path(&folder),
                            found.name
                        ),
                    )),
                    Ok(found) => {
                        to_build.insert(found.name.clone(), found);
                    }
                    Err(error) => problems.push((vec![name.to_string()], format!("{error:#}"))),
                },
                None if record.mods.contains_key(*name) => {}
                None => match archives.get(*name) {
                    Some(archive) => from_archives.push(archive),
                    None => problems.push((
                        vec![name.to_string()],
                        format!(
                            "there's no mod {name} in the mods folders, and it isn't installed"
                        ),
                    )),
                },
            }
        }
        let removing: Vec<String> = record
            .mods
            .keys()
            .filter(|name| !wanted.contains(name))
            .cloned()
            .collect();
        for name in &removing {
            let has_manifest = manifests.iter().any(|(owner, _)| owner == name);
            let present = record.mods[name]
                .files
                .iter()
                .any(|file| self.game.root.join(file).is_file());
            if !has_manifest && present && !force {
                problems.push((
                    vec![name.clone()],
                    format!(
                        "{name} has no manifest (it was installed by mods.py or an older version), \
                         so nothing confirms its files are still its own: reinstall it first, or \
                         force deleting them"
                    ),
                ));
            }
        }
        if !problems.is_empty() {
            return Err(fail("can't apply", problems));
        }

        // built in load order, so each mod's dependencies (and their cooked assets) are built first
        let planned: IndexMap<String, Entry> = to_build
            .values()
            .map(|found| {
                let entry = Entry {
                    name: found.name.clone(),
                    version: found.version.clone(),
                    dependencies: found.dependencies.clone(),
                    ..Entry::default()
                };
                (found.name.clone(), entry)
            })
            .collect();
        let mut built: Vec<(PathBuf, Entry)> = vec![];
        for name in load_order(&planned)? {
            note!("\n=== {name}");
            match self.build_mod(&to_build[&name]) {
                Ok(result) => built.push(result),
                Err(error) => problems.push((vec![name.clone()], format!("{error:#}"))),
            }
        }
        if !problems.is_empty() {
            return Err(fail("couldn't build every mod", problems));
        }

        // the mods as they'll be, checked as a set
        let mut mods = record.mods.clone();
        for name in &removing {
            mods.shift_remove(name);
        }
        for (_, info) in &built {
            mods.insert(info.name.clone(), info.clone());
        }
        for archive in &from_archives {
            mods.insert(archive.manifest.name.clone(), archive.entry());
        }
        problems.extend(check_together(&mods)?);
        let ue4ss_prefix = format!("{}/", UE4SS_FOLDER.join("/"));
        let needing: Vec<String> = built
            .iter()
            .filter(|(_, info)| info.needs_ue4ss)
            .map(|(_, info)| info.name.clone())
            .chain(
                from_archives
                    .iter()
                    .filter(|archive| {
                        archive
                            .manifest
                            .files
                            .iter()
                            .any(|file| file.path.starts_with(&ue4ss_prefix))
                    })
                    .map(|archive| archive.manifest.name.clone()),
            )
            .collect();
        if !needing.is_empty() && !join(&self.game.root, UE4SS_FOLDER).is_dir() {
            let names: Vec<&String> = needing.iter().collect();
            problems.push((
                needing.clone(),
                format!(
                    "{} need(s) UE4SS, and it isn't installed in {}: install it (Setup in the \
                     GUI, or alchemists-tools ue4ss install)",
                    listed(&names),
                    display_path(&self.game.root)
                ),
            ));
        }
        // each file is free: the uninstalled mods' unchanged files are deleted first
        let mut freed = HashSet::new();
        for (_, manifest) in manifests
            .iter()
            .filter(|(owner, _)| removing.contains(owner))
        {
            for file in &manifest.files {
                if force || files.state(file)? == State::Unchanged {
                    freed.insert(file.path.clone());
                }
            }
        }
        let staying: Vec<(String, Manifest)> = manifests
            .iter()
            .filter(|(owner, _)| !removing.contains(owner))
            .cloned()
            .collect();
        let mut claimed: HashMap<String, String> = HashMap::new();
        let mut new_files = vec![];
        for (folder, info) in &built {
            let records = Self::file_records(folder, &info.files)?;
            for file in &records {
                if let Some(other) = claimed.insert(file.path.clone(), info.name.clone()) {
                    problems.push((
                        vec![other.clone(), info.name.clone()],
                        format!("{other} and {} both install {}", info.name, file.path),
                    ));
                }
            }
            let taken =
                Self::files_not_free(&files, &info.name, &records, &record, &staying, &freed)?;
            for problem in taken {
                problems.push((
                    vec![info.name.clone()],
                    format!(
                        "{}: its files would replace one that isn't its own: {problem}",
                        info.name
                    ),
                ));
            }
            new_files.push(records);
        }
        for archive in &from_archives {
            let name = &archive.manifest.name;
            for file in &archive.install_manifest().files {
                if let Some(other) = claimed.insert(file.path.clone(), name.clone()) {
                    problems.push((
                        vec![other.clone(), name.clone()],
                        format!("{other} and {name} both install {}", file.path),
                    ));
                }
            }
            let taken = Self::files_not_free(
                &files,
                name,
                &archive.install_manifest().files,
                &record,
                &staying,
                &freed,
            )?;
            for problem in taken {
                problems.push((
                    vec![name.clone()],
                    format!("{name}: its files would replace one that isn't its own: {problem}"),
                ));
            }
        }
        if !problems.is_empty() {
            return Err(fail("can't install these mods together", problems));
        }
        // a mod no folder of mods holds is kept as a zip there before it's uninstalled
        for name in &removing {
            if self.local_folder(name).is_none() {
                let manifest = manifests
                    .iter()
                    .find(|(owner, _)| owner == name)
                    .map(|(_, manifest)| manifest);
                if let Err(error) = self.keep_copy(&files, &record.mods[name], manifest, force) {
                    problems.push((vec![name.clone()], format!("{error:#}")));
                }
            }
        }
        if !problems.is_empty() {
            return Err(fail("can't uninstall every mod", problems));
        }
        // the saves, if the mods change: a save may use what a mod adds
        let changing = !removing.is_empty()
            || mods.iter().any(|(name, entry)| {
                record
                    .mods
                    .get(name)
                    .is_none_or(|old| old.version != entry.version)
            });
        let saves_backup = if changing {
            self.back_up_saves()?
        } else {
            None
        };

        // from here on, a failure puts the game folder back as it was
        let mut transaction = files.begin()?;
        let mut buildings = vec![];
        for name in &removing {
            let entry = &record.mods[name];
            buildings.extend(entry.new_buildings.iter().cloned());
            match manifests.iter().find(|(owner, _)| owner == name) {
                Some((_, manifest)) => {
                    let result = transaction.delete_listed(&manifest.files, force)?;
                    for path in &result.kept {
                        note!("   kept {path}: it has changed since {name} installed it");
                    }
                    transaction.delete(&GameFiles::manifest_path(name))?;
                }
                None => {
                    transaction.delete_unlisted(&entry.files)?;
                }
            }
            note!("uninstalling {name}");
        }
        record.mods = mods;
        self.merge(&mut record, force, &mut transaction)?;
        for ((folder, info), records) in built.iter().zip(new_files) {
            // its old files that the new build doesn't have
            let stale: Vec<FileRecord> = manifests
                .iter()
                .find(|(owner, _)| *owner == info.name)
                .map(|(_, manifest)| {
                    manifest
                        .files
                        .iter()
                        .filter(|old| !records.iter().any(|new| new.path == old.path))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            for file in &records {
                transaction.copy(&folder.join(&file.path), &file.path)?;
            }
            for kept in transaction.delete_listed(&stale, false)?.kept {
                note!(
                    "   kept {kept}: no longer part of {}, but changed since it was installed",
                    info.name
                );
            }
            self.write_manifest(
                &mut transaction,
                &Self::manifest_for(info, "alchemists-tools", records),
            )?;
        }
        for archive in &from_archives {
            let mut zip = zip::ZipArchive::new(std::fs::File::open(&archive.path)?)?;
            for file in &archive.install_manifest().files {
                let mut contents = vec![];
                std::io::Read::read_to_end(
                    &mut zip.by_name(&file.path).with_context(|| {
                        format!("{} has no {}", display_path(&archive.path), file.path)
                    })?,
                    &mut contents,
                )?;
                transaction.write(&file.path, &contents)?;
                if transaction.files().record(&file.path)? != *file {
                    bail!(
                        "{}: {} isn't as its manifest says (is the zip damaged?)",
                        display_path(&archive.path),
                        file.path
                    );
                }
            }
            self.write_manifest(&mut transaction, &archive.install_manifest())?;
            note!(
                "installing {} from {}",
                archive.manifest.name,
                display_path(&archive.path)
            );
        }
        self.save_record(&mut transaction, &mut record)?;
        transaction.commit()?;

        let installed: Vec<String> = built
            .iter()
            .map(|(_, info)| info.name.clone())
            .chain(
                from_archives
                    .iter()
                    .map(|archive| archive.manifest.name.clone()),
            )
            .collect();
        note!(
            "\ninstalled {}; uninstalled {} (load order: {}); restart the game to load them",
            if installed.is_empty() {
                "none".to_string()
            } else {
                installed.join(", ")
            },
            if removing.is_empty() {
                "none".to_string()
            } else {
                removing.join(", ")
            },
            if record.load_order.is_empty() {
                "none".to_string()
            } else {
                record.load_order.join(", ")
            }
        );
        if !buildings.is_empty() {
            note!(
                "reminder: saves with a placed {} will crash without the mod that adds it; \
                 reinstall it to deconstruct them",
                buildings.join(", ")
            );
        }
        let unknown = self.unowned_containers(&files, &record)?;
        if !unknown.is_empty() {
            warning!(
                "~mods has containers no manifest lists ({}); if one replaces a table that \
                 installed mods edit, only one takes effect (alchemists-tools mod scan shows what \
                 they change)",
                unknown.join(", ")
            );
        }
        Ok(Applied {
            installed,
            uninstalled: removing,
            load_order: record.load_order,
            saves_backup,
        })
    }
}

/// What [`Workspace::apply`] did.
#[derive(Clone, Debug, Default)]
pub struct Applied {
    pub installed: Vec<String>,
    pub uninstalled: Vec<String>,
    pub load_order: Vec<String>,
    /// The backup of the saves made first, if the installed mods changed and there were saves.
    pub saves_backup: Option<PathBuf>,
}

/// What's wrong with a set of mods installed together, each with the mods it's about: a required
/// mod missing or the wrong version, two incompatible mods, two craft types with one number.
fn check_together(mods: &IndexMap<String, Entry>) -> Result<Vec<(Vec<String>, String)>> {
    let mut problems = vec![];
    for (name, entry) in mods {
        for dependency in entry.requires()? {
            let about = vec![name.clone(), dependency.name.clone()];
            match (dependency.kind, mods.get(&dependency.name)) {
                (Kind::Incompatible, Some(_)) => problems.push((
                    about,
                    format!("{name} doesn't work with {}", dependency.name),
                )),
                (kind, None) if kind.is_required() => problems.push((
                    vec![name.clone()],
                    format!(
                        "{name} needs {}, which isn't among the mods to install",
                        requirement_text(&dependency)
                    ),
                )),
                (Kind::Recommended, None) => {
                    note!("note: {name} recommends {}", requirement_text(&dependency))
                }
                (_, None) => {}
                (_, Some(other)) => {
                    if !satisfies(other.version()?, &dependency) {
                        problems.push((
                            about,
                            format!(
                                "{name} needs {}, but {} is {}",
                                requirement_text(&dependency),
                                dependency.name,
                                other.version
                            ),
                        ));
                    }
                }
            }
        }
    }
    let mut numbers: HashMap<u8, (String, String)> = HashMap::new(); // number -> (key, mod)
    for (name, entry) in mods {
        for (key, number) in &entry.craft_type_numbers {
            match numbers.get(number) {
                Some((first_key, first_mod)) if first_key != key => problems.push((
                    vec![first_mod.clone(), name.clone()],
                    format!(
                        "craft type {number} is both {first_key} and {key} (mods made with \
                         different allocations.json files; one of them needs a new number)"
                    ),
                )),
                Some(_) => {}
                None => {
                    numbers.insert(*number, (key.clone(), name.clone()));
                }
            }
        }
    }
    Ok(problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, dependencies: &[&str]) -> Entry {
        Entry {
            name: name.into(),
            version: "0.1.0".into(),
            dependencies: dependencies
                .iter()
                .map(|dependency| dependency.to_string())
                .collect(),
            ..Entry::default()
        }
    }

    #[test]
    fn mods_are_checked_as_a_set() {
        let mods: IndexMap<String, Entry> = [
            entry("A", &["B >= 0.2.0"]),
            entry("B", &[]),
            entry("C", &["! B", "D"]),
        ]
        .into_iter()
        .map(|entry| (entry.name.clone(), entry))
        .collect();
        let problems = check_together(&mods).unwrap();
        let texts: Vec<&str> = problems.iter().map(|(_, text)| text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "A needs B >= 0.2.0, but B is 0.1.0",
                "C doesn't work with B",
                "C needs D, which isn't among the mods to install"
            ]
        );
        assert_eq!(problems[1].0, ["C", "B"]);
    }
}
