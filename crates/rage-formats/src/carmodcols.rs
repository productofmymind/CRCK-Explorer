//! `carmodcols.ymt`: the mod shop's colour menus (`CVehicleModColors`) —
//! metallic, classic, matte, metals, chrome and pearlescent lists naming a
//! carcols colour index and a specular index each. Ported from the reference tool's
//! `CarModColsFile.cs`.

use anyhow::Result;

use crate::meta_read::{parse_tree, root_struct, Fields};
use crate::value::MetaStruct;

/// `CVehicleModColours`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CarModCols {
    pub metallic: Vec<VehicleModColor>,
    pub classic: Vec<VehicleModColor>,
    pub matte: Vec<VehicleModColor>,
    pub metals: Vec<VehicleModColor>,
    pub chrome: Vec<VehicleModColor>,
    pub pearlescent: PearlescentColors,
}

/// `CVehicleModColor`: a menu entry naming a carcols colour (`col`) and a
/// specular setting (`spec`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VehicleModColor {
    pub name: String,
    pub col: u8,
    pub spec: u8,
}

/// `CVehicleModPearlescentColors`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PearlescentColors {
    pub base_cols: Vec<VehicleModColor>,
    pub spec_cols: Vec<VehicleModColor>,
}

fn colors(s: &MetaStruct, name: &str) -> Vec<VehicleModColor> {
    s.structs(name)
        .into_iter()
        .map(|c| VehicleModColor { name: c.text("name"), col: c.u8("col"), spec: c.u8("spec") })
        .collect()
}

/// Parses a `carmodcols.ymt` (PSO, as the game ships it) or its XML form.
pub fn parse_carmodcols(data: &[u8]) -> Result<CarModCols> {
    let root = parse_tree(data)?;
    let s = root_struct(&root, "CVehicleModColors")?;
    let pearlescent = s.child("pearlescent");
    Ok(CarModCols {
        metallic: colors(s, "metallic"),
        classic: colors(s, "classic"),
        matte: colors(s, "matte"),
        metals: colors(s, "metals"),
        chrome: colors(s, "chrome"),
        pearlescent: PearlescentColors {
            base_cols: pearlescent.map(|p| colors(p, "baseCols")).unwrap_or_default(),
            spec_cols: pearlescent.map(|p| colors(p, "specCols")).unwrap_or_default(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_xml_form() {
        let xml = r#"<CVehicleModColors>
  <metallic><Item><name>BLACK</name><col value="0"/><spec value="10"/></Item><Item><name>GRAPHITE</name><col value="1"/><spec value="5"/></Item></metallic>
  <classic/>
  <pearlescent><baseCols><Item><name>BLACK</name><col value="0"/><spec value="0"/></Item></baseCols><specCols/></pearlescent>
</CVehicleModColors>"#;
        let cols = parse_carmodcols(xml.as_bytes()).unwrap();
        assert_eq!(cols.metallic, vec![
            VehicleModColor { name: "BLACK".into(), col: 0, spec: 10 },
            VehicleModColor { name: "GRAPHITE".into(), col: 1, spec: 5 },
        ]);
        assert!(cols.classic.is_empty() && cols.chrome.is_empty());
        assert_eq!(cols.pearlescent.base_cols.len(), 1);
        assert!(cols.pearlescent.spec_cols.is_empty());
    }

    #[test]
    fn reads_the_retail_pso_form() {
        let Some(dir) = std::env::var_os("RAGE_TEST_META_DIR") else { return };
        let Ok(data) = std::fs::read(std::path::Path::new(&dir).join("carmodcols.ymt")) else { return };
        let cols = parse_carmodcols(&data).unwrap();
        assert_eq!(cols.metallic[0], VehicleModColor { name: "BLACK".into(), col: 0, spec: 10 });
        assert_eq!(cols.metallic[1], VehicleModColor { name: "BLACK_GRAPHITE".into(), col: 147, spec: 4 });
        assert!(!cols.classic.is_empty() && !cols.matte.is_empty() && !cols.metals.is_empty() && !cols.chrome.is_empty());
        assert_eq!(cols.pearlescent.base_cols[0].name, "BLACK");
    }
}
