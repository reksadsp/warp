use std::sync::{Arc, Mutex};
use std::num::NonZero;
use std::thread;

use scap::{
    capturer::{Area, Capturer, Options, Point, Size},
    frame::Frame,
};

use winit::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};

// ─── frame format tag ────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
enum FrameFormat {
    Bgra,
    Bgrx,
    Rgbx,
    Rgb,
}

// ─── shared state ────────────────────────────────────────────────────────────

struct FrameBuffer {
    data: Vec<u8>,
    width: u32,
    height: u32,
    dirty: bool,
}

// ─── helpers ─────────────────────────────────────────────────────────────────

// Normalize any captured frame to packed RGBA bytes.
fn to_rgba(data: &[u8], format: FrameFormat) -> Vec<u8> {
    match format {
        // RGBx is already [R, G, B, X] — identical layout to RGBA for our purposes
        FrameFormat::Rgbx => data.to_vec(),
        // BGRA / BGRX: swap B (byte 0) and R (byte 2) in every pixel
        FrameFormat::Bgra | FrameFormat::Bgrx => {
            let mut out = data.to_vec();
            for pixel in out.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            out
        }
        // RGB: pad each 3-byte pixel to 4 bytes with opaque alpha
        FrameFormat::Rgb => data
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255u8])
            .collect(),
    }
}

// Destructure any supported Frame variant into (width, height, bytes, format).
// Returns None for variants we don't handle (e.g. YUV).
fn extract_frame(frame: Frame) -> Option<(u32, u32, Vec<u8>, FrameFormat)> {
    match frame {
        Frame::BGRA(f) => Some((f.width as u32, f.height as u32, f.data, FrameFormat::Bgra)),
        Frame::BGR0(f) => Some((f.width as u32, f.height as u32, f.data, FrameFormat::Bgrx)),
        Frame::RGBx(f) => Some((f.width as u32, f.height as u32, f.data, FrameFormat::Rgbx)),
        Frame::RGB(f)  => Some((f.width as u32, f.height as u32, f.data, FrameFormat::Rgb)),
        _ => None,
    }
}

// ─── main ─────────────────────────────────────────────────────────────────────

fn main() {
    if !scap::is_supported() {
        eprintln!("Platform not supported");
        return;
    }
    if !scap::has_permission() {
        if !scap::request_permission() {
            eprintln!("Permission denied");
            return;
        }
    }
    // Create Options
    let options = Options {
        fps: 120,
        target: None, // None captures the primary display
        show_cursor: false,
        show_highlight: true,
        excluded_targets: None,
        output_type: scap::frame::FrameType::BGRAFrame,
        output_resolution: scap::capturer::Resolution::_1080p,
        crop_area: Some(Area {
            origin: Point { x: 0.0, y: 0.0 },
            size: Size {
                width: 2000.0,
                height: 1000.0,
            },
        }),
        ..Default::default()
    };
    // Create Capturer
    let mut capturer = Capturer::build(options).unwrap();
    // Start Capture
    capturer.start_capture();
    // ── bootstrap: grab one frame to learn dimensions ─────────────────────────
    println!("Waiting for first frame...");
    let (init_w, init_h, init_rgba) = loop {
        match capturer.get_next_frame() {
            Ok(other) => println!("Got unexpected frame variant: {:?}", std::mem::discriminant(&other)),
            Ok(frame) => {
                if let Some((w, h, raw, fmt)) = extract_frame(frame) {
                    println!("Got first frame: {}x{} format={:?}", w, h, fmt);
                    break (w, h, to_rgba(&raw, fmt));
                }
            }
            Err(e) => {
                eprintln!("Frame error: {:?}", e);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    };
    // Stop Capture
    capturer.stop_capture();

    // ── shared frame buffer (always stores normalized RGBA) ───────────────────
    let shared = Arc::new(Mutex::new(FrameBuffer {
        data: init_rgba,
        width: init_w,
        height: init_h,
        dirty: true,
    }));

    // ── capture thread ────────────────────────────────────────────────────────
    let shared_write = Arc::clone(&shared);
    thread::spawn(move || loop {
        match capturer.get_next_frame() {
            Ok(frame) => {
                if let Some((w, h, raw, fmt)) = extract_frame(frame) {
                    // ← to_rgba is called HERE on every subsequent frame
                    let rgba = to_rgba(&raw, fmt);
                    if let Ok(mut buf) = shared_write.lock() {
                        buf.data = rgba;
                        buf.width = w;
                        buf.height = h;
                        buf.dirty = true;
                    }
                }
            }
            Err(e) => {
                eprintln!("Capture error: {:?}", e);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    });

    // ── winit + wgpu setup ────────────────────────────────────────────────────
    let event_loop = EventLoop::new().unwrap();
    let window = Arc::new(
        WindowBuilder::new()
            .with_title("scap preview")
            .with_inner_size(winit::dpi::PhysicalSize::new(init_w, init_h))
            .build(&event_loop)
            .unwrap(),
    );

    let (device, queue, surface, mut config) =
        pollster::block_on(init_wgpu(Arc::clone(&window), init_w, init_h));

    // Always Rgba8Unorm — to_rgba() has already normalized the bytes
    let (mut cap_tex, mut cap_view) = make_texture(&device, init_w, init_h);

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
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

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: None,
        source: wgpu::ShaderSource::Wgsl(BLIT_WGSL.into()),
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None,
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: config.format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: NonZero::new(0),
        cache: None,
    });

    let mut bind_group = make_bind_group(&device, &bgl, &cap_view, &sampler);

    // ── event loop ────────────────────────────────────────────────────────────
    let shared_read = Arc::clone(&shared);
    event_loop.run(move |event, elwt| {
        elwt.set_control_flow(ControlFlow::Poll);

        match event {
            Event::AboutToWait => {
                // Upload latest RGBA frame if one is waiting
                if let Ok(mut buf) = shared_read.lock() {
                    if buf.dirty {
                        let (w, h) = (buf.width, buf.height);
                        if w != cap_tex.width() || h != cap_tex.height() {
                            (cap_tex, cap_view) = make_texture(&device, w, h);
                            bind_group = make_bind_group(&device, &bgl, &cap_view, &sampler);
                        }
                        // ← to_rgba has already been called in the capture thread;
                        //   buf.data is already normalized RGBA, upload directly
                        upload_frame(&queue, &cap_tex, &buf.data, w, h);
                        buf.dirty = false;
                    }
                }

                let frame = match surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(t) => t,
                    wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
                        surface.configure(&device, &config);
                        t
                    }
                    _ => return,
                };

                let view = frame.texture.create_view(&Default::default());
                let mut enc = device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                {
                    let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: None,
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: NonZero::new(0),
                    });
                    rp.set_pipeline(&pipeline);
                    rp.set_bind_group(0, &bind_group, &[]);
                    rp.draw(0..3, 0..1);
                }
                queue.submit(std::iter::once(enc.finish()));
                frame.present();
                window.request_redraw();
            }

            Event::WindowEvent { event: WindowEvent::Resized(size), .. } => {
                config.width = size.width.max(1);
                config.height = size.height.max(1);
                surface.configure(&device, &config);
            }

            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                elwt.exit();
            }

            _ => {}
        }
    }).unwrap();
}

// ─── WGSL shader: one oversized triangle that covers the whole screen ─────────
// scap gives BGRA bytes; our texture format is Bgra8Unorm so sampling already
// returns (b,g,r,a) in the .bgra swizzle order — we correct to rgba here.
const BLIT_WGSL: &str = r#"
@group(0) @binding(0) var cap_tex : texture_2d<f32>;
@group(0) @binding(1) var cap_smp : sampler;

struct V2F { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> V2F {
    // One triangle big enough to cover the NDC square
    var xy = array<vec2<f32>,3>(
        vec2(-1.0, -3.0),
        vec2( 3.0,  1.0),
        vec2(-1.0,  1.0),
    );
    var uv = array<vec2<f32>,3>(
        vec2(0.0, 2.0),
        vec2(2.0, 0.0),
        vec2(0.0, 0.0),
    );
    return V2F(vec4(xy[vi], 0.0, 1.0), uv[vi]);
}
@fragment
fn fs_main(v: V2F) -> @location(0) vec4<f32> {
    let c = textureSample(cap_tex, cap_smp, v.uv);
    return vec4(c.r, c.g, c.b, 1.0);
}
"#;

// ─── wgpu helpers ─────────────────────────────────────────────────────────────

async fn init_wgpu(
    window: Arc<winit::window::Window>,
    w: u32,
    h: u32,
) -> (wgpu::Device, wgpu::Queue, wgpu::Surface<'static>, wgpu::SurfaceConfiguration) {
    let instance = wgpu::Instance::default();
    let surface = instance.create_surface(window).unwrap();
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        })
        .await
        .unwrap();
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: None,
            required_features: wgpu::Features::empty(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            required_limits: wgpu::Limits::default(),
            memory_hints: Default::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .unwrap();
    let caps = surface.get_capabilities(&adapter);
    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: caps.formats[0],
        width: w,
        height: h,
        present_mode: wgpu::PresentMode::AutoVsync,
        alpha_mode: caps.alpha_modes[0],
        view_formats: vec![],
        desired_maximum_frame_latency: 2,
    };
    surface.configure(&device, &config);
    (device, queue, surface, config)
}

// Always Rgba8Unorm — to_rgba() guarantees the bytes match this format
fn make_texture(device: &wgpu::Device, w: u32, h: u32) -> (wgpu::Texture, wgpu::TextureView) {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = tex.create_view(&Default::default());
    (tex, view)
}

fn upload_frame(queue: &wgpu::Queue, tex: &wgpu::Texture, data: &[u8], w: u32, h: u32) {
    queue.write_texture(
        tex.as_image_copy(),
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * w),
            rows_per_image: None,
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
}

fn make_bind_group(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
    view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}
