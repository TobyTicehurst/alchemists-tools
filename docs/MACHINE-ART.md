# Making the art for a custom machine

How to give a machine its own model and build-menu icon, from reference to in-game. It continues
`docs/examples/blueprints.md`, which builds `BP_MyMachine` with the Processor's model through stand-ins.
Parts that the tooling doesn't support yet are marked as such. `docs/examples/cooked-content.md` is the
short version, with what the tools check.

## Tools

| Job | Tool | Why |
|---|---|---|
| Look at and export the game's own models and icons | **FModel** (already set up) | exports meshes (glTF, or `.psk`) and textures (PNG) to use as references |
| Model, unwrap, animate the moving parts, render | **Blender** (free) | the standard free 3D tool; exports FBX that Unreal imports directly |
| Paint textures | **Blender**'s texture paint, or **Substance 3D Painter** (paid) | Blender is enough for this game's simple painted style; Substance is faster for complex materials |
| Paint the icon | **Krita** (free), or Photoshop | the icons are painted, not rendered; Krita is a free painting program |
| Import, set up materials, place in the Blueprint, cook | **Unreal Editor 5.7.4** (the stub project) | the only way to make cooked assets the game can load |

For FModel's `.psk` exports, Blender needs an add-on: the *PSK/PSA importer* by DarklightGames.
Exporting as glTF needs none.

## 1. Study the game's own models

1. In FModel, open `AlchemyFactory/Content/Arts/Mesh/Facilities/`, and export `SM_Processor_1` (body),
   `SM_Processor_2` and `SM_Processor_3` (gears), and the textures their materials use. Use glTF, set in
   FModel's settings under model export format.
2. Import them into Blender (*File → Import → glTF*). Check and note:
   - **Size.** Unreal works in centimetres. The Processor fills a 2 × 2 grid footprint (its
     `DT_Buildings` row), so its bounds give the grid cell size. Measure it, and add it to this guide.
   - **Origin.** Where the mesh's pivot sits relative to the footprint (a corner, or the centre), and
     which way is front. The Blueprint turns the body 180° around Z, and scales it to 0.99.
   - **Detail.** The triangle count (Blender's statistics overlay), texture sizes, and how much is
     modelled versus painted. Aim for the same, so the machine fits in.
   - **Moving parts** are separate meshes with their pivot at the axle, like the gears here.
3. In FModel, also open `/Game/Arts/Material/Facilities/` to see which materials the machines use and
   their parameters (textures, colours). Step 4 needs them.

## 2. Model in Blender

1. **Units:** *Scene Properties → Units*: Unit System *Metric*, Unit Scale **0.01**, Length
   *Centimeters*. One Blender unit is then one Unreal unit.
2. Model the body, using the imported Processor as a size reference. Keep it in the game's style:
   chunky shapes, bevelled edges, simple painted detail.
3. Make **each moving part a separate object** (gears, a crank, a flywheel). Set each one's origin at
   its axle (*Object → Set Origin*), and keep its rotation at zero.
4. **Collision.** In game, clicking and building use the meshes' collision, with the profiles
   `FurnitureNoBuild` (body) and `Furniture` (gears). Either:
   - let Unreal make simple collision on import (enough for a box-like machine); or
   - model simple convex shapes named `UCX_<mesh name>_01`, `_02`, …, which Unreal uses as collision.
5. **UV-unwrap** each mesh (*UV → Smart UV Project* is a fine start). Unreal can make lightmap UVs
   itself.
6. Apply transforms (*Object → Apply → All Transforms*) before exporting.

## 3. Textures

- Paint them in Blender (*Texture Paint*), or bake from a high-detail model. Match the game's sizes
  (measured in step 1; probably 1024 or 2048).
- Export them as PNG: a base colour, plus a normal map and a packed roughness/metal map if the game's
  materials use them (step 1, point 3).

## 4. Materials: the important choice

A new material needs its shaders compiled and shipped with the mod. **The packaging script doesn't do
that yet** (it would need the cook's shader library, `ShaderArchive-*.ushaderbytecode`). Two ways
round it, best first:

1. **A material instance of a game material, through a stand-in.** It ships no shaders.
   1. Find the game's parent material that its machine material instances use (FModel, step 1).
   2. In the stub, make a stand-in **Material** at that exact path and name, with texture and scalar
      parameters of the **same names** as the real one.
   3. Make your own **Material Instance** of it in `Content/Mods/MyMachine/Materials/`, with your
      textures in its texture parameters.

   In game, your instance gets the real parent material and its shaders. The parameter names must
   match exactly, or the textures won't show. Like all stand-ins, the stand-in parent must never be
   packaged. This is untested: an instance that overrides *static* switches compiles its own shaders,
   so override only textures, colours and numbers.
2. **Your own material.** It's simplest to make, but needs the packaging to ship shaders, which it doesn't yet.

## 5. Import into the stub

1. *File → Export → FBX* from Blender, one file per mesh or one file for all. In the export options:
   - *Selected Objects* on;
   - *Apply Scalings*: *FBX Units Scale*;
   - forward and up axes left at the defaults.
2. In the editor, make `Content/Mods/MyMachine/Meshes/`, and drag the FBX files in. On the import
   dialog:
   - *Generate Missing Collision* on, unless you modelled `UCX_` shapes;
   - *Combine Meshes* off, so each moving part stays its own mesh;
   - Nanite off, unless the game's meshes use it (step 1).
3. Name them `SM_MyMachine_Body`, `SM_MyMachine_Gear`, and so on.
4. Drag the PNGs into `Content/Mods/MyMachine/Textures/`. For normal maps, check *Compression Settings:
   Normalmap*; for the packed map, turn *sRGB* off.
5. Make the material instance (step 4), and assign it to each mesh's material slot.
6. Check the size against the Processor: place both in the level, side by side.

## 6. Put it in the Blueprint

In `BP_MyMachine`:
- set each mesh component's *Static Mesh* to yours;
- add or remove gear components to match your moving parts, each a child of the body;
- keep the body's `FixedMesh` tag and both collision presets.

Then compile, save and cook. The packaging script stages the whole `/Game/Mods/MyMachine` folder (meshes,
textures, material instances), and its check shows every reference resolving. A build that's still
pointing at a stand-in shows up there as the stand-in's game path.

Animation is a separate job. The game turns its gears with the material `MI_AdvancedAthanorRotator`
and the construction script's `RegisterGpuAnimationMesh` call, plus a Tick that turns them
(`docs/examples/blueprints.md`). Recreating that needs the game Blueprint's logic read first.

## 7. The build-menu icon

- **What the game uses:** `DisplayIcon` in the building's `DT_Buildings` row, e.g.
  `/Game/Arts/UI/Furnitures/T_UI_Processor`. The cooked file's size matches a **512 × 512 texture, stored
  uncompressed (BGRA) with mipmaps**; confirm it in FModel.
- **Style:** a painted, slightly angled view on a transparent background. Export a few of the game's
  icons from FModel and copy their angle, outline and lighting.
- **Making it:**
  1. Render your model in Blender at that angle as a base: an orthographic camera, transparent
     background (*Film → Transparent*).
  2. Paint over the render in Krita in the game's style.
  3. Export a 512 × 512 PNG with alpha.
- **Import:** into `Content/Mods/MyMachine/Textures/T_UI_MyMachine`. Set *Compression Settings* to
  *UserInterface2D* (or match the game's: uncompressed), *Texture Group* to *UI*, and keep *sRGB* on.
- **Using it:** in the mod's edit file, set the row's
  `"DisplayIcon": "/Game/Mods/MyMachine/Textures/T_UI_MyMachine.T_UI_MyMachine"`. The build adds the
  reference, and checks that the cooked texture is packaged with the mod. Not yet tried in game: the
  first icon is the test of packaging a cooked texture (its pixels are in a `.ubulk` file, unlike a
  Blueprint).

The small machine glyph in research tooltips isn't from any table (`docs/FIELDS.md`, "What isn't in any table"), so it can't be changed
yet.

## Checklist

- [ ] Units at 0.01 / centimetres; sized against the real Processor.
- [ ] Moving parts separate, pivots at their axles, rotations zero.
- [ ] Collision present; body `FurnitureNoBuild`, moving parts `Furniture`.
- [ ] Materials are instances of a game material through a stand-in (or wait for shader packaging).
- [ ] Everything new is under `Content/Mods/MyMachine/`; nothing under `Content/Arts/` gets packaged.
- [ ] The packaging check lists your meshes and material, and passes.
- [ ] Icon 512 × 512 with alpha, in the game's painted style.
