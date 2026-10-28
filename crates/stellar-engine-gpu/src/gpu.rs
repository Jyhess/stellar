//! Contexte wgpu et utilitaires : création des noyaux, lancements, relectures.

use std::cell::RefCell;
use std::collections::HashMap;

use wgpu::util::DeviceExt;

/// Taille des groupes de travail de tous les noyaux.
pub const WG: u32 = 256;

/// Nombre de groupes nécessaires pour `n` éléments.
pub fn groups(n: usize) -> u32 {
    (n as u32).div_ceil(WG).max(1)
}

/// Grille de groupes de `size` fils couvrant `n` éléments, répartie sur
/// deux dimensions au-delà de 65 535 groupes.
pub fn grid(n: usize, size: u32) -> (u32, u32) {
    let g = (n as u32).div_ceil(size).max(1);
    let gx = g.min(65_535);
    (gx, g.div_ceil(gx))
}

const COMMON: &str = include_str!("shaders/common.wgsl");

/// Un noyau de calcul : pipeline à disposition automatique des liaisons.
pub struct Kernel {
    pub name: &'static str,
    pub pipeline: wgpu::ComputePipeline,
}

/// Enregistrement d'une soumission. Les lancements successifs partagent une
/// même passe de calcul (en profilage, chaque noyau a sa passe horodatée) ;
/// copies et effacements la referment.
pub struct Encoder {
    enc: wgpu::CommandEncoder,
    pass: Option<wgpu::ComputePass<'static>>,
}

impl Encoder {
    /// Encodeur sous-jacent (la passe en cours est refermée).
    fn raw(&mut self) -> &mut wgpu::CommandEncoder {
        self.pass = None;
        &mut self.enc
    }

    pub fn clear_buffer(&mut self, buffer: &wgpu::Buffer, offset: u64, size: Option<u64>) {
        self.raw().clear_buffer(buffer, offset, size);
    }

    pub fn copy_buffer_to_buffer(
        &mut self,
        src: &wgpu::Buffer,
        src_offset: u64,
        dst: &wgpu::Buffer,
        dst_offset: u64,
        size: u64,
    ) {
        self.raw()
            .copy_buffer_to_buffer(src, src_offset, dst, dst_offset, size);
    }
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

/// Profilage par horodatage des passes (`STELLAR_GPU_PROFILE=1`, carte
/// propre au moteur uniquement) : temps cumulé par noyau.
struct Profiler {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    /// Noyaux des passes horodatées depuis la dernière soumission.
    pending: Vec<&'static str>,
    totals: HashMap<&'static str, (f64, u64)>,
    period_ns: f64,
}

const PROFILE_PASSES: u32 = 2048;

impl Profiler {
    /// Horodatage du début et de la fin de la prochaine passe (si la
    /// capacité n'est pas épuisée).
    fn slot(&mut self, name: &'static str) -> Option<wgpu::ComputePassTimestampWrites<'_>> {
        let k = self.pending.len() as u32;
        if k >= PROFILE_PASSES {
            return None;
        }
        self.pending.push(name);
        Some(wgpu::ComputePassTimestampWrites {
            query_set: &self.queries,
            beginning_of_pass_write_index: Some(2 * k),
            end_of_pass_write_index: Some(2 * k + 1),
        })
    }
}

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter_name: String,
    pub limits: wgpu::Limits,
    /// Groupes de liaisons déjà créés, par noyau et tampons liés : les
    /// tampons du moteur sont fixes, chaque lancement réutilise le sien.
    bind_groups: RefCell<HashMap<(&'static str, Vec<wgpu::Buffer>), wgpu::BindGroup>>,
    profiler: Option<RefCell<Profiler>>,
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
        let profile = std::env::var_os("STELLAR_GPU_PROFILE").is_some()
            && adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        let features = if profile {
            wgpu::Features::TIMESTAMP_QUERY
        } else {
            wgpu::Features::empty()
        };
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("stellar-gpu-engine"),
                required_features: features,
                required_limits: limits.clone(),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))
        .map_err(|e| format!("ouverture de la carte graphique impossible : {e}"))?;
        let profiler = profile.then(|| {
            RefCell::new(Profiler {
                queries: device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("stellar-profile"),
                    ty: wgpu::QueryType::Timestamp,
                    count: 2 * PROFILE_PASSES,
                }),
                resolve: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("stellar-profile-resolve"),
                    size: 16 * PROFILE_PASSES as u64,
                    usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
                pending: Vec::new(),
                totals: HashMap::new(),
                period_ns: queue.get_timestamp_period() as f64,
            })
        });
        Ok(Self {
            device,
            queue,
            adapter_name: format!("{} ({:?})", info.name, info.backend),
            limits,
            bind_groups: RefCell::default(),
            profiler,
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
            bind_groups: RefCell::default(),
            profiler: None,
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

    /// Tampon de relecture (projetable en lecture).
    pub fn readback(&self, label: &str, size: u64) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: size.max(16),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Attend la soumission `index`, puis passe à `f` les `size` premiers
    /// octets du tampon de relecture `buffer` (projeté le temps de l'appel).
    pub fn map_read(
        &self,
        buffer: &wgpu::Buffer,
        index: wgpu::SubmissionIndex,
        size: u64,
        f: impl FnOnce(&[u8]),
    ) {
        let slice = buffer.slice(..size);
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
        f(&slice.get_mapped_range());
        buffer.unmap();
    }

    pub fn uniform(&self, label: &str, contents: &[u8]) -> wgpu::Buffer {
        self.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            })
    }

    /// Oublie les groupes de liaisons qui référencent `buffer` (tampon
    /// remplacé par un plus grand) : sans quoi ils le garderaient en vie.
    pub fn forget(&self, buffer: &wgpu::Buffer) {
        self.bind_groups
            .borrow_mut()
            .retain(|(_, buffers), _| !buffers.contains(buffer));
    }

    fn bind_group(&self, k: &Kernel, buffers: &[&wgpu::Buffer]) -> wgpu::BindGroup {
        let key = (k.name, buffers.iter().map(|&b| b.clone()).collect::<Vec<_>>());
        if let Some(bg) = self.bind_groups.borrow().get(&key) {
            return bg.clone();
        }
        let bg = self.create_bind_group(k, buffers);
        self.bind_groups.borrow_mut().insert(key, bg.clone());
        bg
    }

    fn create_bind_group(&self, k: &Kernel, buffers: &[&wgpu::Buffer]) -> wgpu::BindGroup {
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
        enc: &mut Encoder,
        k: &Kernel,
        buffers: &[&wgpu::Buffer],
        groups: u32,
    ) {
        self.run_2d(enc, k, buffers, (groups, 1));
    }

    /// Lancement sur une grille de groupes à deux dimensions (au-delà de
    /// 65 535 groupes).
    pub fn run_2d(
        &self,
        enc: &mut Encoder,
        k: &Kernel,
        buffers: &[&wgpu::Buffer],
        (gx, gy): (u32, u32),
    ) {
        let bg = self.bind_group(k, buffers);
        let pass = self.pass(enc, k);
        pass.set_pipeline(&k.pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups(gx, gy, 1);
        if self.profiler.is_some() {
            enc.pass = None;
        }
    }

    /// Lancement indirect (nombre de groupes lu dans `args` à `offset`).
    pub fn run_indirect(
        &self,
        enc: &mut Encoder,
        k: &Kernel,
        buffers: &[&wgpu::Buffer],
        args: &wgpu::Buffer,
        offset: u64,
    ) {
        let bg = self.bind_group(k, buffers);
        let pass = self.pass(enc, k);
        pass.set_pipeline(&k.pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.dispatch_workgroups_indirect(args, offset);
        if self.profiler.is_some() {
            enc.pass = None;
        }
    }

    /// Passe de calcul où enregistrer le prochain lancement de `k` : la passe
    /// en cours, ou, en profilage, une passe propre et horodatée.
    fn pass<'e>(&self, enc: &'e mut Encoder, k: &Kernel) -> &'e mut wgpu::ComputePass<'static> {
        if let Some(prof) = &self.profiler {
            enc.pass = None;
            let mut p = prof.borrow_mut();
            let pass = enc
                .enc
                .begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some(k.name),
                    timestamp_writes: p.slot(k.name),
                })
                .forget_lifetime();
            return enc.pass.insert(pass);
        }
        let Encoder { enc: raw, pass } = enc;
        pass.get_or_insert_with(|| {
            raw.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("stellar"),
                timestamp_writes: None,
            })
            .forget_lifetime()
        })
    }

    pub fn encoder(&self) -> Encoder {
        Encoder {
            enc: self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("stellar-step"),
                }),
            pass: None,
        }
    }

    pub fn submit(&self, enc: Encoder) -> wgpu::SubmissionIndex {
        let Encoder { mut enc, pass } = enc;
        drop(pass);
        let Some(prof) = &self.profiler else {
            return self.queue.submit(Some(enc.finish()));
        };
        let passes = prof.borrow().pending.len() as u32;
        if passes == 0 {
            return self.queue.submit(Some(enc.finish()));
        }
        let size = 16 * passes as u64;
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("stellar-profile-read"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        {
            let p = prof.borrow();
            enc.resolve_query_set(&p.queries, 0..2 * passes, &p.resolve, 0);
            enc.copy_buffer_to_buffer(&p.resolve, 0, &staging, 0, size);
        }
        let index = self.queue.submit(Some(enc.finish()));
        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self
            .device
            .poll(wgpu::Maintain::WaitForSubmissionIndex(index.clone()));
        let stamps: Vec<u64> = bytemuck::cast_slice(&slice.get_mapped_range()).to_vec();
        staging.unmap();
        let mut p = prof.borrow_mut();
        let period = p.period_ns;
        let names = std::mem::take(&mut p.pending);
        for (k, name) in names.into_iter().enumerate() {
            let ns = stamps[2 * k + 1].saturating_sub(stamps[2 * k]) as f64 * period;
            let e = p.totals.entry(name).or_default();
            e.0 += ns * 1e-6;
            e.1 += 1;
        }
        index
    }

    /// Temps cumulés par noyau (ms, nombre de lancements), du plus coûteux
    /// au moins coûteux ; vide sans profilage.
    pub fn profile(&self) -> Vec<(&'static str, f64, u64)> {
        let Some(prof) = &self.profiler else {
            return Vec::new();
        };
        let mut v: Vec<_> = prof
            .borrow()
            .totals
            .iter()
            .map(|(&k, &(ms, n))| (k, ms, n))
            .collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1));
        v
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
}
