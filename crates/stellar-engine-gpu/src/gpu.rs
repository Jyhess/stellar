//! Contexte wgpu et utilitaires : création des noyaux, lancements, relectures.

use wgpu::util::DeviceExt;

/// Taille des groupes de travail de tous les noyaux.
pub const WG: u32 = 256;

/// Nombre de groupes nécessaires pour `n` éléments.
pub fn groups(n: usize) -> u32 {
    (n as u32).div_ceil(WG).max(1)
}

const COMMON: &str = include_str!("shaders/common.wgsl");

/// Un noyau de calcul : pipeline à disposition automatique des liaisons.
pub struct Kernel {
    pub name: &'static str,
    pub pipeline: wgpu::ComputePipeline,
}

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter_name: String,
    pub limits: wgpu::Limits,
}

impl Gpu {
    /// Ouvre la carte graphique la plus performante disponible.
    pub fn open() -> Result<Self, String> {
        let backends =
            wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY | wgpu::Backends::GL);
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends,
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .ok_or_else(|| "aucune carte graphique compatible n'a été trouvée".to_owned())?;
        if !adapter.get_downlevel_capabilities().flags.contains(
            wgpu::DownlevelFlags::COMPUTE_SHADERS | wgpu::DownlevelFlags::INDIRECT_EXECUTION,
        ) {
            return Err("la carte graphique ne gère pas les shaders de calcul".to_owned());
        }
        let info = adapter.get_info();
        let limits = adapter.limits();
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("stellar-gpu-engine"),
                required_features: wgpu::Features::empty(),
                required_limits: limits.clone(),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))
        .map_err(|e| format!("ouverture de la carte graphique impossible : {e}"))?;
        Ok(Self {
            device,
            queue,
            adapter_name: format!("{} ({:?})", info.name, info.backend),
            limits,
        })
    }

    pub fn kernel(&self, name: &'static str, source: &str) -> Kernel {
        let module = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(name),
                source: wgpu::ShaderSource::Wgsl(format!("{COMMON}\n{source}").into()),
            });
        let pipeline = self
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(name),
                layout: None,
                module: &module,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            });
        Kernel { name, pipeline }
    }

    pub fn storage(&self, label: &str, size: u64) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: size.max(16),
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    pub fn indirect(&self, label: &str, size: u64) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: size.max(16),
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::INDIRECT
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    pub fn uniform(&self, label: &str, contents: &[u8]) -> wgpu::Buffer {
        self.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            })
    }

    fn bind_group(&self, k: &Kernel, buffers: &[&wgpu::Buffer]) -> wgpu::BindGroup {
        let entries: Vec<wgpu::BindGroupEntry> = buffers
            .iter()
            .enumerate()
            .map(|(i, b)| wgpu::BindGroupEntry {
                binding: i as u32,
                resource: b.as_entire_binding(),
            })
            .collect();
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(k.name),
            layout: &k.pipeline.get_bind_group_layout(0),
            entries: &entries,
        })
    }

    /// Enregistre un lancement de `k` sur `groups` groupes.
    pub fn run(
        &self,
        enc: &mut wgpu::CommandEncoder,
        k: &Kernel,
        buffers: &[&wgpu::Buffer],
        groups: u32,
    ) {
        let bg = self.bind_group(k, buffers);
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some(k.name),
            timestamp_writes: None,
        });
        pass.set_pipeline(&k.pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups(groups, 1, 1);
    }

    /// Lancement indirect (nombre de groupes lu dans `args` à `offset`).
    pub fn run_indirect(
        &self,
        enc: &mut wgpu::CommandEncoder,
        k: &Kernel,
        buffers: &[&wgpu::Buffer],
        args: &wgpu::Buffer,
        offset: u64,
    ) {
        let bg = self.bind_group(k, buffers);
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some(k.name),
            timestamp_writes: None,
        });
        pass.set_pipeline(&k.pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups_indirect(args, offset);
    }

    pub fn encoder(&self) -> wgpu::CommandEncoder {
        self.device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("stellar-step"),
            })
    }

    pub fn submit(&self, enc: wgpu::CommandEncoder) {
        self.queue.submit(Some(enc.finish()));
    }

    /// Relit `size` octets de `buffer` (attend la fin des travaux en cours).
    pub fn read(&self, buffer: &wgpu::Buffer, offset: u64, size: u64) -> Vec<u8> {
        if size == 0 {
            return Vec::new();
        }
        let size4 = size.div_ceil(4) * 4;
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("stellar-readback"),
            size: size4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = self.encoder();
        enc.copy_buffer_to_buffer(buffer, offset, &staging, 0, size4);
        self.submit(enc);
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = self.device.poll(wgpu::Maintain::Wait);
        rx.recv()
            .expect("relecture GPU interrompue")
            .expect("relecture GPU impossible");
        let data = slice.get_mapped_range()[..size as usize].to_vec();
        staging.unmap();
        data
    }

    pub fn read_u32(&self, buffer: &wgpu::Buffer) -> u32 {
        let b = self.read(buffer, 0, 4);
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    }
}
