//! Models (YDR / YDD / YFT): inspection (LODs, shaders, textures used) and mesh extraction / OBJ export.
//! Parsing is `rage-formats`' (Unlicense); this module only reads its structures. Untested on retail vehicles so far.
pub use rage_formats::ydd::LodLevel;
use anyhow::{bail, Context, Result};
use rage_formats::{
    names::NameTable,
    texture_utils::to_rgba_image,
    math::{Mat4, Vec3},
    ydd::{parse_drawables, Drawable, DrawableEntry, DrawableKind, ShaderParameterValue},
    ytd::YtdTexture,
};
use std::{collections::BTreeSet, fmt::Write as _, fs, path::Path};

#[derive(Debug, Clone)]
pub struct LodStat {
    pub level: String,
    /// distance at which this LOD stops being used (game units)
    pub distance: f32,
    pub models: usize,
    pub geometries: usize,
    pub vertices: usize,
    pub triangles: usize,
}

#[derive(Debug, Clone)]
pub struct ShaderStat {
    pub id: usize,
    pub name: String,
    /// (parameter, texture name)
    pub textures: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct DrawableReport {
    pub name: String,
    pub size: [f32; 3],
    pub lods: Vec<LodStat>,
    pub shaders: Vec<ShaderStat>,
    pub embedded_textures: Vec<String>,
}

/// Vehicle shader names (the game stores only their hash). Verified against the shader hashes of real base-game helicopters.
const VEHICLE_SHADERS: &[&str] = &[
    "vehicle_basic", "vehicle_badges", "vehicle_blurredrotor", "vehicle_blurredrotor_emissive", "vehicle_cloth", "vehicle_cloth2", "vehicle_cutout",
    "vehicle_dash_emissive", "vehicle_dash_emissive_opaque", "vehicle_decal", "vehicle_decal2", "vehicle_detail", "vehicle_detail2", "vehicle_emissive",
    "vehicle_emissive_alpha", "vehicle_emissive_opaque", "vehicle_generic", "vehicle_interior", "vehicle_interior2", "vehicle_licenseplate", "vehicle_lights",
    "vehicle_lightsemissive", "vehicle_mesh", "vehicle_mesh2_enveff", "vehicle_mesh_enveff", "vehicle_nosplash", "vehicle_shuts", "vehicle_tire", "vehicle_track",
    "vehicle_track2", "vehicle_track_ammo", "vehicle_track_emissive", "vehicle_vehglass", "vehicle_vehglass_inner", "vehicle_paint1", "vehicle_paint1_enveff",
    "vehicle_paint2", "vehicle_paint2_enveff", "vehicle_paint3", "vehicle_paint3_enveff", "vehicle_paint3_lvr", "vehicle_paint3_cutout", "vehicle_paint4",
    "vehicle_paint4_emissive", "vehicle_paint4_enveff", "vehicle_paint5", "vehicle_paint5_enveff", "vehicle_paint6", "vehicle_paint6_enveff", "vehicle_paint7",
    "vehicle_paint7_enveff", "vehicle_paint8", "vehicle_paint9", "vehicle_tire_emissive", "vehicle_dials_emissive", "vehicle_mesh_alpha", "vehicle_cloth_normal",
    "ped", "ped_alpha", "ped_default", "ped_default_cloth", "ped_decal", "ped_decal_decoration", "ped_hair_spiked", "ped_hair_cutout_alpha", "normal", "normal_spec",
    "normal_spec_alpha", "spec", "default", "trees", "decal", "glass", "glass_pv",
];

/// the shader's name when it is a known one
pub fn shader_name(hash: u32) -> Option<&'static str> {
    static MAP: std::sync::OnceLock<std::collections::HashMap<u32, &'static str>> = std::sync::OnceLock::new();
    MAP.get_or_init(|| VEHICLE_SHADERS.iter().map(|n| (rage_formats::rage_joaat(n), *n)).collect()).get(&hash).copied()
}

fn kind_of(path: &Path) -> Result<DrawableKind> {
    let ext = path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
    DrawableKind::from_extension(&ext).with_context(|| format!("'{}' is not a .ydr/.ydd/.yft file", path.display()))
}

pub fn load(path: &Path) -> Result<Vec<DrawableEntry>> {
    parse_drawables(&fs::read(path)?, kind_of(path)?).with_context(|| format!("parsing {}", path.display()))
}

const LEVELS: [LodLevel; 4] = [LodLevel::High, LodLevel::Medium, LodLevel::Low, LodLevel::VeryLow];

fn report(e: &DrawableEntry, names: &NameTable) -> DrawableReport {
    let d: &Drawable = &e.drawable;
    let lods = d
        .lods
        .iter()
        .map(|l| {
            let geoms: Vec<_> = l.models.iter().flat_map(|m| m.geometries.iter()).collect();
            let idx = LEVELS.iter().position(|x| *x == l.level).unwrap_or(0);
            LodStat {
                level: l.level.to_string(),
                distance: d.lod_distances[idx],
                models: l.models.len(),
                geometries: geoms.len(),
                vertices: geoms.iter().map(|g| g.vertices_count as usize).sum(),
                triangles: geoms.iter().map(|g| g.triangles_count as usize).sum(),
            }
        })
        .collect();
    let shaders = d
        .shader_group
        .iter()
        .flat_map(|g| g.shaders.iter().enumerate())
        .map(|(id, s)| ShaderStat {
            id,
            name: shader_name(s.name_hash).map(str::to_string).unwrap_or_else(|| names.resolve(s.name_hash).into_owned()),
            textures: s
                .parameters
                .iter()
                .filter_map(|p| match &p.value {
                    ShaderParameterValue::Texture { name, .. } if !name.is_empty() => Some((names.resolve(p.name_hash).into_owned(), name.clone())),
                    _ => None,
                })
                .collect(),
        })
        .collect();
    let embedded_textures = d.shader_group.iter().flat_map(|g| g.textures.iter().map(|t| t.name.clone())).collect();
    let (b, _) = d.best_lod().map(|l| d.bounds_or_computed(l)).unwrap_or_else(|| (d.bounds.clone(), false));
    DrawableReport {
        name: if e.name.is_empty() { d.name.clone() } else { e.name.clone() },
        size: [b.box_max.x - b.box_min.x, b.box_max.y - b.box_min.y, b.box_max.z - b.box_min.z],
        lods,
        shaders,
        embedded_textures,
    }
}

pub fn inspect(path: &Path) -> Result<Vec<DrawableReport>> {
    let names = NameTable::core();
    Ok(load(path)?.iter().map(|e| report(e, &names)).collect())
}

/// textures a model names (any shader parameter) that are neither embedded nor present in `available` (a YTD's texture names)
pub fn missing_textures(reports: &[DrawableReport], available: &[String]) -> Vec<String> {
    let have: BTreeSet<String> = available.iter().map(|s| s.to_lowercase()).collect();
    let mut miss = BTreeSet::new();
    for r in reports {
        let emb: BTreeSet<String> = r.embedded_textures.iter().map(|s| s.to_lowercase()).collect();
        for s in &r.shaders {
            for (_, t) in &s.textures {
                let l = t.to_lowercase();
                if !have.contains(&l) && !emb.contains(&l) {
                    miss.insert(t.clone());
                }
            }
        }
    }
    miss.into_iter().collect()
}

pub fn ytd_names(ytd: &Path) -> Result<Vec<String>> {
    Ok(crate::pipeline::read_ytd(ytd)?.0.into_iter().map(|t| t.name).collect())
}

// ---- meshes ----------------------------------------------------------------------------------------------------

/// One bone of a model's skeleton (a vehicle's: chassis, bonnet, door_dside_f, wheel_lf, ...).
#[derive(Debug, Clone)]
pub struct Bone {
    pub index: usize,
    pub name: String,
    /// the id a fragment's physics children name their bone by
    pub tag: u16,
    pub parent: Option<usize>,
}

/// The skeleton of a fragment's main drawable (bones in index order). Empty when the file has none or it cannot be read.
pub fn fragment_bones(data: &[u8]) -> Vec<Bone> {
    use rage_formats::resource::{prepare_rsc7, u16_le, u64_le, ResReader, SYSTEM_BASE};
    let read = || -> Option<Vec<Bone>> {
        let (system, graphics) = prepare_rsc7(data).ok()?;
        let r = ResReader { system: &system, graphics: &graphics };
        // FragType +0x30 -> main drawable, drawable +0x18 -> skeleton, skeleton +0x20 -> first bone, +0x5E -> bone count
        let fragment = r.resolve(SYSTEM_BASE, 0x60)?;
        let drawable = r.resolve(u64_le(fragment, 0x30), 0x20)?;
        let skeleton = r.resolve(u64_le(drawable, 0x18), 0x68)?;
        let (first, count) = (u64_le(skeleton, 0x20), u16_le(skeleton, 0x5E) as usize);
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            // 80 bytes each: parent index at +0x32, name pointer at +0x38, tag at +0x44
            let b = r.resolve(first + 80 * i as u64, 80)?;
            let parent = i16::from_le_bytes([b[0x32], b[0x33]]);
            out.push(Bone { index: i, name: r.string_at(u64_le(b, 0x38)).unwrap_or_default(), tag: u16_le(b, 0x44), parent: usize::try_from(parent).ok() });
        }
        Some(out)
    };
    read().unwrap_or_default()
}

pub struct MeshPart {
    pub drawable: String,
    /// What this part is: for a vehicle fragment the bone it hangs off (chassis, wheel_lf, bodyshell, ...), otherwise the
    /// drawable's name.
    pub part: String,
    pub model: usize,
    pub geometry: usize,
    pub shader_id: u16,
    pub diffuse: Option<String>,
    /// normal map / specular map texture names, when the shader names them
    pub bump: Option<String>,
    pub spec: Option<String>,
    /// resolved shader name, or `hash_XXXXXXXX` when unknown
    pub shader: String,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub tangents: Vec<[f32; 4]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    /// For a skinned geometry: the skeleton bone most of its vertices follow (the heaviest blend weight of each vertex, mapped
    /// through the geometry's bone palette). `None` for a rigid one.
    pub skin_bone: Option<usize>,
}

/// The bone most vertices of a skinned geometry follow.
fn dominant_bone(vs: &[rage_formats::ydd::UnifiedVertex], palette: &[u16]) -> Option<usize> {
    if palette.is_empty() {
        return None;
    }
    let mut votes: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for v in vs {
        let w = [v.blend_weights.x, v.blend_weights.y, v.blend_weights.z, v.blend_weights.w];
        let k = (0..4).max_by(|&a, &b| w[a].partial_cmp(&w[b]).unwrap_or(std::cmp::Ordering::Equal)).unwrap_or(0);
        if let Some(&bone) = palette.get(v.blend_indices[k] as usize) {
            *votes.entry(bone as usize).or_default() += 1;
        }
    }
    // ties go to the lower bone index so the answer does not depend on hash order
    votes.into_iter().max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0))).map(|(b, _)| b)
}

/// Triangles of one drawable at `level`. Each part carries the bone index and skinning flag of its model.
pub fn drawable_meshes(d: &Drawable, name: &str, level: LodLevel) -> Vec<(MeshPart, usize, bool)> {
    let mut out = vec![];
    let Some(lod) = d.lod(level).filter(|l| !l.models.is_empty()) else { return out };
    for (mi, m) in lod.models.iter().enumerate() {
        for (gi, g) in m.geometries.iter().enumerate() {
            let (Some(vb), Some(ib)) = (&g.vertex_buffer, &g.index_buffer) else { continue };
            let Ok(vs) = vb.to_unified_vertices() else { continue };
            if vs.is_empty() || ib.indices.iter().any(|&i| i as usize >= vs.len()) {
                continue;
            }
            out.push((
                MeshPart {
                    drawable: name.to_string(),
                    part: name.to_string(),
                    model: mi,
                    geometry: gi,
                    shader_id: g.shader_id,
                    diffuse: d.diffuse_texture_name(g.shader_id).map(str::to_string),
                    bump: d.texture_parameter(g.shader_id, rage_formats::rage_joaat("bumpsampler")).map(str::to_string),
                    spec: d.texture_parameter(g.shader_id, rage_formats::rage_joaat("specsampler")).map(str::to_string),
                    shader: d.shader(g.shader_id).map(|sh| shader_name(sh.name_hash).map(str::to_string).unwrap_or_else(|| format!("hash_{:08X}", sh.name_hash))).unwrap_or_default(),
                    tangents: vs.iter().map(|v| [v.tangent.x, v.tangent.y, v.tangent.z, if v.tangent.w == 0.0 { 1.0 } else { v.tangent.w }]).collect(),
                    positions: vs.iter().map(|v| [v.position.x, v.position.y, v.position.z]).collect(),
                    normals: vs.iter().map(|v| [v.normal.x, v.normal.y, v.normal.z]).collect(),
                    uvs: vs.iter().map(|v| [v.texcoord0.x, v.texcoord0.y]).collect(),
                    indices: ib.indices.clone(),
                    skin_bone: if m.is_skinned() { dominant_bone(&vs, &g.bone_ids) } else { None },
                },
                m.bone_index(),
                m.is_skinned(),
            ));
        }
    }
    out
}

/// Triangles of `level` for every drawable in a YDR/YDD (no placement).
pub fn meshes(entries: &[DrawableEntry], level: LodLevel) -> Vec<MeshPart> {
    entries
        .iter()
        .flat_map(|e| drawable_meshes(&e.drawable, if e.name.is_empty() { &e.drawable.name } else { &e.name }, level))
        .map(|(p, _, _)| p)
        .collect()
}

fn xf_point(m: &Mat4, p: [f32; 3]) -> [f32; 3] {
    [
        m.0[0] * p[0] + m.0[4] * p[1] + m.0[8] * p[2] + m.0[12],
        m.0[1] * p[0] + m.0[5] * p[1] + m.0[9] * p[2] + m.0[13],
        m.0[2] * p[0] + m.0[6] * p[1] + m.0[10] * p[2] + m.0[14],
    ]
}

fn xf_dir(m: &Mat4, n: [f32; 3]) -> [f32; 3] {
    let v = m.transform_vector(Vec3::new(n[0], n[1], n[2]));
    let l = (v.x * v.x + v.y * v.y + v.z * v.z).sqrt();
    if l > 1e-6 { [v.x / l, v.y / l, v.z / l] } else { n }
}

/// Parts named after a bone that several geometries share become `bodyshell`, `bodyshell 2`, `bodyshell 3`, ... so each row of the
/// Parts list has its own name (parts named after their drawable keep their model / geometry numbers).
fn number_repeated_parts(parts: &mut [MeshPart]) {
    let mut total: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for p in parts.iter().filter(|p| p.part != p.drawable) {
        *total.entry(p.part.clone()).or_default() += 1;
    }
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for p in parts.iter_mut().filter(|p| p.part != p.drawable) {
        if total[&p.part] > 1 {
            let n = seen.entry(p.part.clone()).or_default();
            *n += 1;
            if *n > 1 {
                p.part = format!("{} {}", p.part, n);
            }
        }
    }
}

/// Like [`meshes`] for any model file; fragments (.yft) are assembled the way the crate's renderer does it:
/// wheels, doors etc. at their placement, right-hand wheels mirrored. **Untested on real vehicle fragments.**
pub fn placed_meshes(path: &Path, level: LodLevel) -> Result<Vec<MeshPart>> {
    let data = fs::read(path)?;
    if !matches!(kind_of(path)?, DrawableKind::Yft) {
        return Ok(meshes(&load(path)?, level));
    }
    let fragment = rage_formats::yft::parse_yft(&data).with_context(|| format!("parsing {}", path.display()))?;
    let mut out = vec![];
    let bones = fragment_bones(&data);
    for part in fragment.render_parts() {
        let name = if part.drawable.name.is_empty() { fragment.name.as_str() } else { part.drawable.name.as_str() };
        for (mut m, bone, skinned) in drawable_meshes(part.drawable, name, level) {
            // the main drawable's models are bound to a bone by index, a physics child's drawable by the child's bone tag
            let bone_name = match part.bone_tag {
                Some(tag) => bones.iter().find(|b| b.tag == tag),
                None => bones.get(bone),
            };
            // a skinned geometry follows the bone its vertices are weighted to (bodyshell, door_dside_f, ...); the model's own
            // binding is the same bone for all of them (chassis)
            let by_skin = m.skin_bone.and_then(|i| bones.get(i)).filter(|b| !b.name.is_empty());
            if let Some(b) = by_skin.or(bone_name.filter(|b| !b.name.is_empty())) {
                m.part = b.name.clone();
            }
            let bone_xf = (!skinned).then(|| part.bone_transforms.get(bone)).flatten();
            for p in &mut m.positions {
                if let Some(b) = bone_xf {
                    *p = xf_point(b, *p);
                }
                *p = xf_point(&part.transform, *p);
            }
            for n in &mut m.normals {
                if let Some(b) = bone_xf {
                    *n = xf_dir(b, *n);
                }
                *n = xf_dir(&part.transform, *n);
            }
            for t in &mut m.tangents {
                let mut d = [t[0], t[1], t[2]];
                if let Some(b) = bone_xf {
                    d = xf_dir(b, d);
                }
                d = xf_dir(&part.transform, d);
                *t = [d[0], d[1], d[2], t[3]];
            }
            out.push(m);
        }
    }
    number_repeated_parts(&mut out);
    Ok(out)
}

/// Textures a model can use: the given YTDs first (any edition), then the ones embedded in the model.
pub fn load_textures(model: &Path, ytds: &[std::path::PathBuf]) -> Result<Vec<YtdTexture>> {
    let mut out = vec![];
    for y in ytds {
        out.extend(crate::pipeline::read_ytd(y)?.0);
    }
    for e in load(model)? {
        if let Some(g) = &e.drawable.shader_group {
            out.extend(g.textures.iter().cloned());
        }
    }
    Ok(out)
}

/// Like [`load_textures`] but remembers which YTD each texture came from (None = embedded in the model), and the model is optional
/// so a texture dictionary can be opened on its own.
pub fn load_textures_sourced(model: Option<&Path>, ytds: &[std::path::PathBuf]) -> Result<Vec<(YtdTexture, Option<std::path::PathBuf>)>> {
    let mut out = vec![];
    for y in ytds {
        out.extend(crate::pipeline::read_ytd(y)?.0.into_iter().map(|t| (t, Some(y.clone()))));
    }
    if let Some(m) = model {
        for e in load(m)? {
            if let Some(g) = &e.drawable.shader_group {
                out.extend(g.textures.iter().cloned().map(|t| (t, None)));
            }
        }
    }
    Ok(out)
}

/// width, height, RGBA8 pixels (top mip)
pub fn texture_rgba(t: &YtdTexture) -> Result<(u32, u32, Vec<u8>)> {
    let img = to_rgba_image(t)?;
    Ok((img.width(), img.height(), img.into_raw()))
}

fn safe(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect()
}

/// Wavefront OBJ + MTL (+ PNGs of the textures found in `ytd`) so any viewer can show model and textures.
/// GTA is Z-up, OBJ viewers are Y-up: positions are written as (x, z, -y), V is flipped.
pub fn export_obj(model: &Path, out_dir: &Path, ytd: Option<&Path>, level: LodLevel) -> Result<String> {
    let entries = load(model)?;
    let parts = placed_meshes(model, level)?;
    if parts.is_empty() {
        bail!("no {level} LOD geometry with readable vertices in {}", model.display());
    }
    fs::create_dir_all(out_dir)?;
    let stem = safe(&model.file_stem().unwrap().to_string_lossy());
    // textures: external YTD first, then embedded ones
    let mut textures: Vec<YtdTexture> = match ytd {
        Some(p) => crate::pipeline::read_ytd(p)?.0,
        None => vec![],
    };
    for e in &entries {
        if let Some(g) = &e.drawable.shader_group {
            textures.extend(g.textures.iter().cloned());
        }
    }
    let mut written = BTreeSet::new();
    let mut png_for = |name: &str| -> Option<String> {
        let t = textures.iter().find(|t| t.name.eq_ignore_ascii_case(name))?;
        let file = format!("{}.png", safe(&t.name));
        if written.insert(file.clone()) {
            fs::create_dir_all(out_dir.join("textures")).ok()?;
            to_rgba_image(t).ok()?.save(out_dir.join("textures").join(&file)).ok()?;
        }
        Some(file)
    };

    let mut obj = format!("# CRCK Explorer export of {}\nmtllib {stem}.mtl\n", model.file_name().unwrap().to_string_lossy());
    let mut mtl = String::new();
    let mut declared = BTreeSet::new();
    let mut base = 1usize;
    let (mut tris, mut verts) = (0usize, 0usize);
    for p in &parts {
        let mat = safe(p.diffuse.as_deref().unwrap_or("untextured"));
        if declared.insert(mat.clone()) {
            let _ = writeln!(mtl, "newmtl {mat}\nKd 1 1 1");
            if let Some(png) = p.diffuse.as_deref().and_then(&mut png_for) {
                let _ = writeln!(mtl, "map_Kd textures/{png}");
            }
            mtl.push('\n');
        }
        let _ = writeln!(obj, "o {}_m{}_g{}\nusemtl {mat}", safe(&p.drawable), p.model, p.geometry);
        for q in &p.positions {
            let _ = writeln!(obj, "v {} {} {}", q[0], q[2], -q[1]);
        }
        for n in &p.normals {
            let _ = writeln!(obj, "vn {} {} {}", n[0], n[2], -n[1]);
        }
        for t in &p.uvs {
            let _ = writeln!(obj, "vt {} {}", t[0], 1.0 - t[1]);
        }
        for tri in p.indices.chunks_exact(3) {
            let f = |i: u32| {
                let k = base + i as usize;
                format!("{k}/{k}/{k}")
            };
            let _ = writeln!(obj, "f {} {} {}", f(tri[0]), f(tri[1]), f(tri[2]));
        }
        base += p.positions.len();
        verts += p.positions.len();
        tris += p.indices.len() / 3;
    }
    fs::write(out_dir.join(format!("{stem}.obj")), obj)?;
    fs::write(out_dir.join(format!("{stem}.mtl")), mtl)?;
    Ok(format!("{} part(s), {verts} vertices, {tris} triangles, {} texture file(s) -> {}", parts.len(), written.len(), out_dir.join(format!("{stem}.obj")).display()))
}
