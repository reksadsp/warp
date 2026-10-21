use anyhow::{Result, anyhow};
use enigo::{Button, Coordinate, Direction, Enigo, Mouse, Settings};

use crate::cli::WarpParams;

pub struct PointerTransfer {
    enigo: Enigo,
    origin: (i32, i32),
}

impl PointerTransfer {
    pub fn new(origin: (i32, i32)) -> Result<Self> {
        Ok(Self {
            enigo: Enigo::new(&Settings::default())
                .map_err(|error| anyhow!("cannot initialise native pointer injection: {error}"))?,
            origin,
        })
    }

    pub fn move_to(&mut self, point: SourcePoint) -> Result<()> {
        self.enigo
            .move_mouse(
                self.origin.0 + point.x,
                self.origin.1 + point.y,
                Coordinate::Abs,
            )
            .map_err(|error| anyhow!("cannot transfer pointer motion: {error}"))
    }

    pub fn button(&mut self, button: Button, direction: Direction) -> Result<()> {
        self.enigo
            .button(button, direction)
            .map_err(|error| anyhow!("cannot transfer pointer button: {error}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePoint {
    pub x: i32,
    pub y: i32,
}

pub fn inverse_warp(
    position: (f64, f64),
    output_size: (u32, u32),
    source_size: (u32, u32),
    params: WarpParams,
) -> Option<SourcePoint> {
    let (output_width, output_height) = output_size;
    let radius_px = output_width.min(output_height) as f64 * 0.5 * params.outer_radius as f64;
    if radius_px <= 0.0 || source_size.0 == 0 || source_size.1 == 0 {
        return None;
    }
    let dx = position.0 - output_width as f64 * 0.5;
    let dy = position.1 - output_height as f64 * 0.5;
    let radius = (dx.mul_add(dx, dy * dy)).sqrt() / radius_px;
    if !(params.inner_radius as f64..=1.0).contains(&radius) {
        return None;
    }
    let angle = dx.atan2(-dy);
    let direction = if params.clockwise { 1.0 } else { -1.0 };
    let start = params.start_angle_deg.to_radians() as f64;
    let span = params.angle_span_deg.to_radians() as f64;
    let u = (direction * (angle - start) / span).rem_euclid(1.0);
    let v = 1.0 - (radius - params.inner_radius as f64) / (1.0 - params.inner_radius as f64);
    Some(SourcePoint {
        x: (u * (source_size.0.saturating_sub(1)) as f64).round() as i32,
        y: (v * (source_size.1.saturating_sub(1)) as f64).round() as i32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> WarpParams {
        WarpParams {
            inner_radius: 0.0,
            outer_radius: 1.0,
            start_angle_deg: 0.0,
            angle_span_deg: 360.0,
            clockwise: true,
            supersample: 1,
        }
    }

    #[test]
    fn inverse_warp_maps_top_center_to_source_left_edge() {
        assert_eq!(
            inverse_warp((50.0, 0.0), (100, 100), (1000, 500), params()),
            Some(SourcePoint { x: 0, y: 0 })
        );
    }

    #[test]
    fn inverse_warp_rejects_outside_disk() {
        assert_eq!(
            inverse_warp((0.0, 0.0), (100, 100), (1000, 500), params()),
            None
        );
    }
}
