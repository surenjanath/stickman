//! wgpu (Metal) renderer for the overlay: clears to fully transparent and draws a
//! [`DrawList`] of SDF primitives (capsules, rings, discs) in one instanced call.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use bytemuck::{Pod, Zeroable};
use winit::window::Window;

use crate::geom::V2;
use crate::stickman::{BoneKind, Segment};

/// Light halo drawn under every bone so the dark figure reads on dark wallpapers.
const OUTLINE_RGBA: [f32; 4] = [1.0, 1.0, 1.0, 0.85];
const OUTLINE_PT: f32 = 1.5;
/// Upper bound on primitives per frame (figure + particles + HUD).
pub const MAX_INSTANCES: usize = 2048;

const KIND_CAPSULE: f32 = 0.0;
const KIND_RING: f32 = 1.0;
const KIND_DISC: f32 = 2.0;
const KIND_RECT: f32 = 3.0;
/// Sprites (grabbed words) drawn per frame, at most.
pub const MAX_SPRITES: usize = 64;

/// A textured quad: a GPU sprite created with [`Renderer::create_sprite`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpriteDraw {
    pub id: u32,
    pub center: V2,
    /// Half extents in points.
    pub half: V2,
    pub rotation: f32,
    pub alpha: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct SpriteInstance {
    center: [f32; 2],
    half: [f32; 2],
    rot_alpha: [f32; 2],
    screen: [f32; 2],
}

struct Sprite {
    _texture: wgpu::Texture,
    bind: wgpu::BindGroup,
}

/// One SDF primitive, in points (converted to pixels at upload).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct Instance {
    a: [f32; 2],
    b: [f32; 2],
    screen: [f32; 2],
    /// half stroke width / radius, kind (0 capsule, 1 ring, 2 disc)
    params: [f32; 2],
    color: [f32; 4],
}

/// Primitives to draw this frame, back to front, in overlay points.
#[derive(Default, Debug, Clone, PartialEq)]
pub struct DrawList {
    items: Vec<Instance>,
    sprites: Vec<SpriteDraw>,
}

impl DrawList {
    pub fn len(&self) -> usize {
        self.items.len() + self.sprites.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.sprites.is_empty()
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.sprites.clear();
    }

    /// A rounded rectangle centred at `center` with half extents `half`.
    pub fn rect(&mut self, center: V2, half: V2, radius: f32, color: [f32; 4]) {
        self.push(center, half, radius, KIND_RECT, color);
    }

    /// A textured sprite, drawn above all SDF primitives.
    pub fn sprite(&mut self, s: SpriteDraw) {
        if self.sprites.len() < MAX_SPRITES && s.alpha > 0.0 {
            self.sprites.push(s);
        }
    }

    fn push(&mut self, a: V2, b: V2, half: f32, kind: f32, color: [f32; 4]) {
        if self.items.len() < MAX_INSTANCES && color[3] > 0.0 {
            self.items.push(Instance {
                a: a.into(),
                b: b.into(),
                screen: [0.0; 2],
                params: [half, kind],
                color,
            });
        }
    }

    /// A line from `a` to `b` with round caps, `half` points thick on each side.
    pub fn capsule(&mut self, a: V2, b: V2, half: f32, color: [f32; 4]) {
        self.push(a, b, half, KIND_CAPSULE, color);
    }

    /// A circle outline of `radius` with stroke half-width `half`.
    pub fn ring(&mut self, center: V2, radius: f32, half: f32, color: [f32; 4]) {
        let r = V2::new(radius, 0.0);
        self.push(center - r, center + r, half, KIND_RING, color);
    }

    /// A filled circle.
    pub fn disc(&mut self, center: V2, radius: f32, color: [f32; 4]) {
        self.push(center, center, radius, KIND_DISC, color);
    }

    /// The stick figure: a light outline pass under a solid fill pass.
    pub fn figure(&mut self, segments: &[Segment], rgb: [u8; 3]) {
        let fill = [
            rgb[0] as f32 / 255.0,
            rgb[1] as f32 / 255.0,
            rgb[2] as f32 / 255.0,
            1.0,
        ];
        for (extra, color) in [(OUTLINE_PT, OUTLINE_RGBA), (0.0, fill)] {
            for s in segments {
                let kind = if s.kind == BoneKind::Circle {
                    KIND_RING
                } else {
                    KIND_CAPSULE
                };
                self.push(
                    s.start.into(),
                    s.end.into(),
                    s.thickness / 2.0 + extra,
                    kind,
                    color,
                );
            }
        }
    }
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    instances: wgpu::Buffer,
    sprite_pipeline: wgpu::RenderPipeline,
    sprite_layout: wgpu::BindGroupLayout,
    sprite_sampler: wgpu::Sampler,
    sprite_instances: wgpu::Buffer,
    sprites: std::collections::HashMap<u32, Sprite>,
    next_sprite: u32,
    window: Arc<Window>,
}

impl Renderer {
    pub fn new(window: Arc<Window>) -> Result<Self> {
        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(window.clone())
            .context("create wgpu surface")?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            // An always-on overlay should sip power.
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .context("no compatible GPU adapter")?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("stickman-device"),
            ..Default::default()
        }))
        .context("request GPU device")?;

        let caps = surface.get_capabilities(&adapter);
        // Non-sRGB target: colours are authored in sRGB and the figure is tiny,
        // so blending in gamma space is fine and keeps colours exact.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .or_else(|| caps.formats.first().copied())
            .ok_or_else(|| anyhow!("surface reports no formats"))?;
        // A non-opaque layer is required for the desktop to show through.
        let alpha_mode = [
            wgpu::CompositeAlphaMode::PostMultiplied,
            wgpu::CompositeAlphaMode::PreMultiplied,
        ]
        .into_iter()
        .find(|m| caps.alpha_modes.contains(m))
        .ok_or_else(|| {
            anyhow!(
                "surface cannot be transparent; alpha modes: {:?}",
                caps.alpha_modes
            )
        })?;

        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or_else(|| anyhow!("surface not supported by adapter"))?;
        config.format = format;
        config.alpha_mode = alpha_mode;
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&device, &config);
        log::info!(
            "renderer: {} {:?} {:?} {}x{} px",
            adapter.get_info().name,
            format,
            alpha_mode,
            config.width,
            config.height
        );

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("stickman.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("stickman.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("stickman-layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let attributes = wgpu::vertex_attr_array![
            0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Float32x2, 4 => Float32x4
        ];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("stickman-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &attributes,
                })],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("stickman-instances"),
            size: (MAX_INSTANCES * std::mem::size_of::<Instance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Sprite pipeline: textured quads with premultiplied alpha.
        let sprite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sprite.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("sprite.wgsl").into()),
        });
        let sprite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sprite-bgl"),
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
            ],
        });
        let sprite_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sprite-layout"),
            bind_group_layouts: &[Some(&sprite_layout)],
            immediate_size: 0,
        });
        let sprite_attrs = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Float32x2];
        let sprite_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sprite-pipeline"),
            layout: Some(&sprite_pl),
            vertex: wgpu::VertexState {
                module: &sprite_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<SpriteInstance>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &sprite_attrs,
                })],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &sprite_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sprite_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("sprite-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let sprite_instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sprite-instances"),
            size: (MAX_SPRITES * std::mem::size_of::<SpriteInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Ok(Renderer {
            surface,
            device,
            queue,
            config,
            pipeline,
            instances,
            sprite_pipeline,
            sprite_layout,
            sprite_sampler,
            sprite_instances,
            sprites: std::collections::HashMap::new(),
            next_sprite: 1,
            window,
        })
    }

    /// Upload a premultiplied RGBA8 image (`w`×`h` pixels) as a sprite.
    pub fn create_sprite(&mut self, rgba: &[u8], w: u32, h: u32) -> Option<u32> {
        if w == 0 || h == 0 || rgba.len() != (w * h * 4) as usize {
            return None;
        }
        let size = wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sprite"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * w),
                rows_per_image: Some(h),
            },
            size,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sprite-bg"),
            layout: &self.sprite_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sprite_sampler),
                },
            ],
        });
        let id = self.next_sprite;
        self.next_sprite += 1;
        self.sprites.insert(
            id,
            Sprite {
                _texture: texture,
                bind,
            },
        );
        Some(id)
    }

    /// Free a sprite's GPU memory.
    pub fn free_sprite(&mut self, id: u32) {
        self.sprites.remove(&id);
    }

    pub fn sprite_count(&self) -> usize {
        self.sprites.len()
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    /// Draw one frame (an empty list clears the overlay to fully transparent).
    /// Returns whether the frame actually reached the screen.
    pub fn render(&mut self, list: &DrawList) -> bool {
        let scale = self.window.scale_factor() as f32;
        let screen = [self.config.width as f32, self.config.height as f32];
        let data: Vec<Instance> = list
            .items
            .iter()
            .map(|i| Instance {
                a: [i.a[0] * scale, i.a[1] * scale],
                b: [i.b[0] * scale, i.b[1] * scale],
                screen,
                params: [i.params[0] * scale, i.params[1]],
                color: i.color,
            })
            .collect();
        if !data.is_empty() {
            self.queue
                .write_buffer(&self.instances, 0, bytemuck::cast_slice(&data));
        }
        let sprites: Vec<SpriteInstance> = list
            .sprites
            .iter()
            .map(|s| SpriteInstance {
                center: [s.center.x * scale, s.center.y * scale],
                half: [s.half.x * scale, s.half.y * scale],
                rot_alpha: [s.rotation, s.alpha.clamp(0.0, 1.0)],
                screen,
            })
            .collect();
        if !sprites.is_empty() {
            self.queue
                .write_buffer(&self.sprite_instances, 0, bytemuck::cast_slice(&sprites));
        }

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return false;
            }
            // Occluded / timeout / validation: skip this frame.
            other => {
                log::debug!("skipping frame: {other:?}");
                return false;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("stickman-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, self.instances.slice(..));
            if !data.is_empty() {
                pass.draw(0..6, 0..data.len() as u32);
            }
            if !sprites.is_empty() {
                pass.set_pipeline(&self.sprite_pipeline);
                pass.set_vertex_buffer(0, self.sprite_instances.slice(..));
                for (i, s) in list.sprites.iter().take(sprites.len()).enumerate() {
                    if let Some(sp) = self.sprites.get(&s.id) {
                        pass.set_bind_group(0, &sp.bind, &[]);
                        pass.draw(0..6, i as u32..i as u32 + 1);
                    }
                }
            }
        }
        self.queue.submit([encoder.finish()]);
        self.window.pre_present_notify();
        self.queue.present(frame);
        true
    }
}
