use serde::{Deserialize, Serialize};

/// A point in MuPDF page space: origin at the top left, y growing downward.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfPoint {
    pub x: f32,
    pub y: f32,
}

impl PdfPoint {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// An axis-aligned rectangle in MuPDF page space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfRect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl PdfRect {
    pub const fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self { x0, y0, x1, y1 }
    }

    pub fn from_points(a: PdfPoint, b: PdfPoint) -> Self {
        Self {
            x0: a.x.min(b.x),
            y0: a.y.min(b.y),
            x1: a.x.max(b.x),
            y1: a.y.max(b.y),
        }
    }

    pub fn from_minmax(
        xs: impl IntoIterator<Item = f32>,
        ys: impl IntoIterator<Item = f32>,
    ) -> Self {
        let mut xs = xs.into_iter();
        let mut ys = ys.into_iter();
        let (Some(mut min_x), Some(mut min_y)) = (xs.next(), ys.next()) else {
            return Self::new(0.0, 0.0, 0.0, 0.0);
        };
        let mut max_x = min_x;
        let mut max_y = min_y;
        for x in xs {
            min_x = min_x.min(x);
            max_x = max_x.max(x);
        }
        for y in ys {
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }
        Self::new(min_x, min_y, max_x, max_y)
    }

    pub fn width(self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(self) -> f32 {
        self.y1 - self.y0
    }

    pub fn is_empty(self) -> bool {
        self.width() < 0.5 || self.height() < 0.5
    }

    pub fn contains(self, p: PdfPoint) -> bool {
        p.x >= self.x0 && p.x <= self.x1 && p.y >= self.y0 && p.y <= self.y1
    }

    pub fn translate(self, dx: f32, dy: f32) -> Self {
        Self::new(self.x0 + dx, self.y0 + dy, self.x1 + dx, self.y1 + dy)
    }

    pub fn inflate(self, amount: f32) -> Self {
        Self::new(
            self.x0 - amount,
            self.y0 - amount,
            self.x1 + amount,
            self.y1 + amount,
        )
    }

    pub fn center(self) -> PdfPoint {
        PdfPoint::new((self.x0 + self.x1) * 0.5, (self.y0 + self.y1) * 0.5)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub fn to_color32(self) -> egui::Color32 {
        egui::Color32::from_rgb(self.r, self.g, self.b)
    }

    pub fn to_unit(self) -> [f32; 3] {
        [
            self.r as f32 / 255.0,
            self.g as f32 / 255.0,
            self.b as f32 / 255.0,
        ]
    }

    pub fn from_unit(rgb: [f32; 3]) -> Self {
        Self::new(
            (rgb[0].clamp(0.0, 1.0) * 255.0).round() as u8,
            (rgb[1].clamp(0.0, 1.0) * 255.0).round() as u8,
            (rgb[2].clamp(0.0, 1.0) * 255.0).round() as u8,
        )
    }

    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

pub const HIGHLIGHT_COLORS: [Rgb; 5] = [
    Rgb::new(255, 214, 0),
    Rgb::new(130, 214, 82),
    Rgb::new(90, 176, 255),
    Rgb::new(255, 130, 176),
    Rgb::new(255, 154, 60),
];

pub const INK_COLORS: [Rgb; 4] = [
    Rgb::new(24, 24, 24),
    Rgb::new(186, 36, 36),
    Rgb::new(28, 78, 186),
    Rgb::new(22, 122, 58),
];

/// 100% zoom: one CSS pixel at 96 dpi per PDF point.
pub const ZOOM_100: f32 = 96.0 / 72.0;
pub const MIN_SCALE: f32 = 0.2;
pub const MAX_SCALE: f32 = 10.0;

pub fn zoom_percent(scale: f32) -> u32 {
    (scale / ZOOM_100 * 100.0).round() as u32
}

/// Quantize a pixels-per-point scale so nearby zooms share a tile cache.
pub fn zoom_bucket(scale: f32) -> f32 {
    let stepped = (scale.log2() * 4.0).round() / 4.0;
    2f32.powf(stepped).clamp(MIN_SCALE, MAX_SCALE)
}

/// Keep the document point under `cursor_offset` fixed while the scale changes.
pub fn zoom_scroll(old_scale: f32, new_scale: f32, scroll: f32, cursor_offset: f32) -> f32 {
    if old_scale <= f32::EPSILON {
        return scroll;
    }
    let anchor = (scroll + cursor_offset) / old_scale;
    anchor * new_scale - cursor_offset
}

pub fn dist_to_segment(p: PdfPoint, a: PdfPoint, b: PdfPoint) -> f32 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len2 = dx * dx + dy * dy;
    if len2 < 1e-6 {
        return (p.x - a.x).hypot(p.y - a.y);
    }
    let t = ((p.x - a.x) * dx + (p.y - a.y) * dy) / len2;
    let t = t.clamp(0.0, 1.0);
    let x = a.x + t * dx;
    let y = a.y + t * dy;
    (p.x - x).hypot(p.y - y)
}

/// Strip a markdown or TeX math wrapper, leaving the formula source.
pub fn normalize_latex(input: &str) -> String {
    let mut s = input.trim();
    if let Some(inner) = s
        .strip_prefix("$$")
        .and_then(|rest| rest.strip_suffix("$$"))
    {
        s = inner.trim();
    } else if let Some(inner) = s.strip_prefix('$').and_then(|rest| rest.strip_suffix('$')) {
        s = inner.trim();
    } else if let Some(inner) = s
        .strip_prefix("\\[")
        .and_then(|rest| rest.strip_suffix("\\]"))
    {
        s = inner.trim();
    } else if let Some(inner) = s
        .strip_prefix("\\(")
        .and_then(|rest| rest.strip_suffix("\\)"))
    {
        s = inner.trim();
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_keeps_the_point_under_the_cursor() {
        let scroll = zoom_scroll(1.0, 2.0, 100.0, 40.0);
        let anchor_before = (100.0 + 40.0) / 1.0;
        let anchor_after = (scroll + 40.0) / 2.0;
        assert!((anchor_before - anchor_after).abs() < 1e-3);
    }

    #[test]
    fn latex_wrappers_are_stripped() {
        assert_eq!(normalize_latex("$$ \\frac{1}{2} $$"), "\\frac{1}{2}");
        assert_eq!(normalize_latex("$x^2$"), "x^2");
        assert_eq!(normalize_latex("\\[a+b\\]"), "a+b");
        assert_eq!(normalize_latex("  \\alpha  "), "\\alpha");
    }

    #[test]
    fn buckets_are_stable_nearby() {
        let a = zoom_bucket(1.30);
        let b = zoom_bucket(1.32);
        assert_eq!(a, b);
    }

    #[test]
    fn center_is_midpoint() {
        let r = PdfRect::new(10.0, 20.0, 30.0, 40.0);
        assert_eq!(r.center(), PdfPoint::new(20.0, 30.0));
    }
}
