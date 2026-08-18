use clap::Parser;

/// Capture a window and warp it from a rectangle onto a disk on the GPU.
#[derive(Parser, Debug, Clone)]
#[command(name = "window-warp", version, about)]
pub struct Cli {
    /// List the capturable windows and exit.
    #[arg(long)]
    pub list: bool,

    /// Title (or part of a title, case insensitive) of the window to capture.
    #[arg(long, value_name = "TITLE")]
    pub window: Option<String>,

    /// Capture the whole main display instead of a window.
    #[arg(long, conflicts_with = "window")]
    pub display: bool,

    /// Capture frame rate.
    #[arg(long, default_value_t = 60)]
    pub fps: u32,

    /// Include the mouse cursor in the capture.
    #[arg(long)]
    pub cursor: bool,

    /// Side length of the output window, in pixels.
    #[arg(long, default_value_t = 1080, value_name = "PIXELS")]
    pub size: u32,

    /// Radius of the hole in the middle, as a fraction of the disk radius.
    #[arg(long, default_value_t = 0.06, value_name = "0..1")]
    pub inner_radius: f32,

    /// Radius of the disk, as a fraction of half the output side length.
    #[arg(long, default_value_t = 0.98, value_name = "0..1")]
    pub outer_radius: f32,

    /// Angle, in degrees clockwise from 12 o'clock, that the left edge of the
    /// window is wrapped to.
    #[arg(long, default_value_t = 270.0, value_name = "DEGREES")]
    pub start_angle: f32,

    /// Arc, in degrees, covered by the full width of the window.
    #[arg(long, default_value_t = 360.0, value_name = "DEGREES")]
    pub angle_span: f32,

    /// Wrap counter-clockwise instead of clockwise.
    #[arg(long)]
    pub counter_clockwise: bool,

    /// Square root of the number of samples per output pixel (1 disables
    /// supersampling).
    #[arg(long, default_value_t = 2, value_name = "N")]
    pub supersample: u32,

    /// Keep the window borderless and always on top.
    #[arg(long)]
    pub overlay: bool,

    /// Encode the warped frames to H.264 and send them, in an MPEG-TS, to this
    /// UDP address, for example udp://239.0.0.1:5004.
    #[arg(long, value_name = "udp://HOST:PORT")]
    pub stream: Option<String>,

    /// Side length of the encoded frames, in pixels. Defaults to --size.
    #[arg(long, value_name = "PIXELS")]
    pub stream_size: Option<u32>,

    /// Encoder bitrate, in bits per second.
    #[arg(long, default_value_t = 8_000_000, value_name = "BITS")]
    pub bitrate: u32,

    /// Frames between key frames. Defaults to two seconds worth of frames.
    #[arg(long, value_name = "FRAMES")]
    pub gop: Option<u32>,

    /// Time to live of the multicast datagrams.
    #[arg(long, default_value_t = 1, value_name = "HOPS")]
    pub multicast_ttl: u32,
}

impl Cli {
    /// H.264 dimensions are macroblock based, so the encoded size is rounded to
    /// an even number of pixels.
    pub fn stream_size(&self) -> u32 {
        let size = self.stream_size.unwrap_or(self.size).max(16);
        size + (size % 2)
    }

    pub fn gop(&self) -> u32 {
        self.gop.unwrap_or_else(|| self.fps.max(1) * 2)
    }

    pub fn warp_params(&self) -> WarpParams {
        WarpParams {
            inner_radius: self.inner_radius.clamp(0.0, 0.95),
            outer_radius: self.outer_radius.clamp(0.05, 1.0),
            start_angle_deg: self.start_angle,
            angle_span_deg: self.angle_span,
            clockwise: !self.counter_clockwise,
            supersample: self.supersample.clamp(1, 8),
        }
    }
}

/// Runtime-tweakable parameters of the warp.
#[derive(Debug, Clone, Copy)]
pub struct WarpParams {
    pub inner_radius: f32,
    pub outer_radius: f32,
    pub start_angle_deg: f32,
    pub angle_span_deg: f32,
    pub clockwise: bool,
    pub supersample: u32,
}
