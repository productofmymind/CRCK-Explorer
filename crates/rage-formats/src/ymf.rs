//! Pack-file manifests (`_manifest.ymf`): which `.ytyp`s each `.ymap` in a
//! resource depends on, HD texture bindings, interior collision lists and
//! timed map-data groups. The game reads them in three containers and
//! FiveM resources ship all three: PSO (most tools), RBF (retail props
//! packs, HD texture bindings only) and plain XML (hand-written). All three
//! decode to the same [`MetaValue`] tree and from there to [`Manifest`].

use anyhow::{bail, Context, Result};

use crate::hash::rage_joaat;
use crate::meta_schema::dump_meta;
use crate::pso::{dump_pso, is_pso};
use crate::rbf;
use crate::resource::{is_fxap, RSC7_MAGIC};
use crate::value::{MetaDump, MetaValue};
use crate::xml::from_xml;

/// A name that may have arrived as text (XML) or only as its hash (PSO).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HashName {
    pub hash: u32,
    pub name: Option<String>,
}

impl HashName {
    fn from_value(v: Option<&MetaValue>) -> Self {
        match v {
            Some(MetaValue::Str(s)) => Self { hash: rage_joaat(s), name: Some(s.clone()) },
            Some(other) => Self { hash: other.as_hash().unwrap_or(0), name: None },
            None => Self::default(),
        }
    }

    fn list(v: Option<&MetaValue>) -> Vec<Self> {
        v.map(MetaValue::items).unwrap_or(&[]).iter().map(|i| Self::from_value(Some(i))).collect()
    }
}

impl std::fmt::Display for HashName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.name {
            Some(n) => f.write_str(n),
            None => write!(f, "{}", crate::names::unknown(self.hash)),
        }
    }
}

/// A `CMapDataGroup`: a named set of map files switched by weather or hour.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MapDataGroup {
    pub name: HashName,
    pub bounds: Vec<HashName>,
    pub weather_types: Vec<HashName>,
    pub flags: u32,
    pub hours_on_off: u32,
}

/// A `CImapDependency` (the older single-ytyp form).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImapDependency {
    pub imap: HashName,
    pub ityp: HashName,
    pub pack_file: HashName,
}

/// A `CImapDependencies` / `CItypDependencies` entry: one map or type file
/// and the type files it needs loaded first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Dependencies {
    pub name: HashName,
    pub manifest_flags: u32,
    pub ityp_deps: Vec<HashName>,
}

/// A `CHDTxdAssetBinding`: the HD texture dictionary to swap in for an asset.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HdTxdBinding {
    /// `AT_TXD`, `AT_DRB`, `AT_DWD` or `AT_FRG` as stored (0..=3), or the
    /// enum member's name from an XML manifest.
    pub asset_type: HashName,
    pub target_asset: String,
    pub hd_txd: String,
}

/// A `CInteriorBoundsFiles`: an interior and its collision files.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InteriorBounds {
    pub name: HashName,
    pub bounds: Vec<HashName>,
}

/// The typed contents of a manifest.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Manifest {
    pub map_data_groups: Vec<MapDataGroup>,
    pub imap_dependencies: Vec<ImapDependency>,
    pub imap_dependencies_2: Vec<Dependencies>,
    pub ityp_dependencies_2: Vec<Dependencies>,
    pub hd_txd_bindings: Vec<HdTxdBinding>,
    pub interiors: Vec<InteriorBounds>,
}

/// Which container a metadata file came in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaContainer {
    Pso,
    Rbf,
    Meta,
    Xml,
}

/// The container of a manifest: any of the four.
pub type ManifestFormat = MetaContainer;

impl std::fmt::Display for MetaContainer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Pso => "PSO",
            Self::Rbf => "RBF",
            Self::Meta => "RSC7 Meta",
            Self::Xml => "XML",
        })
    }
}

impl MetaContainer {
    /// Looks at the first bytes only.
    pub fn detect(data: &[u8]) -> Option<Self> {
        if is_pso(data) {
            Some(Self::Pso)
        } else if rbf::is_rbf(data) {
            Some(Self::Rbf)
        } else if data.len() >= 4 && u32::from_le_bytes(data[0..4].try_into().ok()?) == RSC7_MAGIC {
            Some(Self::Meta)
        } else if data.iter().take(64).find(|b| !b.is_ascii_whitespace() && **b != 0xEF && **b != 0xBB && **b != 0xBF) == Some(&b'<') {
            Some(Self::Xml)
        } else {
            None
        }
    }
}

/// Decodes any metadata file — PSO, RBF, RSC7 Meta or XML, told apart by
/// its first bytes — into the generic tree.
pub fn dump_metadata(data: &[u8]) -> Result<(MetaContainer, MetaDump)> {
    if is_fxap(data) {
        bail!("an escrowed (FXAP) file cannot be read");
    }
    let Some(format) = MetaContainer::detect(data) else {
        bail!("not metadata: neither PSO, RBF, RSC7 Meta nor XML");
    };
    let dump = match format {
        ManifestFormat::Pso => dump_pso(data)?,
        ManifestFormat::Rbf => MetaDump { root: rbf_to_value(&rbf::parse(data)?), warnings: Vec::new() },
        ManifestFormat::Meta => dump_meta(data)?,
        ManifestFormat::Xml => MetaDump { root: from_xml(std::str::from_utf8(data).context("manifest XML is not UTF-8")?)?, warnings: Vec::new() },
    };
    Ok((format, dump))
}

/// [`dump_metadata`] for a manifest.
pub fn dump_ymf(data: &[u8]) -> Result<(ManifestFormat, MetaDump)> {
    dump_metadata(data)
}

/// Parses a manifest in any of its containers.
pub fn parse_ymf(data: &[u8]) -> Result<(ManifestFormat, Manifest)> {
    let (format, dump) = dump_ymf(data)?;
    Ok((format, Manifest::from_value(&dump.root)?))
}

impl Manifest {
    /// Reads the typed lists out of a `CPackFileMetaData` tree.
    pub fn from_value(root: &MetaValue) -> Result<Self> {
        let root = root.as_struct().context("manifest root is not a structure")?;
        if root.type_hash != rage_joaat("CPackFileMetaData") {
            bail!("manifest root is not CPackFileMetaData (structure {:#010x})", root.type_hash);
        }
        let list = |name: &str| root.field(name).map(MetaValue::items).unwrap_or(&[]);
        let sub = |v: &MetaValue| v.as_struct().cloned().unwrap_or_default();
        Ok(Self {
            map_data_groups: list("MapDataGroups")
                .iter()
                .map(|v| {
                    let s = sub(v);
                    MapDataGroup {
                        name: HashName::from_value(s.field("Name")),
                        bounds: HashName::list(s.field("Bounds")),
                        weather_types: HashName::list(s.field("WeatherTypes")),
                        flags: s.field("Flags").and_then(MetaValue::as_u32).unwrap_or(0),
                        hours_on_off: s.field("HoursOnOff").and_then(MetaValue::as_u32).unwrap_or(0),
                    }
                })
                .collect(),
            imap_dependencies: list("imapDependencies")
                .iter()
                .map(|v| {
                    let s = sub(v);
                    ImapDependency {
                        imap: HashName::from_value(s.field("imapName")),
                        ityp: HashName::from_value(s.field("itypName")),
                        pack_file: HashName::from_value(s.field("packFileName")),
                    }
                })
                .collect(),
            imap_dependencies_2: list("imapDependencies_2").iter().map(|v| dependencies(&sub(v), "imapName")).collect(),
            ityp_dependencies_2: list("itypDependencies_2").iter().map(|v| dependencies(&sub(v), "itypName")).collect(),
            hd_txd_bindings: list("HDTxdBindingArray")
                .iter()
                .map(|v| {
                    let s = sub(v);
                    HdTxdBinding {
                        asset_type: HashName::from_value(s.field("assetType")),
                        target_asset: s.field("targetAsset").and_then(MetaValue::as_str).unwrap_or_default().to_owned(),
                        hd_txd: s.field("HDTxd").and_then(MetaValue::as_str).unwrap_or_default().to_owned(),
                    }
                })
                .collect(),
            interiors: list("Interiors")
                .iter()
                .map(|v| {
                    let s = sub(v);
                    InteriorBounds { name: HashName::from_value(s.field("Name")), bounds: HashName::list(s.field("Bounds")) }
                })
                .collect(),
        })
    }

    /// True when nothing at all is listed.
    pub fn is_empty(&self) -> bool {
        self.map_data_groups.is_empty()
            && self.imap_dependencies.is_empty()
            && self.imap_dependencies_2.is_empty()
            && self.ityp_dependencies_2.is_empty()
            && self.hd_txd_bindings.is_empty()
            && self.interiors.is_empty()
    }
}

fn dependencies(s: &crate::value::MetaStruct, name_field: &str) -> Dependencies {
    Dependencies {
        name: HashName::from_value(s.field(name_field)),
        manifest_flags: s.field("manifestFlags").and_then(MetaValue::as_u32).unwrap_or(0),
        ityp_deps: HashName::list(s.field("itypDepArray")),
    }
}

/// Lifts an RBF tree into the generic one: structures keep their names
/// (hashed), attributes and children become members in order.
pub fn rbf_to_value(s: &rbf::RbfStructure) -> MetaValue {
    use crate::value::{MetaArray, MetaStruct};
    let mut fields = Vec::new();
    for (name, v) in s.attributes.iter().chain(&s.children) {
        let value = match v {
            rbf::RbfValue::Structure(inner) => rbf_to_value(inner),
            rbf::RbfValue::Bytes(b) => MetaValue::Bytes(b.clone()),
            rbf::RbfValue::Uint(u) => MetaValue::U32(*u),
            rbf::RbfValue::Bool(b) => MetaValue::Bool(*b),
            rbf::RbfValue::Float(f) => MetaValue::F32(*f),
            rbf::RbfValue::Float3(v) => MetaValue::Vec3(crate::math::Vec3::new(v[0], v[1], v[2])),
            rbf::RbfValue::Str(t) => MetaValue::Str(t.clone()),
        };
        fields.push((rage_joaat(name), value));
    }
    // An RBF list is a structure whose children are all `Item`s.
    let all_items = !fields.is_empty() && s.attributes.is_empty() && s.children.iter().all(|(n, _)| n == "Item");
    if all_items {
        return MetaValue::Array(MetaArray { item_type: None, typed_items: false, items: fields.into_iter().map(|(_, v)| v).collect() });
    }
    MetaValue::Struct(MetaStruct { type_hash: rage_joaat(&s.name), fields })
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="no"?>
<CPackFileMetaData>
  <MapDataGroups/>
  <HDTxdBindingArray>
    <Item>
      <assetType>AT_TXD</assetType>
      <targetAsset>prop_a</targetAsset>
      <HDTxd>prop_a+hi</HDTxd>
    </Item>
  </HDTxdBindingArray>
  <imapDependencies/>
  <imapDependencies_2>
    <Item>
      <imapName>bombapaleto</imapName>
      <manifestFlags/>
      <itypDepArray>
        <Item>v_construction</Item>
        <Item>v_bush</Item>
      </itypDepArray>
    </Item>
  </imapDependencies_2>
  <itypDependencies_2/>
  <Interiors>
    <Item>
      <Name>cafe_milo</Name>
      <Bounds>
        <Item>cafe_milo</Item>
      </Bounds>
    </Item>
  </Interiors>
</CPackFileMetaData>
"#;

    #[test]
    fn an_xml_manifest_parses_to_the_same_lists() {
        let (format, m) = parse_ymf(XML.as_bytes()).unwrap();
        assert_eq!(format, ManifestFormat::Xml);
        assert_eq!(m.imap_dependencies_2.len(), 1);
        let d = &m.imap_dependencies_2[0];
        assert_eq!(d.name.to_string(), "bombapaleto");
        assert_eq!(d.name.hash, rage_joaat("bombapaleto"));
        assert_eq!(d.ityp_deps.iter().map(ToString::to_string).collect::<Vec<_>>(), ["v_construction", "v_bush"]);
        assert_eq!(m.hd_txd_bindings[0].hd_txd, "prop_a+hi");
        assert_eq!(m.hd_txd_bindings[0].asset_type.name.as_deref(), Some("AT_TXD"));
        assert_eq!(m.interiors[0].bounds[0].to_string(), "cafe_milo");
        assert!(m.map_data_groups.is_empty());
    }

    #[test]
    fn a_pso_manifest_parses_with_hashes_only() {
        // The PSO fixture is not a manifest; the typed reader must say so
        // rather than return an empty manifest.
        let err = parse_ymf(&crate::pso::tests::sample_pso(false)).unwrap_err().to_string();
        assert!(err.contains("CPackFileMetaData"), "{err}");
    }

    #[test]
    fn the_container_is_recognised_from_the_first_bytes() {
        assert_eq!(ManifestFormat::detect(b"PSIN\0\0\0\x10"), Some(ManifestFormat::Pso));
        assert_eq!(ManifestFormat::detect(b"RBF0"), Some(ManifestFormat::Rbf));
        assert_eq!(ManifestFormat::detect(b"RSC7\x02\0\0\0"), Some(ManifestFormat::Meta));
        assert_eq!(ManifestFormat::detect(b"\xEF\xBB\xBF  <?xml"), Some(ManifestFormat::Xml));
        assert_eq!(ManifestFormat::detect(b"FXAP"), None);
        assert_eq!(MetaContainer::Pso.to_string(), "PSO");
    }
}
