//! `carvariations.ymt` / `carvariations.meta`: per vehicle model, the colour
//! combinations it spawns in (indices into carcols' colour list: primary,
//! secondary, pearlescent, wheels, interior, dashboard), which of its liveries
//! each combination allows, its mod kits, light and siren settings and plate
//! probabilities (`CVehicleModelInfoVariation`). Ported from the reference tool's
//! `CarVariationsFile.cs`; both forms go through the same tree.

use anyhow::Result;

use crate::meta_read::{parse_tree, root_struct, Fields};
use crate::value::MetaStruct;

/// `CVehicleModelInfoVariation`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CarVariations {
    pub variation_data: Vec<VehicleVariation>,
}

/// `CVehicleVariationData` (the reference tool's `CVehicleModelInfoVariation_418053801`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VehicleVariation {
    pub model_name: String,
    pub colors: Vec<ColorCombination>,
    /// Mod kit names (`CVehicleKit.kitName`), hashed.
    pub kits: Vec<u32>,
    pub windows_with_exposed_edges: Vec<u32>,
    pub plate_probabilities: Vec<PlateProbability>,
    pub light_settings: u8,
    pub siren_settings: u8,
}

/// `CVehicleModelColorIndices`: one way the vehicle spawns painted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColorCombination {
    /// Carcols colour indices: primary, secondary, pearlescent, wheel,
    /// interior trim, dashboard (the game reads the first four at least).
    pub indices: Vec<u8>,
    /// Which livery (`_sign_N` texture, index `N - 1`) this combination may
    /// use; the game's files list eight or more, mostly all false.
    pub liveries: Vec<bool>,
}

/// One `PlateProbabilities` entry: a plate texture set name and its weight.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlateProbability {
    pub name: u32,
    pub value: u32,
}

impl VehicleVariation {
    fn read(s: &MetaStruct) -> Self {
        VehicleVariation {
            model_name: s.text("modelName"),
            colors: s
                .structs("colors")
                .into_iter()
                .map(|c| ColorCombination { indices: c.bytes("indices"), liveries: c.bools("liveries") })
                .collect(),
            kits: s.hashes("kits"),
            windows_with_exposed_edges: s.hashes("windowsWithExposedEdges"),
            plate_probabilities: s
                .child("plateProbabilities")
                .map(|p| {
                    p.structs("Probabilities")
                        .into_iter()
                        .map(|e| PlateProbability { name: e.hash("Name"), value: e.u32("Value") })
                        .collect()
                })
                .unwrap_or_default(),
            light_settings: s.u8("lightSettings"),
            siren_settings: s.u8("sirenSettings"),
        }
    }

    /// Whether livery `index` (0-based) is allowed by any colour combination.
    pub fn allows_livery(&self, index: usize) -> bool {
        self.colors.iter().any(|c| c.liveries.get(index).copied().unwrap_or(false))
    }

    /// How many liveries the combinations describe (the longest list).
    pub fn livery_count(&self) -> usize {
        self.colors.iter().map(|c| c.liveries.len()).max().unwrap_or(0)
    }
}

/// Parses a `carvariations.ymt` (PSO) or `carvariations.meta` (XML).
pub fn parse_carvariations(data: &[u8]) -> Result<CarVariations> {
    let root = parse_tree(data)?;
    let s = root_struct(&root, "CVehicleModelInfoVariation")?;
    Ok(CarVariations { variation_data: s.structs("variationData").into_iter().map(VehicleVariation::read).collect() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::rage_joaat;

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<CVehicleModelInfoVariation>
  <variationData>
	<Item>
      <modelName>retinue2</modelName>
      <colors>
        <Item>
          <indices content="char_array">
            5
            111
            111
            156
			3
			160
          </indices>
          <liveries>
            <Item value="true" />
            <Item value="false" />
          </liveries>
        </Item>
        <Item>
          <indices content="char_array">29 111 138 156 3 160</indices>
          <liveries>0 1 1</liveries>
        </Item>
      </colors>
      <kits>
        <Item>0_default_modkit</Item>
        <Item>1180_retinue2_modkit</Item>
      </kits>
      <windowsWithExposedEdges />
      <plateProbabilities>
        <Probabilities>
          <Item>
            <Name>Standard White</Name>
            <Value value="100" />
          </Item>
        </Probabilities>
      </plateProbabilities>
      <lightSettings value="3" />
      <sirenSettings value="0" />
    </Item>
  </variationData>
</CVehicleModelInfoVariation>"#;

    #[test]
    fn reads_the_xml_form() {
        let v = parse_carvariations(XML.as_bytes()).unwrap();
        assert_eq!(v.variation_data.len(), 1);
        let r = &v.variation_data[0];
        assert_eq!(r.model_name, "retinue2");
        assert_eq!(r.colors[0].indices, vec![5, 111, 111, 156, 3, 160]);
        assert_eq!(r.colors[0].liveries, vec![true, false]);
        assert_eq!(r.colors[1].indices, vec![29, 111, 138, 156, 3, 160]);
        assert_eq!(r.colors[1].liveries, vec![false, true, true], "the numeric fallback the reference tool keeps");
        assert_eq!(r.kits, vec![rage_joaat("0_default_modkit"), rage_joaat("1180_retinue2_modkit")]);
        assert!(r.windows_with_exposed_edges.is_empty());
        assert_eq!(r.plate_probabilities, vec![PlateProbability { name: rage_joaat("Standard White"), value: 100 }]);
        assert_eq!((r.light_settings, r.siren_settings), (3, 0));
        assert!(r.allows_livery(0) && r.allows_livery(2) && !r.allows_livery(5));
        assert_eq!(r.livery_count(), 3);
    }

    #[test]
    fn reads_the_retail_pso_form() {
        let Some(dir) = std::env::var_os("RAGE_TEST_META_DIR") else { return };
        let Ok(data) = std::fs::read(std::path::Path::new(&dir).join("carvariations.ymt")) else { return };
        let v = parse_carvariations(&data).unwrap();
        let ninef = v.variation_data.iter().find(|v| v.model_name == "ninef").expect("ninef");
        assert_eq!(ninef.colors[0].indices, vec![0, 0, 0, 156, 0, 0]);
        assert_eq!(ninef.colors[1].indices, vec![3, 1, 7, 156, 0, 0]);
        assert_eq!(ninef.colors[0].liveries.len(), 30);
        assert!(v.variation_data.iter().find(|v| v.model_name == "police").is_some());
        assert!(v.variation_data.iter().any(|v| v.allows_livery(0)), "some model allows its first livery");
        assert!(!ninef.kits.is_empty());
    }
}
