# Worked example: models, textures and icons

Cooked content is any asset made in the stub project: Blueprints (`docs/examples/blueprints.md`),
meshes, textures and material instances. This page gives a machine its own build-menu icon and model.

**Status: nothing on this page has been tried in game yet.**
- The packaging is proven for Blueprints.
- A texture's pixels are stored differently (in a separate `.ubulk` file), so the first icon will be the
  test.
- A model needs a material with compiled shaders, which the packaging doesn't handle yet (below).

Each step says what the tools already check.

## 1. An icon

A building's icon is `DisplayIcon` in its `DT_Buildings` row; an item's is `DisplayIcon` in `DT_Enemies`.
The game's building icons are 512 × 512, uncompressed, with mipmaps, painted in a slightly angled view on
a transparent background. Export a few from FModel to copy their style.

1. Paint `T_UI_MyMachine.png`: 512 × 512, with transparency. `docs/MACHINE-ART.md` describes doing it in
   Blender and Krita.
2. In the stub project, import it into `Content/Mods/MyMachine/Textures/`. Set:
   - *Compression Settings* to *UserInterface2D*;
   - *Texture Group* to *UI*;
   - *sRGB* on.
3. Save, and cook.
4. Point the building row at it:

   ```json
   {
     "DT_Buildings": {
       "MyMachine": {
         "DisplayIcon": "/Game/Mods/MyMachine/Textures/T_UI_MyMachine.T_UI_MyMachine"
       }
     }
   }
   ```

   An asset path is the folder, the name, a dot, and the name again.
5. Build:
   ```
   alchemists-tools mod build MyMachine --mods examples
   ```
   The build checks that:
   - the path is one of the mod's cooked assets;
   - the asset is a texture, as `DisplayIcon` needs;
   - the packaged container resolves it.

   It lists the new reference.

You can also reuse any of the game's icons, with no cooking: `"DisplayIcon":
"/Game/Arts/UI/Furnitures/T_UI_Kiln.T_UI_Kiln"`. The build checks that the game has it.

## 2. A model

A machine's model is the meshes in its Blueprint. The game's Processor body is `SM_Processor_1`, about
the size of its 2 × 2 footprint. `docs/MACHINE-ART.md` covers measuring the game's models in Blender, and
modelling, collision and unwrapping.

1. Model in Blender with *Unit Scale* 0.01 (centimetres), next to the game's Processor exported from
   FModel for size. Make each moving part a separate object, with its origin at its axle.
2. Export FBX. Import into `Content/Mods/MyMachine/Meshes/` with:
   - *Combine Meshes* off;
   - *Generate Missing Collision* on, unless you modelled `UCX_` collision shapes.
3. In `BP_MyMachine`, set each mesh component's *Static Mesh* to yours. Keep the body's `FixedMesh` tag
   and the collision presets: `FurnitureNoBuild` for the body, `Furniture` for the moving parts.
4. Cook, and build. The build's component list shows your meshes. A mesh still pointing at a stand-in
   shows the stand-in's game path.

## 3. Materials: the open problem

A new **material** compiles shaders, which a mod would have to ship. The packaging doesn't handle shaders
yet. The plan is a **material instance** of one of the game's materials, through a stand-in parent:

1. Find the game material that the machines' material instances use, in FModel.
2. In the stub, make a stand-in *Material* at that exact path and name, with texture and number parameters
   of exactly the same names.
3. Make your own *Material Instance* of it in `Content/Mods/MyMachine/Materials/`, with your textures in
   its parameters. Override only textures, colours and numbers: changing a *static switch* compiles new
   shaders.
4. Assign it to your meshes.

In game, the instance should get the real parent and its shaders. Like every stand-in, the parent must
never be packaged.

## What a mod's content folder can hold

| Asset | Packaged | Tried in game |
|---|---|---|
| Blueprint (a building's class) | yes | yes |
| Texture (icons) | yes | no |
| Static mesh | yes | no |
| Material instance of a game material | yes | no |
| Material | yes, but without shaders | no: it won't render |
| Anything under `/Game/Arts` (stand-ins) | refused | must never be |
| A replacement for a game asset (same path) | only with `--allow-game-paths` | yes, for a Blueprint |

Everything a mod adds lives under `/Game/Mods/<ModName>/`, so no two mods' assets share a path. Every
cooked file there is packaged into `zzz_<ModName>Content_P` when `mod.json` lists the folder in `content`.
