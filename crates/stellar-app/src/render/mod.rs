//! GPU rendering of the bodies, integrated into egui through a wgpu "paint
//! callback".
//!
//! Two sources of bodies, drawn as billboards with additive blending:
//! - **direct**: the buffers of the GPU engine, which computes on the same
//!   device ([`BodiesView`]); position and mass are read as they are, colour
//!   and radius are derived in the shader. No copy. A simulation split into
//!   subsystems ([`SystemsView`]) has one buffer per subsystem, each in its
//!   own frame: the anchor of each draw is the camera's, translated to the
//!   subsystem's origin.
//! - **instances** (CPU engine, playback): at each new snapshot, positions
//!   (relative to an anchor near the camera, to preserve `f32` precision)
//!   are uploaded to an instance buffer.

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use eframe::egui;
use eframe::egui_wgpu::wgpu::util::DeviceExt;
use eframe::egui_wgpu::{self, wgpu};
use glam::DVec3;
use stellar_domain::systems::ViewPart;
use stellar_domain::SystemsView;
use stellar_engine_gpu::BodiesView;

/// One instance = one body on screen (20 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Instance {
    /// Position relative to the anchor (xyz) and radius (w).
    pub pos_radius: [f32; 4],
    /// 8-bit RGBA colour.
    pub color: [u8; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Uniforms {
    pub view: [[f32; 4]; 4],
    pub proj: [[f32; 4]; 4],
    /// x: viewport height (px), y: tan(fov/2), z: minimum size (px).
    pub screen: [f32; 4],
    /// x: brightness, y: intensity floor.
    pub look: [f32; 4],
    /// Direct source: anchor (xyz, subtracted from the positions) and base
    /// radius r₀ (w).
    pub anchor: [f32; 4],
    /// Direct source: x = ln(maximum mass) (0 if ≤ 1), for the colour.
    pub mass: [f32; 4],
}

/// Batch of instances to upload, identified by a generation.
#[derive(Clone)]
pub struct InstanceBatch {
    pub generation: u64,
    pub instances: Arc<Vec<Instance>>,
}

/// Persistent GPU resources, stored in egui's `CallbackResources`.
pub struct PointRenderer {
    pipeline: wgpu::RenderPipeline,
    /// Direct drawing from the GPU engine's body buffers.
    bodies_pipeline: wgpu::RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    bind_group_layout: wgpu::BindGroupLayout,
    /// Live view: one set of uniforms per subsystem, and the subsystems of the
    /// current frame.
    part_uniforms: Vec<(wgpu::Buffer, wgpu::BindGroup)>,
    parts: Arc<Vec<ViewPart>>,
    /// Instance buffers: a single buffer cannot exceed the GPU's
    /// `max_buffer_size` limit (256 MB by default, ~13 million bodies), hence
    /// the split into chunks.
    chunks: Vec<Chunk>,
    /// Maximum number of instances per chunk.
    chunk_capacity: usize,
    generation: u64,
}

struct Chunk {
    buffer: wgpu::Buffer,
    capacity: usize,
    count: u32,
}

/// Cap of instances per chunk (80 MB), well below the usual limits.
const MAX_CHUNK_INSTANCES: usize = 4 * 1024 * 1024;

impl PointRenderer {
    /// Installs the renderer in eframe's wgpu state.
    pub fn install(render_state: &egui_wgpu::RenderState) {
        let renderer = Self::new(&render_state.device, render_state.target_format);
        render_state
            .renderer
            .write()
            .callback_resources
            .insert(renderer);
    }

    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("stellar-points"),
            source: wgpu::ShaderSource::Wgsl(include_str!("points.wgsl").into()),
        });
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("stellar-uniforms"),
            contents: bytemuck::bytes_of(&Uniforms::zeroed()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("stellar-uniforms-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("stellar-uniforms-group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("stellar-points-layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let additive = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        };
        let make_pipeline = |label: &str, entry: &str, buffer: wgpu::VertexBufferLayout| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    buffers: &[buffer],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState {
                            color: additive,
                            alpha: additive,
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview: None,
                cache: None,
            })
        };
        let pipeline = make_pipeline(
            "stellar-points-pipeline",
            "vs_main",
            wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Instance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &[
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x4,
                        offset: 0,
                        shader_location: 0,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Unorm8x4,
                        offset: 16,
                        shader_location: 1,
                    },
                ],
            },
        );
        // Bodies of the GPU engine, read in place: position (3 × f32) and mass.
        let bodies_pipeline = make_pipeline(
            "stellar-bodies-pipeline",
            "vs_bodies",
            wgpu::VertexBufferLayout {
                array_stride: BodiesView::STRIDE,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &[
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x3,
                        offset: BodiesView::POSITION_OFFSET,
                        shader_location: 0,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32,
                        offset: BodiesView::MASS_OFFSET,
                        shader_location: 1,
                    },
                ],
            },
        );
        let stride = std::mem::size_of::<Instance>() as u64;
        let by_limit = (device.limits().max_buffer_size / stride) as usize;
        Self {
            pipeline,
            bodies_pipeline,
            uniform_buffer,
            bind_group,
            bind_group_layout,
            part_uniforms: Vec::new(),
            parts: Arc::default(),
            chunks: Vec::new(),
            chunk_capacity: by_limit.clamp(1024, MAX_CHUNK_INSTANCES),
            generation: u64::MAX,
        }
    }

    /// Uploads the instances, chunk by chunk.
    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, instances: &[Instance]) {
        let parts: Vec<&[Instance]> = instances.chunks(self.chunk_capacity).collect();
        for (i, part) in parts.iter().enumerate() {
            let needed = part.len();
            let fits = self.chunks.get(i).is_some_and(|c| c.capacity >= needed);
            if !fits {
                // 25 % margin to limit reallocations, without exceeding the cap.
                let capacity = (needed + needed / 4).clamp(1024, self.chunk_capacity);
                let chunk = Chunk {
                    buffer: Self::create_instance_buffer(device, capacity),
                    capacity,
                    count: 0,
                };
                if i < self.chunks.len() {
                    self.chunks[i] = chunk;
                } else {
                    self.chunks.push(chunk);
                }
            }
            queue.write_buffer(&self.chunks[i].buffer, 0, bytemuck::cast_slice(part));
            self.chunks[i].count = needed as u32;
        }
        // Chunks no longer needed: released.
        self.chunks.truncate(parts.len());
    }

    fn create_instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("stellar-instances"),
            size: (capacity * std::mem::size_of::<Instance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }
}

/// Bodies to draw.
pub enum PointsSource {
    /// Instances uploaded from a snapshot.
    Batch(Option<InstanceBatch>),
    /// Buffers of the GPU engine, read in place; anchor: position of the camera.
    Direct { view: Arc<SystemsView>, anchor: DVec3 },
}

/// Draw command of a frame.
pub struct PointsCallback {
    pub uniforms: Uniforms,
    pub source: PointsSource,
}

impl egui_wgpu::CallbackTrait for PointsCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(r) = resources.get_mut::<PointRenderer>() else {
            return Vec::new();
        };
        if let PointsSource::Batch(Some(batch)) = &self.source {
            if batch.generation != r.generation {
                r.upload(device, queue, &batch.instances);
                r.generation = batch.generation;
            }
        }
        queue.write_buffer(&r.uniform_buffer, 0, bytemuck::bytes_of(&self.uniforms));
        if let PointsSource::Direct { view, anchor } = &self.source {
            r.parts = view.parts();
            while r.part_uniforms.len() < r.parts.len() {
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("stellar-uniforms-part"),
                    size: std::mem::size_of::<Uniforms>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("stellar-uniforms-part-group"),
                    layout: &r.bind_group_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buffer.as_entire_binding(),
                    }],
                });
                r.part_uniforms.push((buffer, group));
            }
            for (part, (buffer, _)) in r.parts.iter().zip(&r.part_uniforms) {
                let a = (*anchor - part.origin).as_vec3();
                let mut u = self.uniforms;
                u.anchor = [a.x, a.y, a.z, u.anchor[3]];
                queue.write_buffer(buffer, 0, bytemuck::bytes_of(&u));
            }
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let Some(r) = resources.get::<PointRenderer>() else {
            return;
        };
        if let PointsSource::Direct { .. } = &self.source {
            pass.set_pipeline(&r.bodies_pipeline);
            for (part, (_, group)) in r.parts.iter().zip(&r.part_uniforms) {
                let Some(view) = part.view.downcast_ref::<BodiesView>() else {
                    continue;
                };
                let (buffer, count) = view.current();
                if count > 0 {
                    pass.set_bind_group(0, group, &[]);
                    pass.set_vertex_buffer(0, buffer.slice(..count as u64 * BodiesView::STRIDE));
                    pass.draw(0..4, 0..count);
                }
            }
            return;
        }
        if r.chunks.iter().all(|c| c.count == 0) {
            return;
        }
        pass.set_pipeline(&r.pipeline);
        pass.set_bind_group(0, &r.bind_group, &[]);
        let stride = std::mem::size_of::<Instance>() as u64;
        for c in r.chunks.iter().filter(|c| c.count > 0) {
            pass.set_vertex_buffer(0, c.buffer.slice(..c.count as u64 * stride));
            pass.draw(0..4, 0..c.count);
        }
    }
}

/// Colour of a body by its mass (logarithmic scale): pale blue → white →
/// yellow → orange.
pub fn mass_color(mass: f64, max_mass: f64) -> [u8; 4] {
    let t = if max_mass > 1.0 {
        (mass.max(1.0).ln() / max_mass.ln()).clamp(0.0, 1.0)
    } else {
        0.0
    } as f32;
    const STOPS: [(f32, [f32; 3]); 4] = [
        (0.0, [0.55, 0.68, 1.0]),
        (0.35, [0.95, 0.95, 1.0]),
        (0.7, [1.0, 0.85, 0.45]),
        (1.0, [1.0, 0.45, 0.15]),
    ];
    let mut c = STOPS[3].1;
    for w in STOPS.windows(2) {
        let (t0, c0) = w[0];
        let (t1, c1) = w[1];
        if t <= t1 {
            let k = ((t - t0) / (t1 - t0)).clamp(0.0, 1.0);
            c = [
                c0[0] + (c1[0] - c0[0]) * k,
                c0[1] + (c1[1] - c0[1]) * k,
                c0[2] + (c1[2] - c0[2]) * k,
            ];
            break;
        }
    }
    [
        (c[0] * 255.0) as u8,
        (c[1] * 255.0) as u8,
        (c[2] * 255.0) as u8,
        255,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shaders and both pipelines (instances, direct reading of the bodies)
    /// pass wgpu's validation.
    #[test]
    fn pipelines_are_valid() {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::from_env()
                .unwrap_or(wgpu::Backends::PRIMARY | wgpu::Backends::GL),
            ..Default::default()
        });
        let Some(adapter) =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        else {
            eprintln!("test ignoré : aucune carte graphique");
            return;
        };
        let (device, _queue) =
            pollster::block_on(adapter.request_device(&Default::default(), None))
                .expect("ouverture de la carte graphique");
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let _renderer = PointRenderer::new(&device, wgpu::TextureFormat::Bgra8Unorm);
        let error = pollster::block_on(device.pop_error_scope());
        assert!(error.is_none(), "{error:?}");
    }
}
