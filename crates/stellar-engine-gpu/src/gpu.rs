//! wgpu context and utilities: kernel creation, dispatches, readbacks.

use std::cell::RefCell;
use std::collections::HashMap;

use stellar_domain::EngineError;
use wgpu::util::DeviceExt;

/// Workgroup size of every kernel.
pub const WG: u32 = 256;

/// Number of workgroups needed for `n` elements.
pub fn groups(n: usize) -> u32 {
    (n as u32).div_ceil(WG).max(1)
}

/// Grid of workgroups of `size` threads covering `n` elements, spread over
/// two dimensions beyond 65,535 workgroups.
pub fn grid(n: usize, size: u32) -> (u32, u32) {
    let g = (n as u32).div_ceil(size).max(1);
    let gx = g.min(65_535);
    (gx, g.div_ceil(gx))
}

const COMMON: &str = include_str!("shaders/common.wgsl");

/// A compute kernel: pipeline with automatic bind group layout.
pub struct Kernel {
    pub name: &'static str,
    pub pipeline: wgpu::ComputePipeline,
}

/// Recording of a submission. Successive dispatches share one compute pass
/// (when profiling, each kernel gets its own timestamped pass); copies and
/// clears close it.
pub struct Encoder {
    enc: wgpu::CommandEncoder,
    pass: Option<wgpu::ComputePass<'static>>,
}

impl Encoder {
    /// Underlying encoder (the current pass is closed).
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

/// Graphics device opened by the interface, lent to the engine.
#[derive(Clone)]
pub struct SharedDevice {
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

fn check_adapter(adapter: &wgpu::Adapter) -> Result<(), EngineError> {
    if adapter
        .get_downlevel_capabilities()
        .flags
        .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS | wgpu::DownlevelFlags::INDIRECT_EXECUTION)
    {
        Ok(())
    } else {
        Err(EngineError::ComputeUnsupported)
    }
}

/// Profiling by pass timestamps (`STELLAR_GPU_PROFILE=1`, engine on its own
/// device only): cumulative time per kernel.
struct Profiler {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    /// Kernels of the timestamped passes since the last submission.
    pending: Vec<&'static str>,
    totals: HashMap<&'static str, (f64, u64)>,
    period_ns: f64,
}

const PROFILE_PASSES: u32 = 2048;

impl Profiler {
    /// Timestamps of the start and end of the next pass (unless the capacity is
    /// exhausted).
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
    /// Bind groups already created, by kernel and bound buffers: the engine's
    /// buffers are fixed, so each dispatch reuses its own.
    bind_groups: RefCell<HashMap<(&'static str, Vec<wgpu::Buffer>), wgpu::BindGroup>>,
    profiler: Option<RefCell<Profiler>>,
}

impl Gpu {
    /// Opens the most powerful graphics device available.
    pub fn open() -> Result<Self, EngineError> {
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
        .ok_or(EngineError::NoDevice)?;
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
        .map_err(|e| EngineError::DeviceOpen(e.to_string()))?;
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

    /// Uses a device that is already open (the interface's), so that its buffers
    /// can be displayed without copy.
    pub fn shared(shared: &SharedDevice) -> Result<Self, EngineError> {
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

    /// Is the shared device suitable for computation? (Compute shaders, and not
    /// OpenGL, whose limits are too narrow.)
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

    /// Storage buffer that can also be read as a vertex buffer (live display of
    /// the bodies).
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

    /// Readback buffer (mappable for reading).
    pub fn readback(&self, label: &str, size: u64) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: size.max(16),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Waits for submission `index`, then passes to `f` the first `size` bytes
    /// of the readback buffer `buffer` (mapped for the duration of the call).
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
            .expect("GPU readback interrupted")
            .expect("GPU readback failed");
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

    /// Forgets the bind groups that reference `buffer` (a buffer replaced by a
    /// larger one): otherwise they would keep it alive.
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

    /// Records a dispatch of `k` on `groups` workgroups.
    pub fn run(
        &self,
        enc: &mut Encoder,
        k: &Kernel,
        buffers: &[&wgpu::Buffer],
        groups: u32,
    ) {
        self.run_2d(enc, k, buffers, (groups, 1));
    }

    /// Dispatch on a two-dimensional grid of workgroups (beyond 65,535
    /// workgroups).
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

    /// Indirect dispatch (workgroup count read from `args` at `offset`).
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

    /// Compute pass in which to record the next dispatch of `k`: the current
    /// pass or, when profiling, a pass of its own with timestamps.
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

    /// Cumulative times per kernel (ms, dispatch count), from most to least
    /// expensive; empty without profiling.
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

    /// Reads `size` bytes of `buffer` back (waits for the pending work).
    pub fn read(&self, buffer: &wgpu::Buffer, offset: u64, size: u64) -> Vec<u8> {
        self.read_with(buffer, offset, size, <[u8]>::to_vec)
    }

    /// Reads `size` bytes of `buffer` back and passes them to `f` straight from
    /// the mapped memory (no intermediate copy).
    ///
    /// Waits only for the work submitted so far: when the device is shared with
    /// the interface, frames in flight are not waited for.
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
            .expect("GPU readback interrupted")
            .expect("GPU readback failed");
        let r = f(&slice.get_mapped_range()[..size as usize]);
        staging.unmap();
        r
    }
}
