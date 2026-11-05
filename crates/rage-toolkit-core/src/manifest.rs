//! The `_manifest.ymf` of a resource (as XML, which the game tools convert): for each `.ymap`
//! the `.ytyp` files whose archetypes it uses (`imapDependencies_2`), for each interior `.ytyp` the ones its room contents use
//! (`itypDependencies_2`), and the interior bounds list.
use std::collections::{BTreeMap, BTreeSet};

/// what one map needs
pub struct ManifestMap {
    /// the map's name without extension
    pub name: String,
    /// names (without extension) of the `.ytyp` files of the archetypes its entities and grass use
    pub ytyps: BTreeSet<String>,
    /// it places an interior (MLO)
    pub interior: bool,
}

/// An interior archetype: its name, the `.ytyp` that defines it, and the `.ytyp` files its entities use.
pub struct ManifestInterior {
    pub name: String,
    pub ytyp: String,
    pub uses: BTreeSet<String>,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn manifest_xml(maps: &[ManifestMap], interiors: &[ManifestInterior]) -> String {
    let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n<CPackFileMetaData>\n  <MapDataGroups/>\n  <HDTxdBindingArray/>\n  <imapDependencies/>\n");
    if maps.is_empty() {
        s.push_str("  <imapDependencies_2/>\n");
    } else {
        s.push_str("  <imapDependencies_2>\n");
        for m in maps {
            s.push_str("    <Item>\n");
            s.push_str(&format!("      <imapName>{}</imapName>\n", esc(&m.name.to_lowercase())));
            s.push_str(&if m.interior { "      <manifestFlags>INTERIOR_DATA</manifestFlags>\n".to_string() } else { "      <manifestFlags/>\n".to_string() });
            s.push_str("      <itypDepArray>\n");
            for y in &m.ytyps {
                s.push_str(&format!("        <Item>{}</Item>\n", esc(y)));
            }
            s.push_str("      </itypDepArray>\n    </Item>\n");
        }
        s.push_str("  </imapDependencies_2>\n");
    }
    // an interior's .ytyp depends on the others its contents use
    let mut deps: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for i in interiors {
        let e = deps.entry(i.ytyp.as_str()).or_default();
        e.extend(i.uses.iter().map(|u| u.as_str()).filter(|u| *u != i.ytyp));
    }
    if deps.is_empty() {
        s.push_str("  <itypDependencies_2/>\n");
    } else {
        s.push_str("  <itypDependencies_2>\n");
        for (y, list) in &deps {
            s.push_str(&format!("    <Item>\n      <itypName>{}</itypName>\n      <manifestFlags/>\n      <itypDepArray>\n", esc(y)));
            for u in list {
                s.push_str(&format!("        <Item>{}</Item>\n", esc(u)));
            }
            s.push_str("      </itypDepArray>\n    </Item>\n");
        }
        s.push_str("  </itypDependencies_2>\n");
    }
    if interiors.is_empty() {
        s.push_str("  <Interiors/>\n");
    } else {
        s.push_str("  <Interiors itemType=\"CInteriorBoundsFiles\">\n");
        for i in interiors {
            s.push_str(&format!("    <Item>\n      <Name>{0}</Name>\n      <Bounds>\n        <Item>{0}</Item>\n      </Bounds>\n    </Item>\n", esc(&i.name)));
        }
        s.push_str("  </Interiors>\n");
    }
    s.push_str("</CPackFileMetaData>\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manifest_lists_each_maps_ytyps_and_flags_interiors_and_reads_back() {
        let maps = vec![
            ManifestMap { name: "My_Map".into(), ytyps: ["props_a".to_string(), "props_b".to_string()].into(), interior: false },
            ManifestMap { name: "int_map".into(), ytyps: ["v_int".to_string()].into(), interior: true },
        ];
        let ints = vec![ManifestInterior { name: "v_int_01".into(), ytyp: "v_int".into(), uses: ["props_a".to_string(), "v_int".to_string()].into() }];
        let xml = manifest_xml(&maps, &ints);
        assert!(xml.contains("<imapName>my_map</imapName>") && xml.contains("<manifestFlags>INTERIOR_DATA</manifestFlags>"));
        assert!(xml.contains("<itypName>v_int</itypName>") && xml.contains("<Name>v_int_01</Name>"));
        assert!(!xml.contains("<Item>v_int</Item>\n      </itypDepArray>\n    </Item>\n  </itypDependencies_2>"), "an interior does not depend on its own ytyp");
        let root = rage_formats::from_xml(&xml).expect("the XML parses");
        assert!(root.as_struct().is_some());
        let empty = manifest_xml(&[], &[]);
        assert!(empty.contains("<imapDependencies_2/>") && empty.contains("<Interiors/>"));
    }
}
