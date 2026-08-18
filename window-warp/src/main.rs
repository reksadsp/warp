#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(not(windows))]
compile_error!(
    "window-warp only builds for Windows: it uses Windows Graphics Capture and Direct3D 11"
);

mod capture;
mod cli;
mod renderer;
mod window;

use anyhow::{Context, Result};
use clap::Parser;
use scap::Target;

use crate::capture::Capture;
use crate::cli::Cli;
use crate::renderer::Renderer;
use crate::window::OutputWindow;

fn main() -> Result<()> {
    let args = Cli::parse();

    if args.list {
        capture::list_targets();
        return Ok(());
    }

    let target = match (&args.window, args.display) {
        (Some(title), _) => capture::find_window(title)?,
        (None, true) => capture::main_display()?,
        (None, false) => {
            anyhow::bail!("pass --window <TITLE>, --display, or --list to see the targets")
        }
    };
    let target_name = match &target {
        Target::Window(window) => window.title.clone(),
        Target::Display(display) => display.title.clone(),
    };
    println!("capturing {target_name:?}");

    let mut capture = Capture::start(target, args.fps, args.cursor)?;

    let params = args.warp_params();
    let output = OutputWindow::create(
        &format!("window-warp - {target_name}"),
        args.size,
        args.overlay,
        params,
    )
    .context("failed to create the output window")?;

    let (width, height) = output.client_size();
    let mut renderer =
        Renderer::new(output.hwnd, width, height).context("failed to initialise Direct3D 11")?;

    loop {
        let state = output.pump();
        if state.quit {
            break;
        }

        if let Some((width, height)) = state.resized_to {
            renderer.resize(width, height)?;
            output.clear_resize();
        }

        if !state.paused {
            if let Some(frame) = capture.take_new_frame() {
                renderer.upload(&frame)?;
            }
        }

        renderer.render(&state.params)?;
    }

    Ok(())
}
