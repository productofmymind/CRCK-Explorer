//! Menyoo / Spooner placement files (`<SpoonerPlacements>`): every `Placement` of type 3
//! (an object) becomes an entity of a map. The position and the Euler angles (degrees, as the game's entity rotation: pitch about X, roll about
//! Y, heading about Z) are turned into the quaternion a `.ymap` stores, which is the inverse of the world rotation.
use anyhow::{Context, Result};
use glam::{EulerRot, Quat};

#[derive(Debug, Clone, PartialEq)]
pub struct MenyooPlacement {
    pub model_hash: u32,
    /// the readable name when the file has one
    pub hash_name: String,
    /// 1 ped, 2 vehicle, 3 object
    pub kind: i32,
    pub dynamic: bool,
    pub position: [f32; 3],
    /// pitch, roll, yaw in degrees
    pub pitch_roll_yaw: [f32; 3],
    pub lod_distance: f32,
}

impl MenyooPlacement {
    /// the rotation as a `.ymap` stores it (x, y, z, w)
    pub fn stored_rotation(&self) -> [f32; 4] {
        let [p, r, y] = self.pitch_roll_yaw.map(f32::to_radians);
        // the game turns about Z (heading), then X (pitch), then Y (roll)
        let world = Quat::from_euler(EulerRot::ZXY, y, p, r);
        let s = world.conjugate();
        [s.x, s.y, s.z, s.w]
    }

    /// the name to give the archetype: the file's own, else its hash
    pub fn archetype_name(&self) -> String {
        if self.hash_name.trim().is_empty() {
            format!("hash_{:08X}", self.model_hash)
        } else {
            self.hash_name.trim().to_string()
        }
    }
}

fn child<'a>(n: roxmltree::Node<'a, 'a>, name: &str) -> Option<roxmltree::Node<'a, 'a>> {
    n.children().find(|c| c.is_element() && c.tag_name().name() == name)
}

fn text<'a>(n: roxmltree::Node<'a, 'a>, name: &str) -> &'a str {
    child(n, name).and_then(|c| c.text()).unwrap_or("").trim()
}

fn num(n: roxmltree::Node, name: &str) -> f32 {
    text(n, name).parse().unwrap_or(0.0)
}

/// Every placement of the file (all kinds; the caller takes the objects).
pub fn parse(xml: &str) -> Result<Vec<MenyooPlacement>> {
    let doc = roxmltree::Document::parse(xml.trim_start_matches('\u{feff}')).context("this is not an XML file")?;
    let mut out = vec![];
    for p in doc.root_element().children().filter(|c| c.is_element() && c.tag_name().name() == "Placement") {
        let hash_text = text(p, "ModelHash").to_lowercase();
        let hash_text = hash_text.trim_start_matches("0x");
        let Ok(model_hash) = u32::from_str_radix(hash_text, 16) else { continue };
        let pr = child(p, "PositionRotation");
        let v = |n: &str| pr.map_or(0.0, |x| num(x, n));
        out.push(MenyooPlacement {
            model_hash,
            hash_name: text(p, "HashName").to_string(),
            kind: text(p, "Type").parse().unwrap_or(0),
            dynamic: text(p, "Dynamic").eq_ignore_ascii_case("true"),
            position: [v("X"), v("Y"), v("Z")],
            pitch_roll_yaw: [v("Pitch"), v("Roll"), v("Yaw")],
            lod_distance: num(p, "LodDistance"),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<SpoonerPlacements><Note/><ReferenceCoords><X>1</X><Y>2</Y><Z>3</Z></ReferenceCoords>
<Placement><ModelHash>0x1F2A3B4C</ModelHash><Type>3</Type><Dynamic>false</Dynamic><HashName>prop_bench_01a</HashName><LodDistance>500</LodDistance>
<PositionRotation><X>10.5</X><Y>-20</Y><Z>30</Z><Pitch>0</Pitch><Roll>0</Roll><Yaw>90</Yaw></PositionRotation></Placement>
<Placement><ModelHash>0xABC</ModelHash><Type>1</Type><Dynamic>true</Dynamic><PositionRotation><X>0</X><Y>0</Y><Z>0</Z></PositionRotation></Placement>
<Placement><ModelHash>zzz</ModelHash><Type>3</Type></Placement></SpoonerPlacements>"#;

    #[test]
    fn placements_are_read_and_a_bad_one_is_skipped() {
        let v = parse(SAMPLE).unwrap();
        assert_eq!(v.len(), 2, "the placement with no hash is skipped");
        assert_eq!((v[0].model_hash, v[0].kind, v[0].dynamic), (0x1F2A3B4C, 3, false));
        assert_eq!((v[0].position, v[0].lod_distance, v[0].archetype_name().as_str()), ([10.5, -20.0, 30.0], 500.0, "prop_bench_01a"));
        assert_eq!(v[1].archetype_name(), "hash_00000ABC");
        assert!(parse("not xml <").is_err());
    }

    #[test]
    fn a_heading_of_90_degrees_turns_the_world_x_axis_to_y_and_is_stored_inverted() {
        let v = parse(SAMPLE).unwrap();
        let s = v[0].stored_rotation();
        let world = Quat::from_xyzw(s[0], s[1], s[2], s[3]).conjugate();
        assert!((world * Vec3::X - Vec3::Y).length() < 1e-5, "{:?}", world * Vec3::X);
    }
}
