use crate::geom::PdfRect;
use crate::pdf::PageInfo;

pub const CROP_DPI: f32 = 144.0;
pub const CROP_MAX_EDGE_PX: f32 = 2048.0;
pub const CROP_MAX_PNG_BYTES: usize = 5 * 1024 * 1024;

/// Clamp a PDF-space crop rectangle to page bounds.
pub fn clamp_crop_rect(rect: PdfRect, page: PageInfo) -> PdfRect {
    let page_rect = PdfRect::new(page.x0, page.y0, page.x1, page.y1);
    PdfRect::new(
        rect.x0.max(page_rect.x0).min(page_rect.x1),
        rect.y0.max(page_rect.y0).min(page_rect.y1),
        rect.x1.max(page_rect.x0).min(page_rect.x1),
        rect.y1.max(page_rect.y0).min(page_rect.y1),
    )
}

/// Scale (device pixels per PDF point) for a crop at the target DPI, capped so
/// the longest rendered edge stays within `CROP_MAX_EDGE_PX`.
pub fn crop_scale(rect: PdfRect, dpi: f32) -> f32 {
    let base = (dpi / 72.0).max(0.05);
    let w = rect.width().max(0.5) * base;
    let h = rect.height().max(0.5) * base;
    let longest = w.max(h);
    if longest > CROP_MAX_EDGE_PX {
        base * (CROP_MAX_EDGE_PX / longest)
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::PageInfo;

    #[test]
    fn clamp_to_page() {
        let page = PageInfo {
            x0: 0.0,
            y0: 0.0,
            x1: 100.0,
            y1: 200.0,
        };
        let r = clamp_crop_rect(PdfRect::new(-10.0, 50.0, 150.0, 250.0), page);
        assert_eq!(r, PdfRect::new(0.0, 50.0, 100.0, 200.0));
    }

    #[test]
    fn scale_caps_long_edge() {
        let huge = PdfRect::new(0.0, 0.0, 2000.0, 100.0);
        let scale = crop_scale(huge, CROP_DPI);
        let px = huge.width() * scale;
        assert!(px <= CROP_MAX_EDGE_PX + 0.5);
    }
}
