//! Offscreen wgpu renderer for the model viewer: draws into a texture that egui then shows as an image.
//! Own pipelines, own depth buffer, 4x MSAA. Materials: plain textured, vehicle paint (body colour), glass (alpha blended,
//! drawn after the opaque parts), emissive; optional normal maps, specular maps and a cheap sky/ground reflection.
use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};
use glam::{Mat4, Vec3};
use rage_toolkit_core::model::MeshPart;
use std::collections::HashMap;
use wgpu::util::DeviceExt;

const SAMPLES: u32 = 4;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

const SHADER: &str = r#"
struct Globals {
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    flags: vec4<f32>,   // x: textured, y: alpha cutoff, z: effects (normal/spec/reflections)
};
struct Mat {
    tint: vec4<f32>,    // paint colour
    p: vec4<f32>,       // x: kind (0 plain, 1 paint, 2 glass, 3 emissive), y: has normal map, z: has spec map
};
@group(0) @binding(0) var<uniform> g: Globals;
@group(1) @binding(0) var diff_t: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;
@group(1) @binding(2) var bump_t: texture_2d<f32>;
@group(1) @binding(3) var spec_t: texture_2d<f32>;
@group(1) @binding(4) var<uniform> m: Mat;

struct VIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) tangent: vec4<f32>,
};
struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) tangent: vec4<f32>,
};

@vertex fn vs_main(v: VIn) -> VOut {
    var o: VOut;
    o.clip = g.view_proj * vec4<f32>(v.pos, 1.0);
    o.world = v.pos;
    o.normal = v.normal;
    o.uv = v.uv;
    o.tangent = v.tangent;
    return o;
}

fn sky(r: vec3<f32>) -> vec3<f32> {
    let t = clamp(r.z * 0.5 + 0.5, 0.0, 1.0);
    let horizon = vec3<f32>(0.62, 0.68, 0.74);
    let zenith = vec3<f32>(0.30, 0.50, 0.85);
    let ground = vec3<f32>(0.16, 0.15, 0.14);
    return select(mix(ground, horizon, t * 2.0), mix(horizon, zenith, (t - 0.5) * 2.0), t > 0.5);
}

@fragment fn fs_main(i: VOut) -> @location(0) vec4<f32> {
    let tex = textureSample(diff_t, samp, i.uv);
    let base = select(vec4<f32>(0.72, 0.74, 0.78, 1.0), tex, g.flags.x > 0.5);
    let kind = m.p.x;
    let fx = g.flags.z > 0.5;
    if (kind < 1.5 && base.a < g.flags.y) { discard; }

    let v = normalize(g.eye.xyz - i.world);
    var n = normalize(i.normal);
    if (fx && m.p.y > 0.5) {
        let t = normalize(i.tangent.xyz - n * dot(i.tangent.xyz, n));
        let b = cross(n, t) * i.tangent.w;
        let nm = textureSample(bump_t, samp, i.uv).rg * 2.0 - 1.0;
        let nz = sqrt(max(0.0, 1.0 - dot(nm, nm)));
        n = normalize(t * nm.x + b * nm.y + n * nz);
    }
    if (dot(n, v) < 0.0) { n = -n; }

    var albedo = base.rgb;
    if (kind > 0.5 && kind < 1.5) { albedo = m.tint.rgb * mix(vec3<f32>(1.0), base.rgb, 0.35); }

    let key = normalize(vec3<f32>(-0.4, -0.6, 0.7));
    let ndl = max(dot(n, key), 0.0);
    let ndv = max(dot(n, v), 0.0);
    let diffuse = 0.32 + 0.45 * ndl + 0.23 * ndv;

    var spec = 0.0;
    var refl = vec3<f32>(0.0);
    if (fx) {
        var s = 0.25;
        if (m.p.z > 0.5) { s = textureSample(spec_t, samp, i.uv).r; }
        let shin = select(24.0, 80.0, kind > 0.5 && kind < 2.5);
        spec = pow(max(dot(n, normalize(key + v)), 0.0), shin) * s + pow(ndv, shin * 0.5) * s * 0.15;
        let fres = 0.04 + 0.96 * pow(1.0 - ndv, 5.0);
        let r = reflect(-v, n);
        let strength = select(0.0, select(0.55, 1.2, kind > 1.5), kind > 0.5 && kind < 2.5);
        refl = sky(r) * fres * strength;
    }

    if (kind > 2.5) { return vec4<f32>(min(albedo * 1.4, vec3<f32>(1.0)), 1.0); }
    var col = albedo * diffuse + vec3<f32>(spec) + refl;
    if (kind > 1.5 && kind < 2.5) {
        // glass: mostly see-through, darker tint, strong rim reflection
        let a = clamp(select(0.35, tex.a * 0.8, g.flags.x > 0.5), 0.22, 0.85);
        col = albedo * 0.35 * diffuse + vec3<f32>(spec) + refl;
        return vec4<f32>(col, mix(a, 0.95, 1.0 - ndv));
    }
    return vec4<f32>(col, 1.0);
}

struct LIn { @location(0) pos: vec3<f32>, @location(1) color: vec3<f32> };
struct LOut { @builtin(position) clip: vec4<f32>, @location(0) color: vec3<f32> };
@vertex fn vs_line(v: LIn) -> LOut { var o: LOut; o.clip = g.view_proj * vec4<f32>(v.pos, 1.0); o.color = v.color; return o; }
@fragment fn fs_line(i: LOut) -> @location(0) vec4<f32> { return vec4<f32>(i.color, 1.0); }
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    eye: [f32; 4],
    flags: [f32; 4],
}

const VERTEX_ATTRS: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Float32x4];

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    tangent: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LineVertex {
    pos: [f32; 3],
    color: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MatUniform {
    tint: [f32; 4],
    p: [f32; 4],
}

/// how a part is shaded
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Plain = 0,
    Paint = 1,
    Glass = 2,
    Emissive = 3,
}

pub fn classify(shader: &str) -> u32 {
    let s = shader.to_lowercase();
    if s.starts_with("vehicle_vehglass") {
        Kind::Glass as u32
    } else if s.starts_with("vehicle_paint") && !s.contains("emissive") {
        Kind::Paint as u32
    } else if s.contains("emissive") {
        Kind::Emissive as u32
    } else {
        Kind::Plain as u32
    }
}

struct GpuPart {
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    index_count: u32,
    kind: Kind,
    names: [Option<String>; 3], // diffuse, bump, spec (lowercase)
    mat_buf: wgpu::Buffer,
    bind_group: Option<wgpu::BindGroup>,
}

struct Target {
    size: (u32, u32),
    msaa_view: wgpu::TextureView,
    resolve_tex: wgpu::Texture,
    resolve_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    egui_id: egui::TextureId,
}

pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// None when running headless (screenshots): nothing to register the render target with
    renderer: Option<std::sync::Arc<egui::mutex::RwLock<egui_wgpu::Renderer>>>,
    opaque_pipeline: wgpu::RenderPipeline,
    glass_pipeline: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,
    globals_buf: wgpu::Buffer,
    globals_bg: wgpu::BindGroup,
    part_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    white: wgpu::TextureView,
    textures: HashMap<String, wgpu::TextureView>,
    parts: Vec<GpuPart>,
    grid: Option<(wgpu::Buffer, u32)>,
    target: Option<Target>,
}

/// The next mip level down: half the size (at least 1), each pixel the average of the 2x2 above it.
pub fn half(pw: u32, ph: u32, prev: &[u8]) -> (u32, u32, Vec<u8>) {
    let (nw, nh) = ((pw / 2).max(1), (ph / 2).max(1));
    let mut next = vec![0u8; (nw * nh * 4) as usize];
    for y in 0..nh {
        for x in 0..nw {
            let mut acc = [0u32; 4];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let sx = (x * 2 + dx).min(pw - 1);
                let sy = (y * 2 + dy).min(ph - 1);
                let i = ((sy * pw + sx) * 4) as usize;
                for c in 0..4 {
                    acc[c] += prev[i + c] as u32;
                }
            }
            let o = ((y * nw + x) * 4) as usize;
            for c in 0..4 {
                next[o + c] = (acc[c] / 4) as u8;
            }
        }
    }
    (nw, nh, next)
}

/// Halve `rgba` until its longest side is at most `max` (pictures in panels and cards need no full-size copy).
pub fn shrink_to(w: u32, h: u32, rgba: Vec<u8>, max: u32) -> (u32, u32, Vec<u8>) {
    let (mut w, mut h, mut rgba) = (w, h, rgba);
    while w.max(h) > max.max(1) {
        (w, h, rgba) = half(w, h, &rgba);
    }
    (w, h, rgba)
}

fn texture_view(device: &wgpu::Device, queue: &wgpu::Queue, w: u32, h: u32, rgba: &[u8]) -> wgpu::TextureView {
    let levels = 32 - w.max(h).max(1).leading_zeros();
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("model_texture"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COLOR_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let write = |i: u32, lw: u32, lh: u32, data: &[u8]| {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: i, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * lw), rows_per_image: Some(lh) },
            wgpu::Extent3d { width: lw, height: lh, depth_or_array_layers: 1 },
        );
    };
    // the top level straight from the caller's pixels, then each level from the one above (one level kept at a time)
    write(0, w, h, rgba);
    let (mut lw, mut lh, mut prev) = (w, h, None::<Vec<u8>>);
    for i in 1..levels {
        let (nw, nh, next) = half(lw, lh, prev.as_deref().unwrap_or(rgba));
        write(i, nw, nh, &next);
        (lw, lh, prev) = (nw, nh, Some(next));
    }
    tex.create_view(&Default::default())
}

fn tex_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
        count: None,
    }
}

impl Gpu {
    pub fn new(rs: &egui_wgpu::RenderState) -> Self {
        Self::from_device(rs.device.clone(), rs.queue.clone(), Some(rs.renderer.clone()))
    }

    /// Own instance/adapter/device, no window: used by `--screenshot`.
    pub fn headless() -> anyhow::Result<Self> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;
        Ok(Self::from_device(device, queue, None))
    }

    fn from_device(device: wgpu::Device, queue: wgpu::Queue, renderer: Option<std::sync::Arc<egui::mutex::RwLock<egui_wgpu::Renderer>>>) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("viewer"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            }],
        });
        let part_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("part"),
            entries: &[
                tex_entry(0),
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
                tex_entry(2),
                tex_entry(3),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("viewer_layout"), bind_group_layouts: &[Some(&globals_layout), Some(&part_layout)], immediate_size: 0 });
        let line_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("line_layout"), bind_group_layouts: &[Some(&globals_layout)], immediate_size: 0 });
        let depth = |write: bool| {
            Some(wgpu::DepthStencilState { format: DEPTH_FORMAT, depth_write_enabled: Some(write), depth_compare: Some(wgpu::CompareFunction::Less), stencil: Default::default(), bias: Default::default() })
        };
        let multisample = wgpu::MultisampleState { count: SAMPLES, mask: !0, alpha_to_coverage_enabled: false };
        let opaque_target = [Some(wgpu::ColorTargetState { format: COLOR_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })];
        let glass_target = [Some(wgpu::ColorTargetState { format: COLOR_FORMAT, blend: Some(wgpu::BlendState::ALPHA_BLENDING), write_mask: wgpu::ColorWrites::ALL })];
        let vbuf_layout = || wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &VERTEX_ATTRS,
        };
        let mesh_pipeline = |label: &str, target: &[Option<wgpu::ColorTargetState>], write_depth: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState { module: &module, entry_point: Some("vs_main"), compilation_options: Default::default(), buffers: &[Some(vbuf_layout())] },
                primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
                depth_stencil: depth(write_depth),
                multisample,
                fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some("fs_main"), compilation_options: Default::default(), targets: target }),
                multiview_mask: None,
                cache: None,
            })
        };
        let opaque_pipeline = mesh_pipeline("opaque", &opaque_target, true);
        let glass_pipeline = mesh_pipeline("glass", &glass_target, false);
        let line_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lines"),
            layout: Some(&line_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_line"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<LineVertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3] })],
            },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::LineList, ..Default::default() },
            depth_stencil: depth(true),
            multisample,
            fragment: Some(wgpu::FragmentState { module: &module, entry_point: Some("fs_line"), compilation_options: Default::default(), targets: &opaque_target }),
            multiview_mask: None,
            cache: None,
        });
        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("globals"), size: std::mem::size_of::<Globals>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let globals_bg = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("globals"), layout: &globals_layout, entries: &[wgpu::BindGroupEntry { binding: 0, resource: globals_buf.as_entire_binding() }] });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 4,
            ..Default::default()
        });
        let white = texture_view(&device, &queue, 1, 1, &[255, 255, 255, 255]);
        Self { device, queue, renderer, opaque_pipeline, glass_pipeline, line_pipeline, globals_buf, globals_bg, part_layout, sampler, white, textures: HashMap::new(), parts: vec![], grid: None, target: None }
    }

    pub fn set_textures(&mut self, textures: &[(String, u32, u32, Vec<u8>)]) {
        self.textures.clear();
        for (name, w, h, rgba) in textures {
            self.textures.insert(name.to_lowercase(), texture_view(&self.device, &self.queue, *w, *h, rgba));
        }
        self.rebuild_bind_groups();
    }

    /// Replaces the scene. Positions are used as they come (GTA is Z-up, the camera is Z-up too).
    pub fn set_meshes(&mut self, parts: &[MeshPart]) {
        self.parts = parts
            .iter()
            .map(|p| {
                let verts: Vec<Vertex> = (0..p.positions.len())
                    .map(|i| Vertex { pos: p.positions[i], normal: p.normals[i], uv: p.uvs[i], tangent: p.tangents.get(i).copied().unwrap_or([1.0, 0.0, 0.0, 1.0]) })
                    .collect();
                GpuPart {
                    vbuf: self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("vertices"), contents: bytemuck::cast_slice(&verts), usage: wgpu::BufferUsages::VERTEX }),
                    ibuf: self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("indices"), contents: bytemuck::cast_slice(&p.indices), usage: wgpu::BufferUsages::INDEX }),
                    index_count: p.indices.len() as u32 / 3 * 3,
                    kind: match classify(&p.shader) {
                        1 => Kind::Paint,
                        2 => Kind::Glass,
                        3 => Kind::Emissive,
                        _ => Kind::Plain,
                    },
                    names: [p.diffuse.as_ref().map(|s| s.to_lowercase()), p.bump.as_ref().map(|s| s.to_lowercase()), p.spec.as_ref().map(|s| s.to_lowercase())],
                    mat_buf: self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("material"), size: std::mem::size_of::<MatUniform>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false }),
                    bind_group: None,
                }
            })
            .collect();
        self.rebuild_bind_groups();
    }

    fn rebuild_bind_groups(&mut self) {
        for p in &mut self.parts {
            let get = |n: &Option<String>| n.as_ref().and_then(|n| self.textures.get(n));
            let (d, b, s) = (get(&p.names[0]), get(&p.names[1]), get(&p.names[2]));
            let u = MatUniform { tint: [1.0; 4], p: [p.kind as u32 as f32, b.is_some() as u32 as f32, s.is_some() as u32 as f32, 0.0] };
            self.queue.write_buffer(&p.mat_buf, 0, bytemuck::bytes_of(&u));
            p.bind_group = Some(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("part"),
                layout: &self.part_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(d.unwrap_or(&self.white)) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(b.unwrap_or(&self.white)) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(s.unwrap_or(&self.white)) },
                    wgpu::BindGroupEntry { binding: 4, resource: p.mat_buf.as_entire_binding() },
                ],
            }));
        }
    }

    /// Ground grid + axes sized to the model (`size` = longest model dimension).
    pub fn set_grid(&mut self, size: f32, floor_z: f32) {
        let step = 10f32.powf((size.max(0.01) / 8.0).log10().floor()).max(0.001);
        let n = ((size * 1.5 / step).ceil() as i32).clamp(4, 60);
        let mut v: Vec<LineVertex> = vec![];
        let grey = [0.32, 0.34, 0.38];
        for i in -n..=n {
            let t = i as f32 * step;
            let ext = n as f32 * step;
            v.push(LineVertex { pos: [t, -ext, floor_z], color: grey });
            v.push(LineVertex { pos: [t, ext, floor_z], color: grey });
            v.push(LineVertex { pos: [-ext, t, floor_z], color: grey });
            v.push(LineVertex { pos: [ext, t, floor_z], color: grey });
        }
        let a = step * 2.0;
        v.extend([
            LineVertex { pos: [0.0, 0.0, floor_z], color: [0.9, 0.25, 0.25] },
            LineVertex { pos: [a, 0.0, floor_z], color: [0.9, 0.25, 0.25] },
            LineVertex { pos: [0.0, 0.0, floor_z], color: [0.3, 0.85, 0.3] },
            LineVertex { pos: [0.0, a, floor_z], color: [0.3, 0.85, 0.3] },
            LineVertex { pos: [0.0, 0.0, floor_z], color: [0.35, 0.5, 1.0] },
            LineVertex { pos: [0.0, 0.0, floor_z + a], color: [0.35, 0.5, 1.0] },
        ]);
        let buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("grid"), contents: bytemuck::cast_slice(&v), usage: wgpu::BufferUsages::VERTEX });
        self.grid = Some((buf, v.len() as u32));
    }

    fn ensure_target(&mut self, w: u32, h: u32) {
        if self.target.as_ref().map(|t| t.size) == Some((w, h)) {
            return;
        }
        let mk = |format, samples, usage| {
            let t = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("viewport"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            });
            let v = t.create_view(&Default::default());
            (t, v)
        };
        let (_, msaa_view) = mk(COLOR_FORMAT, SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let (resolve_tex, resolve_view) = mk(COLOR_FORMAT, 1, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC);
        let (_, depth_view) = mk(DEPTH_FORMAT, SAMPLES, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let old = self.target.take();
        let egui_id = match (&self.renderer, old) {
            (Some(r), Some(old)) => {
                r.write().update_egui_texture_from_wgpu_texture(&self.device, &resolve_view, wgpu::FilterMode::Linear, old.egui_id);
                old.egui_id
            }
            (Some(r), None) => r.write().register_native_texture(&self.device, &resolve_view, wgpu::FilterMode::Linear),
            (None, _) => egui::TextureId::default(),
        };
        self.target = Some(Target { size: (w, h), msaa_view, resolve_tex, resolve_view, depth_view, egui_id });
    }

    /// Draws the scene into the offscreen target and returns the egui texture to show.
    /// `paint` = body colour for vehicle paint shaders; `effects` = normal/spec maps and reflections.
    #[allow(clippy::too_many_arguments)]
    pub fn render(&mut self, w: u32, h: u32, view_proj: Mat4, eye: Vec3, visible: &[bool], textured: bool, show_grid: bool, paint: [f32; 3], effects: bool) -> egui::TextureId {
        let (w, h) = (w.max(8), h.max(8));
        self.ensure_target(w, h);
        let g = Globals { view_proj: view_proj.to_cols_array_2d(), eye: [eye.x, eye.y, eye.z, 1.0], flags: [if textured { 1.0 } else { 0.0 }, 0.4, if effects { 1.0 } else { 0.0 }, 0.0] };
        self.queue.write_buffer(&self.globals_buf, 0, bytemuck::bytes_of(&g));
        for p in self.parts.iter().filter(|p| p.kind == Kind::Paint) {
            let has = |i: usize| p.names[i].as_ref().map(|n| self.textures.contains_key(n)).unwrap_or(false) as u32 as f32;
            let u = MatUniform { tint: [paint[0], paint[1], paint[2], 1.0], p: [Kind::Paint as u32 as f32, has(1), has(2), 0.0] };
            self.queue.write_buffer(&p.mat_buf, 0, bytemuck::bytes_of(&u));
        }
        let t = self.target.as_ref().unwrap();
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("viewer") });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("viewer_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &t.msaa_view,
                    resolve_target: Some(&t.resolve_view),
                    depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear({ let c = crate::theme::viewport_clear(); wgpu::Color { r: c[0], g: c[1], b: c[2], a: 1.0 } }), store: wgpu::StoreOp::Discard },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &t.depth_view, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }), stencil_ops: None }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if show_grid {
                if let Some((buf, n)) = &self.grid {
                    pass.set_pipeline(&self.line_pipeline);
                    pass.set_bind_group(0, &self.globals_bg, &[]);
                    pass.set_vertex_buffer(0, buf.slice(..));
                    pass.draw(0..*n, 0..1);
                }
            }
            // opaque first, glass last (blended, no depth writes)
            for (glass, pipeline) in [(false, &self.opaque_pipeline), (true, &self.glass_pipeline)] {
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &self.globals_bg, &[]);
                for (i, p) in self.parts.iter().enumerate() {
                    if (p.kind == Kind::Glass) != glass || !visible.get(i).copied().unwrap_or(true) {
                        continue;
                    }
                    let Some(bg) = &p.bind_group else { continue };
                    pass.set_bind_group(1, bg, &[]);
                    pass.set_vertex_buffer(0, p.vbuf.slice(..));
                    pass.set_index_buffer(p.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..p.index_count, 0, 0..1);
                }
            }
        }
        self.queue.submit(Some(enc.finish()));
        t.egui_id
    }

    /// Renders one frame and reads it back as tightly packed RGBA8 (w*h*4).
    #[allow(clippy::too_many_arguments)]
    pub fn screenshot(&mut self, w: u32, h: u32, view_proj: Mat4, eye: Vec3, visible: &[bool], textured: bool, show_grid: bool, paint: [f32; 3], effects: bool) -> anyhow::Result<Vec<u8>> {
        self.render(w, h, view_proj, eye, visible, textured, show_grid, paint, effects);
        let t = self.target.as_ref().unwrap();
        let padded = (w * 4).div_ceil(256) * 256;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("readback"), size: (padded * h) as u64, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &t.resolve_tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit(Some(enc.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        buf.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::PollType::wait_indefinitely())?;
        rx.recv()??;
        let data = buf.slice(..).get_mapped_range()?;
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h as usize {
            out.extend_from_slice(&data[row * padded as usize..row * padded as usize + (w * 4) as usize]);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::half;

    #[test]
    fn half_averages_each_2x2_block_and_stops_at_one_pixel() {
        // 2x2: one white, three black pixels -> one pixel at a quarter
        let px = [255u8, 255, 255, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255];
        assert_eq!(half(2, 2, &px), (1, 1, vec![63, 63, 63, 255]));
        // an odd edge reads its last column twice; a 1-pixel side stays 1
        let (w, h, out) = half(3, 1, &[10, 0, 0, 0, 20, 0, 0, 0, 30, 0, 0, 0]);
        assert_eq!((w, h, out), (1, 1, vec![15, 0, 0, 0]));
        let (w, h, out) = super::shrink_to(2048, 512, vec![9; 2048 * 512 * 4], 512);
        assert_eq!((w, h, out.len(), out[0]), (512, 128, 512 * 128 * 4, 9), "halved until the longest side fits");
        assert_eq!(super::shrink_to(300, 200, vec![1; 300 * 200 * 4], 512).0, 300, "small enough: left as it is");
        let (mut w, mut h, mut levels) = (1024u32, 256u32, 1);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        while w > 1 || h > 1 {
            (w, h, buf) = half(w, h, &buf);
            levels += 1;
        }
        assert_eq!((levels, buf.len()), (32 - 1024u32.leading_zeros(), 4), "the same count texture_view gives the GPU");
    }
}
