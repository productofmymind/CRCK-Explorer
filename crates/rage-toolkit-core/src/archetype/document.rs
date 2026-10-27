//! An open `.ytyp`: the file's values as a `MetaValue` tree (so what the editor does not know about is kept), the edits made to it with
//! undo and redo, and safe saving. The tree is edited by path (`edit`), the fields are described by the game's tables (`schema_view`).
//! Saving builds the file with the Meta or PSO writer, reads it back and compares it with the tree, so a file the writer would
//! damage is refused instead of written.
use super::{
    edit::{self, field, Command, History, Path, Step},
    fidelity::{compare_trees, Container},
    schema_view::{self, FieldKind},
};
use anyhow::{bail, Context, Result};
use rage_formats::{build_meta, build_pso, dump_meta, dump_pso, from_xml, is_pso, rage_joaat, to_xml, MetaValue, NameTable, Schema};
use rage_formats::value::MetaStruct;
use std::{
    fs,
    path::{Path as FsPath, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchKind {
    /// `CBaseArchetypeDef`: a prop or building
    Base,
    /// `CTimeArchetypeDef`: shown only at some hours
    Time,
    /// `CMloArchetypeDef`: an interior with rooms and portals
    Mlo,
    Other,
}

impl ArchKind {
    pub fn of(v: &MetaValue) -> ArchKind {
        match v.as_struct().map(|s| s.type_hash) {
            Some(h) if h == rage_joaat("CBaseArchetypeDef") => ArchKind::Base,
            Some(h) if h == rage_joaat("CTimeArchetypeDef") => ArchKind::Time,
            Some(h) if h == rage_joaat("CMloArchetypeDef") => ArchKind::Mlo,
            _ => ArchKind::Other,
        }
    }

    pub fn struct_name(self) -> &'static str {
        match self {
            ArchKind::Base | ArchKind::Other => "CBaseArchetypeDef",
            ArchKind::Time => "CTimeArchetypeDef",
            ArchKind::Mlo => "CMloArchetypeDef",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ArchKind::Base => "archetype",
            ArchKind::Time => "time archetype",
            ArchKind::Mlo => "MLO",
            ArchKind::Other => "other",
        }
    }
}

pub struct YtypDocument {
    pub root: MetaValue,
    pub container: Container,
    pub path: Option<PathBuf>,
    /// what the reader could not decode (kept in the tree as raw bytes)
    pub read_warnings: Vec<String>,
    /// the game's names plus the ones typed in this session, to show hashes as text
    pub names: NameTable,
    schema: Schema,
    history: History,
}

fn archetypes_path() -> Path {
    vec![field("archetypes")]
}

pub fn archetype_path(i: usize) -> Path {
    vec![field("archetypes"), Step::Index(i)]
}

impl YtypDocument {
    /// Open the bytes of a `.ytyp` (RSC7 Meta or PSO) or of the reference tool-style XML.
    pub fn open(data: &[u8], path: Option<PathBuf>) -> Result<Self> {
        let text = std::str::from_utf8(data).ok().map(|t| t.trim_start_matches('\u{feff}').trim_start());
        let mut schema = Schema::builtin().clone();
        let (root, container, warnings) = if let Some(t) = text.filter(|t| t.starts_with('<')) {
            (from_xml(t).context("reading the XML")?, Container::Meta, vec![])
        } else {
            if let Ok(own) = Schema::from_file(data) {
                schema.merge(own);
            }
            if is_pso(data) {
                let d = dump_pso(data).context("reading the PSO file")?;
                (d.root, Container::Pso, d.warnings)
            } else {
                let d = dump_meta(data).context("reading the file")?;
                (d.root, Container::Meta, d.warnings)
            }
        };
        let mut root = root;
        if text.map(|t| t.starts_with('<')).unwrap_or(false) {
            schema_view::coerce_tree(&mut root, rage_joaat("CMapTypes"));
        }
        match root.as_struct() {
            Some(s) if s.type_hash == rage_joaat("CMapTypes") => {}
            _ => bail!("this is not an archetype file: its root is not CMapTypes"),
        }
        Ok(Self { root, container, path, read_warnings: warnings, names: NameTable::core(), schema, history: History::default() })
    }

    pub fn open_file(path: &FsPath) -> Result<Self> {
        let data = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::open(&data, Some(path.to_path_buf()))
    }

    /// A new, empty archetype file called `name`.
    pub fn new_file(name: &str) -> Self {
        let root = MetaValue::Struct(MetaStruct {
            type_hash: rage_joaat("CMapTypes"),
            fields: vec![
                (rage_joaat("extensions"), MetaValue::Array(Default::default())),
                (rage_joaat("archetypes"), MetaValue::Array(rage_formats::value::MetaArray { item_type: None, typed_items: true, items: vec![] })),
                (rage_joaat("name"), MetaValue::Hash(schema_view::name_hash(name))),
            ],
        });
        let mut names = NameTable::core();
        names.add_lowercase(name);
        Self { root, container: Container::Meta, path: None, read_warnings: vec![], names, schema: Schema::builtin().clone(), history: History::default() }
    }

    // ---- reading --------------------------------------------------------------------------------------------------------

    pub fn get(&self, path: &[Step]) -> Option<&MetaValue> {
        edit::get(&self.root, path)
    }

    pub fn archetypes(&self) -> &[MetaValue] {
        self.get(&archetypes_path()).map(|v| v.items()).unwrap_or(&[])
    }

    pub fn len(&self) -> usize {
        self.archetypes().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// the name hash of archetype `i` (`name`, or `assetName` when that is 0)
    pub fn name_hash(&self, i: usize) -> u32 {
        let s = self.archetypes().get(i).and_then(|a| a.as_struct());
        let n = s.and_then(|s| s.field("name")).and_then(|v| v.as_hash()).unwrap_or(0);
        if n != 0 {
            n
        } else {
            s.and_then(|s| s.field("assetName")).and_then(|v| v.as_hash()).unwrap_or(0)
        }
    }

    pub fn name(&self, i: usize) -> String {
        let h = self.name_hash(i);
        if h == 0 {
            "(no name)".into()
        } else {
            self.names.resolve(h).into_owned()
        }
    }

    pub fn kind(&self, i: usize) -> ArchKind {
        self.archetypes().get(i).map(ArchKind::of).unwrap_or(ArchKind::Other)
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// A copy to try changes on (a merge preview): the same data, a fresh history, nothing written anywhere.
    pub fn fork(&self) -> YtypDocument {
        YtypDocument { root: self.root.clone(), container: self.container, path: None, read_warnings: vec![], names: self.names.clone(), schema: self.schema.clone(), history: History::default() }
    }

    // ---- editing --------------------------------------------------------------------------------------------------------

    /// Do one change. `key`: changes with the same key one after another are one undo step.
    pub fn run(&mut self, cmd: Command, label: &str, key: Option<String>) -> Result<()> {
        self.history.run(&mut self.root, cmd, label, key)
    }

    pub fn undo(&mut self) -> Result<()> {
        self.history.undo(&mut self.root)
    }

    pub fn redo(&mut self) -> Result<()> {
        self.history.redo(&mut self.root)
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.history.undo_label()
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.history.redo_label()
    }

    pub fn is_dirty(&self) -> bool {
        self.history.is_dirty()
    }

    /// how many undo steps there are: changes whenever the file is edited (for caches)
    pub fn undo_steps(&self) -> usize {
        self.history.steps()
    }

    /// Set the value at `path` from typed text, by the kind the table gives that member.
    pub fn set_text(&mut self, path: &Path, kind: &FieldKind, text: &str) -> Result<()> {
        let old = self.get(path).cloned();
        let value = schema_view::parse_text(kind, text, old.as_ref())?;
        if matches!(kind, FieldKind::Hash) {
            self.names.add_lowercase(text.trim());
        }
        let label = format!("Edit {}", path.last().map(|s| if let Step::Field(h) = s { schema_view::name_of(*h) } else { "item".into() }).unwrap_or_default());
        let key = format!("{path:?}");
        self.run(Command::Set(path.clone(), value), &label, Some(key))
    }

    /// A new archetype of `kind` called `name`, added at the end. Returns its index.
    pub fn add_archetype(&mut self, kind: ArchKind, name: &str) -> Result<usize> {
        let name = name.trim();
        if name.is_empty() {
            bail!("the archetype needs a name");
        }
        let item = self.new_archetype_value(kind, name);
        let at = self.len();
        self.names.add_lowercase(name);
        self.run(Command::Insert(archetypes_path(), at, item), "Add archetype", None)?;
        Ok(at)
    }

    /// A copy of archetype `i` under a new name, inserted right after it. Returns the copy's index.
    pub fn duplicate_archetype(&mut self, i: usize, new_name: &str) -> Result<usize> {
        let new_name = new_name.trim();
        if new_name.is_empty() {
            bail!("the copy needs a name");
        }
        let mut copy = self.archetypes().get(i).cloned().context("no such archetype")?;
        let old = self.name_hash(i);
        let new = schema_view::name_hash(new_name);
        if let MetaValue::Struct(s) = &mut copy {
            for (h, v) in s.fields.iter_mut() {
                // the name, and the asset name when it was the same as the name
                if (*h == rage_joaat("name") || *h == rage_joaat("assetName")) && v.as_hash() == Some(old) {
                    *v = MetaValue::Hash(new);
                }
            }
        }
        self.names.add_lowercase(new_name);
        self.run(Command::Insert(archetypes_path(), i + 1, copy), "Duplicate archetype", None)?;
        Ok(i + 1)
    }

    /// Replace archetype `i` by the archetype in `xml` (the text of the XML tab): parsed, made to hold what the game's table says
    /// (hashed names, enum members, list item types), checked to be an archetype, and set as one undo step.
    pub fn replace_archetype_from_xml(&mut self, i: usize, xml: &str) -> Result<()> {
        if i >= self.len() {
            bail!("no such archetype");
        }
        let mut v = from_xml(xml.trim_start_matches('\u{feff}').trim()).context("the XML is not valid")?;
        let Some(h) = v.as_struct().map(|s| s.type_hash) else { bail!("the XML must be one archetype, for example <CBaseArchetypeDef> ... </CBaseArchetypeDef>") };
        if ![rage_joaat("CBaseArchetypeDef"), rage_joaat("CTimeArchetypeDef"), rage_joaat("CMloArchetypeDef")].contains(&h) {
            bail!("the root element must be CBaseArchetypeDef, CTimeArchetypeDef or CMloArchetypeDef, not {}", schema_view::name_of(h));
        }
        schema_view::coerce_tree(&mut v, h);
        self.run(Command::Set(archetype_path(i), v), "Edit archetype XML", None)
    }

    pub fn remove_archetype(&mut self, i: usize) -> Result<()> {
        if i >= self.len() {
            bail!("no such archetype");
        }
        self.run(Command::Remove(archetypes_path(), i), "Remove archetype", None)
    }

    /// Give archetype `i` a new name; `assetName` follows when it was the same as the old name.
    pub fn rename_archetype(&mut self, i: usize, new_name: &str) -> Result<()> {
        let new_name = new_name.trim();
        if new_name.is_empty() {
            bail!("the archetype needs a name");
        }
        let old = self.name_hash(i);
        let new = schema_view::name_hash(new_name);
        let base = archetype_path(i);
        let with = |this: &mut Self, f: &str| -> Result<()> {
            let mut p = base.clone();
            p.push(field(f));
            this.run(Command::Set(p, MetaValue::Hash(new)), "Rename archetype", None)
        };
        self.names.add_lowercase(new_name);
        with(self, "name")?;
        let asset = self.get(&[base.clone(), vec![field("assetName")]].concat()).and_then(|v| v.as_hash());
        if asset == Some(old) {
            with(self, "assetName")?;
        }
        Ok(())
    }

    /// The value of a new archetype: every member of its structure (from the game's table) empty, then the ones a prop needs set.
    pub fn new_archetype_value(&self, kind: ArchKind, name: &str) -> MetaValue {
        let type_hash = rage_joaat(kind.struct_name());
        let h = schema_view::name_hash(name);
        let mut fields: Vec<(u32, MetaValue)> = vec![];
        for f in schema_view::describe(type_hash).unwrap_or_default() {
            let mut v = schema_view::default_value(&f.kind);
            match f.name.as_str() {
                "lodDist" | "hdTextureDist" => v = MetaValue::F32(100.0),
                "flags" => v = MetaValue::U32(32),
                "bbMin" => v = MetaValue::Vec3(rage_formats::Vec3::new(-1.0, -1.0, -1.0)),
                "bbMax" => v = MetaValue::Vec3(rage_formats::Vec3::new(1.0, 1.0, 1.0)),
                "bsRadius" => v = MetaValue::F32(1.7320508),
                "name" | "assetName" | "textureDictionary" => v = MetaValue::Hash(h),
                "assetType" => {
                    if let Ok(e) = schema_view::parse_text(&f.kind, "ASSET_TYPE_DRAWABLE", None) {
                        v = e;
                    }
                }
                _ => {}
            }
            fields.push((f.hash, v));
        }
        MetaValue::Struct(MetaStruct { type_hash, fields })
    }

    /// One line per archetype in the format of the `Spike ytyp` oracle (the reference library's own reading of the file), to compare the two.
    pub fn oracle_summary(&self) -> String {
        // C#'s "0.###" on a float: the shortest digits that read back as the float, rounded half away from zero at 3 decimals
        fn num(v: f32) -> String {
            let shortest = format!("{v}");
            let (neg, body) = match shortest.strip_prefix('-') {
                Some(b) => (true, b.to_string()),
                None => (false, shortest),
            };
            let (int, frac) = body.split_once('.').unwrap_or((&body, ""));
            let mut digits: Vec<u8> = int.bytes().chain(frac.bytes().take(3)).map(|b| b - b'0').collect();
            let keep_frac = frac.len().min(3);
            while digits.len() < int.len() + 3 {
                digits.push(0);
            }
            if frac.as_bytes().get(3).map(|d| d - b'0' >= 5).unwrap_or(false) {
                let mut i = digits.len();
                loop {
                    if i == 0 {
                        digits.insert(0, 1);
                        break;
                    }
                    i -= 1;
                    if digits[i] == 9 {
                        digits[i] = 0;
                    } else {
                        digits[i] += 1;
                        break;
                    }
                }
            }
            let _ = keep_frac;
            let split = digits.len() - 3;
            let int_part: String = digits[..split].iter().map(|d| (d + b'0') as char).collect();
            let frac_part: String = digits[split..].iter().map(|d| (d + b'0') as char).collect();
            let frac_part = frac_part.trim_end_matches('0');
            let s = if frac_part.is_empty() { int_part } else { format!("{int_part}.{frac_part}") };
            if s == "0" || s.is_empty() {
                // .NET prints a negative number that rounds to nothing as -0
                if neg { "-0".into() } else { "0".into() }
            } else if neg {
                format!("-{s}")
            } else {
                s
            }
        }
        let mut out = format!("archetypes {}\n", self.len());
        for a in self.archetypes() {
            let Some(s) = a.as_struct() else { continue };
            let kind = match ArchKind::of(a) {
                ArchKind::Mlo => "mlo",
                ArchKind::Time => "time",
                _ => "base",
            };
            let h = |n: &str| s.field(n).and_then(|v| v.as_hash()).unwrap_or(0);
            let name = if h("name") != 0 { h("name") } else { h("assetName") };
            let bb = s.field("bbMin").and_then(|v| v.as_vec3()).map(|v| format!("{},{},{}", num(v.x), num(v.y), num(v.z))).unwrap_or_else(|| "0,0,0".into());
            let flags = s.field("flags").and_then(|v| v.as_i64()).unwrap_or(0);
            out += &format!("{kind} {name:08x} lod={} flags={flags} bbMin={bb} txd={:08x} asset={:08x}", num(s.field("lodDist").and_then(|v| v.as_f32()).unwrap_or(0.0)), h("textureDictionary"), h("assetName"));
            if kind == "mlo" {
                let n = |f: &str| s.field(f).map(|v| v.items().len()).unwrap_or(0);
                out += &format!(" rooms={} portals={} entities={}", n("rooms"), n("portals"), n("entities"));
            }
            out.push('\n');
        }
        out
    }

    // ---- saving ---------------------------------------------------------------------------------------------------------

    /// The file's bytes. Built with the Meta or PSO writer, read back and compared with the tree: an error says what would be lost.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let built = match self.container {
            Container::Meta => build_meta(&self.root, &self.schema),
            Container::Pso => build_pso(&self.root, &self.schema),
        }
        .context("building the file")?;
        if let Some(w) = built.warnings.first() {
            bail!("the writer cannot save part of this file: {w}");
        }
        let back = match self.container {
            Container::Meta => dump_meta(&built.bytes),
            Container::Pso => dump_pso(&built.bytes),
        }
        .context("reading the saved bytes back")?;
        let (diffs, total) = compare_trees(&self.root, &back.root, &self.names);
        if let Some(first) = diffs.first() {
            bail!("saving would change the file: {total} value(s) differ, the first at {}{} ({} became {}); nothing was written", first.path, if first.path.is_empty() { "(root)" } else { "" }, first.before, first.after);
        }
        Ok(built.bytes)
    }

    /// The reference tool-style XML of the whole file, or of one archetype.
    pub fn to_xml(&self, archetype: Option<usize>) -> String {
        match archetype.and_then(|i| self.archetypes().get(i)) {
            Some(a) => to_xml(a, &self.names),
            None => to_xml(&self.root, &self.names),
        }
    }

    /// Save to `path`: the first overwrite keeps the old file as `.bak`; the new one is written to a temporary file and moved over.
    pub fn save(&mut self, path: &FsPath) -> Result<()> {
        let bytes = self.to_bytes()?;
        if path.exists() {
            let bak = PathBuf::from(format!("{}.bak", path.display()));
            if !bak.exists() {
                fs::copy(path, &bak).with_context(|| format!("keeping a backup of {}", path.display()))?;
            }
        }
        let tmp = PathBuf::from(format!("{}.rage-toolkit-tmp", path.display()));
        fs::write(&tmp, &bytes).with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, path).with_context(|| format!("saving {}", path.display()))?;
        self.path = Some(path.to_path_buf());
        self.history.mark_saved();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<CMapTypes>
  <extensions/>
  <archetypes>
    <Item type="CBaseArchetypeDef">
      <lodDist value="100"/>
      <flags value="32"/>
      <bbMin x="-1" y="-1" z="-1"/>
      <bbMax x="1" y="1" z="1"/>
      <bsCentre x="0" y="0" z="0"/>
      <bsRadius value="1.7"/>
      <name>prop_one</name>
      <assetName>prop_one</assetName>
      <extensions/>
    </Item>
    <Item type="CBaseArchetypeDef">
      <lodDist value="60"/>
      <name>prop_two</name>
      <extensions/>
    </Item>
  </archetypes>
  <name>test_types</name>
</CMapTypes>"#;

    fn doc() -> YtypDocument {
        let mut d = YtypDocument::open(XML.as_bytes(), None).unwrap();
        d.names.add_lowercase("prop_one");
        d.names.add_lowercase("prop_two");
        d
    }

    #[test]
    fn the_xml_of_an_archetype_can_be_edited_and_applied() {
        let mut d = doc();
        let xml = d.to_xml(Some(0));
        assert!(xml.contains("<lodDist value=\"100\"/>"), "{xml}");
        d.replace_archetype_from_xml(0, &xml.replace("<lodDist value=\"100\"/>", "<lodDist value=\"321\"/>")).unwrap();
        assert_eq!(d.get(&[field("archetypes"), Step::Index(0), field("lodDist")]).and_then(|v| v.as_f32()), Some(321.0));
        assert_eq!(d.name_hash(0), rage_joaat("prop_one"));
        d.to_bytes().unwrap();
        assert!(d.replace_archetype_from_xml(0, "<CBaseArchetypeDef><lodDist").is_err());
        assert!(d.replace_archetype_from_xml(0, "<CHandlingData/>").is_err());
        d.undo().unwrap();
        assert_eq!(d.get(&[field("archetypes"), Step::Index(0), field("lodDist")]).and_then(|v| v.as_f32()), Some(100.0));
    }

    #[test]
    fn a_file_opens_lists_its_archetypes_and_rejects_other_files() {
        let d = doc();
        assert_eq!((d.len(), d.name(0), d.name(1), d.kind(0)), (2, "prop_one".to_string(), "prop_two".to_string(), ArchKind::Base));
        assert!(YtypDocument::open(b"<CHandlingDataMgr/>", None).is_err());
        assert!(YtypDocument::open(b"not a ytyp at all", None).is_err());
    }

    #[test]
    fn add_rename_duplicate_remove_and_edit_a_field_all_undo() {
        let mut d = doc();
        let before = d.to_xml(None);
        let i = d.add_archetype(ArchKind::Base, "My_New_Prop").unwrap();
        assert_eq!((d.len(), d.name(i)), (3, "My_New_Prop".to_string()));
        assert_eq!(d.name_hash(i), rage_joaat("my_new_prop"), "names are hashed in lowercase");
        assert_eq!(d.get(&[field("archetypes"), Step::Index(i), field("assetName")]).and_then(|v| v.as_hash()), Some(rage_joaat("my_new_prop")));
        d.rename_archetype(0, "prop_uno").unwrap();
        assert_eq!(d.get(&[field("archetypes"), Step::Index(0), field("assetName")]).and_then(|v| v.as_hash()), Some(rage_joaat("prop_uno")), "assetName followed the name");
        let c = d.duplicate_archetype(1, "prop_two_b").unwrap();
        assert_eq!((c, d.len(), d.name(c)), (2, 4, "prop_two_b".to_string()));
        d.remove_archetype(c).unwrap();
        // a field edited by text, like typing in a box
        let lod = vec![field("archetypes"), Step::Index(0), field("lodDist")];
        d.set_text(&lod, &FieldKind::Float, "250").unwrap();
        d.set_text(&lod, &FieldKind::Float, "251").unwrap();
        assert!(d.set_text(&lod, &FieldKind::Float, "far").is_err());
        assert_eq!(d.get(&lod).and_then(|v| v.as_f32()), Some(251.0));
        assert!(d.is_dirty());
        while d.can_undo() {
            d.undo().unwrap();
        }
        assert_eq!(d.to_xml(None), before);
        assert!(!d.is_dirty() || d.can_redo());
        while d.can_redo() {
            d.redo().unwrap();
        }
        assert_eq!(d.get(&lod).and_then(|v| v.as_f32()), Some(251.0));
    }

    #[test]
    fn a_file_saves_reopens_with_every_change_and_keeps_a_backup() {
        let dir = std::env::temp_dir().join(format!("rage_toolkit_ytyp_doc_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("props.ytyp");
        let mut d = doc();
        d.add_archetype(ArchKind::Base, "brand_new").unwrap();
        d.set_text(&vec![field("archetypes"), Step::Index(1), field("lodDist")], &FieldKind::Float, "75.5").unwrap();
        d.save(&file).unwrap();
        assert!(!d.is_dirty());
        let bytes = fs::read(&file).unwrap();
        assert_eq!(&bytes[..4], b"RSC7");
        let mut back = YtypDocument::open(&bytes, Some(file.clone())).unwrap();
        back.names.add_lowercase("brand_new");
        back.names.add_lowercase("prop_two");
        assert_eq!((back.len(), back.name(2)), (3, "brand_new".to_string()));
        assert_eq!(back.get(&[field("archetypes"), Step::Index(1), field("lodDist")]).and_then(|v| v.as_f32()), Some(75.5));
        // the older reader of rage-formats agrees on the names
        let y = rage_formats::parse_ytyp(&bytes).unwrap();
        assert!(y.archetypes.iter().any(|a| a.name_hash == rage_joaat("brand_new")));
        // saving over it keeps the old file once
        back.add_archetype(ArchKind::Base, "another").unwrap();
        back.save(&file).unwrap();
        assert!(PathBuf::from(format!("{}.bak", file.display())).is_file());
        assert_eq!(YtypDocument::open_file(&file).unwrap().len(), 4);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn time_and_mlo_archetypes_get_the_members_of_their_own_structure() {
        let d = doc();
        let m = d.new_archetype_value(ArchKind::Mlo, "my_interior");
        let s = m.as_struct().unwrap();
        for f in ["rooms", "portals", "entities", "entitySets", "lodDist", "bbMin"] {
            assert!(s.field(f).is_some(), "{f} missing from a new MLO");
        }
        let t = d.new_archetype_value(ArchKind::Time, "my_night_prop");
        assert_eq!(ArchKind::of(&t), ArchKind::Time);
        // and an MLO survives a save
        let mut d = doc();
        d.add_archetype(ArchKind::Mlo, "my_interior").unwrap();
        d.add_archetype(ArchKind::Time, "night_prop").unwrap();
        let bytes = d.to_bytes().unwrap();
        assert!(rage_formats::parse_ytyp(&bytes).unwrap().mlos.iter().any(|m| m.name_hash == rage_joaat("my_interior")));
    }

    #[test]
    fn a_new_empty_file_can_be_filled_and_saved() {
        let mut d = YtypDocument::new_file("My_Props");
        d.add_archetype(ArchKind::Base, "crate_a").unwrap();
        let bytes = d.to_bytes().unwrap();
        assert_eq!(YtypDocument::open(&bytes, None).unwrap().len(), 1);
    }
}
