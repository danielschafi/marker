use egui::Color32;
use mupdf::Pixmap;

use super::engine::{PageInfo, TILE_PX};

pub(crate) fn tile_device_rect(
    info: PageInfo,
    scale: f32,
    col: i32,
    row: i32,
) -> (i32, i32, i32, i32) {
    let x0 = (info.x0 * scale).floor() as i32;
    let y0 = (info.y0 * scale).floor() as i32;
    let x1 = (info.x1 * scale).ceil() as i32;
    let y1 = (info.y1 * scale).ceil() as i32;
    let tx0 = x0 + col * TILE_PX;
    let ty0 = y0 + row * TILE_PX;
    let tx1 = (tx0 + TILE_PX).min(x1);
    let ty1 = (ty0 + TILE_PX).min(y1);
    (tx0, ty0, tx1, ty1)
}

pub(crate) fn rgba_from_pixmap(pixmap: &Pixmap) -> Vec<u8> {
    let width = pixmap.width() as usize;
    let height = pixmap.height() as usize;
    let components = pixmap.n() as usize;
    let stride = pixmap.stride() as usize;
    let samples = pixmap.samples();
    let mut out = vec![255u8; width * height * 4];
    for y in 0..height {
        let row = &samples[y * stride..];
        for x in 0..width {
            let source = x * components;
            let dest = (y * width + x) * 4;
            if components >= 3 {
                let (r, g, b, a) = knock_out_paper(row[source], row[source + 1], row[source + 2]);
                out[dest] = r;
                out[dest + 1] = g;
                out[dest + 2] = b;
                out[dest + 3] = a;
            }
        }
    }
    out
}

pub(crate) fn premultiplied_tile_pixels_from_pixmap(pixmap: &Pixmap) -> Vec<u8> {
    let width = pixmap.width() as usize;
    let height = pixmap.height() as usize;
    let components = pixmap.n() as usize;
    let stride = pixmap.stride() as usize;
    let samples = pixmap.samples();
    let mut out = vec![0u8; width * height * 4];
    for y in 0..height {
        let row = &samples[y * stride..];
        for x in 0..width {
            let source = x * components;
            let dest = (y * width + x) * 4;
            if components >= 3 {
                let r = row[source];
                let g = row[source + 1];
                let b = row[source + 2];
                let min_c = r.min(g).min(b);
                if min_c >= 253 {
                    continue;
                }
                let premul = knock_out_to_premultiplied(r, g, b);
                out[dest..dest + 4].copy_from_slice(&premul);
            }
        }
    }
    out
}

pub(crate) fn knock_out_to_premultiplied(r: u8, g: u8, b: u8) -> [u8; 4] {
    let (ur, ug, ub, ua) = knock_out_paper(r, g, b);
    let c = Color32::from_rgba_unmultiplied(ur, ug, ub, ua);
    [c.r(), c.g(), c.b(), c.a()]
}

/// Convert white-backed page pixels to ink-on-transparent.
///
/// Lets highlight fills underpaint glyphs without a Multiply blend mode. Achromatic
/// anti-aliased edges are unpremultiplied against white so gray AA becomes dark ink
/// at partial alpha (clean over marker tint) instead of an opaque silver rim.
///
/// Transparent paper keeps RGB white so LINEAR texture filtering does not pull a
/// dark fringe from `(0,0,0,0)` neighbors into glyph edges.
pub(crate) fn knock_out_paper(r: u8, g: u8, b: u8) -> (u8, u8, u8, u8) {
    let max_c = r.max(g).max(b);
    let min_c = r.min(g).min(b);
    let chroma = max_c as u16 - min_c as u16;

    // Pure / near-white paper (white RGB, alpha 0 — important for LINEAR filtering).
    if min_c >= 253 {
        return (255, 255, 255, 0);
    }

    if chroma <= 16 {
        // Grayscale ink on white → recover coverage for clean underpaint.
        let a = 255u16 - min_c as u16;
        if a == 0 {
            return (255, 255, 255, 0);
        }
        if a == 255 {
            return (r, g, b, 255);
        }
        return (
            unpremul_against_white(r, a),
            unpremul_against_white(g, a),
            unpremul_against_white(b, a),
            a as u8,
        );
    }

    // Chromatic content (images / colored ink): keep the body opaque; only soft-knock
    // near-white fringes so photos are not rewritten into dark translucent mush.
    let dist = 255u16 - min_c as u16;
    const RAMP: u16 = 12;
    if dist >= RAMP {
        (r, g, b, 255)
    } else if dist == 0 {
        (255, 255, 255, 0)
    } else {
        let a = ((dist * 255) / RAMP).max(1);
        (
            unpremul_against_white(r, a),
            unpremul_against_white(g, a),
            unpremul_against_white(b, a),
            a as u8,
        )
    }
}

pub(crate) fn unpremul_against_white(c: u8, a: u16) -> u8 {
    debug_assert!(a > 0);
    let v = 255u32.saturating_sub(((255u32 - c as u32) * 255) / a as u32);
    v.min(255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annot::HIGHLIGHT_OPACITY;
    use crate::geom::Rgb;

    #[test]
    fn knock_out_paper_white_is_transparent() {
        assert_eq!(knock_out_paper(255, 255, 255), (255, 255, 255, 0));
        assert_eq!(knock_out_paper(254, 254, 253), (255, 255, 255, 0));
    }

    #[test]
    fn knock_out_paper_black_stays_opaque() {
        assert_eq!(knock_out_paper(0, 0, 0), (0, 0, 0, 255));
    }

    #[test]
    fn knock_out_paper_gray_aa_becomes_dark_partial_alpha() {
        let (r, g, b, a) = knock_out_paper(128, 128, 128);
        assert!(a > 100 && a < 140, "coverage alpha {a}");
        assert!(
            r < 16 && g < 16 && b < 16,
            "ink should be near-black, got {r},{g},{b}"
        );
    }

    #[test]
    fn knock_out_paper_chromatic_midtone_stays_opaque() {
        assert_eq!(knock_out_paper(100, 150, 180), (100, 150, 180, 255));
    }

    #[test]
    fn knock_out_to_premultiplied_matches_egui_unmultiplied_path() {
        let samples = [
            (255, 255, 255),
            (254, 254, 253),
            (0, 0, 0),
            (128, 128, 128),
            (100, 150, 180),
            (250, 250, 250),
            (255, 200, 100),
            (240, 241, 242),
        ];
        for (r, g, b) in samples {
            let (ur, ug, ub, ua) = knock_out_paper(r, g, b);
            let expected = Color32::from_rgba_unmultiplied(ur, ug, ub, ua);
            let got = knock_out_to_premultiplied(r, g, b);
            assert_eq!(
                [expected.r(), expected.g(), expected.b(), expected.a()],
                got,
                "rgb=({r},{g},{b})"
            );
        }
    }

    /// Simulates on-screen underpaint: marker tint on white, then knocked-out ink on top.
    fn underpaint_composite(paper_rgb: (u8, u8, u8), marker: Rgb) -> (u8, u8, u8) {
        let (ink_r, ink_g, ink_b, ink_a) = knock_out_paper(paper_rgb.0, paper_rgb.1, paper_rgb.2);
        let ma = HIGHLIGHT_OPACITY.clamp(0.0, 1.0);
        // Marker over white paper.
        let base_r = marker.r as f32 * ma + 255.0 * (1.0 - ma);
        let base_g = marker.g as f32 * ma + 255.0 * (1.0 - ma);
        let base_b = marker.b as f32 * ma + 255.0 * (1.0 - ma);
        let ia = ink_a as f32 / 255.0;
        let out_r = ink_r as f32 * ia + base_r * (1.0 - ia);
        let out_g = ink_g as f32 * ia + base_g * (1.0 - ia);
        let out_b = ink_b as f32 * ia + base_b * (1.0 - ia);
        (
            out_r.round() as u8,
            out_g.round() as u8,
            out_b.round() as u8,
        )
    }

    #[test]
    fn underpaint_keeps_black_ink_dark_and_marker_visible() {
        let yellow = Rgb::new(255, 214, 0);
        let on_ink = underpaint_composite((0, 0, 0), yellow);
        let luminance =
            0.2126 * on_ink.0 as f32 + 0.7152 * on_ink.1 as f32 + 0.0722 * on_ink.2 as f32;
        assert!(
            luminance < 40.0,
            "black ink over marker should stay dark, got {on_ink:?} lum={luminance}"
        );

        let on_paper = underpaint_composite((255, 255, 255), yellow);
        assert!(
            on_paper.0 > 220 && on_paper.1 > 200 && on_paper.2 < on_paper.1,
            "paper underpaint should read as yellow marker tint, got {on_paper:?}"
        );
    }
}
