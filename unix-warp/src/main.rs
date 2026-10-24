mod capture;
mod cli;
mod input_server;
mod pointer;
mod receiver;
mod remote_pointer;
mod renderer;
mod streaming;

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use enigo::{Button, Direction};
use winit::{
    dpi::PhysicalSize,
    event::{ElementState, Event, MouseButton, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    keyboard::{Key, NamedKey},
    window::{WindowBuilder, WindowLevel},
};

use crate::capture::Capture;
use crate::cli::Cli;
use crate::pointer::{PointerTransfer, inverse_warp};
use crate::receiver::Receiver;
use crate::remote_pointer::RemotePointer;
use crate::renderer::Renderer;
use crate::streaming::Streaming;

enum FrameSource {
    Capture(Capture),
    Receiver(Receiver),
}

impl FrameSource {
    fn take_new_frame(&mut self) -> Option<capture::CapturedFrame> {
        match self {
            Self::Capture(capture) => capture.take_new_frame(),
            Self::Receiver(receiver) => receiver.take_new_frame(),
        }
    }

    fn source_size(&self) -> Option<(u32, u32)> {
        match self {
            Self::Capture(capture) => capture.source_size(),
            Self::Receiver(_) => None,
        }
    }
}

enum PointerTarget {
    Local(PointerTransfer),
    Remote {
        address: String,
        source_size: (u32, u32),
        pointer: Option<RemotePointer>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.list {
        capture::list_targets();
        return Ok(());
    }

    let receive_size = cli
        .receive
        .as_ref()
        .map(|_| cli.receive_size())
        .transpose()?;
    let mut source = match (&cli.window, cli.display, &cli.receive) {
        (_, _, Some(url)) => FrameSource::Receiver(Receiver::start(
            url,
            receive_size.expect("receive size was validated"),
        )?),
        (Some(title), _, None) => FrameSource::Capture(Capture::start(
            capture::find_window(title)?,
            cli.fps,
            cli.cursor,
        )?),
        (None, true, None) => FrameSource::Capture(Capture::start(
            capture::main_display()?,
            cli.fps,
            cli.cursor,
        )?),
        (None, false, None) => {
            anyhow::bail!("pass --window <TITLE>, --display, --receive, or --list")
        }
    };

    let event_loop = EventLoop::new().context("create event loop")?;
    let window = Arc::new({
        let builder = WindowBuilder::new()
            .with_title("unix-warp")
            .with_inner_size(PhysicalSize::new(cli.size, cli.size))
            .with_decorations(!cli.overlay);
        let builder = if cli.overlay {
            builder.with_window_level(WindowLevel::AlwaysOnTop)
        } else {
            builder
        };
        builder
            .build(&event_loop)
            .context("create preview window")?
    });
    let mut renderer = pollster::block_on(Renderer::new(Arc::clone(&window)))?;
    let mut streaming = match &cli.stream {
        Some(destination) => Some(Streaming::start(&mut renderer, &cli, destination)?),
        None => None,
    };
    if let Some(bind) = &cli.control_bind {
        if cli.window.is_some() && cli.input_origin.is_none() {
            anyhow::bail!("--control-bind with --window requires --input-origin X,Y");
        }
        let source_size = source
            .source_size()
            .context("determine captured source size for pointer control")?;
        input_server::start(bind, source_size, cli.input_origin()?)?;
    }
    let mut pointer = if let Some(control) = &cli.control {
        Some(PointerTarget::Remote {
            address: control.clone(),
            source_size: receive_size.expect("--control requires --receive"),
            pointer: None,
        })
    } else if cli.transfer_input {
        if cli.window.is_some() && cli.input_origin.is_none() {
            anyhow::bail!("--transfer-input with --window requires --input-origin X,Y");
        }
        Some(PointerTarget::Local(PointerTransfer::new(cli.input_origin()?).context(
            "input transfer needs macOS Accessibility permission or an X11/Wayland injection backend",
        )?))
    } else {
        None
    };
    let mut params = cli.warp_params();
    let mut cursor_position = None;
    let mut stream_frame_ready = false;
    let mut paused = false;

    event_loop.run(move |event, target| {
        target.set_control_flow(ControlFlow::Poll);
        match event {
            Event::AboutToWait => {
                if !paused && let Some(frame) = source.take_new_frame() {
                    if let Err(error) = renderer.upload(&frame) {
                        eprintln!("video upload failed: {error:#}");
                    } else {
                        stream_frame_ready = true;
                    }
                }
                window.request_redraw();
            }
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => target.exit(),
                WindowEvent::Resized(size) => renderer.resize(size.width, size.height),
                WindowEvent::KeyboardInput { event, .. }
                    if event.state == ElementState::Pressed =>
                {
                    match event.logical_key {
                        Key::Named(NamedKey::Escape) => target.exit(),
                        Key::Named(NamedKey::ArrowLeft) => params.start_angle_deg -= 2.0,
                        Key::Named(NamedKey::ArrowRight) => params.start_angle_deg += 2.0,
                        Key::Named(NamedKey::ArrowUp) => {
                            params.inner_radius = (params.inner_radius + 0.01).min(0.95)
                        }
                        Key::Named(NamedKey::ArrowDown) => {
                            params.inner_radius = (params.inner_radius - 0.01).max(0.0)
                        }
                        Key::Named(NamedKey::PageUp) => {
                            params.supersample = (params.supersample + 1).min(8)
                        }
                        Key::Named(NamedKey::PageDown) => {
                            params.supersample = params.supersample.saturating_sub(1).max(1)
                        }
                        Key::Character(ref key) if key == " " => paused = !paused,
                        _ => {}
                    }
                }
                WindowEvent::CursorMoved { position, .. } => {
                    cursor_position = Some((position.x, position.y));
                    transfer_pointer(
                        pointer.as_mut(),
                        cursor_position,
                        window.inner_size(),
                        renderer.source_size(),
                        params,
                    );
                }
                WindowEvent::MouseInput { state, button, .. } => {
                    if let (Some(pointer), Some(position), Some(source_size)) =
                        (pointer.as_mut(), cursor_position, renderer.source_size())
                    {
                        if let Some(point) = inverse_warp(
                            position,
                            (window.inner_size().width, window.inner_size().height),
                            source_size,
                            params,
                        ) {
                            if let Err(error) = transfer_motion(pointer, point)
                                .and_then(|()| transfer_button(pointer, point, button, state))
                            {
                                eprintln!("input transfer failed: {error:#}");
                            }
                        }
                    }
                }
                WindowEvent::RedrawRequested => match renderer.render(params) {
                    Ok(()) => {
                        if stream_frame_ready
                            && let Some(streaming) = streaming.as_mut()
                            && let Err(error) = streaming.push_frame(&mut renderer, params)
                        {
                            eprintln!("stream encoding failed: {error:#}");
                        }
                        stream_frame_ready = false;
                    }
                    Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                        let size = window.inner_size();
                        renderer.resize(size.width, size.height);
                    }
                    Err(wgpu::SurfaceError::OutOfMemory) => target.exit(),
                    Err(wgpu::SurfaceError::Timeout) => {}
                },
                _ => {}
            },
            _ => {}
        }
    })?;
    Ok(())
}

fn transfer_pointer(
    pointer: Option<&mut PointerTarget>,
    cursor: Option<(f64, f64)>,
    output_size: PhysicalSize<u32>,
    source_size: Option<(u32, u32)>,
    params: cli::WarpParams,
) {
    let (Some(pointer), Some(cursor), Some(source_size)) = (pointer, cursor, source_size) else {
        return;
    };
    if let Some(point) = inverse_warp(
        cursor,
        (output_size.width, output_size.height),
        source_size,
        params,
    ) && let Err(error) = transfer_motion(pointer, point)
    {
        eprintln!("input transfer failed: {error:#}");
    }
}

fn transfer_motion(pointer: &mut PointerTarget, point: pointer::SourcePoint) -> Result<()> {
    match pointer {
        PointerTarget::Local(pointer) => pointer.move_to(point),
        PointerTarget::Remote {
            address,
            source_size,
            pointer,
        } => {
            if pointer.is_none() {
                *pointer = Some(
                    RemotePointer::connect(address, *source_size)
                        .context("connect to remote pointer-control server")?,
                );
            }
            pointer
                .as_mut()
                .expect("remote pointer was initialized")
                .move_to(point)
        }
    }
}

fn transfer_button(
    pointer: &mut PointerTarget,
    point: pointer::SourcePoint,
    button: MouseButton,
    state: ElementState,
) -> Result<()> {
    match pointer {
        PointerTarget::Local(pointer) => {
            pointer.button(to_enigo_button(button), to_enigo_direction(state))
        }
        PointerTarget::Remote { pointer, .. } => pointer
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("remote pointer server is not connected"))?
            .button(point, to_enigo_button(button), to_enigo_direction(state)),
    }
}

fn to_enigo_button(button: MouseButton) -> Button {
    match button {
        MouseButton::Left => Button::Left,
        MouseButton::Right => Button::Right,
        MouseButton::Middle => Button::Middle,
        MouseButton::Back => Button::Back,
        MouseButton::Forward => Button::Forward,
        MouseButton::Other(_) => Button::Left,
    }
}

fn to_enigo_direction(state: ElementState) -> Direction {
    match state {
        ElementState::Pressed => Direction::Press,
        ElementState::Released => Direction::Release,
    }
}
