# Contributing

## Building

```sh
cargo build --release    # -> target/release/alchemists-tools
cargo test
cargo test --release -- --ignored   # also the tests that need the installed game (core/tests/)
cargo clippy
cargo fmt                # default rustfmt settings; keep lines within 100 columns
```

retoc and repak are git dependencies, pinned in `Cargo.lock` (seeded from retoc's own lockfile, as
retoc doesn't build against the latest jmap). Keep `Cargo.lock` committed, and install with `--locked`.

## Layout

| Directory     | What                                                      |
|---------------|-----------------------------------------------------------|
| `core/`       | Reading and writing game files, edits, mods               |
| `cli/`        | The `alchemists-tools` command                            |
| `ue4ss/`      | Lua built into the program: the probe template and the AlchemistsLibrary mod |
| `calculator/` | Production and demand calculations *(not yet ported)*     |
| `gui/`        | Desktop interface *(not yet written)*                     |

In `core/src/`:

| Module | What |
|---|---|
| `unpack`, `reader`, `usmap`, `schema`, `datatable`, `text`, `extract` | Reading: containers, packages, layouts, tables, texts |
| `write` | Writing DataTables back (below, "How writing works") |
| `edits` | Edit files -> edited tables, with every check |
| `package` | Writing and checking IoStore containers |
| `blueprint`, `content` | Reading cooked Blueprints; packaging cooked content |
| `save` | Save files, and edits to the template save |
| `mods` | Mod folders: build, install (merging), uninstall, zip, scan |
| `files` | Manifests, and the checks before a file in the game folder is deleted or replaced |
| `probe` | UE4SS probe mods |
| `stub` | Setting up the stub Unreal project |
| `update` | The steps after a game update |
| `facts` | Game knowledge that isn't in the tables (which machine runs which craft type) |

## How extraction works

Everything happens in memory; only the output is written.

1. **Unpack** (`core/src/unpack.rs`): [retoc](https://github.com/trumank/retoc) converts every
   package under `AlchemyFactory/Content/` from IoStore (`.utoc`/`.ucas`) to legacy
   `.uasset`/`.uexp` bytes. Only those whose main object is a DataTable, curve or `StringTable`
   are kept (the classes are listed in `extract.rs`); a package that fails to convert is skipped
   with a warning. [repak](https://github.com/trumank/repak)
   reads the English `Game.locres` from the `.pak`.
2. **Schemas** (`core/src/usmap.rs`, `schema.rs`): cooked assets use unversioned property
   serialization, which stores values without field names or types. Every layout (the table and
   curve classes, each table's row struct, and the structs inside them) comes from the `.usmap`.
   The ones used are saved as `schemas.json`:
   ```json
   "CustomerConfig": {"super": "TableRowBase", "properties": [
     {"name": "UnlockLevel", "type": "Int", "array_dimension": 1},
     {"name": "BasePr", "type": "Double", "array_dimension": 1}, ...
   ```
3. **Decode** (`core/src/datatable.rs`, `text.rs`): each object is read using its class's schema
   (a DataTable's own properties give its `RowStruct`, then each row is read with that), and
   enums become names (`3` → `"Remedies"`). Decoding must end exactly at the 4-byte package tag,
   which catches a wrong layout. Texts are looked up in the string tables and `Game.locres`.
   References to other objects, stored as indices into the package's import/export tables, become
   full object paths by following each object's outer chain
   (`-651` → `/Game/Arts/UI/Ingredients/T_UI_PortalSigil.T_UI_PortalSigil`).
   Each table and curve is written to `data/<its path under Content>.json`. One that can't be
   decoded, e.g. because its struct isn't in the `.usmap`, is skipped with a warning (two Ultra
   Dynamic Sky editor tables, currently).

Things learnt the hard way:
- Unversioned property indices number a struct's **own** properties first, then its super's.
- `Quat`, `Vector`, `Color`, `RichCurveKey` and similar structs are stored as their fields in
  order, with no property header (`NATIVE_STRUCTS` in `datatable.rs`; the fields still come from
  the `.usmap`).
- Unversioned serialization leaves out properties equal to the struct's default, which comes from
  the game's C++ constructor and isn't in the `.usmap`, so they're written as `null`. Only the
  header's zero mask means a real zero.
- A fixed-size array property (e.g. `CurveLinearColor`'s `FloatCurves[4]`) takes one property
  index per element.
- A `CompositeDataTable` (e.g. `DT_SkillMerge`) has its own properties before DataTable's.
- Cooked packages don't record their engine version; `unpack::package_version()` sets it.
- Property names follow the `.usmap`'s spelling, which can differ in case from the game's
  source (`position` vs `Position`).

## How writing works

`core/src/write.rs` writes DataTables back, for mods. The aim is that everything a mod doesn't
change keeps the cooker's exact bytes:

1. **Decode with spans.** The decoder records where each value was (`Spans` in
   `datatable.rs`): a path such as `Processor/GridConfigList/0/position` -> (start, end). A
   property the header marks as zero has an empty span; one the file doesn't store has none. Texts
   are decoded untranslated (string-table entries and keys), so they can be written back.
2. **Encode.** Only changed rows are encoded, and inside them any value still equal to the original
   is copied from its span. In a changed struct, an unchanged property keeps the cooker's choice:
   left out, marked zero, or stored. A changed one is stored unless it's `null`, and marked zero if
   it's zero. A new row follows an existing row's layout (`Table::add_row(name, row, like)`), so a
   copy keeps its original's bytes wherever it's the same.
3. **Patch the header.** Missing names are inserted after the ones export data uses, and new
   imports are appended, each with its class from the field it's in (or written with it, as the
   editor's *Copy Reference* gives it). A preload dependency is added on each, and the export's size
   is fixed. The summary is patched in place.

The tests in `core/tests/game_files.rs` check this against the installed game: every table saves
unchanged byte for byte, and **re-encoding every row from its decoded values gives the game's own
bytes**. That tests every encoder against the cooker.

Things learnt the hard way, writing:
- retoc's `FLegacyPackageHeader::serialize` doesn't reproduce the headers its own conversion
  writes (all differ from byte 44 on), so the header is patched rather than rewritten.
- The cooker writes a localisable text's empty namespace as just its terminator (length 1), not as
  an empty string (length 0).
- Some texts have flags (e.g. `8`), which the untranslated decode keeps as `"flags"` so they can be
  written back.

## Rules

- **Nothing derived from the game exe.** Struct layouts come from the `.usmap`. Knowledge of how
  the engine itself serializes things, from the Unreal Engine source (e.g. which structs are
  stored natively), is fine, as are game mechanics worked out from the exe.
- **Names are full words**, with no abbreviations: `position`, not `pos`; `property`, not `prop`.
  This applies to variables, functions, types and the keys of JSON the tools write. The exceptions:
  - loop indices (`i`, `j`, `k`);
  - common abbreviations (`cli`, `gui`, `json`, `id`);
  - Rust and Unreal type names (`u8`, `i32`, `fstring`, `fname`);
  - file formats (`usmap`, `uasset`, `uexp`, `locres`, `pak`).
- **Game data is never committed.** It's extracted on each user's machine; `data/` is ignored.
