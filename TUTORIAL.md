# CRCK Explorer tutorial

What every part of CRCK Explorer does and how to use it. The window (`crck-explorer.exe`) is the main way in; everything it does is also a command in `crck-explorer-cli.exe`, shown under each feature where one exists.

Contents: [Getting started](#getting-started) · [The four modes](#the-four-modes) · [Explorer](#explorer) · [Viewer](#viewer) · [A vehicle from the game](#a-vehicle-from-the-game) · [Texture dictionaries (YTD)](#texture-dictionaries-ytd) · [Images to YTD](#images-to-ytd) · [Add-on vehicle pack](#add-on-vehicle-pack) · [Check a pack](#check-a-pack) · [Handling editor](#handling-editor) · [Liveries](#liveries) · [Sirens](#sirens) · [Archetype editor](#archetype-editor-ytyp) · [Clothing pack](#clothing-pack) · [Batch rename](#batch-rename) · [Archives (RPF)](#archives-rpf) · [The "mods" folder](#the-mods-folder) · [Text editor and meta validator](#text-editor-and-meta-validator) · [Settings](#settings) · [Safety rules](#safety-rules) · [Limits](#known-limits)

---

## Getting started

1. Unzip the release anywhere. You get `crck-explorer.exe` (the window), `crck-explorer-cli.exe` (the command line) and this documentation. Nothing to install; no .NET or Visual C++ runtime needed.
2. Double-click `crck-explorer.exe`. (Double-clicking the CLI exe also opens the window.) The exes are not code-signed, so SmartScreen may warn the first time: **More info > Run anyway**.
3. **Your GTA V folder** is found by itself (Steam, Rockstar and Epic locations on every drive). The Home screen says whether it was found. To point at another place:

```
crck-explorer-cli game --dir "D:\Games\Grand Theft Auto V" info
```

   It remembers the folder. `crck-explorer-cli game info` also works out the archive keys from your own `GTA5.exe` (cached in `%LOCALAPPDATA%\CRCK Explorer\keys`, never sent anywhere, nothing is shipped with the program).
4. **No game installed?** The Viewer, the Explorer on your own folders, and all the Tools still work on your own files. Only the features marked *needs the game* do not.

Everything works on **copies**. Files read out of the game archives land in a cache folder (`%LOCALAPPDATA%\CRCK Explorer\cache`); your game files are never changed.

## The four modes

The switch at the top left has four modes:

| Mode | What it is |
|---|---|
| **Home** | A screen of cards that asks *what do you want to do?* Click a card and you land in the right place. A switch at the bottom turns this screen off at start (the program then starts in the Explorer). |
| **Explorer** | File browser for the game and your own folders. |
| **Viewer** | 3D view of a model with its parts and textures. |
| **Tools** | One page per task: pack builder, checker, handling, liveries, images to YTD, sirens, archetypes, clothing, rename, archives. |

Home cards: *Browse the game's files*, *Look at a model*, *A vehicle from the game*, *Images to YTD*, *Archetype editor*, *Clothing pack*, *Batch rename*, *Check a pack for mistakes*, *Archives (RPF)*. The **Home** button at the top left always comes back. Every Tools page works the same way: numbered cards, one big button.

---

## Explorer

Browse like a classic modding tool: a bar with back / forward / up, the path (every part is clickable) and a search box; a tree on the left; the contents of the selected folder on the right.

- **The tree** shows the game's folders and archives. Archives open in place and nested archives expand like folders. Add your own folders with **Add folder...** or by dropping a folder on the window.
- **The list** is grouped by type (Folder, Archive, Fragment, Texture dictionary, Application, ...) with sortable *Name / Type / Size* columns.
- **Double-click** goes into a folder or archive. A model (`.yft`, `.ydr`, `.ydd`) opens in the Viewer together with the `<name>.ytd` / `<name>+hi.ytd` next to it (for vehicles also the dictionaries the game chains to). A `.ytd` opens in its own texture window. A `.ytyp` opens in the archetype editor; text and `.meta` files open in the text editor.
- **Right-click:** Open, Add to current textures, Extract (a file, or a whole folder/archive), Copy name.
- **Drag out:** drag files from the Explorer to Windows Explorer or another program to extract them there.
- **Drop into folders:** drop files from Windows on a folder in the Explorer.
- **Delete** files and folders on disk (your own folders): they go to the Recycle Bin.
- **Keys:** Backspace = up, Alt+Left / Alt+Right = back / forward, F5 = refresh, Enter = open, Ctrl+F = search.
- **Search** (`Ctrl+F`): over the game and every folder you added. Every word must match; `*` and `?` make a pattern for the whole name (`*.ytd`); a word containing `/` is looked for in the whole path (`dlcpacks/mpheist`); archives count as folders. Choose files, folders or both, and *Show in folder* jumps to the result. Takes about half a second over the whole game.

```
crck-explorer-cli game search "vehshare *.ytd" --kind files
crck-explorer-cli game find volatus.yft
crck-explorer-cli game ls x64e.rpf/levels/gta5/vehicles.rpf
crck-explorer-cli game get <path from find> out.yft
```

## Viewer

Opens a `.ydr` (drawable), `.ydd` (drawable dictionary) or `.yft` (fragment: vehicles, props with physics) with its textures. Open files with **Look at a model**, from the Explorer, or by dropping them (and `.ytd` files) on the window.

- **Parts list:** every geometry with show/hide, its shader, and whether its texture was found. Vehicle parts are named after the skeleton's bones (`chassis`, `wheel_lf`, `door_dside_f`, ...).
- **Textures panel:** thumbnails of every loaded texture including embedded specular and normal maps; click to enlarge. Textures a model names that nothing loaded can be looked up in the game's vehicle dictionaries (**Find in the game**).
- **Detail level (LOD)** selector: high / medium / low / very low.
- **Toggles:** textured / untextured, grid, **Normal/spec/reflections**.
- **Paint colour:** a colour button recolours `vehicle_paint*` materials. Glass is drawn transparent after the opaque parts. Emissive materials are unlit.
- **Camera:** drag = orbit, right-drag = pan, wheel = zoom, double-click = frame the model.
- **Texture editing:** right-click a texture of a `.ytd` > **Replace with image or DDS...**, **Export as DDS**, **Export as PNG**. A DDS is used as it is; an image is encoded in the original texture's format ("keep format") and optionally scaled to the original size ("keep size"). The first save over a file you own keeps a `.ytd.bak`; for a file that came from the game the program asks where to save the edited copy instead.
- Both current-generation ("Enhanced") and Legacy texture dictionaries load; they are written back as Legacy.

```
crck-explorer-cli model-info <model> [--ytd file.ytd]       # LODs, triangles, shaders, which textures are missing
crck-explorer-cli model-export <model> <outdir> [--ytd file.ytd] [--lod high]   # OBJ + MTL + textures as PNG
crck-explorer --screenshot out.png [--size WxH] [--yaw deg] [--pitch deg] [--lod high] model [ytd]
```

## A vehicle from the game

*Needs the game.* On Home, **A vehicle from the game**, or in the Viewer **Vehicle from game...**: type a spawn name like `adder` or `police3`. CRCK Explorer follows the game's own texture rule from `vehicles.meta` (`txdRelationships`): the model's dictionary, then its parent, then that one's parent, e.g. `adder` > `vehicles_supergt_interior` > `vehshare`. The model shows with the textures the game would give it.

```
crck-explorer-cli game vehicle adder out\     # model, _hi, its dictionaries, the parents and the shared vehshare*.ytd
```

When several DLC packs define the same file, the last one wins, like in the game.

---

## Texture dictionaries (YTD)

Double-click a `.ytd` in the Explorer to open it in its own window: every texture with a thumbnail, size, format and mip count.

- **Export** a texture as DDS or PNG, **replace** it from an image or DDS, **rename**, **delete**, or **add** (drop images on the window).
- **Save / Ctrl+S** writes the dictionary; the window asks about unsaved changes when you close it. It edits a working copy, and a `.bak` is kept the first time.
- A read-only game file shows an amber bar; save elsewhere.

Command-line YTD tools (Legacy and Enhanced inputs are accepted; output is always Legacy; names compare case-insensitively):

```
crck-explorer-cli ytd merge a.ytd b.ytd -o out.ytd [--on-duplicate error|first|last]
crck-explorer-cli ytd split in.ytd outdir [--max-mb 12 | --per-texture | --prefix N]
crck-explorer-cli ytd rename in.ytd out.ytd old=new [old2=new2 ...]
crck-explorer-cli ytd remove in.ytd out.ytd name [name ...]
crck-explorer-cli ytd optimize in.ytd out.ytd [--max-size N] [--pow2 round|down|up|keep] [--drop-mips-below N] [--format bc1|bc3|bc4|bc5|bc7|rgba8] [--target-mb N]
crck-explorer-cli ytd report in.ytd
crck-explorer-cli ytd-info file.ytd
```

- `split` defaults to files under 12 MB (the FiveM limit is about 16 MB).
- `optimize` never enlarges a texture and copies untouched ones unchanged.
- `report` flags `NON_POW2`, `NOT_MULTIPLE_OF_4`, `MISSING_MIPS`, `ALPHA_UNUSED`, `NORMAL_MAP_FORMAT`, `DUPLICATE_CONTENT`. These are hints; many vanilla textures trip them.

## Images to YTD

Tools > **Images to YTD**. Drop pictures (png, jpg, tga, bmp, webp, dds) or whole folders, then choose how:

- **All into one YTD**, or **each folder / each image into its own**.
- **Format** per image is picked for you: BC1 for opaque, BC3 when there is alpha, BC5 for normal maps (`_n` names). Or force `bc1 / bc3 / bc5 / bc7 / rgba8`.
- **Size:** a maximum side, and power-of-two handling (`round / down / up / keep`) with Lanczos resizing.
- **Mipmaps** are generated; encoding runs in parallel; a per-texture report and a **size budget** warning (default 16 MB) tell you what you produced. DDS inputs are used as they are.

```
crck-explorer-cli textures paint\ out\myveh.ytd [--max-size N] [--pow2 round|down|up|keep] [--format bc1|bc3|bc5|bc7|rgba8] [--budget-mb 16]
```

---

## Add-on vehicle pack

Tools > **Add-on vehicle pack**. Drop the vehicle's folder (model files `.yft`, `_hi.yft`, `.ytd`, and the metas `vehicles`, `handling`, `carvariations`, `carcols`, `vehiclelayouts`), pick the output and create:

- a **FiveM resource** (`fxmanifest.lua`, `stream/` folder, `data_file` lines), or
- a **single-player DLC folder**.

The result is checked straight away with the same rules as *Check a pack*.

```
crck-explorer-cli pack fivem myveh src\ out\
```

Not built: a "new vehicle" wizard that writes starter metas from a template; the pack creator needs the metas to exist (the handling editor can copy a vanilla vehicle's handling into a new file).

## Check a pack

Tools > **Check a pack for mistakes**, or on the command line. It cross-checks, case-insensitively, that names agree across `vehicles`, `handling`, `carvariations`, `carcols` and `vehiclelayouts`, that the model files exist, and (with FiveM on) that the manifest matches the files. Every finding has a severity (**E**rror / **W**arning / **I**nfo), says where it is, and says how to fix it. Things the pack does not define are Info only, since they could be vanilla.

With `--fivem` it also checks:

- **Manifest:** `fxmanifest.lua` / legacy manifest, `fx_version`, `files {}` and `data_file` lines against the real files.
- **Stream folder:** nothing in `stream/`, models without a `.ytd`, `_hi` / `+hi` without a base, files outside `stream/`.
- **Size limits:** YTD over 16 MB is an error and 12 MB or more a warning; YFT over 25 MB; stream folder over 50 MB. Stray junk files are reported.
- **Livery rules:** kit type, `lodDistances` type, labels, livery fragment size.

```
crck-explorer-cli validate out\myveh --fivem [--json]
```

Exit code 1 on errors; `--json` gives stable issue codes (e.g. `YTD_OVER_LIMIT`) for scripts.

## Handling editor

Tools > **Handling editor**. Open a `handling.meta` and edit values with context:

- The **allowed range** for every field and a **vanilla vehicle's value** beside it, with a button to take it. Pick which vanilla vehicle to compare with.
- **Copy** a vanilla vehicle's handling into your file, **rename** or **remove** entries.
- **Saving changes only the text you edited:** your comments, spacing and number formats stay. The first save keeps a `.bak`.

Command line:

```
crck-explorer-cli handling check <handling.meta | folder>... [--json]
crck-explorer-cli handling compare my.meta --game [--fields mass,top-speed,...] [--sort col] [--desc] [--filter a,b] [--limit N] [--csv]
crck-explorer-cli handling ranges [files] [--game]
```

- `check` errors: duplicate `handlingName`, missing core field, non-numeric value, bad XML, non-hex flags. Warnings: value outside the usual band for its vehicle class, fractional integer field, duplicate field, wrong item type. Info: a field not found in any vanilla entry.
- `compare` puts your files first and the vanilla vehicles after (`--game`): mass, drive force, top speed (raw handling units, not km/h), brake force, traction, drive bias, gears, steering lock. `--filter` takes name fragments, `--csv` exports.
- `ranges` shows how many vanilla vehicles fall outside the slider ranges and the warning bands.

## Liveries

Tools > **Liveries**. Four cards: read a resource's livery kits, add a livery, turn a `.dds` into a livery `.yft`, and check an edit against the live server's file.

`add` keeps four things in step with plain text splicing (the rest of each file keeps its formatting): the kit in `carcols.meta`, the kit link in `carvariations.meta`, the `AddTextEntry` label in `client.lua`, and the `fxmanifest.lua` lines. New liveries are **appended**; existing ones are never reordered, so the liveries already saved on your server do not shift. Re-running it changes nothing.

```
crck-explorer-cli livery list <pack>
crck-explorer-cli livery add <pack> --vehicle myveh --name lspd2 [--display "LSPD 2"] [--label KEY] [--kit-id 4800] [--yft file.yft]
crck-explorer-cli livery check <old carcols.meta> <new carcols.meta>
```

`check` is the safety net before updating a live server: it fails if any existing livery would change index, be removed, or if a kit id changed or was reused. Kit ids must be unique across your whole server (checked inside the pack only).

## Sirens

Tools > **Sirens**. Checks siren settings in `carcols.meta` and has calculators.

```
crck-explorer-cli siren check <carcols.meta | pack folder> [--json]
crck-explorer-cli siren calc deg2rad 90 | rad2deg 1.5708 | seq2dec 10 | dec2seq 2863311530
crck-explorer-cli siren calc speed 220 0b10          # speed for a BPM and flash pattern
crck-explorer-cli siren calc start 3 90 [--steady]   # flashiness start from siren number and delta
```

`check` reports (with line numbers and a fix hint): duplicate or missing setting ids, sequencers out of range, delta/start values that look like degrees (they are radians), malformed colours (must be `0xAARRGGBB`), the number of `<sirens>` items, ids above 255 or colliding with a vanilla setting, `carvariations` references to settings the pack does not define, unused settings. The conversions between degrees/radians and decimal/binary sequencers are exact. The `speed` and `start` calculators follow a community guide; `start` could not be reproduced from vanilla data, so treat it as a starting point and test in game.

---

## Archetype editor (.ytyp)

Home > **Archetype editor** (or **New archetype file**), or double-click a `.ytyp` in the Explorer. Edits props, time archetypes and MLO interiors with every field of the real structure.

- **Left:** the archetypes with a search box, `prop` / `time` / `MLO` tags and a mark where there are findings.
- **Properties tab:** every member of the selected archetype's structure; numbers, names, vectors, enums, flags and nested lists are edited in place. A value applies when you leave the box or press Enter; an invalid value turns red and is not applied. **Undo / redo** throughout.
- **XML tab:** editable XML of the selected archetype, checked as you type (line, column, underlines). **Apply** parses it back.
- **Look up models in the game:** indexes the game's models and texture dictionaries (and folders you add) so missing references are reported and hashes show their names.
- **Checks:** missing models, broken portals and other findings, with thresholds calibrated on the game's own files so vanilla data raises no alarms.
- **Merge another .ytyp:** pick a file and review each archetype (new / same / differs, with the members that differ). Choose Add, Skip or Keep mine, Replace mine, or Add as another name. The result of the checks is shown before you press Merge; the whole merge is one undo step.
- **Safe saving:** the file is built, read back and compared with the open data; any difference refuses the save. The first overwrite keeps a `.bak`.

```
crck-explorer-cli ytyp check <files|folders>... [--game] [-v]    # read, rewrite, compare: reports LOSS or FAIL
```

## Clothing pack

Tools > **Clothing pack**. Laid out like other clothing tools: your drawables on the left, the selected one in the middle, a **3D preview** of it on the freemode body from your game on the right.

1. **Drop** `.ydd` models and `.ytd` textures (or plain images).
2. They are **paired by name** and grouped by component (`jbib`, `lowr`, `p_head`, ...).
3. They are **checked:** missing textures, number gaps (drawable numbers must run 0, 1, 2, ... per component and sex because the YMT stores drawables by position), duplicates, 1 to 26 textures per drawable. Nothing is renumbered without asking.
4. **Build** a FiveM resource: `fxmanifest.lua`, one `mp_m_freemode_01_<pack>.meta` / `mp_f_freemode_01_<pack>.meta`, and in `stream/` the YMT and every file named `<ped>_<pack>^<name>` (props `<ped>_p_<pack>^<name>`). Existing output is never overwritten.

Projects can be saved and loaded. Build output has not been loaded in a running game.

## Batch rename

Tools > **Batch rename**. Build a list of steps, each working on the name without its extension, in order, each on the result of the one before. You see **every new name before anything happens**; conflicts and invalid names block the run; a failure rolls back; **Undo** restores the old names (it refuses if an old name was taken again).

Steps:

- **Sequential numbers** (`001`, `002`, ...): start, digit count, prefix/suffix, replace or append, restart per folder.
- **Texture letters:** files in one folder that differ only in one single-letter part are renumbered `a`, `b`, `c`, ... in the order of their old letters (`_a_`, `_c_`, `_f_` become `_a_`, `_b_`, `_c_`).
- **Drawable numbers:** renumber per component prefix and folder from a start number; drawables and textures get the same mapping.
- **Find and replace:** plain or regular expressions (no look-around or back-references; `$1` / `${1}` in the replacement).
- **Add / remove prefix or suffix.**
- **Case:** lower, upper, title.
- **Your own pattern:** `{name} {ext} {parent} {n} {n:3}` with `|upper` / `|lower`.

Sorting before numbering: as listed, A-Z, natural, by the first number in the name, by extension; optionally reversed. Ready-made presets are included. No command-line verb yet.

## Archives (RPF)

Tools > **Archives (RPF)**. List, extract and build `.rpf` archives, nested ones too. Nested `.rpf` extract as folders, so the edit loop is extract > edit > rebuild.

```
crck-explorer-cli list <file.rpf>
crck-explorer-cli extract <file.rpf> <outdir>
crck-explorer-cli rpf <folder> <out.rpf>
crck-explorer-cli verify <dlc folder>     # a built single-player DLC folder: every content.xml path exists
crck-explorer-cli png <image> <file.ytd>  # put one image into a YTD (replace by name or add)
```

(Run `crck-explorer-cli --help` for the rest; every command has its own `--help`.)

**Edit mode** (Explorer, like classic modding tools): add, replace, rename and delete files inside a copy in the `mods` folder or your own archives. It is in place and append-only; the archive is read back and every edit compared afterwards; a saved archive is OPEN (unencrypted), which the game reads only with the mods-folder ASI plugin. **The game's own archives are always refused.**

```
crck-explorer-cli edit mods\x64e.rpf --in levels/gta5/vehicles.rpf put adder.ytd my_adder.ytd
crck-explorer-cli edit mods\update\update.rpf rm common/data/old.meta
crck-explorer-cli edit my.rpf mv data/a.meta b.meta
```

The space of replaced files is not reclaimed; copy the archive from the game again to start clean.

## The "mods" folder

Classic convention: a copy of a game archive at the same path under `<game>\mods` (for example `mods\update\update.rpf`, `mods\x64e.rpf`) is loaded by the game instead of the original, **when the mods-folder plugin and an ASI loader are installed**. CRCK Explorer installs neither and loads nothing into the game.

- Inside one of the game's own archives the Explorer shows a red notice: **Copy to "mods" folder** copies the outermost archive on a background thread (progress bar, Cancel), never overwrites an existing copy, and then shows the same folder inside the copy.
- **Change "mods" folder settings** opens Settings > Game files, where the notice can be switched off and another mods folder chosen.
- Opening one of the game's own **loose** files (in the game folder, not in `mods`) in the text, texture or archetype editor asks first: copy to mods and edit the copy, edit the game's file, or cancel.

```
crck-explorer-cli game copy-to-mods update/update.rpf [--mods DIR]
```

## Text editor and meta validator

Text and `.meta` / `.xml` files open in the text editor window, which shows the validator's findings **live** as you type. The same validator runs from the command line:

```
crck-explorer-cli meta check <files|folders>... [--notes]
```

It prints `file:line:col: error|warning [CODE] message` and a `fix:` line for each finding. Syntax mistakes are all listed, not only the first. When the root element is a known game structure (`CMapTypes`, `CMapData`, `CVehicleModelInfoVarGlobal`, ...), the members are checked against it as well; handling, carcols and carvariations files also get the handling and siren checks. `--notes` adds information-level findings. Exit code 1 on any error.

## Settings

A Settings page holds: the GTA V folder, the "mods" folder and its notice, and display options. The settings live in `settings.txt` under `%LOCALAPPDATA%\CRCK Explorer`.

## Safety rules

- Your game's own files are not changed. The only thing written into the game folder is a copy you ask for in its `mods` folder.
- Your files are edited in place only when you say so; the first save over a file keeps a `.bak`.
- Names that come from inside an archive are checked before anything is written to disk (no path escapes).
- Nothing from the game ships with the program, and nothing leaves your PC: no accounts, no network calls.

## Known limits

- The pack layouts, validator rules and FiveM size limits come from community guides and vanilla data. They were **not** verified by loading packs in a running game. Expect rough edges and report them as issues.
- The 3D view is solid on vehicles and clothing. Skinned characters in pose, the livery layer and dirt layers are not drawn. Reflections are a fake gradient, not the game's environment map.
- A livery `.yft` cannot be generated from scratch; livery fragments need a blank template made elsewhere.
- Not done: writing into the game's own archives (read-only by design), the Enhanced-edition install, reading `dlclist.xml` (the last DLC or update copy in walk order wins).
- Textures with a side that is not a power of two re-encode with fewer mip levels.
