//! Shared wgpu device, used by both the capture overlays and the settings
//! window (through egui-wgpu).

use std::cell::RefCell;
use std::collections::HashMap;

use egui_wgpu::wgpu;

pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub bind_group_layout: wgpu::BindGroupLayout,
    /// Stand-in for the annotation layer until a region is selected.
    pub empty_layer: wgpu::TextureView,
    pipeline_layout: wgpu::PipelineLayout,
    shader: wgpu::ShaderModule,
    /// Overlay pipelines, one per surface format in use.
    pipelines: RefCell<HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>>,
}

impl Gpu {
    pub fn new() -> Result<Self, String> {
        // One backend at a time: with all of them enabled, wgpu loads every
        // vendor's Vulkan, DX12 and OpenGL drivers just to pick one (100+ MB).
        // DX12 first on Windows, presenting through DirectComposition: a
        // fullscreen Vulkan window looks like a game, so NVIDIA's overlay
        // announces itself on every capture and hooks in. Metal on macOS and
        // Vulkan on Linux otherwise. `WGPU_BACKEND`
        // and `WGPU_DX12_PRESENTATION_SYSTEM` override it.
        let preferred = if cfg!(windows) {
            [wgpu::Backends::DX12, wgpu::Backends::VULKAN]
        } else {
            [wgpu::Backends::PRIMARY, wgpu::Backends::GL]
        };
        let (instance, adapter) = preferred
            .into_iter()
            .find_map(|backends| {
                let defaults = wgpu::InstanceDescriptor::new_without_display_handle();
                let instance = wgpu::Instance::new(
                    wgpu::InstanceDescriptor {
                        backends,
                        backend_options: wgpu::BackendOptions {
                            dx12: wgpu::Dx12BackendOptions {
                                presentation_system: wgpu::Dx12SwapchainKind::DxgiFromVisual,
                                ..defaults.backend_options.dx12.clone()
                            },
                            ..defaults.backend_options.clone()
                        },
                        ..defaults
                    }
                    .with_env(),
                );
                let adapter = pollster::block_on(
                    instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
                )
                .ok()?;
                Some((instance, adapter))
            })
            .ok_or("no usable graphics adapter")?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("snapr"),
            // Allow textures as large as the hardware does, for very wide monitors.
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .map_err(|e| format!("failed to open graphics device: {e}"))?;

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("overlay"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("overlay"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            ..Default::default()
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("overlay"),
            source: wgpu::ShaderSource::Wgsl(include_str!("overlay.wgsl").into()),
        });

        let empty_layer = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("empty layer"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default());

        Ok(Self {
            empty_layer,
            instance,
            adapter,
            device,
            queue,
            bind_group_layout,
            pipeline_layout,
            shader,
            pipelines: RefCell::default(),
        })
    }

    pub fn overlay_pipeline(&self, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
        self.pipelines
            .borrow_mut()
            .entry(format)
            .or_insert_with(|| {
                self.device
                    .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                        label: Some("overlay"),
                        layout: Some(&self.pipeline_layout),
                        vertex: wgpu::VertexState {
                            module: &self.shader,
                            entry_point: Some("vs"),
                            compilation_options: Default::default(),
                            buffers: &[],
                        },
                        fragment: Some(wgpu::FragmentState {
                            module: &self.shader,
                            entry_point: Some("fs"),
                            compilation_options: Default::default(),
                            targets: &[Some(format.into())],
                        }),
                        primitive: wgpu::PrimitiveState::default(),
                        depth_stencil: None,
                        multisample: wgpu::MultisampleState::default(),
                        multiview_mask: None,
                        cache: None,
                    })
            })
            .clone()
    }

    /// Lets egui render the settings window on this same device.
    pub fn egui_setup(&self) -> egui_wgpu::WgpuSetup {
        egui_wgpu::WgpuSetup::Existing(egui_wgpu::WgpuSetupExisting {
            instance: self.instance.clone(),
            adapter: self.adapter.clone(),
            device: self.device.clone(),
            queue: self.queue.clone(),
        })
    }
}
