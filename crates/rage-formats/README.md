# rage-formats

Parsers, and where it matters writers, for the RAGE resource formats GTA V
streams: the RSC7-wrapped files found inside `.rpf` archives. Bytes in,
plain Rust structs out. No game install, no unsafe, no C dependencies.

Getting the bytes out of an archive is
[`rpf-archive`](https://github.com/VIRUXE/rpf-archive-rs)'s job; drawing
what this crate parsed is [`rage-render`](https://github.com/VIRUXE/rage-render)'s;
[`rage-cli`](https://github.com/VIRUXE/rage-cli) is the command line over
all three.

```toml
[dependencies]
rage-formats = "0.4"
```

## What it reads and writes

| Format | Read | Write | Entry point | You get |
|---|:-:|:-:|---|---|
| `.ytd` texture dictionary | yes | | `parse_ytd` | `YtdTexture` per texture, decoded to RGBA by `texture_utils` |
| `.ydr` drawable | yes | | `parse_ydr`, `parse_drawables` | `Drawable`: bounds, LODs, models, geometry, shader group, embedded textures |
| `.ydd` drawable dictionary | yes | | `parse_ydd`, `parse_drawables` | one `DrawableEntry` per member, uniquely named |
| `.yft` fragment | yes | | `parse_yft` | `Fragment`: main drawable, physics children with transforms, bone pose |
| `.ybn` collision | yes | | `parse_ybn` | the `phBound` tree; `Ybn::triangles()` flattens it to world space |
| `.ynv` navmesh | yes | yes | `parse_ynv`, `serialize_ynv` | `Ynv`: polygons with vertices, flags and edge adjacency, portals, points |
| `.ynd` path nodes | yes | yes | `parse_ynd`, `serialize_ynd`, `ynd_to_xml`, `ynd_from_xml` | `Ynd`: nodes with positions, street hashes, flags and their links; junction heightmaps; the reference tool's `<NodeDictionary>` XML both ways |
| `.ymap` placements | yes | | `parse_ymap`, `parse_ymap_entities`, `parse_ymap_mlo_instances` | `YmapHeader` (name, parent, flags, extents); `YmapEntity` per entity, with `to_world()` and its LOD fields (`lod_dist`, `child_lod_dist`, `lod_level`, `num_children`, `parent_index`); `MloInstance` per interior, with its default entity sets |
| `.ytyp` archetypes | yes | | `parse_ytyp` | every archetype's box and texture dictionary; each MLO's entities, named rooms, portals and entity sets |
| `.ymt` ped variation | yes | | `parse_ymt` | `PedVariationInfo`: each slot's drawables and textures, and their file names by the reference tool's rules (`uppr_001_r`, `uppr_diff_001_a_whi`) |
| `vehicles.meta` | yes | | `parse_vehicles_meta` | `VehicleInitData` per model, every member the reference tool's `VehiclesFile` reads |
| `carcols.ymt` / `.meta` | yes | | `parse_carcols` | `CarCols`: colours, metallic and window settings, plates, lights, sirens, mod kits with liveries, wheels, xenon colours |
| `carvariations.ymt` / `.meta` | yes | | `parse_carvariations` | per model: colour combinations with livery flags, kits, plate probabilities |
| `carmodcols.ymt` | yes | | `parse_carmodcols` | the mod shop colour menus |
| `peds.ymt` / `peds.meta` | yes | | `parse_peds_meta` | `PedInitData` per ped, every member the reference tool's `PedsFile` reads |
| `_manifest.ymf` | yes | | `parse_ymf` | `Manifest`: map/type dependencies, HD texture bindings, interior collision lists — from PSO, RBF, Meta or XML |
| any Meta file (`ytyp`, `ymap`, `ymt`) | yes | | `dump_meta`, `to_xml`, `to_json` | the whole file as a `MetaValue` tree from its own schema, with names from `NameTable` |
| any PSO file (`ymf`, `pso`, `ymt`) | yes | | `dump_pso`, `to_xml`, `to_json` | the same tree from the big-endian container |
| `gtxd.ymt` / `gtxd.meta` | yes | | `parse_txd_relationships` | texture dictionary parent chain |
| `cache_y.dat` world cache | yes | | `parse_cache_dat` | `CacheDat`: every map file's parent and extents, every placed interior (archetype, placing map, position, rotation, box), every collision file's bounds |
| `heightmap.dat` world heightmap | yes | yes | `parse_heightmap`, `serialize_heightmap` | `WorldHeightmap`: the highest and lowest ground per cell, scaled into the file's box; `max_height_at`, `cell_size` |
| `water.xml` water surface | yes | | `parse_water_xml` | `WaterData`: the water, calming and wave quads, as the reference tool's `Water.cs` reads them |
| RSC7 container | yes | yes | `prepare_rsc7`, `build_rsc7`, `build_rsc7_paged` | sections in, a valid file out; `is_fxap` names an escrowed FiveM asset instead |

Hashing is `rage_joaat`, the Jenkins one-at-a-time every name in these
formats is looked up by.

## How the formats fit together

```
 .rpf archive (rpf-archive)
   |
   +-- RSC7 file: 16-byte header, deflated body
         |         version, system and graphics page flags
         v
     prepare_rsc7  ->  system section + graphics section
         |
         v
     ResReader     resolves 0x50000000-based (system) and 0x60000000-based
         |         (graphics) virtual pointers into slices
         v
     one parser per format, chasing pointers block by block
```

Three families of file share that container but differ inside:

- **Fixed-layout resources** (`ytd`, `ydr`, `ydd`, `yft`, `ybn`, `ynv`):
  structs with pointers to other structs, laid out by the engine's own C++.
  The parsers here mirror those layouts field by field.
- **Meta** (`ytyp`, `ymap`, ped `ymt`): a self-describing block table, each
  block tagged with a structure-name hash, with packed block:offset pointers
  between them. `ytyp.rs` walks the table once; `ymap.rs` and the MLO reader
  reuse that. The header also carries the schema — every structure's size
  and members — which `meta_schema.rs` reads to decode any block generically.
- **PSO** (`_manifest.ymf`, `.pso`, many `.ymt`): not RSC7 at all — a
  big-endian file of sections (`PSIN` data, `PMAP` block table, `PSCH`
  schema). `pso.rs` reads it and decodes it through the same generic walker.
- **RBF** (`gtxd.ymt`, retail `_manifest.ymf`): a flat record stream with
  its own descriptor table; `rbf.rs` reads it whole.

Every byte offset was ported from the reference library (`Bounds.cs`, `Nav.cs`,
`YnvFile.cs`, `MetaTypes.cs`, `DrawableBase.cs` and friends) and checked
against retail files.

## Examples

### Drawables

`parse_drawables` reads any drawable-bearing resource, a lone `.ydr`, a
`.ydd` dictionary or a `.yft` fragment, into one flat list of named
entries, so a caller can stay agnostic about which it was handed:

```rust
use rage_formats::{parse_drawables, DrawableKind};

let entries = parse_drawables(&ydd_bytes, DrawableKind::Ydd)?;
for entry in &entries {
    println!("{} (0x{:08X}): {} triangle(s)", entry.name, entry.hash, entry.drawable.geometry_count());
}
```

Each `DrawableEntry { hash, name, drawable }` carries a `Drawable` with
bounds, LODs, shader group and geometry. Entries that would otherwise share
a name (dictionary members with no name of their own, or a fragment's extra
drawables with no names array) get unique `0x…` names instead of
overwriting each other.

### Textures to images

`.ytd` textures decode straight to an `image::RgbaImage`, ready to resize
and encode:

```rust
use rage_formats::{parse_ytd, texture_utils::{to_rgba_image, fit_max_size, encode_image, ImageFormat}};

let textures = parse_ytd(&ytd_bytes)?;
for texture in &textures {
    let image = to_rgba_image(texture)?;
    let image = fit_max_size(image, 512);
    let png_bytes = encode_image(&image, ImageFormat::Png, 90)?;
    std::fs::write(format!("{}.png", texture.name), png_bytes)?;
}
```

The `image` feature (on by default) provides those three helpers; without
it `decompress_texture` still gives raw RGBA bytes and the crate has no
`image` dependency.

### Collision to world-space triangles

A `.ybn` is a tree: a composite whose children are triangle meshes, each
placed by its own transform. `triangles()` applies the transforms and hands
back the soup:

```rust
use rage_formats::parse_ybn;

let ybn = parse_ybn(&ybn_bytes)?;
for t in ybn.triangles() {
    let n = t.normal();
    if n.z > 0.7 { /* walkable */ }
}
```

An MLO's collision is in the interior's own space; place it with the
entity that positions the MLO in the world:

```rust
use rage_formats::{parse_ymap_entities, parse_ybn};

let placement = parse_ymap_entities(&milo_ymap)?.into_iter().find(|e| e.is_mlo_instance).unwrap();
let world: Vec<_> = parse_ybn(&ybn_bytes)?.triangles().into_iter()
    .map(|mut t| { for v in &mut t.vertices { *v = placement.to_world(*v); } t })
    .collect();
```

`to_world` applies scale, the stored rotation (map entities store the
inverse quaternion, as the reference tool's `YmapEntityDef` does) and translation.

### Navmesh cells, read and write

```rust
use rage_formats::{parse_ynv, serialize_ynv, NavEdge, NavPoly, Vec3, cell_file_name, cell_for_position};

let mut cell = parse_ynv(&std::fs::read("navmesh[108][96].ynv")?)?;
println!("area {} polys {} portals {}", cell.area_id, cell.polys.len(), cell.portals.len());

let mut poly = NavPoly::new(vec![
    Vec3::new(-580.0, -1064.0, 21.3), Vec3::new(-578.0, -1064.0, 21.3),
    Vec3::new(-578.0, -1062.0, 21.3), Vec3::new(-580.0, -1062.0, 21.3),
]);
poly.set_interior(true);
poly.set_flat_ground(true);
poly.edges[1] = NavEdge::neighbour(cell.area_id, 42); // edge 1 joins polygon 42 of this cell
cell.polys.push(poly);

std::fs::write("navmesh[108][96].ynv", serialize_ynv(&cell)?)?;
assert_eq!(cell_for_position(-578.0, -1063.0), (36, 32));
assert_eq!(cell_file_name(36, 32), "navmesh[108][96].ynv");
```

Polygons are counter-clockwise; edge `i` runs from vertex `i` to `i+1` and
names the one polygon across it (`NavEdge::NONE` when there is none). The
writer rebuilds everything derived: quantised vertices deduplicated inside
the cell box, index and edge lists in 16 KiB parts, the adjacent-area table,
portal links, per-polygon cell boxes and part ids, and the two-level sector
quadtree, then wraps it in an RSC7 file whose blocks never straddle a page.

### Path nodes, read and write

```rust
use rage_formats::{parse_ynd, serialize_ynd, ynd_cell_file_name, ynd_cell_for_position, PathLink, Vec3};

let mut cell = parse_ynd(&std::fs::read("nodes489.ynd")?)?;
println!("{} nodes, {} junctions", cell.nodes.len(), cell.junctions.len());
for n in cell.nodes.iter().filter(|n| n.is_junction()) {
    println!("junction at {:?}: {} links, {}", n.position, n.links.len(), n.special().name());
}

let id = cell.nodes.len() as u16;
let node = cell.add_node(489, Vec3::new(-3500.0, -400.0, 20.0));
node.links.push(PathLink::to(489, 0));            // to node 0 of this cell
cell.nodes[0].links.push(PathLink::to(489, id));  // and back
cell.recalculate_node_indices();                   // vehicle nodes first, the counts refreshed

std::fs::write("nodes489.ynd", serialize_ynd(&cell)?)?;
assert_eq!(ynd_cell_for_position(-3500.0, -400.0), (9, 15));
assert_eq!(ynd_cell_file_name(9, 15), "nodes489.ynd");
```

A cell is 512 m square on a 32×32 grid from (-8192, -8192); its area id is
`y * 32 + x` and its file `nodes<area>.ynd`. Cayo Perico's cells carry 1024
on top (`ynd_is_island_area`), streaming in over the sea cells they replace,
and their links name the cell without it (`ynd_same_cell`). Positions are
stored to a quarter of a metre in x and y and a 32nd in z; `set_position`
quantises as the reference tool does. Every node's flag bits have accessors
(`is_junction`, `highway`, `tunnel`, `is_disabled`, `speed`, `special`,
`is_ped_node`, ...), as do a link's lane counts, `shortcut` and
`dont_use_for_navigation`. The writer computes the link and heightmap
offsets and lays the five arrays out with the same block graph
`ResourceBuilder.Build` uses (resource version 1): a retail cell rewrites to
the same nodes, links and junctions and serialises byte-identically the
second time. `ynd_to_xml`/`ynd_from_xml` are the reference tool's `<NodeDictionary>`
XML; `dump_ynd_xml`/`build_ynd_from_xml` go straight from and to bytes.
A retail cell survives parse, serialize, parse with every polygon, edge,
flag, portal and point equal, and a second serialize is byte-identical.

### MLO definitions

```rust
use rage_formats::parse_ytyp;

let ytyp = parse_ytyp(&int_ytyp)?;
for mlo in &ytyp.mlos {
    for room in &mlo.rooms { println!("room {} {:?}..{:?}", room.name, room.bb_min, room.bb_max); }
    for p in &mlo.portals { println!("portal {} -> {} ({} corners)", p.room_from, p.room_to, p.corners.len()); }
    for s in &mlo.entity_sets { println!("set {:#010x}, {} entities", s.name_hash, s.entities.len()); }
    for e in &mlo.entities { println!("prop {:#010x} at {:?}", e.archetype_hash, e.position); }
}
let boxes: std::collections::HashMap<u32, _> = ytyp.archetypes.iter().map(|a| (a.name_hash, (a.bb_min, a.bb_max))).collect();
```

Entities inside an MLO are relative to the MLO origin; apply the entity's
own `to_world` and then the MLO instance's.

### Map header and manifest

```rust
use rage_formats::{parse_ymap, parse_ymf};

let ymap = parse_ymap(&bytes)?;
println!("{:#010x} parent {:#010x} flags {:#x} {:?}", ymap.header.name_hash, ymap.header.parent_hash, ymap.header.flags, ymap.header.content_flag_names());
println!("{} entities within {:?}..{:?}", ymap.entities.len(), ymap.header.entities_extents_min, ymap.header.entities_extents_max);

let (format, manifest) = parse_ymf(&manifest_bytes)?;   // PSO, RBF, Meta or XML
for dep in &manifest.imap_dependencies_2 {
    println!("{} needs {:?}", dep.name, dep.ityp_deps);  // names print as text when the file had it, else hash_XXXXXXXX
}
```

### World cache

`gta5_cache_y.dat` (in `update.rpf/common/data`) and each DLC pack's
`x64/data/cacheloaderdata_dlc/*_cache_y.dat` are what the game reads
instead of opening every map file: which map places which interior, and
every map's extents.

```rust
use rage_formats::parse_cache_dat;

let cache = parse_cache_dat(&bytes)?;
for interior in &cache.interior_proxies {
    println!("{:#010x} placed by map {:#010x} at {:?}", interior.name, interior.parent, interior.position);
}
println!("{} maps, {} collision files", cache.map_nodes.len(), cache.bounds.len());
```

Maps loaded by script (heist apartments, some story interiors) are in no
cache file, so a complete list of placements still has to read those maps.

### World heightmap and water

`heightmap.dat` and `heightmapheistisland.dat` (in `update.rpf/common/data/levels/gta5`)
are a coarse grid of the highest and lowest ground, one byte each per cell,
read and written as the reference tool's `HeightmapFile` does; `water.xml` and
`water_heistisland.xml` (`common.rpf` and `update.rpf` under
`data/levels/gta5`) are the water quads the game draws.

```rust
use rage_formats::{parse_heightmap, parse_water_xml};

let hm = parse_heightmap(&bytes)?;
let (ix, iy) = (90, 120);
println!("{:?} rises to {:.0} m", hm.position_of(ix, iy), hm.max_height_at(ix, iy).unwrap());

let water = parse_water_xml(&std::fs::read_to_string("water.xml")?)?;
let quads = water.quads.iter().filter(|q| q.intersects(-1700.0, -1200.0, -1100.0, -600.0)).count();
println!("{quads} water quads over the docks");
```

### Any Meta or PSO file, as XML or JSON

Both containers carry their own schema, so any file decodes without a
per-format reader:

```rust
use rage_formats::{dump_meta, dump_pso, is_pso, to_json, to_xml, NameTable};

let dump = if is_pso(&bytes) { dump_pso(&bytes)? } else { dump_meta(&bytes)? };
for w in &dump.warnings { eprintln!("{w}"); }      // anything left undecoded, never an error
let names = NameTable::core();                     // built-in structure/member/enum names
print!("{}", to_xml(&dump.root, &names));          // the reference tool's XML layout, diffable against its export
let json = to_json(&dump.root, &names).pretty(2);  // {"$type": "CMapData", "name": "map1", ...}
```

The tree is `MetaValue`: structures with hashed member names, arrays,
vectors, hashes, strings, enums and flags. `MetaStruct::field("name")`
looks a member up by name. Hashes with no known name print as
`hash_XXXXXXXX`; the built-in list names every structure, member and enum
The reference tool knows (some 20,000 of the game's own schema names, compiled
from its `MetaNames` table and each verified against its hash — `itemType` names like
`CVehicleModelColorIndices` never appear in any file and can only come from
such a table), and `NameTable::add_list` takes more (one name per line), so
content names — archetypes, texture dictionaries — can be supplied by
whoever knows them. `from_xml` reads the XML layout back into a tree.

The vehicle and ped metadata readers above are built on that tree:
`meta_read::parse_tree` reads a PSO stream, an RSC7 Meta resource or the
XML layout into one `MetaValue`, and the `Fields` helpers read members by
name whichever form they came in (an XML `<indices>0 1 2</indices>` and a
PSO byte array both come back as bytes; an enum by its name or its value).
`meta_enum!` defines an enum with the reference tool's member names and values
plus an `Unknown` for anything else, as `VehicleModBone` and the other
carcols enums are.

The same schema walk knows where every hash-typed field sits, so a name can
be changed without rewriting the file: `meta_schema::replace_hashes` and
`pso::replace_hashes` rewrite every hash field equal to one of the old
values (a Meta file is re-paged into a fresh RSC7 container, a PSO file
is patched in place) and report how many they changed. `hash_sites` lists
the offsets themselves.

### Drawables and bounds from XML

`.ydr` drawables and `.ybn` bounds convert to and from the reference tool's XML
through a port of its resource block graph (`blocks`, an arena of blocks
that point at each other by `BlockId`, laid into RSC7 pages the way
`ResourceBuilder.Build` does):

| Function | Does |
|---|---|
| `read_ydr(&[u8])` / `read_ybn(&[u8])` | file bytes to `(Graph, BlockId)` |
| `write_ydr(&mut Graph, BlockId)` / `write_ybn(..)` | graph to file bytes (drawable version 165, bounds version 43) |
| `dump_ydr_xml(&[u8], &NameTable, Option<&Path>)` / `dump_ybn_xml(&[u8])` | file to XML; embedded textures go to the folder as `.dds` |
| `build_ydr_from_xml(&str, Option<&Path>)` / `build_ybn_from_xml(&str)` | XML to file; `<FileName>.dds` textures are read from the folder |
| `build_ydr_from_xml_checked(&str, Option<&Path>)` / `build_ybn_from_xml_checked(&str)` | the same, then the bytes are read back and their XML compared with the written graph's; returns a `Built` (`bytes`, `xml`, `warnings`) or an error, never a file that reads back differently |

```rust
use rage_formats::{build_ydr_from_xml, dump_ydr_xml, NameTable};

let xml = dump_ydr_xml(&bytes, &NameTable::core(), Some(dds_dir))?;
let rebuilt = build_ydr_from_xml(&xml, Some(dds_dir))?;
```

Parity: build accepts every valid document the reference tool's `XmlYdr.GetYdr` /
`XmlYbn.GetYbn` accept; dump emits everything `YdrXml.GetXml` / `YbnXml.GetXml`
emit. Legacy PC resources only (a Gen9 `.ytd` converts with
`convert_ytd_to_legacy`; see `gen9::Edition`). Writing re-derives what a file read from disk
lacks (a bound's BVH and polygon order), so take any comparison XML from the
graph after `write_ydr` / `write_ybn`.

Where the reference tool guesses, build is stricter and stops with an error: an unknown
or missing bound `type` (the reference tool makes a sphere), a vertex row with fewer
values than its layout needs, a `CompositeTransform` that is not 16 numbers, a
composite inside a composite, a `<DrawableModelsX>` list next to a LOD list,
a count too big for its 8- or 16-bit field (65536 vertices in a geometry, say),
an XML whose root is not `<Drawable>` (drawable) or `<BoundsFile>` / `<Bounds>`
(bound). What the reference tool accepts silently but the game may not comes back as
`Built::warnings`: a shader parameter naming a texture that is not embedded
(the game looks it up in the archetype's texture dictionary) and a bone
without a name.

A dump→build cycle is not bit-exact for bounds, exactly as the reference tool's own
import is not: a build may move a bound's vertices by up to one quantum per
axis (they are stored as 16-bit multiples of the quantum, which is recomputed
from the box), and the drift can add up over repeated cycles; a `GeometryBVH`'s
polygons are put back in the order its rebuilt BVH gives them. The `_checked`
functions compare XML dumped with no DDS folder, so texture pixels are not part
of the comparison, only the texture's fields. A dump writes each embedded
texture as a plain file name inside the folder, whatever the texture is
called: path separators, `: * ? " < > |` and control characters become `_`
(`blocks::texture::dds_file_name`), and `<FileName>` says the same name.

A corrupt or crafted file is an error, never a panic: a pointer that lands on a
block already read as another type is refused (a plain array or vertex data
there is read again on its own, as the reference tool's `ResourceDataReader` does, which some modded
files need).

## Features

| Feature | Default | Effect |
|---|:-:|---|
| `image` | on | `to_rgba_image`, `fit_max_size`, `encode_image`, `ImageFormat`, re-exports `image` |
| `test-support` | off | exposes `ydd::tests::minimal_ydr_sections`, `ytyp::tests::minimal_mlo_ytyp`, `pso::tests::sample_pso` and `meta_schema::tests::sample_meta` for downstream tests |

## Testing

`cargo test` runs on hand-built byte fixtures and needs no game. The
integration tests in `tests/real_files.rs` are `#[ignore]` and read real
files from environment variables:

```sh
RAGE_TEST_YNV='C:\...\navmesh[108][96].ynv' \
RAGE_TEST_YND='C:\...\nodes489.ynd' \
RAGE_TEST_YBN='C:\...\stream\ybn\interior.ybn' \
RAGE_TEST_YMAP='C:\...\stream\ymap\interior_milo_.ymap' \
RAGE_TEST_MLO_DIR='C:\...\stream' \
RAGE_TEST_YMF='C:\...\stream\_manifest.ymf' \
RAGE_TEST_YTYP='C:\...\stream\props.ytyp' \
RAGE_TEST_HEIGHTMAP='C:\...\heightmap.dat' \
RAGE_TEST_WATER='C:\...\water.xml' \
cargo test --test real_files -- --ignored --nocapture
```

They print what they found, assert the navmesh, path node and heightmap
round trips (`RAGE_TEST_YND` may name a folder of cells, `RAGE_TEST_HEIGHTMAP`
a folder of heightmaps), and check that the generic dump of the map and type
files names every structure member.

## License

[Unlicense](LICENSE), public domain.
