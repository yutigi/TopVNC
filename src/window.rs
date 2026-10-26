//! The viewer's windows. winit delivers input as the system reports it,
//! and wgpu presents each frame as soon as it arrives, at up to the display's
//! refresh rate, without waiting for vertical sync.

use std::error::Error;
use std::sync::Arc;
use std::time::Duration;
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, DeviceEvents, EventLoop, EventLoopProxy};
use winit::platform::pump_events::EventLoopExtPumpEvents;
use winit::window::{Window, WindowAttributes, WindowId};

/// Window input and wake-ups, in the order they arrived.
pub enum UiEvent {
    Window(WindowId, WindowEvent),
    /// Raw device input, such as unaccelerated-by-the-window mouse motion,
    /// delivered only while the viewer is focused.
    Device(DeviceEvent),
    /// Another thread woke the loop, as when a frame has arrived.
    Wake,
}

/// The event loop every window shares. It runs inside the caller's loop:
/// [`Ui::pump`] returns as soon as events arrive.
pub struct Ui {
    event_loop: EventLoop<()>,
    collector: Collector,
    /// Every surface and the device come from this one instance.
    instance: wgpu::Instance,
    gpu: Option<Arc<Gpu>>,
}

#[derive(Default)]
struct Collector {
    events: Vec<UiEvent>,
    pending: Option<WindowAttributes>,
    created: Option<Result<Window, winit::error::OsError>>,
}

impl Collector {
    /// Windows can only be created while the event loop runs.
    fn create_pending(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(attributes) = self.pending.take() {
            self.created = Some(event_loop.create_window(attributes));
        }
    }
}

impl ApplicationHandler for Collector {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, _cause: StartCause) {
        self.create_pending(event_loop);
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.create_pending(event_loop);
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, (): ()) {
        self.events.push(UiEvent::Wake);
    }

    fn window_event(&mut self, _event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        self.events.push(UiEvent::Window(id, event));
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device: DeviceId,
        event: DeviceEvent,
    ) {
        self.events.push(UiEvent::Device(event));
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.create_pending(event_loop);
    }
}

/// Wakes [`Ui::pump`] from another thread.
#[derive(Clone)]
pub struct Waker(EventLoopProxy<()>);

impl Waker {
    pub fn wake(&self) {
        // Fails only once the event loop is gone.
        let _ = self.0.send_event(());
    }
}

impl Ui {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        let event_loop = EventLoop::with_user_event().build()?;
        event_loop.listen_device_events(DeviceEvents::WhenFocused);
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(
            Box::new(event_loop.owned_display_handle()),
        ));
        Ok(Self {
            event_loop,
            collector: Collector::default(),
            instance,
            gpu: None,
        })
    }

    pub fn waker(&self) -> Waker {
        Waker(self.event_loop.create_proxy())
    }

    /// Dispatch pending window and device events, first waiting up to
    /// `timeout` for one, and return them in arrival order.
    pub fn pump(&mut self, timeout: Duration) -> Vec<UiEvent> {
        self.event_loop
            .pump_app_events(Some(timeout), &mut self.collector);
        std::mem::take(&mut self.collector.events)
    }

    pub fn create_window(
        &mut self,
        attributes: WindowAttributes,
    ) -> Result<Arc<Window>, Box<dyn Error>> {
        self.collector.pending = Some(attributes);
        // The first pump finishes launching the application.
        for _ in 0..100 {
            self.event_loop
                .pump_app_events(Some(Duration::ZERO), &mut self.collector);
            if let Some(created) = self.collector.created.take() {
                return Ok(Arc::new(created?));
            }
        }
        self.collector.pending = None;
        Err("the window system did not create a window".into())
    }

    /// The device shared by every window, created with the first one.
    fn gpu(&mut self, surface: &wgpu::Surface<'_>) -> Result<Arc<Gpu>, Box<dyn Error>> {
        if let Some(gpu) = &self.gpu {
            return Ok(Arc::clone(gpu));
        }
        let gpu = Arc::new(Gpu::new(&self.instance, surface)?);
        self.gpu = Some(Arc::clone(&gpu));
        Ok(gpu)
    }
}

/// The GPU device and queue, which any thread may use to upload textures.
pub struct Gpu {
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl Gpu {
    fn new(instance: &wgpu::Instance, surface: &wgpu::Surface<'_>) -> Result<Self, Box<dyn Error>> {
        let adapter = [false, true]
            .into_iter()
            .find_map(|force_fallback_adapter| {
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    force_fallback_adapter,
                    compatible_surface: Some(surface),
                    ..Default::default()
                }))
                .ok()
            })
            .ok_or("no graphics adapter can present to the window")?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("TopVNC"),
                // The framebuffer may be as large as the adapter allows.
                required_limits: adapter.limits(),
                ..Default::default()
            }))?;
        Ok(Self {
            adapter,
            device,
            queue,
        })
    }
}

/// A texture drawn as a rectangle of a window. Its pixels may be uploaded
/// from any thread.
pub struct Layer {
    texture: wgpu::Texture,
    queue: wgpu::Queue,
    placement: wgpu::Buffer,
    /// Bind groups sampling with nearest and linear filtering.
    bind_groups: [wgpu::BindGroup; 2],
    width: u32,
    height: u32,
}

impl Layer {
    /// Upload the `width` by `height` area at (`x`, `y`) of `pixels`,
    /// 0x00RRGGBB rows of `stride` pixels laid out like the texture.
    pub fn upload(
        &self,
        pixels: &[u32],
        stride: usize,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
    ) {
        let fits = x + width <= self.width as usize
            && y + height <= self.height as usize
            && width > 0
            && height > 0
            && (y + height - 1) * stride + x + width <= pixels.len();
        if !fits {
            return;
        }
        // Little-endian 0x00RRGGBB pixels are B, G, R, X bytes.
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: x as u32,
                    y: y as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(pixels),
            wgpu::TexelCopyBufferLayout {
                offset: ((y * stride + x) * 4) as u64,
                bytes_per_row: Some((stride * 4) as u32),
                rows_per_image: Some(height as u32),
            },
            wgpu::Extent3d {
                width: width as u32,
                height: height as u32,
                depth_or_array_layers: 1,
            },
        );
    }
}

/// One layer to draw: where in the window, in physical pixels, and which
/// part of the layer, in its pixels, both as x, y, width, and height.
pub struct Draw<'a> {
    pub layer: &'a Layer,
    pub target: [f64; 4],
    pub source: [f64; 4],
    pub smooth: bool,
}

/// Presents layers to one window.
pub struct Presenter {
    window: Arc<Window>,
    gpu: Arc<Gpu>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    samplers: [wgpu::Sampler; 2],
    /// The format of layer textures: sRGB only when the surface is, so
    /// pixel values reach the display unchanged.
    layer_format: wgpu::TextureFormat,
}

const SHADER: &str = r"
struct Placement {
    // Normalized device coordinates: left, top, right, bottom.
    area: vec4<f32>,
    // Texture coordinates: left, top, right, bottom.
    source: vec4<f32>,
};

@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;
@group(0) @binding(2) var<uniform> placement: Placement;

struct Varyings {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> Varyings {
    // Two triangles covering the rectangle.
    let corner = vec2<f32>(
        f32(index == 1u || index == 4u || index == 5u),
        f32(index == 2u || index == 3u || index == 5u),
    );
    var out: Varyings;
    out.position = vec4<f32>(mix(placement.area.xy, placement.area.zw, corner), 0.0, 1.0);
    out.uv = mix(placement.source.xy, placement.source.zw, corner);
    return out;
}

@fragment
fn fragment(input: Varyings) -> @location(0) vec4<f32> {
    // Pixels carry no alpha.
    return vec4<f32>(textureSample(image, image_sampler, input.uv).rgb, 1.0);
}
";

impl Presenter {
    pub fn new(ui: &mut Ui, window: Arc<Window>) -> Result<Self, Box<dyn Error>> {
        let surface = ui.instance.create_surface(Arc::clone(&window))?;
        let gpu = ui.gpu(&surface)?;
        let capabilities = surface.get_capabilities(&gpu.adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|format| !format.is_srgb())
            .or_else(|| capabilities.formats.first().copied())
            .ok_or("the window surface supports no formats")?;
        let layer_format = if format.is_srgb() {
            wgpu::TextureFormat::Bgra8UnormSrgb
        } else {
            wgpu::TextureFormat::Bgra8Unorm
        };
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            // Present at once instead of at the next vertical blank: tearing
            // where the platform allows it, never a queue of finished frames.
            present_mode: wgpu::PresentMode::AutoNoVsync,
            // Two drawables on Metal; one queued frame on DX12.
            desired_maximum_frame_latency: 1,
            alpha_mode: if capabilities
                .alpha_modes
                .contains(&wgpu::CompositeAlphaMode::Opaque)
            {
                wgpu::CompositeAlphaMode::Opaque
            } else {
                capabilities.alpha_modes[0]
            },
            view_formats: Vec::new(),
        };
        surface.configure(&gpu.device, &config);
        let device = &gpu.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("TopVNC layer"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("TopVNC layer"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX,
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
            label: Some("TopVNC layer"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("TopVNC layer"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = |filter| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("TopVNC layer"),
                mag_filter: filter,
                min_filter: filter,
                ..Default::default()
            })
        };
        let samplers = [
            sampler(wgpu::FilterMode::Nearest),
            sampler(wgpu::FilterMode::Linear),
        ];
        Ok(Self {
            window,
            gpu,
            surface,
            config,
            pipeline,
            bind_group_layout,
            samplers,
            layer_format,
        })
    }

    /// A `width` by `height` layer, black until uploaded.
    pub fn layer(&self, width: u32, height: u32) -> Layer {
        let device = &self.gpu.device;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("TopVNC layer"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.layer_format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let placement = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("TopVNC layer placement"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = |sampler: &wgpu::Sampler| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("TopVNC layer"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: placement.as_entire_binding(),
                    },
                ],
            })
        };
        let bind_groups = [bind_group(&self.samplers[0]), bind_group(&self.samplers[1])];
        Layer {
            texture,
            queue: self.gpu.queue.clone(),
            placement,
            bind_groups,
            width: width.max(1),
            height: height.max(1),
        }
    }

    /// Match the surface to the window's current size.
    pub fn resize(&mut self) {
        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        if (size.width, size.height) != (self.config.width, self.config.height) {
            self.config.width = size.width;
            self.config.height = size.height;
            self.surface.configure(&self.gpu.device, &self.config);
        }
    }

    /// Clear the window to black, draw `draws` in order, and present.
    /// Returns false when the window could not take a frame, as when it is
    /// not yet on screen, minimized, or occluded; draw again later.
    pub fn draw(&mut self, draws: &[Draw<'_>]) -> Result<bool, Box<dyn Error>> {
        self.resize();
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.gpu.device, &self.config);
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err("the window surface could not provide a frame".into());
            }
        };
        let (width, height) = (f64::from(self.config.width), f64::from(self.config.height));
        for draw in draws {
            let [x, y, w, h] = draw.target;
            let [u, v, uw, vh] = draw.source;
            let (layer_width, layer_height) =
                (f64::from(draw.layer.width), f64::from(draw.layer.height));
            let placement: [f32; 8] = [
                (x / width * 2.0 - 1.0) as f32,
                (1.0 - y / height * 2.0) as f32,
                ((x + w) / width * 2.0 - 1.0) as f32,
                (1.0 - (y + h) / height * 2.0) as f32,
                (u / layer_width) as f32,
                (v / layer_height) as f32,
                ((u + uw) / layer_width) as f32,
                ((v + vh) / layer_height) as f32,
            ];
            self.gpu
                .queue
                .write_buffer(&draw.layer.placement, 0, bytemuck::cast_slice(&placement));
        }
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("TopVNC frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("TopVNC frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.pipeline);
            for draw in draws {
                pass.set_bind_group(0, &draw.layer.bind_groups[usize::from(draw.smooth)], &[]);
                pass.draw(0..6, 0..1);
            }
        }
        self.gpu.queue.submit([encoder.finish()]);
        self.window.pre_present_notify();
        self.gpu.queue.present(frame);
        Ok(true)
    }
}
