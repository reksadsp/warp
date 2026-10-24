use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};

use anyhow::{Context, Result};

use crate::cli::Cli;
use crate::renderer::Renderer;

pub struct Streaming {
    child: Child,
    input: Option<ChildStdin>,
}

impl Streaming {
    pub fn start(renderer: &mut Renderer, cli: &Cli, destination: &str) -> Result<Self> {
        let size = cli.stream_size();
        let separator = if destination.contains('?') { '&' } else { '?' };
        let destination = format!(
            "{destination}{separator}pkt_size=1316&ttl={}",
            cli.multicast_ttl
        );
        renderer.enable_stream_target(size, size);
        let mut command = Command::new("ffmpeg");
        command
            .args([
                "-hide_banner",
                "-loglevel",
                "warning",
                "-f",
                "rawvideo",
                "-pixel_format",
                "bgra",
                "-video_size",
                &format!("{size}x{size}"),
                "-framerate",
                &cli.fps.max(1).to_string(),
                "-i",
                "pipe:0",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        if cli.encoder == "h264_vaapi" {
            command.args([
                "-vaapi_device",
                &cli.vaapi_device,
                "-vf",
                "format=nv12,hwupload",
            ]);
        }
        command.args([
            "-c:v",
            &cli.encoder,
            "-b:v",
            &cli.bitrate.to_string(),
            "-g",
            &cli.gop().to_string(),
            "-bf",
            "0",
            "-f",
            "mpegts",
            &destination,
        ]);
        let mut child = command.spawn().context(
            "start ffmpeg H.264 stream encoder; install ffmpeg or use nix develop .#unix-warp",
        )?;
        let input = child.stdin.take().context("capture ffmpeg encoder stdin")?;
        println!(
            "streaming {size}x{size} H.264 via {} at {} kbit/s to {destination}",
            cli.encoder,
            cli.bitrate / 1000
        );
        Ok(Self {
            child,
            input: Some(input),
        })
    }

    pub fn push_frame(
        &mut self,
        renderer: &mut Renderer,
        params: crate::cli::WarpParams,
    ) -> Result<()> {
        let bgra = renderer.render_stream(params)?;
        self.input
            .as_mut()
            .context("ffmpeg encoder has already stopped")?
            .write_all(&bgra)
            .context("write warped frame to ffmpeg encoder")
    }
}

impl Drop for Streaming {
    fn drop(&mut self) {
        self.input.take();
        let _ = self.child.wait();
    }
}
