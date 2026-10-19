//! Game knowledge that isn't in the data tables.

/// Which machines (DT_Buildings rows) run each craft type, the main one first. A machine's craft
/// type is set in its Blueprint, not in a table, so this is matched by name and checked against
/// the game's wiki.
pub const CRAFT_MACHINES: &[(&str, &[&str])] = &[
    ("TableSaw", &["TableSaw"]),
    ("StoneCrusher", &["StoneCrusher"]),
    ("Smelter", &["IronSmelter"]),
    ("Plant", &["SeedPlot", "AutoNursery", "WorldTreeNursery"]),
    ("Grind", &["Grinder", "EnhancedGrinder"]),
    ("Extract", &["Extractor", "ThermalExtractor"]),
    ("Calcinate", &["Crucible", "StackableCrucible"]),
    ("Refine", &["Refiner"]),
    ("Fire", &["Kiln"]),
    ("Process", &["Processor"]),
    ("Assemble", &["Assembler"]),
    ("Blend", &["Blender"]),
    ("AdBlend", &["AdvancedBlender"]),
    ("Distill", &["Alembic"]),
    ("AdDistill", &["AdvancedAlembic"]),
    ("Athanor", &["Athanor"]),
    ("AdAthanor", &["AdvancedAthanor"]),
    ("OmniCraft", &["Omni-Machine"]),
    ("Shaper", &["Shaper"]),
    ("AdShaper", &["AdvancedShaper"]),
    ("AtShaper", &["AetherShaper"]),
    ("Paradox", &["ParadoxCrucible"]),
    ("Cauldron", &["Cauldron", "AdvancedCauldron"]),
    ("AdProcess", &["ArcaneProcessor"]),
    ("LiquidTap", &["BrewBarrel"]),
];

/// The machines that run a craft type.
pub fn machines_for(craft_type: &str) -> &'static [&'static str] {
    CRAFT_MACHINES
        .iter()
        .find(|(name, _)| *name == craft_type)
        .map(|(_, machines)| *machines)
        .unwrap_or(&[])
}
