//! The World tab's renderer: many instances of a few hundred models, textured, with a sky and fog, drawn into an offscreen texture
//! that egui shows as an image (the same way `gpu.rs` does for the model Viewer). Models arrive from worker threads as ready
//! GPU buffers ([`GpuModel`]); the caller sends one transform per placed entity each frame.
use eframe::egui::{self, mutex::RwLock};
use eframe::egui_wgpu::{self, wgpu};
use glam::{Mat4, Vec3};
use std::{collections::HashMap, sync::{Arc, Mutex}};
use wgpu::util::DeviceExt;

const SAMPLES: u32 = 4;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// side of the sun's depth map, and how far round the camera (metres) it reaches
const SHADOW_SIZE: u32 = 2048;
/// the sun's shadow is cut into cascades: each covers a square of this half size round the camera at the same map size, so what is near gets
/// finer shadows than what is far
const CASCADE_RADII: [f32; 3] = [40.0, 150.0, 600.0];

/// Where the light is at `hour`: the sun on an arc from east (6:00) over the south (13:00) to the west (20:00), tilted by the timecycle's
/// sun roll (122 degrees); between 21:00 and 5:00 the moon, on its own arc. It never goes below the horizon.
/// how strong the specular term is (a material's `specularIntensityMult` would set it, which this viewer does not read: one value for all;
/// CRCK_WORLD_SPEC tries others)
fn spec_scale() -> f32 {
    static V: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("CRCK_WORLD_SPEC").ok().and_then(|v| v.parse().ok()).unwrap_or(0.3))
}

fn hdr_light_dir(hour: f32) -> Vec3 {
    let pi = std::f32::consts::PI;
    let roll = 122f32.to_radians();
    let turn = |v: Vec3, a: f32| Vec3::new(v.x, v.y * a.cos() - v.z * a.sin(), v.y * a.sin() + v.z * a.cos());
    let day = pi * (0.5 + (hour - 6.0) / 14.0);
    let night = pi * (if hour > 12.0 { hour - 7.0 } else { hour + 17.0 }) / 9.0;
    let sun = turn(Vec3::new(day.sin(), -day.cos(), 0.0), roll).normalize();
    let moon = turn(Vec3::new(-night.sin(), 0.0, -night.cos()), roll).normalize();
    let mut l = if !(5.0..=21.0).contains(&hour) { moon } else { sun };
    l.z = l.z.max(0.0);
    if l.length_squared() < 1e-6 { Vec3::Z } else { l.normalize() }
}

/// The exposure the tone mapping settles on: 0.72 over the mean brightness of the picture, which is limited to 0.2..10 (so in
/// full daylight it is a fixed 0.072). The mean is estimated from the light and the sky rather than measured: the sky as a third of the picture, the rest
/// surfaces of albedo 0.3 lit by the ambient light and by the sun at about a third of its strength.
fn hdr_exposure(t: &rage_toolkit_core::timecycle::Sky) -> f32 {
    let lum = |c: [f32; 3]| c[0] * 0.299 + c[1] * 0.587 + c[2] * 0.114;
    let avg = |a: [f32; 3], b: [f32; 3], c: [f32; 3], d: [f32; 3]| [0, 1, 2].map(|k| (a[k] + b[k] + c[k] + d[k]) * 0.25);
    let sky = lum(avg(t.zenith, t.east, t.west, t.azimuth_mid)) * t.sky_hdr.max(1.0);
    let surfaces = 0.3 * (lum(t.amb_dir_hdr) + 0.5 * (lum(t.nat_up_hdr) + lum(t.nat_down_hdr)) + 0.35 * lum(t.sun_hdr));
    let mean = (0.3 * sky + 0.7 * surfaces).clamp(0.2, 10.0);
    0.72 / (mean + 0.001)
}

const SHADER: &str = r#"
struct Globals {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    eye: vec4<f32>,     // xyz camera, w fog density
    sun: vec4<f32>,     // xyz direction towards the sun
    tod: vec4<f32>,     // x daylight 0..1, y dawn/dusk warmth 0..1, z shadows on, w debug view (0 normal, 1 texture, 2 normals, 3 tangents, 4 vertex AO, 5 UV, 6 LOD level, 7 vertex colour, 8 vertex alpha, 9 blended parts magenta, 10 sky magenta)
    light_vp: array<mat4x4<f32>, 3>,
    wx: vec4<f32>,      // weather: x cloud cover 0..1, y fog density factor (already in eye.w), z sunlight strength 0..1, w linear light
    casc: vec4<f32>,    // the three cascade half sizes; w is the cascade a shadow pass draws
    tm: vec4<f32>,      // x time in seconds (the water's waves)
    // the timecycle's light, sky and fog (linear colours); sun_col.w is 1 when they are used, 0 for the fixed day curve
    sun_col: vec4<f32>, // direct light
    amb_up: vec4<f32>,
    amb_dn: vec4<f32>,
    fog_col: vec4<f32>,
    zen: vec4<f32>,     // w the position of the middle colour
    zen_mid: vec4<f32>,
    east: vec4<f32>,
    west: vec4<f32>,
    az_mid: vec4<f32>,  // w the position of the middle colour
    disc: vec4<f32>,    // the sun's glow
    cl_lit: vec4<f32>,
    cl_base: vec4<f32>,
    amb_dir: vec4<f32>, // HDR lighting: the directional ambient (HDR)
    exp: vec4<f32>,     // x exposure, y the sky's HDR multiplier, z 1 = HDR lighting
    zcon: vec4<f32>,    // the sky's zenith constants: blend start, east blend, west blend, transition position
};
@group(0) @binding(0) var<uniform> g: Globals;
@group(1) @binding(0) var diff_t: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;
@group(2) @binding(0) var norm_t: texture_2d<f32>;
@group(2) @binding(1) var spec_t: texture_2d<f32>;
// a terrain part's layers 1-3 and its blend mask (group 2 holds these instead of the normal and specular maps)
@group(2) @binding(2) var lay1_t: texture_2d<f32>;
@group(2) @binding(3) var lay2_t: texture_2d<f32>;
@group(2) @binding(4) var lay3_t: texture_2d<f32>;
@group(2) @binding(5) var mask_t: texture_2d<f32>;
@group(3) @binding(0) var shadow_t: texture_depth_2d_array;
@group(3) @binding(1) var shadow_s: sampler_comparison;

// how much of the sun reaches `world`: 1 lit, 0 in shadow. The cascade is chosen by the distance from the camera (the nearest one that still
// holds the point), and its depth map is filtered 3 by 3; always 1 when shadows are off or beyond the last cascade.
fn shadow_at(world: vec3<f32>, n: vec3<f32>) -> f32 {
    if (g.tod.z < 0.5) { return 1.0; }
    let d = length((world - g.eye.xyz).xy);
    var c = 2;
    var radius = g.casc.z;
    if (d < g.casc.x * 0.92) { c = 0; radius = g.casc.x; }
    else if (d < g.casc.y * 0.92) { c = 1; radius = g.casc.y; }
    else if (d > g.casc.z * 0.95) { return 1.0; }
    // the texel grows with the cascade, so the normal offset does too
    let p = g.light_vp[c] * vec4<f32>(world + n * (0.05 + radius * 0.0012), 1.0);
    let uv = vec2<f32>(p.x * 0.5 + 0.5, 0.5 - p.y * 0.5);
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 || p.z > 1.0) { return 1.0; }
    var lit = 0.0;
    let texel = 1.0 / 2048.0;
    for (var dx = -1; dx <= 1; dx = dx + 1) {
        for (var dy = -1; dy <= 1; dy = dy + 1) {
            lit = lit + textureSampleCompareLevel(shadow_t, shadow_s, uv + vec2<f32>(f32(dx), f32(dy)) * texel, c, p.z - 0.0007);
        }
    }
    return lit / 9.0;
}

// Lighting. With linear light on (the game shades in linear light: textures are sRGB, the lighting is added in linear and the
// result is tone-mapped back), the albedo is made linear, lit, softly clipped and turned back to display values; off, it is the older shading
// done directly on display values.
fn shade(albedo: vec3<f32>, amb: vec3<f32>, direct: vec3<f32>, glint: f32) -> vec3<f32> {
    if (g.wx.w < 0.5) { return albedo * (amb + direct) + vec3<f32>(glint); }
    let a = pow(albedo, vec3<f32>(2.2));
    let c = a * (amb * 0.8 + direct * 1.6) + vec3<f32>(glint * 0.8);
    let m = c * (1.0 + c / 4.0) / (1.0 + c);
    return pow(clamp(m, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(1.0 / 2.2));
}

fn tc_on() -> bool { return g.sun_col.w > 0.5; }
// HDR lighting: the light is in HDR units (the sun about 64 at noon), the picture's own values are used as they are
// (no gamma), the result is scaled by an exposure and passed through a soft shoulder, and shown as it is
fn hdr_on() -> bool { return g.sun_col.w > 0.5 && g.exp.z > 0.5; }
fn tonemap(x: vec3<f32>) -> vec3<f32> {
    var c = max(x, vec3<f32>(0.0)) * g.exp.x;
    c = c * (1.0 + c / 1.5);
    return c / (1.0 + c);
}
// The lighting: the picture times (the directional ambient + the sun times how much of it reaches the surface), plus the natural
// ambient (weighted by the normal's height, the "down" colour for what faces up, as it does) times the baked occlusion of the vertex colour's red
fn hdr_light(albedo: vec3<f32>, n: vec3<f32>, lit: f32, ao: f32) -> vec3<f32> {
    let a = clamp(albedo, vec3<f32>(0.0), vec3<f32>(1.0));
    let lf = max(dot(n, normalize(g.sun.xyz)), 0.0) * lit;
    let bf = n.z * 0.5 + 0.5;
    let amb = g.amb_up.xyz * clamp(1.0 - bf, 0.0, 1.0) + g.amb_dn.xyz * clamp(bf, 0.0, 1.0);
    return a * (g.amb_dir.xyz + g.sun_col.xyz * lf) + a * amb * ao;
}
// its specular term: the sun's colour times a sharp lobe about the mirror direction, times the specular map squared
fn hdr_spec(n: vec3<f32>, to_eye: vec3<f32>, sp: f32, lit: f32) -> vec3<f32> {
    let refl = reflect(-to_eye, n);
    let specb = clamp(dot(refl, normalize(g.sun.xyz)), 0.0, 1.0);
    let specp = max(exp(specb * 10.0) - 1.0, 0.0);
    return g.sun_col.xyz * (0.00006 * g.exp.w) * specp * sp * sp * lit;
}
fn hdr_fog(hdr: vec3<f32>, dist: f32) -> vec3<f32> {
    let fog = clamp(1.0 - exp(-dist * g.eye.w), 0.0, 0.92);
    return mix(hdr, sky_gradient(vec3<f32>(0.0, 1.0, 0.0)) * max(g.exp.y, 1.0), fog);
}
fn hdr_finish(hdr: vec3<f32>, dist: f32) -> vec3<f32> {
    return tonemap(hdr_fog(hdr, dist));
}
fn disp(c: vec3<f32>) -> vec3<f32> { return pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.2)); }

// the sun's colour as light on a surface facing it
fn sun_term() -> vec3<f32> {
    if (tc_on()) { return g.sun_col.xyz; }
    return vec3<f32>(0.62 * g.tod.x * g.wx.z) * mix(vec3<f32>(1.0, 1.0, 1.0), vec3<f32>(1.0, 0.62, 0.35), g.tod.y);
}
// how strong the sun is (1 at noon): glints and the water's sparkle
fn sun_k() -> f32 {
    if (tc_on()) { return (g.sun_col.x + g.sun_col.y + g.sun_col.z) / 1.86; }
    return g.tod.x * g.wx.z;
}
// the ambient light for a surface whose normal points `nz` up
fn ambient_at(nz: f32) -> vec3<f32> {
    if (tc_on()) { return mix(g.amb_dn.xyz, g.amb_up.xyz, nz * 0.5 + 0.5); }
    return vec3<f32>(mix(0.34, 0.58, nz * 0.5 + 0.5) * mix(0.30, 1.0, g.tod.x));
}

// the colour of the sky at the horizon: it is also the fog colour
fn horizon_col() -> vec3<f32> {
    if (tc_on()) { return disp(g.fog_col.xyz); }
    let day = vec3<f32>(0.70, 0.78, 0.88);
    let night = vec3<f32>(0.025, 0.035, 0.07);
    let c = mix(night, day, g.tod.x) + vec3<f32>(0.30, 0.13, 0.0) * g.tod.y * 0.6;
    // under cloud the horizon goes grey
    let grey = vec3<f32>((c.r + c.g + c.b) / 3.0) * 0.9;
    return mix(c, grey, g.wx.x * 0.7);
}

fn hash2(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2<f32>(127.1, 311.7))) * 43758.5453);
}
fn vnoise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    return mix(mix(hash2(i), hash2(i + vec2<f32>(1.0, 0.0)), u.x), mix(hash2(i + vec2<f32>(0.0, 1.0)), hash2(i + vec2<f32>(1.0, 1.0)), u.x), u.y);
}
fn fbm(p: vec2<f32>) -> f32 {
    var a = 0.5;
    var q = p;
    var s = 0.0;
    for (var k = 0; k < 5; k = k + 1) {
        s = s + a * vnoise(q);
        q = q * 2.03 + vec2<f32>(17.0, 9.0);
        a = a * 0.5;
    }
    return s;
}

// The sky gradient: the azimuth colours east, middle and west by the direction's east-west position, moved towards
// the middle zenith colour by a blend that depends on that position, and towards the zenith colour with height. In its own units (times the HDR
// multiplier by the caller).
fn sky_gradient(r: vec3<f32>) -> vec3<f32> {
    let zc = g.zcon;
    let ap = g.az_mid.w;
    let el = abs(r.z);
    var x = clamp((el - zc.x) / max(1.0 - zc.x, 0.0001), 0.0, 1.0);
    x = clamp(x / max(zc.w, 0.0001), 0.0, 1.0);
    let y = sqrt(clamp((1.0 - r.x / max(length(r.xy), 0.0001)) * 0.5, 0.0, 1.0));
    var az = mix(g.az_mid.xyz, g.west.xyz, clamp((y - ap) / max(1.0 - ap, 0.0001), 0.0, 1.0));
    if (y < ap) { az = mix(g.east.xyz, g.az_mid.xyz, y / max(ap, 0.0001)); }
    let zt = g.zen_mid.xyz - az;
    let yz = y * (zc.z - zc.y) + zc.y;
    var col = mix(zt * yz + az, g.zen.xyz, x);
    if (el < zc.x) { col = zt * yz * (el / max(zc.x, 0.0001)) + az; }
    return col;
}

// the game's sky: the azimuth colours (west, middle, east) round the horizon, blending up through the middle zenith colour to the zenith
fn sky_tc(r: vec3<f32>) -> vec3<f32> {
    let horizon = horizon_col();
    let t = clamp(r.z, -1.0, 1.0);
    if (t < 0.0) { return mix(horizon, horizon * 0.6, clamp(-t * 6.0, 0.0, 1.0)); }
    let e = clamp(r.x / max(length(r.xy), 0.0001) * 0.5 + 0.5, 0.0, 1.0);
    let ap = g.az_mid.w;
    let az = select(mix(g.west.xyz, g.az_mid.xyz, e / ap), mix(g.az_mid.xyz, g.east.xyz, (e - ap) / (1.0 - ap)), e > ap);
    let u = pow(t, 0.6);
    let zp = g.zen.w;
    let c = select(mix(az, g.zen_mid.xyz, u / zp), mix(g.zen_mid.xyz, g.zen.xyz, (u - zp) / (1.0 - zp)), u > zp);
    return mix(horizon, disp(c), smoothstep(0.0, 0.1, t));
}

fn sky(r: vec3<f32>) -> vec3<f32> {
    if (hdr_on()) {
        // the pale haze of cloud and dust along the horizon (thick near the horizon): the gradient is
        // mixed towards a light blue-white of a middle brightness. A fit to what it draws, not its cloud shader.
        let hdr = max(g.exp.y, 1.0);
        let haze = vec3<f32>(0.80, 0.90, 1.0) * hdr * 0.31;
        return mix(sky_gradient(r) * hdr, haze, smoothstep(0.34, 0.0, abs(r.z)) * 0.78);
    }
    if (tc_on()) { return sky_tc(r); }
    let horizon = horizon_col();
    let zenith = mix(vec3<f32>(0.005, 0.01, 0.04), vec3<f32>(0.22, 0.42, 0.80), g.tod.x);
    let ground = mix(vec3<f32>(0.02, 0.02, 0.03), vec3<f32>(0.36, 0.38, 0.36), g.tod.x);
    let t = clamp(r.z, -1.0, 1.0);
    if (t >= 0.0) { return mix(horizon, zenith, pow(t, 0.55)); }
    // below the horizon nothing is loaded that far: haze rather than a dark ground, so it reads as distance
    return mix(horizon, mix(horizon, ground, 0.35), clamp(-t * 6.0, 0.0, 1.0));
}

struct SOut { @builtin(position) clip: vec4<f32>, @location(0) ndc: vec2<f32> };
@vertex fn vs_sky(@builtin(vertex_index) i: u32) -> SOut {
    var p = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    var o: SOut;
    o.clip = vec4<f32>(p[i], 1.0, 1.0);
    o.ndc = p[i];
    return o;
}
@fragment fn fs_sky(i: SOut) -> @location(0) vec4<f32> {
    // debug view 10: the sky is flat magenta, so that anything the world leaves open shows up against it
    if (i32(g.tod.w + 0.5) == 10) { return vec4<f32>(1.0, 0.0, 1.0, 1.0); }
    let far = g.inv_view_proj * vec4<f32>(i.ndc, 1.0, 1.0);
    let r = normalize(far.xyz / far.w - g.eye.xyz);
    var c = sky(r);
    let sd = max(dot(r, normalize(g.sun.xyz)), 0.0);
    let sun = pow(sd, 600.0) * select(g.wx.z, 1.0, tc_on());
    if (hdr_on()) {
        // the sun: a small bright disc and a soft glow round it, in HDR units
        c = c + g.disc.xyz * max(g.exp.y, 1.0) * (pow(sd, 1500.0) * 1.5 + pow(sd, 24.0) * 0.05);
    } else {
        c = c + select(vec3<f32>(1.0, 0.95, 0.8), disp(g.disc.xyz), tc_on()) * sun;
    }
    // one layer of cloud on a plane overhead: more of the noise passes the threshold as the cover grows; lit by the sun, grey and dark when thick
    if (r.z > 0.02 && g.wx.x > 0.02) {
        let uv = r.xy / (r.z + 0.12) * 1.3;
        let n = fbm(uv);
        let dens = smoothstep(1.0 - g.wx.x * 0.85, 1.0 - g.wx.x * 0.85 + 0.25, n);
        let day = mix(0.12, 1.0, g.tod.x);
        let base = mix(vec3<f32>(1.0, 1.0, 1.0), vec3<f32>(0.55, 0.57, 0.62), g.wx.x * smoothstep(0.3, 0.9, n));
        var lit = base * day * (0.75 + 0.25 * pow(sd, 3.0)) + vec3<f32>(0.30, 0.13, 0.0) * g.tod.y * 0.5 * day;
        if (tc_on()) { lit = mix(disp(g.cl_lit.xyz), disp(g.cl_base.xyz), g.wx.x * smoothstep(0.3, 0.9, n)) * (0.75 + 0.25 * pow(sd, 3.0)); }
        if (hdr_on()) { lit = mix(g.cl_lit.xyz, g.cl_base.xyz, g.wx.x * smoothstep(0.3, 0.9, n)) * (0.75 + 0.25 * pow(sd, 3.0)) * max(g.exp.y, 1.0); }
        c = mix(c, lit, dens * smoothstep(0.02, 0.2, r.z));
    }
    if (hdr_on()) { return vec4<f32>(tonemap(c), 1.0); }
    return vec4<f32>(c, 1.0);
}

struct VIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(7) col: vec4<f32>,
    @location(8) tan: vec4<f32>,
    @location(3) m0: vec4<f32>,
    @location(4) m1: vec4<f32>,
    @location(5) m2: vec4<f32>,
    @location(6) m3: vec4<f32>,
};
struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) ao: f32,
    @location(4) tan: vec4<f32>,
    @location(5) lvl: f32,
    @location(6) vcol: vec4<f32>,
};
@vertex fn vs_main(v: VIn) -> VOut {
    // the fourth row of an instance matrix is free: the first column's w holds 1 + the entity's LOD level (0: none), for the LOD colour view
    let m = mat4x4<f32>(vec4<f32>(v.m0.xyz, 0.0), v.m1, v.m2, v.m3);
    let w = m * vec4<f32>(v.pos, 1.0);
    var o: VOut;
    o.clip = g.view_proj * w;
    o.world = w.xyz;
    o.lvl = v.m0.w - 1.0;
    o.vcol = v.col;
    // the instance matrix is a rotation with a scale: its upper 3x3 turns the normal well enough
    o.normal = (m * vec4<f32>(v.normal, 0.0)).xyz;
    o.uv = v.uv;
    o.tan = vec4<f32>((m * vec4<f32>(v.tan.xyz, 0.0)).xyz, v.tan.w);
    // the baked colour is the game's ambient occlusion (grey) and tint; it darkens the ambient light in creases and under eaves
    // only grey is ambient occlusion: a coloured vertex colour is a tint or a layer weight, and must not darken the light
    let spread = max(v.col.r, max(v.col.g, v.col.b)) - min(v.col.r, min(v.col.g, v.col.b));
    o.ao = select(1.0, clamp(mix(1.0, (v.col.r + v.col.g + v.col.b) / 3.0, 0.9), 0.4, 1.0), spread < 0.118);
    return o;
}
@vertex fn vs_shadow(v: VIn) -> @builtin(position) vec4<f32> {
    let m = mat4x4<f32>(vec4<f32>(v.m0.xyz, 0.0), v.m1, v.m2, v.m3);
    return g.light_vp[u32(g.casc.w)] * (m * vec4<f32>(v.pos, 1.0));
}
@fragment fn fs_main(i: VOut) -> @location(0) vec4<f32> {
    let tex = textureSample(diff_t, samp, i.uv);
    if (tex.a < 0.4) { discard; }
    let view = g.eye.xyz - i.world;
    let dist = length(view);
    var n = normalize(i.normal);
    // HDR lighting uses a surface's stored normal, whichever side is seen; the older shading turns it towards the viewer
    if (!hdr_on() && dot(n, view) < 0.0) { n = -n; }
    let dbg = i32(g.tod.w + 0.5);
    if (dbg == 1) { return vec4<f32>(tex.rgb, 1.0); }
    if (dbg == 2) { return vec4<f32>(n * 0.5 + 0.5, 1.0); }
    if (dbg == 3) { return vec4<f32>(i.tan.xyz * 0.5 + 0.5, 1.0); }
    if (dbg == 4) { return vec4<f32>(vec3<f32>(i.ao), 1.0); }
    if (dbg == 5) { return vec4<f32>(fract(i.uv), 0.0, 1.0); }
    if (dbg == 7) { return vec4<f32>(i.vcol.rgb, 1.0); }
    if (dbg == 8) { return vec4<f32>(vec3<f32>(i.vcol.a), 1.0); }
    if (dbg == 6) {
        // a colour per LOD level: HD green, LOD yellow, SLOD1 orange, SLOD2 red, SLOD3 magenta, orphan HD cyan, SLOD4 violet
        var c = vec3<f32>(0.6, 0.6, 0.6);
        let l = i32(i.lvl + 0.5);
        if (l == 0) { c = vec3<f32>(0.2, 0.85, 0.25); }
        else if (l == 1) { c = vec3<f32>(0.95, 0.9, 0.15); }
        else if (l == 2) { c = vec3<f32>(1.0, 0.55, 0.1); }
        else if (l == 3) { c = vec3<f32>(0.95, 0.15, 0.15); }
        else if (l == 4) { c = vec3<f32>(0.9, 0.2, 0.8); }
        else if (l == 5) { c = vec3<f32>(0.2, 0.85, 0.9); }
        else if (l == 6) { c = vec3<f32>(0.5, 0.3, 0.95); }
        return vec4<f32>(mix(tex.rgb * 0.5, c, 0.55) * (0.5 + 0.5 * max(dot(n, normalize(g.sun.xyz)), 0.0)), 1.0);
    }
    // the normal map (its red and green hold x and y), turned into the surface's frame
    let nm = textureSample(norm_t, samp, i.uv).rg * 2.0 - 1.0;
    let nz = sqrt(max(0.0, 1.0 - dot(nm, nm)));
    // a tangent that is missing, or lies along the normal, leaves the normal alone (normalising it would give NaN: black pixels)
    let tr = i.tan.xyz - n * dot(n, i.tan.xyz);
    if (dot(tr, tr) > 0.0004) {
        let tn = normalize(tr);
        let bent = tn * nm.x + cross(n, tn) * (nm.y * select(1.0, i.tan.w, abs(i.tan.w) > 0.5)) + n * nz;
        if (dot(bent, bent) > 0.0001) { n = normalize(bent); }
    }
    let sun = normalize(g.sun.xyz);
    let ndl = max(dot(n, sun), 0.0);
    let spec = textureSample(spec_t, samp, i.uv).r;
    let half_v = normalize(sun + normalize(view));
    let lit = shadow_at(i.world, n);
    if (hdr_on()) {
        let col = hdr_light(tex.rgb, n, lit, i.vcol.r) + hdr_spec(n, view / max(dist, 0.001), spec, lit);
        return vec4<f32>(hdr_finish(col, dist), 1.0);
    }
    let glint = pow(max(dot(n, half_v), 0.0), 40.0) * spec * 0.6 * sun_k() * lit;
    let col = shade(tex.rgb, ambient_at(n.z) * i.ao, ndl * lit * sun_term(), glint);
    let fog = 1.0 - exp(-dist * g.eye.w);
    return vec4<f32>(mix(col, horizon_col(), clamp(fog, 0.0, 0.92)), 1.0);
}

// terrain: four texture layers blended by a mask (the second texture coordinates, kept in tan.xy):
// (layer 0, layer 1 by the mask's blue) and (layer 2, layer 3 by the mask's blue), those two by the mask's green
@fragment fn fs_terrain(i: VOut) -> @location(0) vec4<f32> {
    let c0 = textureSample(diff_t, samp, i.uv).rgb;
    let c1 = textureSample(lay1_t, samp, i.uv).rgb;
    let c2 = textureSample(lay2_t, samp, i.uv).rgb;
    let c3 = textureSample(lay3_t, samp, i.uv).rgb;
    let m = textureSample(mask_t, samp, i.tan.xy);
    let tv = mix(mix(c0, c1, m.b), mix(c2, c3, m.b), m.g);
    let view = g.eye.xyz - i.world;
    let dist = length(view);
    var n = normalize(i.normal);
    // HDR lighting uses a surface's stored normal, whichever side is seen; the older shading turns it towards the viewer
    if (!hdr_on() && dot(n, view) < 0.0) { n = -n; }
    let dbg = i32(g.tod.w + 0.5);
    if (dbg == 1) { return vec4<f32>(tv, 1.0); }
    if (dbg == 2) { return vec4<f32>(n * 0.5 + 0.5, 1.0); }
    if (dbg == 6) { return vec4<f32>(mix(tv * 0.5, vec3<f32>(0.2, 0.9, 0.4), 0.55), 1.0); }
    let sun = normalize(g.sun.xyz);
    let ndl = max(dot(n, sun), 0.0);
    let lit = shadow_at(i.world, n);
    if (hdr_on()) { return vec4<f32>(hdr_finish(hdr_light(tv, n, lit, 1.0), dist), 1.0); }
    let col = shade(tv, ambient_at(n.z), ndl * lit * sun_term(), 0.0);
    let fog = 1.0 - exp(-dist * g.eye.w);
    return vec4<f32>(mix(col, horizon_col(), clamp(fog, 0.0, 0.92)), 1.0);
}

// decals, glass and other blended shaders: the texture's own alpha, lit the same way, fogged
@fragment fn fs_blend(i: VOut) -> @location(0) vec4<f32> {
    let tex = textureSample(diff_t, samp, i.uv);
    if (i32(g.tod.w + 0.5) == 9) { return vec4<f32>(1.0, 0.0, 1.0, 0.6); }
    if (tex.a < 0.02) { discard; }
    let view = g.eye.xyz - i.world;
    let dist = length(view);
    var n = normalize(i.normal);
    // HDR lighting uses a surface's stored normal, whichever side is seen; the older shading turns it towards the viewer
    if (!hdr_on() && dot(n, view) < 0.0) { n = -n; }
    if (hdr_on()) { return vec4<f32>(hdr_finish(hdr_light(tex.rgb, n, 1.0, i.vcol.r), dist), tex.a * i.vcol.a); }
    let ndl = max(dot(n, normalize(g.sun.xyz)), 0.0);
    let col = shade(tex.rgb, ambient_at(n.z) * i.ao, ndl * sun_term(), 0.0);
    let fog = clamp(1.0 - exp(-dist * g.eye.w), 0.0, 0.92);
    return vec4<f32>(mix(col, horizon_col(), fog), tex.a);
}

// water: the surface is bent by four crossing waves (their slopes make the normal), mirrors the sky in proportion to how flat the view is
// (Fresnel), shows the water's own colour more when looked at from above, and catches the sun in glints. Not the game's water shader (which also
// uses flow maps, foam and depth), but the same ingredients.
fn wave_normal(p: vec2<f32>, dist: f32) -> vec3<f32> {
    let t = g.tm.x;
    // far waves are fainter: they would only shimmer
    let fade = 1.0 / (1.0 + dist * 0.008);
    var d = vec2<f32>(0.0, 0.0);
    let w0 = dot(vec2<f32>(0.8, 0.6), p) * 0.35 + t * 0.9;
    d = d + vec2<f32>(0.8, 0.6) * (0.35 * 0.06 * cos(w0));
    let w1 = dot(vec2<f32>(-0.4, 0.9), p) * 0.8 + t * 1.3;
    d = d + vec2<f32>(-0.4, 0.9) * (0.8 * 0.03 * cos(w1));
    let w2 = dot(vec2<f32>(0.95, -0.3), p) * 1.7 + t * 1.9;
    d = d + vec2<f32>(0.95, -0.3) * (1.7 * 0.015 * cos(w2));
    let w3 = dot(vec2<f32>(-0.7, -0.7), p) * 3.5 + t * 2.6;
    d = d + vec2<f32>(-0.7, -0.7) * (3.5 * 0.008 * cos(w3));
    return normalize(vec3<f32>(-d.x * fade, -d.y * fade, 1.0));
}
@fragment fn fs_water(i: VOut) -> @location(0) vec4<f32> {
    let tex = textureSample(diff_t, samp, i.uv);
    let view = g.eye.xyz - i.world;
    let dist = length(view);
    let v = view / max(dist, 0.001);
    let n = wave_normal(i.world.xy, dist);
    let cosv = clamp(dot(n, v), 0.0, 1.0);
    let fres = 0.02 + 0.98 * pow(1.0 - cosv, 5.0);
    let r = reflect(-v, n);
    let sky_col = sky(vec3<f32>(r.x, r.y, abs(r.z)));
    let sun = normalize(g.sun.xyz);
    if (hdr_on()) {
        let body = hdr_light(tex.rgb, n, 1.0, 1.0);
        let wcol = mix(body, sky_col, fres) + g.sun_col.xyz * pow(max(dot(r, sun), 0.0), 220.0) * 0.05;
        return vec4<f32>(hdr_finish(wcol, dist), clamp(tex.a + fres * 0.6, 0.0, 1.0));
    }
    let glint = pow(max(dot(r, sun), 0.0), 220.0) * 2.0 * sun_k();
    let day = select(mix(0.25, 1.0, g.tod.x), 0.25 + 0.75 * clamp(sun_k() * 2.0, 0.0, 1.0), tc_on());
    var body = tex.rgb * day;
    if (g.wx.w > 0.5) { body = pow(body, vec3<f32>(1.2)); }
    var col = mix(body, sky_col, fres) + vec3<f32>(glint);
    let fog = clamp(1.0 - exp(-dist * g.eye.w), 0.0, 0.92);
    col = mix(col, horizon_col(), fog);
    // see-through from above, nearly opaque where it mirrors
    let a = clamp(tex.a + fres * 0.6, 0.0, 1.0);
    return vec4<f32>(col, a);
}

// collision: flat colour per surface material, lit by the sun; g.sun.w is how opaque it is
@fragment fn fs_coll(i: VOut) -> @location(0) vec4<f32> {
    let tex = textureSample(diff_t, samp, i.uv);
    let view = g.eye.xyz - i.world;
    var n = normalize(i.normal);
    // HDR lighting uses a surface's stored normal, whichever side is seen; the older shading turns it towards the viewer
    if (!hdr_on() && dot(n, view) < 0.0) { n = -n; }
    let ndl = max(dot(n, normalize(g.sun.xyz)), 0.0);
    return vec4<f32>(tex.rgb * (0.45 + 0.55 * ndl), g.sun.w);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    inv_view_proj: [[f32; 4]; 4],
    eye: [f32; 4],
    sun: [f32; 4],
    tod: [f32; 4],
    light_vp: [[[f32; 4]; 4]; 3],
    wx: [f32; 4],
    casc: [f32; 4],
    tm: [f32; 4],
    sun_col: [f32; 4],
    amb_up: [f32; 4],
    amb_dn: [f32; 4],
    fog_col: [f32; 4],
    zen: [f32; 4],
    zen_mid: [f32; 4],
    east: [f32; 4],
    west: [f32; 4],
    az_mid: [f32; 4],
    disc: [f32; 4],
    cl_lit: [f32; 4],
    cl_base: [f32; 4],
    amb_dir: [f32; 4],
    exp: [f32; 4],
    zcon: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub col: [u8; 4],
    pub tan: [f32; 4],
}

const VERTEX_ATTRS: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 7 => Unorm8x4, 8 => Float32x4];
const INSTANCE_ATTRS: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![3 => Float32x4, 4 => Float32x4, 5 => Float32x4, 6 => Float32x4];

/// One drawable part of a model on the GPU.
pub struct GpuPart {
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    index_count: u32,
    bind_group: Arc<wgpu::BindGroup>,
    /// the normal map and the specular map (flat ones when the shader has none)
    bump_group: Arc<wgpu::BindGroup>,
    /// 0 opaque or cut out, 1 decal (blended over the surface it lies on), 2 translucent (glass, alpha shaders)
    blend: u8,
    /// the triangle edges as lines (collision parts only), for the wireframe view
    wire: Option<(wgpu::Buffer, u32)>,
}

pub struct GpuModel {
    pub parts: Vec<GpuPart>,
}

/// What a worker thread needs to build GPU resources: cheap to clone, usable from any thread.
#[derive(Clone)]
pub struct Factory {
    device: wgpu::Device,
    queue: wgpu::Queue,
    tex_layout: Arc<wgpu::BindGroupLayout>,
    sampler: Arc<wgpu::Sampler>,
    white: Arc<wgpu::BindGroup>,
    /// texture key -> its bind group (shared by every part that uses the texture)
    textures: Arc<Mutex<HashMap<String, (Arc<wgpu::BindGroup>, u64)>>>,
    /// keys of pictures uploaded smaller than they could be (memory was short): models using them are loaded again when the camera comes close
    reduced: Arc<Mutex<std::collections::HashSet<String>>>,
    /// bytes of pictures (with their mips) on the GPU through [`Factory::add_texture`]
    tex_bytes: Arc<std::sync::atomic::AtomicU64>,
    /// texture key -> its view, for the normal and specular maps of a part
    views: Arc<Mutex<HashMap<String, Arc<wgpu::TextureView>>>>,
    /// textures whose alpha is soft (many values between clear and solid): decals, dirt, glass; a hard cut-out edge is not
    soft: Arc<Mutex<std::collections::HashSet<String>>>,
    bump_layout: Arc<wgpu::BindGroupLayout>,
    flat_normal: Arc<wgpu::TextureView>,
    no_spec: Arc<wgpu::TextureView>,
    /// (normal key, specular key) -> bind group
    bump_groups: Arc<Mutex<HashMap<(String, String), Arc<wgpu::BindGroup>>>>,
    flat_group: Arc<wgpu::BindGroup>,
    terrain_layout: Arc<wgpu::BindGroupLayout>,
}

impl Factory {
    pub fn has_texture(&self, key: &str) -> bool {
        self.textures.lock().unwrap().contains_key(key)
    }

    /// whether the texture has soft alpha (see `soft`)
    pub fn is_soft(&self, key: &str) -> bool {
        self.soft.lock().unwrap().contains(key)
    }

    /// bytes of pictures on the GPU
    pub fn texture_bytes(&self) -> u64 {
        self.tex_bytes.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Free the pictures no part uses any more (those only the cache holds), and the normal/specular pairs nothing uses; a picture a model still
    /// draws with is kept. Returns how many pictures went.
    pub fn evict_unused(&self) -> usize {
        self.bump_groups.lock().unwrap().retain(|_, g| Arc::strong_count(g) > 1);
        let mut textures = self.textures.lock().unwrap();
        let mut views = self.views.lock().unwrap();
        let mut gone = 0;
        let mut freed = 0u64;
        textures.retain(|k, (bg, bytes)| {
            if Arc::strong_count(bg) > 1 {
                return true;
            }
            views.remove(k);
            self.reduced.lock().unwrap().remove(k);
            freed += *bytes;
            gone += 1;
            false
        });
        self.tex_bytes.fetch_sub(freed, std::sync::atomic::Ordering::Relaxed);
        gone
    }

    pub fn mark_reduced(&self, key: &str) {
        self.reduced.lock().unwrap().insert(key.to_string());
    }

    pub fn is_reduced(&self, key: &str) -> bool {
        self.reduced.lock().unwrap().contains(key)
    }

    pub fn texture_count(&self) -> usize {
        self.textures.lock().unwrap().len()
    }

    /// Upload `rgba` (with mip levels) under `key` unless it is already there.
    pub fn add_texture(&self, key: &str, w: u32, h: u32, rgba: &[u8]) {
        if self.has_texture(key) {
            return;
        }
        let mid = rgba.chunks_exact(4).filter(|p| p[3] > 24 && p[3] < 232).count();
        let view = crate::gpu::texture_view(&self.device, &self.queue, w, h, rgba);
        // the picture and its mips, about 5 bytes a pixel
        self.register_texture(key, view, w as u64 * h as u64 * 5, mid * 100 > (rgba.len() / 4) * 12);
    }

    /// Whether the device reads block-compressed pictures (BC1-BC7): then textures are uploaded as stored, with their own mip levels.
    pub fn bc_supported(&self) -> bool {
        self.device.features().contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    }

    /// Upload a texture as the game stores it (see `Ytd::compressed`); false when the device cannot read it.
    pub fn add_texture_bc(&self, key: &str, c: &rage_toolkit_core::worldmodel::Compressed) -> bool {
        use rage_toolkit_core::worldmodel::Bc;
        if !self.bc_supported() {
            return false;
        }
        if self.has_texture(key) {
            return true;
        }
        let format = match c.kind {
            Bc::Bc1 => wgpu::TextureFormat::Bc1RgbaUnorm,
            Bc::Bc2 => wgpu::TextureFormat::Bc2RgbaUnorm,
            Bc::Bc3 => wgpu::TextureFormat::Bc3RgbaUnorm,
            Bc::Bc7 => wgpu::TextureFormat::Bc7RgbaUnorm,
        };
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("world_texture_bc"),
            size: wgpu::Extent3d { width: c.width, height: c.height, depth_or_array_layers: 1 },
            mip_level_count: c.levels.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut bytes = 0u64;
        for (i, data) in c.levels.iter().enumerate() {
            let (lw, lh) = ((c.width >> i).max(4), (c.height >> i).max(4));
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: i as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                data,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(lw / 4 * c.kind.block_bytes()), rows_per_image: Some(lh / 4) },
                wgpu::Extent3d { width: lw, height: lh, depth_or_array_layers: 1 },
            );
            bytes += data.len() as u64;
        }
        self.register_texture(key, tex.create_view(&Default::default()), bytes, c.soft);
        true
    }

    fn register_texture(&self, key: &str, view: wgpu::TextureView, bytes: u64, soft: bool) {
        if soft {
            self.soft.lock().unwrap().insert(key.to_string());
        }
        let bg = Arc::new(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("world_tex"),
            layout: &self.tex_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) }],
        }));
        self.views.lock().unwrap().insert(key.to_string(), Arc::new(view));
        self.tex_bytes.fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
        self.textures.lock().unwrap().insert(key.to_string(), (bg, bytes));
    }

    fn bump_group_for(&self, bump: Option<&str>, spec: Option<&str>) -> Arc<wgpu::BindGroup> {
        let (b, s) = (bump.filter(|k| self.has_texture(k)), spec.filter(|k| self.has_texture(k)));
        if b.is_none() && s.is_none() {
            return self.flat_group.clone();
        }
        let key = (b.unwrap_or_default().to_string(), s.unwrap_or_default().to_string());
        if let Some(g) = self.bump_groups.lock().unwrap().get(&key) {
            return g.clone();
        }
        let views = self.views.lock().unwrap();
        let nv = b.and_then(|k| views.get(k).cloned()).unwrap_or_else(|| self.flat_normal.clone());
        let sv = s.and_then(|k| views.get(k).cloned()).unwrap_or_else(|| self.no_spec.clone());
        let g = Arc::new(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("world_bump"),
            layout: &self.bump_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&nv) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&sv) }],
        }));
        self.bump_groups.lock().unwrap().insert(key, g.clone());
        g
    }

    /// Build a part; `texture` is the key given to [`Factory::add_texture`] (None or unknown: plain light grey).
    pub fn part(&self, positions: &[[f32; 3]], normals: &[[f32; 3]], uvs: &[[f32; 2]], indices: &[u32], colors: &[[u8; 4]], tangents: &[[f32; 4]], texture: Option<&str>, maps: (Option<&str>, Option<&str>), blend: u8) -> Option<GpuPart> {
        if positions.is_empty() || indices.len() < 3 {
            return None;
        }
        let verts: Vec<Vertex> = (0..positions.len()).map(|i| Vertex { pos: positions[i], normal: normals.get(i).copied().unwrap_or([0.0, 0.0, 1.0]), uv: uvs.get(i).copied().unwrap_or([0.0, 0.0]), col: colors.get(i).copied().unwrap_or([255; 4]), tan: tangents.get(i).copied().unwrap_or([0.0; 4]) }).collect();
        let n = indices.len() / 3 * 3;
        let bind_group = texture.and_then(|k| self.textures.lock().unwrap().get(k).map(|t| t.0.clone())).unwrap_or_else(|| self.white.clone());
        Some(GpuPart {
            vbuf: self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("world_v"), contents: bytemuck::cast_slice(&verts), usage: wgpu::BufferUsages::VERTEX }),
            ibuf: self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("world_i"), contents: bytemuck::cast_slice(&indices[..n]), usage: wgpu::BufferUsages::INDEX }),
            index_count: n as u32,
            bind_group,
            bump_group: self.bump_group_for(maps.0, maps.1),
            blend,
            wire: None,
        })
    }

    /// A terrain part: `keys` are the texture keys of layers 0 to 3 and the blend mask; `uvs1` are the mask's texture coordinates (carried in the
    /// tangent). None when a texture is missing from the GPU.
    pub fn terrain_part(&self, positions: &[[f32; 3]], normals: &[[f32; 3]], uvs: &[[f32; 2]], uvs1: &[[f32; 2]], indices: &[u32], keys: [&str; 5]) -> Option<GpuPart> {
        let tangents: Vec<[f32; 4]> = uvs1.iter().map(|t| [t[0], t[1], 0.0, 0.0]).collect();
        let mut part = self.part(positions, normals, uvs, indices, &[], &tangents, Some(keys[0]), (None, None), 3)?;
        let views = self.views.lock().unwrap();
        let v = |k: &str| views.get(k).cloned();
        let (l1, l2, l3, m) = (v(keys[1])?, v(keys[2])?, v(keys[3])?, v(keys[4])?);
        part.bump_group = Arc::new(self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("world_terrain"), layout: &self.terrain_layout, entries: &[wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&l1) }, wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&l2) }, wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&l3) }, wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&m) }] }));
        Some(part)
    }

    /// A part made of line segments (two points each, world space), all of one colour: drawn only by the line pass.
    pub fn line_part(&self, segments: &[[f32; 3]], colour: [u8; 4], key: &str) -> Option<GpuPart> {
        if segments.len() < 2 {
            return None;
        }
        if !self.has_texture(key) {
            self.add_texture(key, 1, 1, &colour);
        }
        // the constructor wants a triangle; the lines use their own index list
        let idx: Vec<u32> = (0..segments.len() as u32).collect();
        let normals = vec![[0.0, 0.0, 1.0]; segments.len()];
        let mut part = self.part(segments, &normals, &[], &[0, 1, 1], &[], &[], Some(key), (None, None), 0)?;
        part.wire = Some((self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("world_lines"), contents: bytemuck::cast_slice(&idx), usage: wgpu::BufferUsages::INDEX }), idx.len() as u32));
        Some(part)
    }

    /// Give a part the edges of its triangles as a line list, for the wireframe view.
    pub fn add_wire(&self, part: &mut GpuPart, indices: &[u32]) {
        let lines: Vec<u32> = indices.chunks_exact(3).flat_map(|t| [t[0], t[1], t[1], t[2], t[2], t[0]]).collect();
        part.wire = Some((self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("world_wire"), contents: bytemuck::cast_slice(&lines), usage: wgpu::BufferUsages::INDEX }), lines.len() as u32));
    }
}

struct Target {
    size: (u32, u32),
    msaa_view: wgpu::TextureView,
    resolve_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    egui_id: egui::TextureId,
}

pub struct WorldGpu {
    factory: Factory,
    renderer: Arc<RwLock<egui_wgpu::Renderer>>,
    sky_pipeline: wgpu::RenderPipeline,
    mesh_pipeline: wgpu::RenderPipeline,
    terrain_pipeline: wgpu::RenderPipeline,
    blend_pipeline: wgpu::RenderPipeline,
    /// the mesh pipeline with both sides drawn (cut-outs, trees, cloth)
    mesh_pipeline_2s: wgpu::RenderPipeline,
    /// proxies (reflection and shadow models) as a pushed-back fill
    proxy_pipeline: wgpu::RenderPipeline,
    water_pipeline: wgpu::RenderPipeline,
    /// collision drawn solid, and drawn see-through on top of the textured models
    coll_solid: wgpu::RenderPipeline,
    coll_overlay: wgpu::RenderPipeline,
    coll_wire: wgpu::RenderPipeline,
    globals_buf: wgpu::Buffer,
    globals_bg: wgpu::BindGroup,
    /// the sun's depth map, the group that lets the main pass read it, and the pipeline that draws into it
    shadow_views: Vec<wgpu::TextureView>,
    /// globals for each cascade's pass (the same but for the cascade the pass draws)
    cascade_bufs: Vec<wgpu::Buffer>,
    cascade_bgs: Vec<wgpu::BindGroup>,
    shadow_group: wgpu::BindGroup,
    shadow_pipeline: wgpu::RenderPipeline,
    instances: wgpu::Buffer,
    instance_capacity: usize,
    target: Option<Target>,
}

impl WorldGpu {
    pub fn new(rs: &egui_wgpu::RenderState) -> Self {
        let device = rs.device.clone();
        let queue = rs.queue.clone();
        if std::env::var_os("CRCK_WORLD_DEBUG").is_some() {
            device.on_uncaptured_error(std::sync::Arc::new(|e| eprintln!("WGPU ERROR: {e}")));
        }
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("world"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("world_globals"),
            entries: &[wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None }],
        });
        let tex_layout = Arc::new(device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("world_tex"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            ],
        }));
        let sampler = Arc::new(device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        }));
        let white_view = crate::gpu::texture_view(&device, &queue, 1, 1, &[200, 204, 210, 255]);
        let white = Arc::new(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("world_white"),
            layout: &tex_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&white_view) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) }],
        }));
        let bump_entry = |binding: u32| wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None };
        let bump_layout = Arc::new(device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("world_bump"), entries: &[bump_entry(0), bump_entry(1)] }));
        let terrain_layout = Arc::new(device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("world_terrain"), entries: &[bump_entry(2), bump_entry(3), bump_entry(4), bump_entry(5)] }));
        let flat_normal = Arc::new(crate::gpu::texture_view(&device, &queue, 1, 1, &[128, 128, 255, 255]));
        let no_spec = Arc::new(crate::gpu::texture_view(&device, &queue, 1, 1, &[0, 0, 0, 255]));
        let flat_group = Arc::new(device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("world_flat"), layout: &bump_layout, entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&flat_normal) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&no_spec) }] }));
        let shadow_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("world_shadow"),
            entries: &[
                wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Depth, view_dimension: wgpu::TextureViewDimension::D2Array, multisampled: false }, count: None },
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison), count: None },
            ],
        });
        let shadow_tex = device.create_texture(&wgpu::TextureDescriptor { label: Some("world_shadow"), size: wgpu::Extent3d { width: SHADOW_SIZE, height: SHADOW_SIZE, depth_or_array_layers: 3 }, mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: DEPTH_FORMAT, usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING, view_formats: &[] });
        let shadow_view = shadow_tex.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2Array), ..Default::default() });
        let shadow_views: Vec<wgpu::TextureView> = (0..3).map(|i| shadow_tex.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2), base_array_layer: i, array_layer_count: Some(1), ..Default::default() })).collect();
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor { address_mode_u: wgpu::AddressMode::ClampToEdge, address_mode_v: wgpu::AddressMode::ClampToEdge, mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, compare: Some(wgpu::CompareFunction::LessEqual), ..Default::default() });
        let shadow_group = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("world_shadow"), layout: &shadow_layout, entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&shadow_view) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&shadow_sampler) }] });
        let mesh_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("world_mesh"), bind_group_layouts: &[Some(&globals_layout), Some(&tex_layout), Some(&bump_layout), Some(&shadow_layout)], immediate_size: 0 });
        let sky_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("world_sky"), bind_group_layouts: &[Some(&globals_layout)], immediate_size: 0 });
        let multisample = wgpu::MultisampleState { count: SAMPLES, mask: !0, alpha_to_coverage_enabled: false };
        let target = [Some(wgpu::ColorTargetState { format: COLOR_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })];
        let depth = |write: bool| Some(wgpu::DepthStencilState { format: DEPTH_FORMAT, depth_write_enabled: Some(write), depth_compare: Some(wgpu::CompareFunction::LessEqual), stencil: Default::default(), bias: Default::default() });
        let sky_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("world_sky"),
            layout: Some(&sky_layout),
            vertex: wgpu::VertexState { module: &module, entry_point: Some("vs_sky"), compilation_options: Default::default(), buffers: &[] },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: depth(false),
            multisample,
            fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some("fs_sky"), compilation_options: Default::default(), targets: &target }),
            multiview_mask: None,
            cache: None,
        });
        // opaque surfaces show only their front side (counter-clockwise is the front); the cut-out and tree shaders both sides
        let front = if std::env::var_os("CRCK_WORLD_FRONTCW").is_some() { wgpu::FrontFace::Cw } else { wgpu::FrontFace::Ccw };
        let mesh_pipeline_2s = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("world_mesh_2s"),
            layout: Some(&mesh_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[
                    Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<Vertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &VERTEX_ATTRS }),
                    Some(wgpu::VertexBufferLayout { array_stride: 64, step_mode: wgpu::VertexStepMode::Instance, attributes: &INSTANCE_ATTRS }),
                ],
            },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: Some(wgpu::Face::Back), front_face: front, ..Default::default() },
            depth_stencil: depth(true),
            multisample,
            fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some("fs_main"), compilation_options: Default::default(), targets: &target }),
            multiview_mask: None,
            cache: None,
        });
        let mesh_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("world_mesh"),
            layout: Some(&mesh_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[
                    Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<Vertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &VERTEX_ATTRS }),
                    Some(wgpu::VertexBufferLayout { array_stride: 64, step_mode: wgpu::VertexStepMode::Instance, attributes: &INSTANCE_ATTRS }),
                ],
            },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: Some(wgpu::Face::Back), front_face: front, ..Default::default() },
            depth_stencil: depth(true),
            multisample,
            fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some("fs_main"), compilation_options: Default::default(), targets: &target }),
            multiview_mask: None,
            cache: None,
        });
        let terrain_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("world_terrain"), bind_group_layouts: &[Some(&globals_layout), Some(&tex_layout), Some(&terrain_layout), Some(&shadow_layout)], immediate_size: 0 });
        let terrain_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("world_terrain"),
            layout: Some(&terrain_pl),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[
                    Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<Vertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &VERTEX_ATTRS }),
                    Some(wgpu::VertexBufferLayout { array_stride: 64, step_mode: wgpu::VertexStepMode::Instance, attributes: &INSTANCE_ATTRS }),
                ],
            },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
            depth_stencil: depth(true),
            multisample,
            fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some("fs_terrain"), compilation_options: Default::default(), targets: &target }),
            multiview_mask: None,
            cache: None,
        });
        let coll_pipeline = |label: &str, entry: &str, blend: Option<wgpu::BlendState>, write_depth: bool, bias: i32, topology: wgpu::PrimitiveTopology| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&mesh_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[
                        Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<Vertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &VERTEX_ATTRS }),
                        Some(wgpu::VertexBufferLayout { array_stride: 64, step_mode: wgpu::VertexStepMode::Instance, attributes: &INSTANCE_ATTRS }),
                    ],
                },
                primitive: wgpu::PrimitiveState { topology, cull_mode: None, ..Default::default() },
                depth_stencil: Some(wgpu::DepthStencilState { format: DEPTH_FORMAT, depth_write_enabled: Some(write_depth), depth_compare: Some(wgpu::CompareFunction::LessEqual), stencil: Default::default(), bias: wgpu::DepthBiasState { constant: bias, slope_scale: bias as f32 / 2.0, clamp: 0.0 } }),
                multisample,
                fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some(entry), compilation_options: Default::default(), targets: &[Some(wgpu::ColorTargetState { format: COLOR_FORMAT, blend, write_mask: wgpu::ColorWrites::ALL })] }),
                multiview_mask: None,
                cache: None,
            })
        };
        let coll_solid = coll_pipeline("world_coll_solid", "fs_coll", None, true, 0, wgpu::PrimitiveTopology::TriangleList);
        // pulled a little towards the camera so it shows on top of the surface it coincides with
        let coll_overlay = coll_pipeline("world_coll_overlay", "fs_coll", Some(wgpu::BlendState::ALPHA_BLENDING), false, -8, wgpu::PrimitiveTopology::TriangleList);
        let coll_wire = coll_pipeline("world_coll_wire", "fs_coll", None, false, 0, wgpu::PrimitiveTopology::LineList);
        // decals lie on a surface: pulled towards the camera, and nothing is written to the depth buffer so what is behind them still draws
        let water_pipeline = coll_pipeline("world_water", "fs_water", Some(wgpu::BlendState::ALPHA_BLENDING), false, -4, wgpu::PrimitiveTopology::TriangleList);
        let blend_pipeline = coll_pipeline("world_blend", "fs_blend", Some(wgpu::BlendState::ALPHA_BLENDING), false, std::env::var("CRCK_DECAL_BIAS").ok().and_then(|v| v.parse().ok()).unwrap_or(-64), wgpu::PrimitiveTopology::TriangleList);
        // proxies: the mesh shader, pushed far back in depth so that anything real in front of them wins, even when it is coplanar
        let proxy_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("world_proxy"),
            layout: Some(&mesh_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[
                    Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<Vertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &VERTEX_ATTRS }),
                    Some(wgpu::VertexBufferLayout { array_stride: 64, step_mode: wgpu::VertexStepMode::Instance, attributes: &INSTANCE_ATTRS }),
                ],
            },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { format: DEPTH_FORMAT, depth_write_enabled: Some(true), depth_compare: Some(wgpu::CompareFunction::LessEqual), stencil: Default::default(), bias: wgpu::DepthBiasState { constant: 40_000, slope_scale: 0.0, clamp: 0.0 } }),
            multisample,
            fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some("fs_main"), compilation_options: Default::default(), targets: &target }),
            multiview_mask: None,
            cache: None,
        });
        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("world_globals"), size: std::mem::size_of::<Globals>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let cascade_bufs: Vec<wgpu::Buffer> = (0..3).map(|_| device.create_buffer(&wgpu::BufferDescriptor { label: Some("world_cascade"), size: std::mem::size_of::<Globals>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false })).collect();
        let cascade_bgs: Vec<wgpu::BindGroup> = cascade_bufs.iter().map(|b| device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("world_cascade"), layout: &globals_layout, entries: &[wgpu::BindGroupEntry { binding: 0, resource: b.as_entire_binding() }] })).collect();
        let globals_bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("world_globals"), layout: &globals_layout, entries: &[wgpu::BindGroupEntry { binding: 0, resource: globals_buf.as_entire_binding() }] });
        let instance_capacity = 4096;
        let instances = device.create_buffer(&wgpu::BufferDescriptor { label: Some("world_instances"), size: (instance_capacity * 64) as u64, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let factory = Factory { device: device.clone(), queue, tex_layout, sampler, white, textures: Arc::new(Mutex::new(HashMap::new())), reduced: Default::default(), views: Arc::new(Mutex::new(HashMap::new())), tex_bytes: Arc::new(std::sync::atomic::AtomicU64::new(0)), soft: Default::default(), bump_layout, flat_normal, no_spec, bump_groups: Arc::new(Mutex::new(HashMap::new())), flat_group, terrain_layout: terrain_layout.clone() };
        // the depth-only pipeline of the sun's view: the same vertices and instances, no colour, a bias against self-shadowing
        let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("world_shadow"),
            layout: Some(&sky_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_shadow"),
                compilation_options: Default::default(),
                buffers: &[
                    Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<Vertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &VERTEX_ATTRS }),
                    Some(wgpu::VertexBufferLayout { array_stride: 64, step_mode: wgpu::VertexStepMode::Instance, attributes: &INSTANCE_ATTRS }),
                ],
            },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState { format: DEPTH_FORMAT, depth_write_enabled: Some(true), depth_compare: Some(wgpu::CompareFunction::LessEqual), stencil: Default::default(), bias: wgpu::DepthBiasState { constant: 2, slope_scale: 2.0, clamp: 0.0 } }),
            multisample: wgpu::MultisampleState::default(),
            fragment: None,
            multiview_mask: None,
            cache: None,
        });
        Self { factory, renderer: rs.renderer.clone(), sky_pipeline, mesh_pipeline, terrain_pipeline, blend_pipeline, mesh_pipeline_2s, proxy_pipeline, water_pipeline, coll_solid, coll_overlay, coll_wire, globals_buf, globals_bg, shadow_views, cascade_bufs, cascade_bgs, shadow_group, shadow_pipeline, instances, instance_capacity, target: None }
    }

    pub fn factory(&self) -> Factory {
        self.factory.clone()
    }

    fn ensure_target(&mut self, w: u32, h: u32) {
        if self.target.as_ref().map(|t| t.size) == Some((w, h)) {
            return;
        }
        let device = &self.factory.device;
        let mk = |format, samples, usage| {
            let t = device.create_texture(&wgpu::TextureDescriptor { label: Some("world_view"), size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: samples, dimension: wgpu::TextureDimension::D2, format, usage, view_formats: &[] });
            t.create_view(&Default::default())
        };
        let msaa_view = mk(COLOR_FORMAT, SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let resolve_view = mk(COLOR_FORMAT, 1, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING);
        let depth_view = mk(DEPTH_FORMAT, SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let old = self.target.take();
        let egui_id = match old {
            Some(old) => {
                self.renderer.write().update_egui_texture_from_wgpu_texture(device, &resolve_view, wgpu::FilterMode::Linear, old.egui_id);
                old.egui_id
            }
            None => self.renderer.write().register_native_texture(device, &resolve_view, wgpu::FilterMode::Linear),
        };
        self.target = Some(Target { size: (w, h), msaa_view, resolve_view, depth_view, egui_id });
    }

    /// Draw the sky and every group of instances: `groups` are (model, first instance, instance count) into `matrices`.
    /// Returns the egui texture holding the picture.
    pub fn render(&mut self, w: u32, h: u32, view_proj: Mat4, eye: Vec3, matrices: &[[f32; 16]], groups: &[(Arc<GpuModel>, u32, u32)], coll_groups: &[(Arc<GpuModel>, u32, u32)], coll_alpha: f32, coll_wire: bool, line_groups: &[(Arc<GpuModel>, u32, u32)], debug: u32, weather: [f32; 3], linear: bool, time: f32, hour: f32, shadows: bool, tc: Option<&rage_toolkit_core::timecycle::Sky>, cw: bool, fog: bool) -> egui::TextureId {
        // the sun crosses the sky from east (6:00) over the south at noon to west (18:00); at night the light is the faint moon's
        let a = (hour - 6.0) / 12.0 * std::f32::consts::PI;
        let cw = cw && tc.is_some();
        let sun_dir = if cw { hdr_light_dir(hour) } else { Vec3::new(a.cos() * 0.75, -0.45, a.sin()).normalize() };
        let elev = if cw { sun_dir.z } else { a.sin() };
        let daylight = ((elev + 0.25) / 0.55).clamp(0.0, 1.0);
        let warmth = (1.0 - (elev.abs() / 0.35)).clamp(0.0, 1.0) * (daylight.max(0.25));
        let (w, h) = (w.max(8), h.max(8));
        self.ensure_target(w, h);
        // the sun's view: a box round the camera, looking along the light, its position snapped so the shadows do not swim as the camera moves
        let sd = Vec3::new(sun_dir.x, sun_dir.y, sun_dir.z.max(0.15)).normalize();
        // one view of the sun per cascade: a box round the camera, looking along the light, its position snapped so the shadows do not swim
        let mut light_vps = [[[0.0f32; 4]; 4]; 3];
        for (k, r) in CASCADE_RADII.iter().enumerate() {
            let snap = 2.0 * r / SHADOW_SIZE as f32 * 8.0;
            let centre = Vec3::new((eye.x / snap).round() * snap, (eye.y / snap).round() * snap, eye.z);
            let light_view = Mat4::look_at_rh(centre + sd * 700.0, centre, if sd.z.abs() > 0.95 { Vec3::Y } else { Vec3::Z });
            light_vps[k] = (Mat4::orthographic_rh(-r, *r, -r, *r, 1.0, 1500.0) * light_view).to_cols_array_2d();
        }
        let v4 = |c: [f32; 3], w: f32| [c[0], c[1], c[2], w];
        let t = tc.copied().unwrap_or_default();
        let mut g = Globals { view_proj: view_proj.to_cols_array_2d(), inv_view_proj: view_proj.inverse().to_cols_array_2d(), eye: [eye.x, eye.y, eye.z, if cw && !fog { 0.0 } else if tc.is_some() { 0.00007 * t.fog_density } else { 0.00035 * weather[1] }], sun: [sun_dir.x, sun_dir.y, sun_dir.z.max(0.15), coll_alpha], tod: [daylight, warmth, if shadows && daylight > 0.05 { 1.0 } else { 0.0 }, debug as f32], light_vp: light_vps, wx: [weather[0], weather[1], weather[2], if linear { 1.0 } else { 0.0 }], casc: [CASCADE_RADII[0], CASCADE_RADII[1], CASCADE_RADII[2], 0.0], tm: [time, 0.0, 0.0, 0.0], sun_col: v4(t.sun, if tc.is_some() { 1.0 } else { 0.0 }), amb_up: v4(t.amb_up, 0.0), amb_dn: v4(t.amb_down, 0.0), fog_col: v4(t.fog, 0.0), zen: v4(t.zenith, t.zenith_mid_pos), zen_mid: v4(t.zenith_mid, 0.0), east: v4(t.east, 0.0), west: v4(t.west, 0.0), az_mid: v4(t.azimuth_mid, t.azimuth_mid_pos), disc: v4(t.sun_disc, 0.0), cl_lit: v4(t.cloud_lit, 0.0), cl_base: v4(t.cloud_base, 0.0), amb_dir: v4(t.amb_dir_hdr, 0.0), exp: [hdr_exposure(&t), t.sky_hdr, if cw { 1.0 } else { 0.0 }, spec_scale()], zcon: [t.zenith_blend_start, t.zenith_east_blend, t.zenith_west_blend, t.zenith_mid_pos] };
        if cw {
            g.sun_col = v4(t.sun_hdr, 1.0);
            g.amb_up = v4(t.nat_up_hdr, 0.0);
            g.amb_dn = v4(t.nat_down_hdr, 0.0);
        }
        self.factory.queue.write_buffer(&self.globals_buf, 0, bytemuck::bytes_of(&g));
        for (k, buf) in self.cascade_bufs.iter().enumerate() {
            g.casc[3] = k as f32;
            self.factory.queue.write_buffer(buf, 0, bytemuck::bytes_of(&g));
        }
        if matrices.len() > self.instance_capacity {
            self.instance_capacity = matrices.len().next_power_of_two();
            self.instances = self.factory.device.create_buffer(&wgpu::BufferDescriptor { label: Some("world_instances"), size: (self.instance_capacity * 64) as u64, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        }
        if !matrices.is_empty() {
            self.factory.queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(matrices));
        }
        let t = self.target.as_ref().unwrap();
        let mut enc = self.factory.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("world") });
        if shadows && daylight > 0.05 {
          for k in 0..CASCADE_RADII.len() {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("world_shadow_pass"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &self.shadow_views[k], depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }), stencil_ops: None }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.shadow_pipeline);
            pass.set_bind_group(0, &self.cascade_bgs[k], &[]);
            pass.set_vertex_buffer(1, self.instances.slice(..));
            for (model, first, count) in groups {
                for p in model.parts.iter().filter(|p| p.blend == 0 || p.blend == 6) {
                    pass.set_vertex_buffer(0, p.vbuf.slice(..));
                    pass.set_index_buffer(p.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..p.index_count, 0, *first..*first + *count);
                }
            }
          }
        }
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("world_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &t.msaa_view, resolve_target: Some(&t.resolve_view), depth_slice: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.7, g: 0.78, b: 0.88, a: 1.0 }), store: wgpu::StoreOp::Discard } })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &t.depth_view, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }), stencil_ops: None }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.sky_pipeline);
            pass.set_bind_group(0, &self.globals_bg, &[]);
            pass.draw(0..3, 0..1);
            pass.set_pipeline(&self.mesh_pipeline);
            pass.set_bind_group(0, &self.globals_bg, &[]);
            pass.set_bind_group(3, &self.shadow_group, &[]);
            pass.set_vertex_buffer(1, self.instances.slice(..));
            for (model, first, count) in groups {
                for p in model.parts.iter().filter(|p| p.blend == 0) {
                    pass.set_bind_group(1, &*p.bind_group, &[]);
                    pass.set_bind_group(2, &*p.bump_group, &[]);
                    pass.set_vertex_buffer(0, p.vbuf.slice(..));
                    pass.set_index_buffer(p.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..p.index_count, 0, *first..*first + *count);
                }
            }
            // cut-outs, trees and cloth: both sides
            pass.set_pipeline(&self.mesh_pipeline_2s);
            pass.set_bind_group(0, &self.globals_bg, &[]);
            pass.set_bind_group(3, &self.shadow_group, &[]);
            pass.set_vertex_buffer(1, self.instances.slice(..));
            for (model, first, count) in groups {
                for p in model.parts.iter().filter(|p| p.blend == 6) {
                    pass.set_bind_group(1, &*p.bind_group, &[]);
                    pass.set_bind_group(2, &*p.bump_group, &[]);
                    pass.set_vertex_buffer(0, p.vbuf.slice(..));
                    pass.set_index_buffer(p.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..p.index_count, 0, *first..*first + *count);
                }
            }
            // terrain parts: four layers blended by their mask
            pass.set_pipeline(&self.terrain_pipeline);
            pass.set_bind_group(0, &self.globals_bg, &[]);
            pass.set_bind_group(3, &self.shadow_group, &[]);
            pass.set_vertex_buffer(1, self.instances.slice(..));
            for (model, first, count) in groups {
                for p in model.parts.iter().filter(|p| p.blend == 3) {
                    pass.set_bind_group(1, &*p.bind_group, &[]);
                    pass.set_bind_group(2, &*p.bump_group, &[]);
                    pass.set_vertex_buffer(0, p.vbuf.slice(..));
                    pass.set_index_buffer(p.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..p.index_count, 0, *first..*first + *count);
                }
            }
            // proxies: a pushed-back fill
            pass.set_pipeline(&self.proxy_pipeline);
            pass.set_bind_group(0, &self.globals_bg, &[]);
            pass.set_bind_group(3, &self.shadow_group, &[]);
            pass.set_vertex_buffer(1, self.instances.slice(..));
            for (model, first, count) in groups {
                for p in model.parts.iter().filter(|p| p.blend == 5) {
                    pass.set_bind_group(1, &*p.bind_group, &[]);
                    pass.set_bind_group(2, &*p.bump_group, &[]);
                    pass.set_vertex_buffer(0, p.vbuf.slice(..));
                    pass.set_index_buffer(p.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..p.index_count, 0, *first..*first + *count);
                }
            }
            // blended parts after everything opaque
            pass.set_pipeline(&self.blend_pipeline);
            pass.set_bind_group(0, &self.globals_bg, &[]);
            pass.set_vertex_buffer(1, self.instances.slice(..));
            for (model, first, count) in groups {
                for p in model.parts.iter().filter(|p| p.blend != 0 && p.blend != 3 && p.blend != 4 && p.blend != 5 && p.blend != 6) {
                    pass.set_bind_group(1, &*p.bind_group, &[]);
                    pass.set_bind_group(2, &*p.bump_group, &[]);
                    pass.set_vertex_buffer(0, p.vbuf.slice(..));
                    pass.set_index_buffer(p.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..p.index_count, 0, *first..*first + *count);
                }
            }
            // water: waves, mirroring the sky, glints
            pass.set_pipeline(&self.water_pipeline);
            pass.set_bind_group(0, &self.globals_bg, &[]);
            pass.set_vertex_buffer(1, self.instances.slice(..));
            for (model, first, count) in groups {
                for p in model.parts.iter().filter(|p| p.blend == 4) {
                    pass.set_bind_group(1, &*p.bind_group, &[]);
                    pass.set_bind_group(2, &*p.bump_group, &[]);
                    pass.set_vertex_buffer(0, p.vbuf.slice(..));
                    pass.set_index_buffer(p.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..p.index_count, 0, *first..*first + *count);
                }
            }
            if !coll_groups.is_empty() {
                pass.set_pipeline(if coll_wire { &self.coll_wire } else if coll_alpha >= 1.0 { &self.coll_solid } else { &self.coll_overlay });
                pass.set_bind_group(0, &self.globals_bg, &[]);
                pass.set_vertex_buffer(1, self.instances.slice(..));
                for (model, first, count) in coll_groups {
                    for p in &model.parts {
                        pass.set_bind_group(1, &*p.bind_group, &[]);
                    pass.set_bind_group(2, &*p.bump_group, &[]);
                        pass.set_vertex_buffer(0, p.vbuf.slice(..));
                        match (&p.wire, coll_wire) {
                            (Some((w, n)), true) => {
                                pass.set_index_buffer(w.slice(..), wgpu::IndexFormat::Uint32);
                                pass.draw_indexed(0..*n, 0, *first..*first + *count);
                            }
                            (None, true) => {}
                            _ => {
                                pass.set_index_buffer(p.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                                pass.draw_indexed(0..p.index_count, 0, *first..*first + *count);
                            }
                        }
                    }
                }
            }
            if !line_groups.is_empty() {
                pass.set_pipeline(&self.coll_wire);
                pass.set_bind_group(0, &self.globals_bg, &[]);
                pass.set_vertex_buffer(1, self.instances.slice(..));
                for (model, first, count) in line_groups {
                    for p in &model.parts {
                        if let Some((w, n)) = &p.wire {
                            pass.set_bind_group(1, &*p.bind_group, &[]);
                            pass.set_bind_group(2, &*p.bump_group, &[]);
                            pass.set_vertex_buffer(0, p.vbuf.slice(..));
                            pass.set_index_buffer(w.slice(..), wgpu::IndexFormat::Uint32);
                            pass.draw_indexed(0..*n, 0, *first..*first + *count);
                        }
                    }
                }
            }
        }
        self.factory.queue.submit(Some(enc.finish()));
        t.egui_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shader is only compiled when the World tab first opens: a mistake in it would show then, on a user's machine. This checks it here.
    #[test]
    fn the_world_shader_parses_and_validates() {
        let module = naga::front::wgsl::parse_str(SHADER).unwrap_or_else(|e| panic!("{}", e.emit_to_string(SHADER)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty()).validate(&module).unwrap_or_else(|e| panic!("{e:?}"));
        for entry in ["vs_main", "fs_main", "fs_water", "fs_blend", "fs_coll", "vs_sky", "fs_sky", "vs_shadow"] {
            assert!(module.entry_points.iter().any(|e| e.name == entry), "{entry}");
        }
    }
}

#[cfg(test)]
mod light_tests {
    use super::*;
    use rage_toolkit_core::timecycle::Sky;

    #[test]
    fn the_light_follows_the_sun_arc() {
        // 6:00 the sun is in the east on the horizon; at 13:00 it is high in the south; it never goes below the horizon
        let dawn = hdr_light_dir(6.0);
        assert!(dawn.x > 0.95 && dawn.z.abs() < 0.05, "{dawn:?}");
        let noon = hdr_light_dir(13.0);
        assert!(noon.y < -0.4 && noon.z > 0.8, "{noon:?}");
        for h in [0.0, 3.0, 5.0, 7.0, 12.0, 18.0, 21.0, 23.5] {
            let l = hdr_light_dir(h);
            assert!(l.z >= 0.0 && (l.length() - 1.0).abs() < 1e-4, "{h}: {l:?}");
        }
    }

    #[test]
    fn exposure_is_nearly_fixed_in_daylight_and_rises_as_the_light_fails() {
        let day = Sky { sun_hdr: [57.0, 50.0, 45.0], amb_dir_hdr: [2.0, 1.7, 1.2], nat_up_hdr: [3.9, 3.5, 2.2], nat_down_hdr: [1.9, 2.6, 3.8], sky_hdr: 38.0, zenith: [0.04, 0.2, 0.38], east: [0.33, 0.52, 0.94], west: [0.33, 0.52, 0.94], azimuth_mid: [0.3, 0.5, 0.7], ..Default::default() };
        // about 0.072 (the mean brightness the program caps at 10), a little more where the estimate lands just under the cap
        assert!((0.070..0.080).contains(&hdr_exposure(&day)), "{}", hdr_exposure(&day));
        let night = Sky { sun_hdr: [0.0; 3], amb_dir_hdr: [0.004; 3], nat_up_hdr: [0.01; 3], nat_down_hdr: [0.01; 3], sky_hdr: 0.2, zenith: [0.02, 0.03, 0.06], east: [0.08, 0.15, 0.26], west: [0.08, 0.15, 0.26], azimuth_mid: [0.08, 0.15, 0.26], ..Default::default() };
        // the program limits the mean to 0.2, so the exposure to 0.72 / 0.2 = 3.6
        assert!((hdr_exposure(&night) - 3.6).abs() < 0.05, "{}", hdr_exposure(&night));
    }
}

