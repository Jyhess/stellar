//! Rendu GPU des corps, intégré à egui par un « paint callback » wgpu.
//!
//! Le GPU ne sert qu'à l'affichage : à chaque nouvel instantané, les
//! positions (relatives à un point d'ancrage proche de la caméra, pour
//! préserver la précision en `f32`) sont téléversées dans un tampon
//! d'instances, puis dessinées en billboards avec mélange additif.

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};
use eframe::egui_wgpu::wgpu::util::DeviceExt;

/// Une instance = un corps à l'écran (20 octets).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Instance {
    /// Position relative à l'ancre (xyz) et rayon (w).
    pub pos_radius: [f32; 4],
    /// Couleur RGBA 8 bits.
    pub color: [u8; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Uniforms {
    pub view: [[f32; 4]; 4],
    pub proj: [[f32; 4]; 4],
    /// x : hauteur du viewport (px), y : tan(fov/2), z : taille minimale (px).
    pub screen: [f32; 4],
    /// x : luminosité, y : plancher d'intensité.
    pub look: [f32; 4],
}

/// Lot d'instances à téléverser, identifié par une génération.
#[derive(Clone)]
pub struct InstanceBatch {
    pub generation: u64,
    pub instances: Arc<Vec<Instance>>,
}

/// Ressources GPU persistantes, stockées dans les `CallbackResources` d'egui.
pub struct PointRenderer {
    pipeline: wgpu::RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// Tampons d'instances : un même tampon ne peut dépasser la limite
    /// `max_buffer_size` du GPU (256 Mo par défaut, soit ~13 millions de
    /// corps), d'où un découpage en tronçons.
    chunks: Vec<Chunk>,
    /// Nombre maximal d'instances par tronçon.
    chunk_capacity: usize,
    generation: u64,
}

struct Chunk {
    buffer: wgpu::Buffer,
    capacity: usize,
    count: u32,
}

/// Plafond d'instances par tronçon (80 Mo), bien en deçà des limites usuelles.
const MAX_CHUNK_INSTANCES: usize = 4 * 1024 * 1024;

impl PointRenderer {
    /// Installe le moteur de rendu dans l'état wgpu d'eframe.
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
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("stellar-points-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
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
                }],
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
                    blend: Some(wgpu::BlendState { color: additive, alpha: additive }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        let stride = std::mem::size_of::<Instance>() as u64;
        let by_limit = (device.limits().max_buffer_size / stride) as usize;
        Self {
            pipeline,
            uniform_buffer,
            bind_group,
            chunks: Vec::new(),
            chunk_capacity: by_limit.clamp(1024, MAX_CHUNK_INSTANCES),
            generation: u64::MAX,
        }
    }

    /// Téléverse les instances, tronçon par tronçon.
    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, instances: &[Instance]) {
        let parts: Vec<&[Instance]> = instances.chunks(self.chunk_capacity).collect();
        for (i, part) in parts.iter().enumerate() {
            let needed = part.len();
            let fits = self.chunks.get(i).is_some_and(|c| c.capacity >= needed);
            if !fits {
                // Marge de 25 % pour limiter les réallocations, sans dépasser le plafond.
                let capacity = (needed + needed / 4).clamp(1024, self.chunk_capacity);
                let chunk = Chunk { buffer: Self::create_instance_buffer(device, capacity), capacity, count: 0 };
                if i < self.chunks.len() {
                    self.chunks[i] = chunk;
                } else {
                    self.chunks.push(chunk);
                }
            }
            queue.write_buffer(&self.chunks[i].buffer, 0, bytemuck::cast_slice(part));
            self.chunks[i].count = needed as u32;
        }
        // Tronçons devenus inutiles : libérés.
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

/// Commande de dessin d'une image.
pub struct PointsCallback {
    pub uniforms: Uniforms,
    pub batch: Option<InstanceBatch>,
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
        if let Some(batch) = &self.batch {
            if batch.generation != r.generation {
                r.upload(device, queue, &batch.instances);
                r.generation = batch.generation;
            }
        }
        queue.write_buffer(&r.uniform_buffer, 0, bytemuck::bytes_of(&self.uniforms));
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

/// Couleur d'un corps selon sa masse (échelle logarithmique) :
/// bleu pâle → blanc → jaune → orangé.
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
    [(c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8, 255]
}
