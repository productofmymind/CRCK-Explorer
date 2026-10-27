//! `LightAttributes`, ported from the reference tool's `Drawable.cs`: one 168-byte
//! light of a drawable. Only the legacy PC layout is handled.

use super::xml::{child_attr_f32, child_attr_u32, child_text, child_vec3, float, hash_string, parse_hash, Node, XmlOut};
use super::{Pod, Writer};
use crate::math::Vec3;
use crate::names::NameTable;

/// `LightType`'s names and values.
pub const LIGHT_TYPES: [(&str, u8); 3] = [("Point", 1), ("Spot", 2), ("Capsule", 4)];

/// `LightAttributes`, every field in file order; the `unknown_*` ones round-trip through bytes.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Light {
    pub unknown_0h: u32,
    pub unknown_4h: u32,
    pub position: Vec3,
    pub unknown_14h: u32,
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub flashiness: u8,
    pub intensity: f32,
    pub flags: u32,
    pub bone_id: u16,
    pub light_type: u8,
    pub group_id: u8,
    pub time_flags: u32,
    pub falloff: f32,
    pub falloff_exponent: f32,
    pub culling_plane_normal: Vec3,
    pub culling_plane_offset: f32,
    pub shadow_blur: u8,
    pub unknown_45h: u8,
    pub unknown_46h: u16,
    pub unknown_48h: u32,
    pub volume_intensity: f32,
    pub volume_size_scale: f32,
    pub vo_r: u8,
    pub vo_g: u8,
    pub vo_b: u8,
    pub light_hash: u8,
    pub volume_outer_intensity: f32,
    pub corona_size: f32,
    pub volume_outer_exponent: f32,
    pub light_fade_distance: u8,
    pub shadow_fade_distance: u8,
    pub specular_fade_distance: u8,
    pub volumetric_fade_distance: u8,
    pub shadow_near_clip: f32,
    pub corona_intensity: f32,
    pub corona_z_bias: f32,
    pub direction: Vec3,
    pub tangent: Vec3,
    pub cone_inner_angle: f32,
    pub cone_outer_angle: f32,
    pub extent: Vec3,
    pub projected_texture_hash: u32,
    pub unknown_a4h: u32,
}

/// A forward reader over a slice of little-endian fields.
struct Rd<'a> { b: &'a [u8], pos: usize }
impl Rd<'_> {
    fn get<T: Pod>(&mut self) -> T {
        let v = T::read(&self.b[self.pos..]);
        self.pos += T::SIZE;
        v
    }
}

impl Pod for Light {
    const SIZE: usize = 168;
    fn write(&self, w: &mut Writer) {
        w.u32(self.unknown_0h);
        w.u32(self.unknown_4h);
        w.vec3(self.position);
        w.u32(self.unknown_14h);
        w.u8(self.r); w.u8(self.g); w.u8(self.b); w.u8(self.flashiness);
        w.f32(self.intensity);
        w.u32(self.flags);
        w.u16(self.bone_id);
        w.u8(self.light_type);
        w.u8(self.group_id);
        w.u32(self.time_flags);
        w.f32(self.falloff);
        w.f32(self.falloff_exponent);
        w.vec3(self.culling_plane_normal);
        w.f32(self.culling_plane_offset);
        w.u8(self.shadow_blur);
        w.u8(self.unknown_45h);
        w.u16(self.unknown_46h);
        w.u32(self.unknown_48h);
        w.f32(self.volume_intensity);
        w.f32(self.volume_size_scale);
        w.u8(self.vo_r); w.u8(self.vo_g); w.u8(self.vo_b); w.u8(self.light_hash);
        w.f32(self.volume_outer_intensity);
        w.f32(self.corona_size);
        w.f32(self.volume_outer_exponent);
        w.u8(self.light_fade_distance);
        w.u8(self.shadow_fade_distance);
        w.u8(self.specular_fade_distance);
        w.u8(self.volumetric_fade_distance);
        w.f32(self.shadow_near_clip);
        w.f32(self.corona_intensity);
        w.f32(self.corona_z_bias);
        w.vec3(self.direction);
        w.vec3(self.tangent);
        w.f32(self.cone_inner_angle);
        w.f32(self.cone_outer_angle);
        w.vec3(self.extent);
        w.u32(self.projected_texture_hash);
        w.u32(self.unknown_a4h);
    }
    fn read(b: &[u8]) -> Self {
        let mut r = Rd { b, pos: 0 };
        Light {
            unknown_0h: r.get(), unknown_4h: r.get(), position: r.get(), unknown_14h: r.get(),
            r: r.get(), g: r.get(), b: r.get(), flashiness: r.get(),
            intensity: r.get(), flags: r.get(), bone_id: r.get(), light_type: r.get(), group_id: r.get(),
            time_flags: r.get(), falloff: r.get(), falloff_exponent: r.get(),
            culling_plane_normal: r.get(), culling_plane_offset: r.get(),
            shadow_blur: r.get(), unknown_45h: r.get(), unknown_46h: r.get(), unknown_48h: r.get(),
            volume_intensity: r.get(), volume_size_scale: r.get(),
            vo_r: r.get(), vo_g: r.get(), vo_b: r.get(), light_hash: r.get(),
            volume_outer_intensity: r.get(), corona_size: r.get(), volume_outer_exponent: r.get(),
            light_fade_distance: r.get(), shadow_fade_distance: r.get(),
            specular_fade_distance: r.get(), volumetric_fade_distance: r.get(),
            shadow_near_clip: r.get(), corona_intensity: r.get(), corona_z_bias: r.get(),
            direction: r.get(), tangent: r.get(), cone_inner_angle: r.get(), cone_outer_angle: r.get(),
            extent: r.get(), projected_texture_hash: r.get(), unknown_a4h: r.get(),
        }
    }
}

impl Light {
    /// `LightAttributes.WriteXml`, tags in the reference tool's order.
    pub fn write_xml(&self, x: &mut XmlOut, names: &NameTable) {
        let type_name = LIGHT_TYPES.iter().find(|(_, v)| *v == self.light_type).map(|(n, _)| n.to_string())
            .unwrap_or_else(|| self.light_type.to_string());
        x.vec3("Position", self.position);
        x.rgb("Colour", self.r, self.g, self.b);
        x.value("Flashiness", self.flashiness);
        x.value("Intensity", float(self.intensity));
        x.value("Flags", self.flags);
        x.value("BoneId", self.bone_id);
        x.string("Type", &type_name);
        x.value("GroupId", self.group_id);
        x.value("TimeFlags", self.time_flags);
        x.value("Falloff", float(self.falloff));
        x.value("FalloffExponent", float(self.falloff_exponent));
        x.vec3("CullingPlaneNormal", self.culling_plane_normal);
        x.value("CullingPlaneOffset", float(self.culling_plane_offset));
        x.value("Unknown45", self.unknown_45h);
        x.value("Unknown46", self.unknown_46h);
        x.value("VolumeIntensity", float(self.volume_intensity));
        x.value("VolumeSizeScale", float(self.volume_size_scale));
        x.rgb("VolumeOuterColour", self.vo_r, self.vo_g, self.vo_b);
        x.value("LightHash", self.light_hash);
        x.value("VolumeOuterIntensity", float(self.volume_outer_intensity));
        x.value("CoronaSize", float(self.corona_size));
        x.value("VolumeOuterExponent", float(self.volume_outer_exponent));
        x.value("LightFadeDistance", self.light_fade_distance);
        x.value("ShadowBlur", self.shadow_blur);
        x.value("ShadowFadeDistance", self.shadow_fade_distance);
        x.value("SpecularFadeDistance", self.specular_fade_distance);
        x.value("VolumetricFadeDistance", self.volumetric_fade_distance);
        x.value("ShadowNearClip", float(self.shadow_near_clip));
        x.value("CoronaIntensity", float(self.corona_intensity));
        x.value("CoronaZBias", float(self.corona_z_bias));
        x.vec3("Direction", self.direction);
        x.vec3("Tangent", self.tangent);
        x.value("ConeInnerAngle", float(self.cone_inner_angle));
        x.value("ConeOuterAngle", float(self.cone_outer_angle));
        x.vec3("Extent", self.extent);
        x.string("ProjectedTextureHash", &hash_string(self.projected_texture_hash, names));
    }

    /// `LightAttributes.ReadXml`; the words the XML does not carry (the `unknown_*` ones) stay 0.
    pub fn read_xml(n: Node) -> Light {
        let u = |name: &str, attr: &str| child_attr_u32(n, name, attr);
        let f = |name: &str| child_attr_f32(n, name, "value");
        let type_text = child_text(n, "Type");
        let light_type = LIGHT_TYPES.iter().find(|(name, _)| name.eq_ignore_ascii_case(type_text.trim())).map_or(0, |(_, v)| *v);
        Light {
            position: child_vec3(n, "Position"),
            r: u("Colour", "r") as u8, g: u("Colour", "g") as u8, b: u("Colour", "b") as u8,
            flashiness: u("Flashiness", "value") as u8,
            intensity: f("Intensity"),
            flags: u("Flags", "value"),
            bone_id: u("BoneId", "value") as u16,
            light_type,
            group_id: u("GroupId", "value") as u8,
            time_flags: u("TimeFlags", "value"),
            falloff: f("Falloff"),
            falloff_exponent: f("FalloffExponent"),
            culling_plane_normal: child_vec3(n, "CullingPlaneNormal"),
            culling_plane_offset: f("CullingPlaneOffset"),
            unknown_45h: u("Unknown45", "value") as u8,
            unknown_46h: u("Unknown46", "value") as u16,
            volume_intensity: f("VolumeIntensity"),
            volume_size_scale: f("VolumeSizeScale"),
            vo_r: u("VolumeOuterColour", "r") as u8, vo_g: u("VolumeOuterColour", "g") as u8, vo_b: u("VolumeOuterColour", "b") as u8,
            light_hash: u("LightHash", "value") as u8,
            volume_outer_intensity: f("VolumeOuterIntensity"),
            corona_size: f("CoronaSize"),
            volume_outer_exponent: f("VolumeOuterExponent"),
            light_fade_distance: u("LightFadeDistance", "value") as u8,
            shadow_blur: u("ShadowBlur", "value") as u8,
            shadow_fade_distance: u("ShadowFadeDistance", "value") as u8,
            specular_fade_distance: u("SpecularFadeDistance", "value") as u8,
            volumetric_fade_distance: u("VolumetricFadeDistance", "value") as u8,
            shadow_near_clip: f("ShadowNearClip"),
            corona_intensity: f("CoronaIntensity"),
            corona_z_bias: f("CoronaZBias"),
            direction: child_vec3(n, "Direction"),
            tangent: child_vec3(n, "Tangent"),
            cone_inner_angle: f("ConeInnerAngle"),
            cone_outer_angle: f("ConeOuterAngle"),
            extent: child_vec3(n, "Extent"),
            projected_texture_hash: parse_hash(&child_text(n, "ProjectedTextureHash")),
            ..Light::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::xml::XmlOut;
    use crate::blocks::{Pod, Writer};
    use crate::math::Vec3;
    use crate::names::NameTable;

    fn sample() -> Light {
        Light {
            unknown_0h: 1, unknown_4h: 2, position: Vec3::new(1.5, 2.5, 3.5), unknown_14h: 3,
            r: 1, g: 2, b: 3, flashiness: 4, intensity: 5.5, flags: 6, bone_id: 7, light_type: 2, group_id: 8,
            time_flags: 9, falloff: 10.5, falloff_exponent: 11.5, culling_plane_normal: Vec3::new(0.0, 1.0, 0.0),
            culling_plane_offset: 12.5, shadow_blur: 13, unknown_45h: 14, unknown_46h: 15, unknown_48h: 16,
            volume_intensity: 17.5, volume_size_scale: 18.5, vo_r: 19, vo_g: 20, vo_b: 21, light_hash: 22,
            volume_outer_intensity: 23.5, corona_size: 24.5, volume_outer_exponent: 25.5, light_fade_distance: 26,
            shadow_fade_distance: 27, specular_fade_distance: 28, volumetric_fade_distance: 29,
            shadow_near_clip: 30.5, corona_intensity: 31.5, corona_z_bias: 32.5, direction: Vec3::new(0.0, 0.0, -1.0),
            tangent: Vec3::new(1.0, 0.0, 0.0), cone_inner_angle: 33.5, cone_outer_angle: 34.5,
            extent: Vec3::new(4.5, 5.5, 6.5), projected_texture_hash: 0xDEADBEEF, unknown_a4h: 35,
        }
    }

    #[test]
    fn light_round_trips_through_bytes_and_xml() {
        let l = sample();
        let mut w = Writer::new(0, 0);
        l.write(&mut w);
        let bytes = w.into_inner();
        assert_eq!(bytes.len(), 168);
        assert_eq!(Light::SIZE, 168);
        assert_eq!(Light::read(&bytes), l);

        let names = NameTable::core();
        let mut x = XmlOut::new();
        x.open("Item");
        l.write_xml(&mut x, &names);
        x.close("Item");
        assert!(x.out.contains("<Type>Spot</Type>"));
        assert!(x.out.contains("<Colour r=\"1\" g=\"2\" b=\"3\" />"));
        let doc = roxmltree::Document::parse(&x.out).unwrap();
        let mut back = Light::read_xml(doc.root_element());
        // the XML leaves out the unknowns the C# does not write
        back.unknown_0h = l.unknown_0h; back.unknown_4h = l.unknown_4h; back.unknown_14h = l.unknown_14h;
        back.unknown_48h = l.unknown_48h; back.unknown_a4h = l.unknown_a4h;
        assert_eq!(back, l);
    }
}
