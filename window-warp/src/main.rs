#[cfg_attr(not(windows), allow(dead_code))]
mod cli;
// Both are platform independent, but only the Windows binary drives them.
#[cfg_attr(not(windows), allow(dead_code))]
mod mpegts;
#[cfg_attr(not(windows), allow(dead_code))]
mod stream;

#[cfg(windows)]
mod capture;
#[cfg(windows)]
mod encoder;
#[cfg(windows)]
mod nv12;
#[cfg(windows)]
mod renderer;
#[cfg(windows)]
mod window;

#[cfg(not(windows))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!(
        "window-warp only runs on Windows: it uses Windows Graphics Capture and Direct3D 11"
    )
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    windows_main::run()
}

#[cfg(windows)]
mod windows_main {
    use anyhow::{Context, Result};
    use clap::Parser;
    use scap::Target;

    use crate::capture::Capture;
    use crate::cli::Cli;
    use crate::encoder::{EncodedFrame, EncoderConfig, H264Encoder};
    use crate::renderer::Renderer;
    use crate::stream::UdpStream;
    use crate::window::OutputWindow;

    pub fn run() -> Result<()> {
        let args = Cli::parse();

        if args.list {
            crate::capture::list_targets();
            return Ok(());
        }

        let target = match (&args.window, args.display) {
            (Some(title), _) => crate::capture::find_window(title)?,
            (None, true) => crate::capture::main_display()?,
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
        let mut renderer = Renderer::new(output.hwnd, width, height)
            .context("failed to initialise Direct3D 11")?;

        let mut streaming = match &args.stream {
            Some(url) => Some(Streaming::start(&mut renderer, &args, url)?),
            None => None,
        };

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

            if let Some(streaming) = streaming.as_mut() {
                streaming.push_frame(&mut renderer, &state.params)?;
            }

            renderer.render(&state.params)?;
        }

        if let Some(streaming) = streaming.as_mut() {
            streaming.finish()?;
        }
        Ok(())
    }

    /// The encoder and the network sink, driven once per rendered frame.
    struct Streaming {
        encoder: H264Encoder,
        sink: UdpStream,
        frames: Vec<EncodedFrame>,
    }

    impl Streaming {
        fn start(renderer: &mut Renderer, args: &Cli, url: &str) -> Result<Self> {
            let size = args.stream_size();
            renderer.enable_encode_target(size, size)?;

            let config = EncoderConfig {
                width: size,
                height: size,
                fps: args.fps.max(1),
                bitrate: args.bitrate,
                gop: args.gop(),
            };
            let encoder = H264Encoder::new(renderer.device(), renderer.context(), config)
                .context("failed to set up the H.264 encoder")?;
            let sink = UdpStream::open(url, args.multicast_ttl)?;
            println!(
                "streaming {size}x{size} H.264 at {} kbit/s to udp://{}",
                config.bitrate / 1000,
                sink.destination()
            );

            Ok(Self {
                encoder,
                sink,
                frames: Vec::new(),
            })
        }

        fn push_frame(
            &mut self,
            renderer: &mut Renderer,
            params: &crate::cli::WarpParams,
        ) -> Result<()> {
            let frame = renderer.render_encode_frame(params)?;
            self.encoder.encode(&frame, &mut self.frames)?;
            self.send_pending()
        }

        fn finish(&mut self) -> Result<()> {
            self.encoder.drain(&mut self.frames)?;
            self.send_pending()?;
            self.sink.flush()
        }

        fn send_pending(&mut self) -> Result<()> {
            for frame in self.frames.drain(..) {
                self.sink
                    .send_access_unit(&frame.data, frame.pts_90khz(), frame.keyframe)?;
            }
            Ok(())
        }
    }
}
