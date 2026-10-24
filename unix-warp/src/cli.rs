use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "unix-warp",
    version,
    about = "Capture local video or receive a remote MPEG-TS stream and GPU-warp it"
)]
pub struct Cli {
    /// List capturable windows and displays, then exit.
    #[arg(long)]
    pub list: bool,

    /// Capture a window whose title contains this text.
    #[arg(long, value_name = "TITLE", conflicts_with_all = ["display", "receive"])]
    pub window: Option<String>,

    /// Capture the primary display.
    #[arg(long, conflicts_with = "receive")]
    pub display: bool,

    /// Receive an H.264 MPEG-TS stream, for example udp://0.0.0.0:5004.
    #[arg(long, value_name = "UDP_URL", conflicts_with_all = ["window", "display"])]
    pub receive: Option<String>,

    /// Decoded remote stream dimensions as WIDTHxHEIGHT. Required with --receive.
    #[arg(long, value_name = "WIDTHxHEIGHT", requires = "receive")]
    pub receive_size: Option<String>,

    /// TCP address of the Windows sender's --control-bind listener.
    #[arg(long, value_name = "HOST:PORT", requires = "receive")]
    pub control: Option<String>,

    /// Encode the GPU-warped preview to H.264 and send it to this UDP address.
    #[arg(long, value_name = "UDP_URL", conflicts_with = "receive")]
    pub stream: Option<String>,

    /// Bind a trusted-LAN remote pointer-control listener.
    #[arg(long, value_name = "HOST:PORT", conflicts_with = "receive")]
    pub control_bind: Option<String>,

    /// Side length of the encoded stream; defaults to --size.
    #[arg(long, value_name = "PIXELS", requires = "stream")]
    pub stream_size: Option<u32>,

    /// H.264 bitrate in bits per second.
    #[arg(long, default_value_t = 8_000_000, requires = "stream")]
    pub bitrate: u32,

    /// Frames between H.264 key frames; defaults to two seconds.
    #[arg(long, value_name = "FRAMES", requires = "stream")]
    pub gop: Option<u32>,

    /// FFmpeg H.264 encoder, such as h264_vaapi or h264_nvenc.
    #[arg(long, default_value = "h264_vaapi", requires = "stream")]
    pub encoder: String,

    /// VAAPI render device used by h264_vaapi.
    #[arg(
        long,
        default_value = "/dev/dri/renderD128",
        requires = "stream",
        hide_default_value = true
    )]
    pub vaapi_device: String,

    /// Multicast TTL for UDP stream datagrams.
    #[arg(long, default_value_t = 1, requires = "stream")]
    pub multicast_ttl: u32,

    /// Capture frame rate.
    #[arg(long, default_value_t = 60)]
    pub fps: u32,

    /// Include the pointer cursor in captured frames.
    #[arg(long)]
    pub cursor: bool,

    /// Side length of the preview window in pixels.
    #[arg(long, default_value_t = 1080)]
    pub size: u32,

    /// Radius of the disk's inner hole as a fraction of the outer radius.
    #[arg(long, default_value_t = 0.06)]
    pub inner_radius: f32,

    /// Radius of the disk as a fraction of half the shortest output side.
    #[arg(long, default_value_t = 0.98)]
    pub outer_radius: f32,

    /// Angle, in degrees clockwise from 12 o'clock, for the source left edge.
    #[arg(long, default_value_t = 270.0)]
    pub start_angle: f32,

    /// Arc, in degrees, covered by the source width.
    #[arg(long, default_value_t = 360.0)]
    pub angle_span: f32,

    /// Wrap counter-clockwise instead of clockwise.
    #[arg(long)]
    pub counter_clockwise: bool,

    /// Square root of samples per output pixel.
    #[arg(long, default_value_t = 2)]
    pub supersample: u32,

    /// Make the preview borderless and always on top.
    #[arg(long)]
    pub overlay: bool,

    /// Send pointer motion and button events on the warped preview to the source.
    #[arg(long)]
    pub transfer_input: bool,

    /// Top-left source position in desktop pixels, as X,Y. Required for window capture input.
    #[arg(long, value_name = "X,Y", requires = "transfer_input")]
    pub input_origin: Option<String>,
}

impl Cli {
    pub fn warp_params(&self) -> WarpParams {
        WarpParams {
            inner_radius: self.inner_radius.clamp(0.0, 0.95),
            outer_radius: self.outer_radius.clamp(0.05, 1.0),
            start_angle_deg: self.start_angle,
            angle_span_deg: self.angle_span.clamp(0.01, 360.0),
            clockwise: !self.counter_clockwise,
            supersample: self.supersample.clamp(1, 8),
        }
    }

    pub fn input_origin(&self) -> anyhow::Result<(i32, i32)> {
        let value = self.input_origin.as_deref().unwrap_or("0,0");
        let (x, y) = value
            .split_once(',')
            .ok_or_else(|| anyhow::anyhow!("--input-origin must be X,Y"))?;
        Ok((
            x.trim()
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid X in --input-origin"))?,
            y.trim()
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid Y in --input-origin"))?,
        ))
    }

    pub fn receive_size(&self) -> anyhow::Result<(u32, u32)> {
        let value = self
            .receive_size
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("--receive requires --receive-size WIDTHxHEIGHT"))?;
        let (width, height) = value
            .split_once('x')
            .ok_or_else(|| anyhow::anyhow!("--receive-size must be WIDTHxHEIGHT"))?;
        let width = width
            .trim()
            .parse::<u32>()
            .map_err(|_| anyhow::anyhow!("invalid receive width"))?;
        let height = height
            .trim()
            .parse::<u32>()
            .map_err(|_| anyhow::anyhow!("invalid receive height"))?;
        if width == 0 || height == 0 {
            anyhow::bail!("--receive-size dimensions must be non-zero");
        }
        Ok((width, height))
    }

    pub fn stream_size(&self) -> u32 {
        let size = self.stream_size.unwrap_or(self.size).max(16);
        size + size % 2
    }

    pub fn gop(&self) -> u32 {
        self.gop.unwrap_or_else(|| self.fps.max(1) * 2)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct WarpParams {
    pub inner_radius: f32,
    pub outer_radius: f32,
    pub start_angle_deg: f32,
    pub angle_span_deg: f32,
    pub clockwise: bool,
    pub supersample: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_origin_requires_two_coordinates() {
        let cli = Cli::parse_from([
            "unix-warp",
            "--display",
            "--transfer-input",
            "--input-origin",
            "10,20",
        ]);
        assert_eq!(cli.input_origin().unwrap(), (10, 20));
        let cli = Cli::parse_from([
            "unix-warp",
            "--display",
            "--transfer-input",
            "--input-origin",
            "10",
        ]);
        assert!(cli.input_origin().is_err());
    }

    #[test]
    fn receive_size_requires_positive_dimensions() {
        let cli = Cli::parse_from([
            "unix-warp",
            "--receive",
            "udp://0.0.0.0:5004",
            "--receive-size",
            "1080x1080",
        ]);
        assert_eq!(cli.receive_size().unwrap(), (1080, 1080));

        let cli = Cli::parse_from([
            "unix-warp",
            "--receive",
            "udp://0.0.0.0:5004",
            "--receive-size",
            "0x1080",
        ]);
        assert!(cli.receive_size().is_err());
    }

    #[test]
    fn stream_size_is_even_and_has_a_sensible_minimum() {
        let cli = Cli::parse_from(["unix-warp", "--display", "--stream", "udp://127.0.0.1:5004"]);
        assert_eq!(cli.stream_size(), 1080);
        let cli = Cli::parse_from([
            "unix-warp",
            "--display",
            "--stream",
            "udp://127.0.0.1:5004",
            "--stream-size",
            "15",
        ]);
        assert_eq!(cli.stream_size(), 16);
    }
}
