//! The lights a model carries (`LightAttributes`, 168 bytes each, in the list at offset 0xB0 of a `.ydr` drawable). The LOD-light generator turns them into the map's LOD lights.
use crate::{
    math::Vec3,
    resource::{prepare_rsc7, u16_le, u32_le, vec3_le, ResReader, SYSTEM_BASE},
};
use anyhow::Result;

const LIGHT_SIZE: usize = 168;
const LIST_OFFSET: u64 = 0xB0;

#[derive(Debug, Clone, PartialEq)]
pub struct DrawableLight {
    pub position: Vec3,
    pub color: [u8; 3],
    pub flashiness: u8,
    pub intensity: f32,
    pub flags: u32,
    pub bone_id: u16,
    /// 1 point, 2 spot, 4 capsule
    pub light_type: u8,
    pub group_id: u8,
    pub time_flags: u32,
    pub falloff: f32,
    pub falloff_exponent: f32,
    pub corona_size: f32,
    pub corona_intensity: f32,
    pub direction: Vec3,
    pub tangent: Vec3,
    pub cone_inner_angle: f32,
    pub cone_outer_angle: f32,
    pub extent: Vec3,
}

fn f32_at(b: &[u8], o: usize) -> f32 {
    f32::from_bits(u32_le(b, o))
}

/// The lights of a single-drawable `.ydr` (none for a dictionary or a fragment, whose lights are elsewhere).
pub fn parse_ydr_lights(data: &[u8]) -> Result<Vec<DrawableLight>> {
    let (system, graphics) = prepare_rsc7(data)?;
    let reader = ResReader { system: &system, graphics: &graphics };
    let Some(h) = reader.read_pointer_list_header(SYSTEM_BASE + LIST_OFFSET) else { return Ok(vec![]) };
    if h.pointer == 0 || h.count == 0 || h.count > 512 {
        return Ok(vec![]);
    }
    let Some(bytes) = reader.resolve(h.pointer, h.count as usize * LIGHT_SIZE) else { return Ok(vec![]) };
    Ok(bytes
        .chunks_exact(LIGHT_SIZE)
        .map(|b| DrawableLight {
            position: vec3_le(b, 0x08),
            color: [b[0x18], b[0x19], b[0x1A]],
            flashiness: b[0x1B],
            intensity: f32_at(b, 0x1C),
            flags: u32_le(b, 0x20),
            bone_id: u16_le(b, 0x24),
            light_type: b[0x26],
            group_id: b[0x27],
            time_flags: u32_le(b, 0x28),
            falloff: f32_at(b, 0x2C),
            falloff_exponent: f32_at(b, 0x30),
            corona_size: f32_at(b, 0x5C),
            corona_intensity: f32_at(b, 0x6C),
            direction: vec3_le(b, 0x74),
            tangent: vec3_le(b, 0x80),
            cone_inner_angle: f32_at(b, 0x8C),
            cone_outer_angle: f32_at(b, 0x90),
            extent: vec3_le(b, 0x94),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_and_non_resources_are_refused_or_empty_never_a_panic() {
        assert!(parse_ydr_lights(b"nope").is_err());
        assert!(parse_ydr_lights(&[]).is_err());
    }

    /// on a real install: a street lamp has lights, a bench has none, and the values are sane
    #[test]
    fn real_props_report_their_lights() {
        let Some(dir) = crate::testenv::var("CRCK_YDR_DIR") else { return };
        let mut with_lights = 0;
        for e in std::fs::read_dir(dir).unwrap().flatten().take(400) {
            let Ok(b) = std::fs::read(e.path()) else { continue };
            if let Ok(l) = parse_ydr_lights(&b) {
                for x in &l {
                    assert!(x.intensity.is_finite() && x.falloff.is_finite() && x.position.x.is_finite());
                    assert!(matches!(x.light_type, 0..=8), "type {}", x.light_type);
                }
                with_lights += (!l.is_empty()) as usize;
            }
        }
        eprintln!("{with_lights} files with lights");
    }
}
