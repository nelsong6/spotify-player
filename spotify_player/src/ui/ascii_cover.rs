use crate::config::{self, AsciiCoverMode};
use image::{imageops::FilterType, DynamicImage};
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

/// Characters from darkest to brightest.
const RAMP: &[u8] = b" .:-=+*#%@";
/// The lowest log-scale bands, which cover roughly the bass range, drive the pulse.
const BASS_BANDS: usize = 6;

/// The ASCII cover beside the audio visualization: its mode and the cover sampled to one
/// cell per pixel, kept until the cover or the area changes.
#[derive(Debug)]
pub struct AsciiCover {
    pub mode: AsciiCoverMode,
    url: String,
    area: Rect,
    /// Per cell, row by row: luminance in [0, 1] and color
    cells: Vec<(f32, [u8; 3])>,
}

impl AsciiCover {
    pub fn new() -> Self {
        Self {
            mode: config::get_config().app_config.ascii_cover,
            url: String::new(),
            area: Rect::default(),
            cells: Vec::new(),
        }
    }

    /// Render the cover `img` (from `url`) into `area`. `levels` are the visualization's band
    /// levels, which pulse the cover in `Pulse` mode.
    pub fn render(
        &mut self,
        frame: &mut Frame,
        url: &str,
        img: &DynamicImage,
        area: Rect,
        levels: Option<&[f32]>,
    ) {
        if area.is_empty() {
            return;
        }
        if self.url != url || self.area != area {
            self.cells = sample(img, area);
            url.clone_into(&mut self.url);
            self.area = area;
        }

        let gain = match (self.mode, levels) {
            (AsciiCoverMode::Pulse, Some(levels)) => {
                let bass = levels.iter().take(BASS_BANDS).sum::<f32>() / BASS_BANDS as f32;
                0.4 + bass
            }
            _ => 1.0,
        };
        let lines: Vec<Line> = self
            .cells
            .chunks(area.width as usize)
            .map(|row| {
                Line::from(
                    row.iter()
                        .map(|&cell| cell_span(cell, gain))
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), area);
    }
}

/// Resize `img` to one pixel per cell of `area`. A cell is about twice as tall as it is wide,
/// so an area twice as wide as it is tall keeps a square cover square.
fn sample(img: &DynamicImage, area: Rect) -> Vec<(f32, [u8; 3])> {
    let mut cells: Vec<(f32, [u8; 3])> = img
        .resize_exact(
            u32::from(area.width),
            u32::from(area.height),
            FilterType::Triangle,
        )
        .to_rgb8()
        .pixels()
        .map(|p| {
            let [r, g, b] = p.0;
            let lum =
                (0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b)) / 255.0;
            (lum, p.0)
        })
        .collect();
    // Stretch the luminance over the full ramp, so a mostly dark or mostly bright cover
    // still shows its shapes.
    let (min, max) = cells
        .iter()
        .fold((1.0_f32, 0.0_f32), |(min, max), &(lum, _)| {
            (min.min(lum), max.max(lum))
        });
    if max > min {
        for (lum, _) in &mut cells {
            *lum = (*lum - min) / (max - min);
        }
    }
    cells
}

/// A cell's character and color, both scaled by `gain`.
fn cell_span((lum, rgb): (f32, [u8; 3]), gain: f32) -> Span<'static> {
    let lum = (lum * gain).clamp(0.0, 1.0);
    let ch = RAMP[(lum * (RAMP.len() - 1) as f32).round() as usize];
    let [r, g, b] = rgb.map(|c| (f32::from(c) * gain).clamp(0.0, 255.0) as u8);
    Span::styled(
        char::from(ch).to_string(),
        Style::default().fg(Color::Rgb(r, g, b)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_fills_one_cell_per_pixel_and_ramps_by_brightness() {
        let img = DynamicImage::ImageRgb8(image::RgbImage::from_fn(32, 32, |x, _| {
            let v = (x * 8) as u8;
            image::Rgb([v, v, v])
        }));
        let cells = sample(&img, Rect::new(0, 0, 16, 8));
        assert_eq!(cells.len(), 16 * 8);
        assert!(cells[0].0 < cells[15].0);
        assert_eq!(cell_span((0.0, [0, 0, 0]), 1.0).content, " ");
        assert_eq!(cell_span((1.0, [255, 255, 255]), 1.0).content, "@");
        // pulse gain brightens a mid-gray cell's character
        assert!(cell_span((0.5, [128; 3]), 1.4).content != cell_span((0.5, [128; 3]), 1.0).content);
    }
}
