//! Merging the installed mods' table and template-save edits into `zzz_ModTables_P` and
//! `zzz_ModSave_P`.

use super::*;

impl Workspace {
    fn report_clashes(&self, order: &[String], mods: &IndexMap<String, Entry>) -> Result<()> {
        // the game's tables (and the .usmap to read them) only if a mod edits tables
        let edits_tables = order
            .iter()
            .any(|name| !mods[name].edits.is_empty() || !mods[name].optional_edits.is_empty());
        let game_tables = if edits_tables {
            Some((self.assets()?, self.schemas()?))
        } else {
            None
        };
        let mut game_rows: HashMap<String, HashSet<String>> = HashMap::new();
        let mut is_game_row = |table: &str, row: &str| -> bool {
            let Some((assets, schemas)) = game_tables else {
                return false;
            };
            game_rows
                .entry(table.to_string())
                .or_insert_with(|| {
                    let matches = assets.find(table);
                    let rows = matches
                        .first()
                        .and_then(|path| assets.asset(path).ok())
                        .and_then(|asset| read_datatable(asset, schemas, None).ok());
                    rows.and_then(|decoded| {
                        decoded["rows"]
                            .as_object()
                            .map(|rows| rows.keys().cloned().collect())
                    })
                    .unwrap_or_default()
                })
                .contains(row)
        };
        let present: HashSet<String> = order.iter().cloned().collect();
        let related: HashMap<&String, HashSet<String>> = order
            .iter()
            .map(|name| Ok((name, ancestors(name, mods)?)))
            .collect::<Result<_>>()?;
        let mut set_by: HashMap<Vec<String>, (String, Value)> = HashMap::new();
        let mut made_by: HashMap<(String, String, String), String> = HashMap::new();
        let mut problems = vec![];
        for name in order {
            for source in table_edits_of(&mods[name], &present) {
                for (table, rows) in source.contents.as_object().into_iter().flatten() {
                    if table.starts_with('_') {
                        continue;
                    }
                    for (row, fields) in rows.as_object().into_iter().flatten() {
                        if row.starts_with('_') || !fields.is_object() {
                            continue;
                        }
                        let mut claims = vec![];
                        if !is_game_row(table, row) {
                            claims.push((
                                ("new row".to_string(), table.clone(), row.clone()),
                                "add the row",
                            ));
                        }
                        if let Some(slot) = fields.get("_slot").and_then(Value::as_str) {
                            claims.push((
                                ("slot".to_string(), table.clone(), slot.to_string()),
                                "put a recipe in the unused row",
                            ));
                        }
                        for (key, what) in claims {
                            let first = made_by
                                .entry(key.clone())
                                .or_insert_with(|| name.clone())
                                .clone();
                            if first != *name && !related[name].contains(&first) {
                                problems.push((
                                    vec![first.clone(), name.clone()],
                                    format!("{first} and {name} both {what} {table}.{}", key.2),
                                ));
                            }
                        }
                        let mut values_set = vec![];
                        leaf_values(fields, vec![table.clone(), row.clone()], &mut values_set);
                        for (path, value) in values_set {
                            if let Some((first, old)) = set_by.get(&path) {
                                if first != name && *old != value && !related[name].contains(first)
                                {
                                    report::warning_about(
                                        &[first.clone(), name.clone()],
                                        format!(
                                            "{first} and {name} both set {}; {name}'s value \
                                        is used (it loads later). \
                                         Make one depend on the other to decide.",
                                            path.join(".")
                                        ),
                                    );
                                }
                            }
                            set_by.insert(path, (name.clone(), value));
                        }
                    }
                }
            }
        }
        // Game functions: two detours of one function can't both work, and a hook beside a
        // detour, or another mod's hook, may not see what it expects
        let mut detoured_by: BTreeMap<&String, Vec<&String>> = BTreeMap::new();
        let mut hooked_by: BTreeMap<&String, Vec<&String>> = BTreeMap::new();
        for name in order {
            for function in &mods[name].detours {
                detoured_by.entry(function).or_default().push(name);
            }
            for function in mods[name]
                .hooks
                .iter()
                .filter(|function| !is_engine_function(function))
            {
                hooked_by.entry(function).or_default().push(name);
            }
        }
        for (function, names) in &detoured_by {
            if names.len() > 1 {
                problems.push((
                    names.iter().map(|name| name.to_string()).collect(),
                    format!(
                        "{} each detour {function}, and only one mod can replace a game \
                         function's code",
                        listed(names)
                    ),
                ));
            }
            let hooking: Vec<&String> = hooked_by
                .get(function)
                .into_iter()
                .flatten()
                .filter(|name| !names.contains(name))
                .copied()
                .collect();
            if !hooking.is_empty() {
                report::warning_about(
                    &names
                        .iter()
                        .chain(&hooking)
                        .map(|name| name.to_string())
                        .collect::<Vec<_>>(),
                    format!(
                        "{} {} {function}, which {} {}: check the hooks still see what they \
                         expect",
                        listed(&hooking),
                        if hooking.len() == 1 { "hooks" } else { "hook" },
                        listed(names),
                        if names.len() == 1 {
                            "detours"
                        } else {
                            "detour"
                        }
                    ),
                );
            }
        }
        for (function, names) in &hooked_by {
            let unrelated = names.iter().any(|name| {
                names.iter().any(|other| {
                    other != name
                        && !related[*name].contains(*other)
                        && !related[*other].contains(*name)
                })
            });
            if unrelated && !detoured_by.contains_key(function) {
                report::warning_about(
                    &names
                        .iter()
                        .map(|name| name.to_string())
                        .collect::<Vec<_>>(),
                    format!(
                        "{} each hook {function}: check they don't undo each other's changes",
                        listed(names)
                    ),
                );
            }
        }
        if !problems.is_empty() {
            return Err(fail("mods clash", problems));
        }
        Ok(())
    }

    /// Build the merged template save into a work folder; None when no mod edits it.
    fn merge_save(
        &self,
        order: &[String],
        mods: &IndexMap<String, Entry>,
    ) -> Result<Option<PathBuf>> {
        let mut edits = vec![];
        let mut set_by: HashMap<SaveEditKey, (String, Value)> = HashMap::new();
        for name in order {
            let related = ancestors(name, mods)?;
            for edit in &mods[name].save_edits {
                let key = (
                    edit.property.clone(),
                    edit.occurrence,
                    edit.after.clone(),
                    edit.part.clone(),
                );
                if let Some((first, value)) = set_by.get(&key) {
                    if first != name && *value != edit.value && !related.contains(first) {
                        report::warning_about(
                            &[first.clone(), name.clone()],
                            format!(
                                "{first} and {name} both change {} in the template save; \
                                {name}'s value is used (it loads later)",
                                edit.property
                            ),
                        );
                    }
                }
                set_by.insert(key, (name.clone(), edit.value.clone()));
                edits.push(edit.clone());
            }
        }
        if edits.is_empty() {
            return Ok(None);
        }
        let with_edits: Vec<&str> = order
            .iter()
            .filter(|name| !mods[*name].save_edits.is_empty())
            .map(|name| name.as_str())
            .collect();
        note!(
            "== merging the template-save edits of {}",
            with_edits.join(", ")
        );
        let work = self.build.join(".mergesave");
        let _ = std::fs::remove_dir_all(&work);
        self.write_save(&edits, &work, &save_container(MERGED_SAVE))?;
        Ok(Some(work))
    }

    /// Check that the tool's merged files `~mods/<base>.*` are exactly what it last wrote (as
    /// recorded), before any is replaced; a record from before the tool kept hashes accepts the
    /// merged files it finds once. With `force`, a changed or unrecorded one is replaced anyway.
    fn check_generated(
        &self,
        files: &GameFiles,
        record: &Record,
        base: &str,
        force: bool,
    ) -> Result<()> {
        let legacy = record.generated.is_empty();
        let prefix = PAK_FOLDER.join("/");
        for extension in ["pak", "utoc", "ucas"] {
            let relative = format!("{prefix}/{base}.{extension}");
            if !files.resolve(&relative)?.exists() {
                continue;
            }
            let problem = match record.generated.get(&relative) {
                Some(recorded) if files.state(recorded)? == State::Unchanged => continue,
                Some(_) => "has changed since the tool wrote it",
                None if legacy => continue,
                None => "wasn't written by the tool (it has no record of it)",
            };
            if force {
                warning!("{relative} {problem}; replacing it anyway (--force)");
                continue;
            }
            bail!(
                "{relative} {problem}, so it isn't replaced; move it away (it may be another \
                 mod's) and run this again, or pass --force to replace it"
            );
        }
        Ok(())
    }

    /// Replace the tool's merged files `~mods/<base>.*` with those in `work` (or remove them if
    /// None), once [`Self::check_generated`] has passed. Returns whether any were removed.
    fn replace_generated(
        &self,
        transaction: &mut Transaction,
        record: &mut Record,
        base: &str,
        work: Option<&Path>,
    ) -> Result<bool> {
        let prefix = PAK_FOLDER.join("/");
        let mut removed = false;
        for extension in ["pak", "utoc", "ucas"] {
            let relative = format!("{prefix}/{base}.{extension}");
            removed |= transaction.delete(&relative)?;
            record.generated.shift_remove(&relative);
        }
        if let Some(work) = work {
            for file in files_below(work) {
                let relative = format!("{prefix}/{file}");
                transaction.copy(&work.join(&file), &relative)?;
                record
                    .generated
                    .insert(relative.clone(), transaction.files().record(&relative)?);
            }
        }
        Ok(removed)
    }

    /// Rebuild `zzz_ModTables_P` (and `zzz_ModSave_P`) from every installed mod's edits, as part of
    /// a change to the game folder.
    pub(super) fn merge(
        &self,
        record: &mut Record,
        force: bool,
        transaction: &mut Transaction,
    ) -> Result<()> {
        let order = load_order(&record.mods)?;
        for name in &order {
            for caution in &record.mods[name].cautions {
                report::warning_about(std::slice::from_ref(name), caution.clone());
            }
        }
        self.report_clashes(&order, &record.mods)?;
        let present: HashSet<String> = order.iter().cloned().collect();
        let sources: Vec<edits::Source> = order
            .iter()
            .flat_map(|name| table_edits_of(&record.mods[name], &present))
            .collect();
        // which mods edit each row, to say which mods a warning about one is about
        let mut row_editors: HashMap<(String, String), Vec<String>> = HashMap::new();
        for name in &order {
            for source in table_edits_of(&record.mods[name], &present) {
                for (table, rows) in source.contents.as_object().into_iter().flatten() {
                    for row in rows.as_object().into_iter().flatten().map(|(row, _)| row) {
                        let editors = row_editors.entry((table.clone(), row.clone())).or_default();
                        if !editors.contains(name) {
                            editors.push(name.clone());
                        }
                    }
                }
            }
        }
        record.load_order = order.clone();
        let files = transaction.files();
        let base = data_container(MERGED);
        let save_base = save_container(MERGED_SAVE);
        // every merged file is checked before any is touched, and both are built before either
        // is replaced, so a refusal or a failed build leaves the game folder as it was
        self.check_generated(files, record, &base, force)?;
        self.check_generated(files, record, &save_base, force)?;
        let save_work = self.merge_save(&order, &record.mods)?;
        let mut written = None;
        if !sources.is_empty() {
            // build first: if it fails, the installed container stays as it was
            note!("== merging the table edits of {}", order.join(", "));
            let result = edits::build(&self.game_for_edits()?, &sources)?;
            for warning in &result.warnings {
                // "<table>.<row>: ..."
                let editors = warning
                    .split_once(':')
                    .and_then(|(place, _)| place.split_once('.'))
                    .and_then(|(table, row)| {
                        row_editors.get(&(table.to_string(), row.to_string()))
                    });
                report::warning_about(editors.map(Vec::as_slice).unwrap_or(&[]), warning.clone());
            }
            let work = self.build.join(".mergework");
            let _ = std::fs::remove_dir_all(&work);
            let utoc = work.join(format!("{base}.utoc"));
            if !result.tables.is_empty() {
                write_container(
                    &utoc,
                    &result.tables,
                    retoc::version::EngineVersion::UE5_6,
                    script_objects(&self.game)?,
                )?;
                check_container(&utoc, &result.tables, &self.game)?;
                written = Some(work);
            }
        }
        if self.replace_generated(transaction, record, &save_base, save_work.as_deref())?
            && save_work.is_none()
        {
            note!("no installed mod edits the template save any more: removed the merged one");
        }
        if let Some(work) = save_work {
            let _ = std::fs::remove_dir_all(&work);
        }
        let removed = self.replace_generated(transaction, record, &base, written.as_deref())?;
        match written {
            Some(work) => drop(std::fs::remove_dir_all(&work)),
            None if removed => {
                note!("no installed mod edits tables any more: removed the merged data container")
            }
            None => {}
        }
        Ok(())
    }

    /// Re-merge the installed mods' edits against the game's current tables (after a game update).
    pub fn remerge(&self) -> Result<()> {
        let mut record = self.record()?;
        if record.mods.is_empty() {
            return Ok(());
        }
        let names: Vec<&str> = record.mods.keys().map(|name| name.as_str()).collect();
        note!(
            "   re-merging the installed mods ({}) against the new tables",
            names.join(", ")
        );
        let files = GameFiles::new(&self.game.root)?;
        let mut transaction = files.begin()?;
        self.merge(&mut record, false, &mut transaction)?;
        self.save_record(&mut transaction, &record)?;
        transaction.commit()
    }
}
