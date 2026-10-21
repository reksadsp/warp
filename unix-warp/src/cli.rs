use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "unix-warp",
    version,
    about = "Capture and GPU-warp a Unix window or display"
)]
pub struct Cli {
    /// List capturable windows and displays, then exit.
    #[arg(long)]
    pub list: bool,

    /// Capture a window whose title contains this text.
    #[arg(long, value_name = "TITLE", conflicts_with = "display")]
    pub window: Option<String>,

    /// Capture the primary display.
    #[arg(long)]
    pub display: bool,

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
}
