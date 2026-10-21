mod capture;
mod cli;
mod pointer;
mod renderer;

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use enigo::{Button, Direction};
use winit::{
    dpi::PhysicalSize,
    event::{ElementState, Event, MouseButton, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::{WindowBuilder, WindowLevel},
};

use crate::capture::Capture;
use crate::cli::Cli;
use crate::pointer::{PointerTransfer, inverse_warp};
use crate::renderer::Renderer;

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.list {
        capture::list_targets();
        return Ok(());
    }
    let target = match (&cli.window, cli.display) {
        (Some(title), _) => capture::find_window(title)?,
        (None, true) => capture::main_display()?,
        (None, false) => anyhow::bail!("pass --window <TITLE>, --display, or --list"),
    };
    let mut capture = Capture::start(target, cli.fps, cli.cursor)?;
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
    let mut pointer = if cli.transfer_input {
        if cli.window.is_some() && cli.input_origin.is_none() {
            anyhow::bail!("--transfer-input with --window requires --input-origin X,Y");
        }
        Some(PointerTransfer::new(cli.input_origin()?).context(
            "input transfer needs macOS Accessibility permission or an X11/Wayland injection backend",
        )?)
    } else {
        None
    };
    let params = cli.warp_params();
    let mut cursor_position = None;

    event_loop.run(move |event, target| {
        target.set_control_flow(ControlFlow::Poll);
        match event {
            Event::AboutToWait => {
                if let Some(frame) = capture.take_new_frame() {
                    if let Err(error) = renderer.upload(&frame) {
                        eprintln!("capture upload failed: {error:#}");
                    }
                }
                window.request_redraw();
            }
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => target.exit(),
                WindowEvent::Resized(size) => renderer.resize(size.width, size.height),
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
                            if let Err(error) = pointer.move_to(point).and_then(|()| {
                                pointer.button(to_enigo_button(button), to_enigo_direction(state))
                            }) {
                                eprintln!("input transfer failed: {error:#}");
                            }
                        }
                    }
                }
                WindowEvent::RedrawRequested => match renderer.render(params) {
                    Ok(()) => {}
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
    pointer: Option<&mut PointerTransfer>,
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
    ) && let Err(error) = pointer.move_to(point)
    {
        eprintln!("input transfer failed: {error:#}");
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
