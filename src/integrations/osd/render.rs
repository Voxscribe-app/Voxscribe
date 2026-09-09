use crate::core::state::Phase;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub fn parse(spec: &str) -> Option<Self> {
        let hex = spec.trim().trim_start_matches('#');
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        let channels: [u8; 4] = match hex.len() {
            3 => {
                let nibble = |i: usize| u8::from_str_radix(&hex[i..i + 1], 16).ok().map(|v| v * 17);
                [nibble(0)?, nibble(1)?, nibble(2)?, 255]
            }
            6 => [byte(0)?, byte(2)?, byte(4)?, 255],
            8 => [byte(0)?, byte(2)?, byte(4)?, byte(6)?],
            _ => return None,
        };
        Some(Self::rgba(
            channels[0] as f32 / 255.0,
            channels[1] as f32 / 255.0,
            channels[2] as f32 / 255.0,
            channels[3] as f32 / 255.0,
        ))
    }

    pub fn with_alpha(self, a: f32) -> Self {
        Self {
            a: a.clamp(0.0, 1.0),
            ..self
        }
    }

    pub fn mix(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        Self {
            r: self.r + (other.r - self.r) * t,
            g: self.g + (other.g - self.g) * t,
            b: self.b + (other.b - self.b) * t,
            a: self.a + (other.a - self.a) * t,
        }
    }

    pub fn lighter(self, amount: f32) -> Self {
        self.mix(Self::rgba(1.0, 1.0, 1.0, self.a), amount)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub background: Color,
    pub border: Color,
    pub accent: Color,
    pub muted: Color,
    pub danger: Color,
}

impl Default for Theme {
    fn default() -> Self {
        let background = Color::parse("#1c1c1e").unwrap();
        Self {
            border: background.lighter(0.18),
            background,
            accent: Color::parse("#ffffff").unwrap(),
            muted: Color::parse("#8a8a8f").unwrap(),
            danger: Color::parse("#e06c75").unwrap(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub width: f32,
    pub height: f32,
    pub radius: f32,
    pub bars: usize,
    pub bar_width: f32,
    pub bar_spacing: f32,
    pub bar_min: f32,
    pub bar_max: f32,
    pub icon: f32,
    pub gap: f32,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            width: 172.0,
            height: 36.0,
            radius: 16.0,
            bars: 16,
            bar_width: 3.0,
            bar_spacing: 3.0,
            bar_min: 3.0,
            bar_max: 18.0,
            icon: 20.0,
            gap: 8.0,
        }
    }
}

impl Layout {
    fn bars_width(&self) -> f32 {
        let bars = self.bars.max(1) as f32;
        bars * self.bar_width + (bars - 1.0) * self.bar_spacing
    }

    pub fn content_width(&self) -> f32 {
        self.icon + self.gap + self.bars_width()
    }
}

pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub scale: u32,
    pub data: Vec<u32>,
}

impl Canvas {
    pub fn new(width: u32, height: u32, scale: u32) -> Self {
        let scale = scale.max(1);
        let (width, height) = (width.max(1), height.max(1));
        Self {
            width: width * scale,
            height: height * scale,
            scale,
            data: vec![0; (width * scale * height * scale) as usize],
        }
    }

    pub fn stride(&self) -> u32 {
        self.width * 4
    }

    pub fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.data.as_ptr() as *const u8, self.data.len() * 4) }
    }

    fn clear(&mut self) {
        self.data.fill(0);
    }

    fn blend(&mut self, x: u32, y: u32, color: Color, coverage: f32) {
        let alpha = color.a * coverage;
        if alpha <= 0.0 {
            return;
        }
        let index = (y * self.width + x) as usize;
        let dst = self.data[index];
        let unpack = |shift: u32| ((dst >> shift) & 0xff) as f32 / 255.0;
        let (da, dr, dg, db) = (unpack(24), unpack(16), unpack(8), unpack(0));
        let inv = 1.0 - alpha;
        let pack = |v: f32| ((v.clamp(0.0, 1.0) * 255.0).round() as u32) & 0xff;
        self.data[index] = (pack(alpha + da * inv) << 24)
            | (pack(color.r * alpha + dr * inv) << 16)
            | (pack(color.g * alpha + dg * inv) << 8)
            | pack(color.b * alpha + db * inv);
    }
}

fn sd_round_rect(px: f32, py: f32, half_w: f32, half_h: f32, radius: f32) -> f32 {
    let radius = radius.min(half_w).min(half_h).max(0.0);
    let qx = px.abs() - (half_w - radius);
    let qy = py.abs() - (half_h - radius);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - radius
}

fn sd_segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (pax, pay) = (px - ax, py - ay);
    let (bax, bay) = (bx - ax, by - ay);
    let denom = bax * bax + bay * bay;
    let t = if denom > 0.0 {
        ((pax * bax + pay * bay) / denom).clamp(0.0, 1.0)
    } else {
        0.0
    };
    ((pax - bax * t).powi(2) + (pay - bay * t).powi(2)).sqrt()
}

fn sd_arc(px: f32, py: f32, radius: f32, cut: f32) -> f32 {
    if py >= cut {
        return ((px * px + py * py).sqrt() - radius).abs();
    }
    let tip_x = (radius * radius - cut * cut).max(0.0).sqrt();
    let tip = |x: f32| ((px - x).powi(2) + (py - cut).powi(2)).sqrt();
    tip(-tip_x).min(tip(tip_x))
}

fn coverage(distance: f32, scale: f32) -> f32 {
    (0.5 - distance * scale).clamp(0.0, 1.0)
}

#[derive(Debug, Clone)]
pub struct Renderer {
    pub theme: Theme,
    pub layout: Layout,
    smooth_level: f32,
    wave: f32,
    opacity: f32,
}

impl Renderer {
    pub fn new(theme: Theme, layout: Layout) -> Self {
        Self {
            theme,
            layout,
            smooth_level: 0.0,
            wave: 0.0,
            opacity: 1.0,
        }
    }

    pub fn advance(&mut self, phase: Phase, level: f32) {
        let target = match phase {
            Phase::Recording => level.clamp(0.0, 1.0),
            Phase::Processing => 0.45,
            Phase::Paused => 0.08,
            _ => 0.0,
        };
        self.smooth_level += (target - self.smooth_level) * 0.3;
        self.wave += 0.35;
        if self.wave > std::f32::consts::TAU * 64.0 {
            self.wave -= std::f32::consts::TAU * 64.0;
        }
    }

    pub fn set_opacity(&mut self, opacity: f32) {
        self.opacity = opacity.clamp(0.0, 1.0);
    }

    fn foreground(&self, phase: Phase) -> Color {
        match phase {
            Phase::Recording => self.theme.accent,
            Phase::Processing => self.theme.accent.mix(self.theme.muted, 0.35),
            Phase::Paused => self.theme.muted,
            Phase::Error => self.theme.danger,
            _ => self.theme.muted,
        }
    }

    fn amplitude(&self) -> f32 {
        (self.smooth_level.max(0.0).sqrt() * 1.6).min(1.0)
    }

    pub fn draw(&self, canvas: &mut Canvas, phase: Phase) {
        canvas.clear();
        let scale = canvas.scale as f32;
        let fade = self.opacity;
        if fade <= 0.0 {
            return;
        }

        let island = self
            .theme
            .background
            .with_alpha(self.theme.background.a * fade);
        let border = self.theme.border.with_alpha(self.theme.border.a * fade);
        let ink = self.foreground(phase);
        let ink = ink.with_alpha(ink.a * fade);

        let (cx, cy) = (self.layout.width / 2.0, self.layout.height / 2.0);
        let content = self.layout.content_width();
        let icon_cx = cx - content / 2.0 + self.layout.icon / 2.0;
        let bars_left = cx - content / 2.0 + self.layout.icon + self.layout.gap;

        let amplitude = self.amplitude();
        let bar_heights: Vec<f32> = (0..self.layout.bars.max(1))
            .map(|index| {
                let sway = 0.4 + 0.6 * (self.wave + index as f32 * 0.7).sin().abs();
                self.layout.bar_min + (self.layout.bar_max - self.layout.bar_min) * amplitude * sway
            })
            .collect();

        for y in 0..canvas.height {
            let py = (y as f32 + 0.5) / scale;
            for x in 0..canvas.width {
                let px = (x as f32 + 0.5) / scale;

                let outer = sd_round_rect(
                    px - cx,
                    py - cy,
                    self.layout.width / 2.0,
                    self.layout.height / 2.0,
                    self.layout.radius,
                );
                let inside = coverage(outer, scale);
                if inside <= 0.0 {
                    continue;
                }
                canvas.blend(x, y, island, inside);
                let edge = coverage(outer.abs() - 0.5, scale) * inside;
                if edge > 0.0 {
                    canvas.blend(x, y, border, edge);
                }

                let mut mark = self.icon_distance(px - icon_cx, py - cy);
                for (index, height) in bar_heights.iter().enumerate() {
                    let bar_cx = bars_left
                        + self.layout.bar_width / 2.0
                        + index as f32 * (self.layout.bar_width + self.layout.bar_spacing);
                    mark = mark.min(sd_round_rect(
                        px - bar_cx,
                        py - cy,
                        self.layout.bar_width / 2.0,
                        height / 2.0,
                        self.layout.bar_width / 2.0,
                    ));
                }
                let ink_coverage = coverage(mark, scale) * inside;
                if ink_coverage > 0.0 {
                    canvas.blend(x, y, ink, ink_coverage);
                }
            }
        }
    }

    fn icon_distance(&self, px: f32, py: f32) -> f32 {
        let unit = self.layout.icon / 16.0;
        let (px, py) = (px / unit, py / unit + 0.4);
        let stroke = 0.75;

        let head = sd_round_rect(px, py + 4.4, 2.0, 3.0, 2.0);
        let cradle = sd_arc(px, py + 3.6, 4.3, 0.6) - stroke;
        let stem = sd_segment(px, py, 0.0, 1.45, 0.0, 5.0) - stroke;
        let base = sd_segment(px, py, -2.9, 5.9, 2.9, 5.9) - stroke;
        head.min(cradle).min(stem).min(base) * unit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha_at(canvas: &Canvas, x: u32, y: u32) -> u8 {
        ((canvas.data[(y * canvas.width + x) as usize] >> 24) & 0xff) as u8
    }

    #[test]
    fn colors_parse_in_every_accepted_length() {
        assert_eq!(Color::parse("#fff"), Some(Color::rgba(1.0, 1.0, 1.0, 1.0)));
        assert_eq!(
            Color::parse("000000"),
            Some(Color::rgba(0.0, 0.0, 0.0, 1.0))
        );
        let half = Color::parse("#ff000080").unwrap();
        assert!((half.a - 0.5019608).abs() < 1e-6);
        assert!(Color::parse("#12345").is_none());
        assert!(Color::parse("nope").is_none());
    }

    #[test]
    fn the_island_is_opaque_in_the_middle_and_clear_at_the_corners() {
        let mut canvas = Canvas::new(172, 36, 1);
        let renderer = Renderer::new(Theme::default(), Layout::default());
        renderer.draw(&mut canvas, Phase::Recording);
        assert!(alpha_at(&canvas, 86, 18) > 240);
        assert_eq!(alpha_at(&canvas, 0, 0), 0);
        assert_eq!(alpha_at(&canvas, 171, 35), 0);
    }

    #[test]
    fn bars_grow_with_the_level() {
        let layout = Layout::default();
        let mut quiet = Renderer::new(Theme::default(), layout.clone());
        let mut loud = Renderer::new(Theme::default(), layout);
        for _ in 0..40 {
            quiet.advance(Phase::Recording, 0.0);
            loud.advance(Phase::Recording, 1.0);
        }
        loud.wave = quiet.wave;
        assert!(loud.amplitude() > quiet.amplitude() + 0.5);
    }

    #[test]
    fn a_hidden_island_draws_nothing() {
        let mut canvas = Canvas::new(172, 36, 1);
        let mut renderer = Renderer::new(Theme::default(), Layout::default());
        renderer.set_opacity(0.0);
        renderer.draw(&mut canvas, Phase::Recording);
        assert!(canvas.data.iter().all(|pixel| *pixel == 0));
    }

    #[test]
    fn scaling_multiplies_the_buffer_but_not_the_layout() {
        let canvas = Canvas::new(172, 36, 2);
        assert_eq!((canvas.width, canvas.height), (344, 72));
        assert_eq!(canvas.stride(), 344 * 4);
        assert_eq!(canvas.bytes().len(), (344 * 72 * 4) as usize);
    }

    #[test]
    fn phases_pick_distinct_colors() {
        let renderer = Renderer::new(Theme::default(), Layout::default());
        assert_eq!(
            renderer.foreground(Phase::Recording),
            Theme::default().accent
        );
        assert_eq!(renderer.foreground(Phase::Error), Theme::default().danger);
        assert_ne!(
            renderer.foreground(Phase::Processing),
            renderer.foreground(Phase::Paused)
        );
    }
}
