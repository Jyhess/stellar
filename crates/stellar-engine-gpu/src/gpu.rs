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

/// Carte graphique ouverte par l'interface, prêtée au moteur.
#[derive(Clone)]
pub struct SharedDevice {
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

fn check_adapter(adapter: &wgpu::Adapter) -> Result<(), String> {
    if adapter
        .get_downlevel_capabilities()
        .flags
        .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS | wgpu::DownlevelFlags::INDIRECT_EXECUTION)
    {
        Ok(())
    } else {
        Err("la carte graphique ne gère pas les shaders de calcul".to_owned())
    }
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
        check_adapter(&adapter)?;
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

    /// Utilise une carte graphique déjà ouverte (celle de l'interface), pour
    /// que ses tampons puissent être affichés sans copie.
    pub fn shared(shared: &SharedDevice) -> Result<Self, String> {
        check_adapter(&shared.adapter)?;
        let info = shared.adapter.get_info();
        Ok(Self {
            device: shared.device.clone(),
            queue: shared.queue.clone(),
            adapter_name: format!("{} ({:?})", info.name, info.backend),
            limits: shared.device.limits(),
        })
    }

    /// La carte partagée convient-elle au calcul ? (shaders de calcul, et
    /// pas d'OpenGL, dont les limites sont trop étroites.)
    pub fn supports(shared: &SharedDevice) -> bool {
        shared.adapter.get_info().backend != wgpu::Backend::Gl
            && check_adapter(&shared.adapter).is_ok()
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

    /// Tampon de stockage également lisible comme tampon de sommets
    /// (affichage direct des corps).
    pub fn storage_vertex(&self, label: &str, size: u64) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: size.max(16),
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::VERTEX
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

    pub fn submit(&self, enc: wgpu::CommandEncoder) -> wgpu::SubmissionIndex {
        self.queue.submit(Some(enc.finish()))
    }

    /// Relit `size` octets de `buffer` (attend la fin des travaux en cours).
    pub fn read(&self, buffer: &wgpu::Buffer, offset: u64, size: u64) -> Vec<u8> {
        self.read_with(buffer, offset, size, <[u8]>::to_vec)
    }

    /// Relit `size` octets de `buffer` et les passe à `f` directement depuis
    /// la mémoire projetée (sans copie intermédiaire).
    ///
    /// N'attend que les travaux soumis jusqu'ici : quand la carte est
    /// partagée avec l'interface, les images en cours ne sont pas attendues.
    pub fn read_with<R>(
        &self,
        buffer: &wgpu::Buffer,
        offset: u64,
        size: u64,
        f: impl FnOnce(&[u8]) -> R,
    ) -> R {
        if size == 0 {
            return f(&[]);
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
        let index = self.submit(enc);
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = self
            .device
            .poll(wgpu::Maintain::WaitForSubmissionIndex(index));
        rx.recv()
            .expect("relecture GPU interrompue")
            .expect("relecture GPU impossible");
        let r = f(&slice.get_mapped_range()[..size as usize]);
        staging.unmap();
        r
    }

    pub fn read_u32(&self, buffer: &wgpu::Buffer) -> u32 {
        let b = self.read(buffer, 0, 4);
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    }
}
