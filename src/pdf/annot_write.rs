use mupdf::pdf::{
    AnnotationFlags, AnnotationTextAlign, LineEndingStyle, PdfAnnotation, PdfAnnotationType,
    PdfDocument, PdfPage,
};
use mupdf::Quad;

use crate::annot::{AnnotKind, Annotation, ShapeKind, HIGHLIGHT_OPACITY};
use crate::geom::{PdfPoint, PdfRect, Rgb};

use super::annot_read::{pdf_markup_type, read_marker, MarkerKind};
use super::appearance::{
    install_image_appearance, install_math_appearance, pixel_hash, write_image_marker,
};
use super::convert::{rect_quad, rgb_color, to_point, to_rect};
use super::engine::{Error, InlineMathSave};

pub(crate) fn write_marker(
    doc: &PdfDocument,
    annot: &PdfAnnotation,
    kind: MarkerKind,
    id: u64,
    text_size: Option<f32>,
    text_color: Option<Rgb>,
    source: Option<&str>,
) -> Result<(), Error> {
    write_marker_full(doc, annot, kind, id, None, text_size, text_color, source)
}

// RF7 packs marker metadata into a struct.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_marker_full(
    doc: &PdfDocument,
    annot: &PdfAnnotation,
    kind: MarkerKind,
    id: u64,
    parent_id: Option<u64>,
    text_size: Option<f32>,
    text_color: Option<Rgb>,
    source: Option<&str>,
) -> Result<(), Error> {
    let id_i32 = i32::try_from(id).map_err(|_| Error::from("annotation id does not fit in i32"))?;
    let mut marker = doc.new_dict()?;
    marker.dict_put("Kind", doc.new_name(kind.as_str())?)?;
    marker.dict_put("Id", doc.new_int(id_i32)?)?;
    if let Some(parent_id) = parent_id {
        let parent_i32 = i32::try_from(parent_id)
            .map_err(|_| Error::from("annotation id does not fit in i32"))?;
        marker.dict_put("ParentId", doc.new_int(parent_i32)?)?;
    }
    if let Some(size) = text_size {
        marker.dict_put("TextSize", doc.new_real(size)?)?;
    }
    if let Some(color) = text_color {
        let [r, g, b] = color.to_unit();
        let mut array = doc.new_array()?;
        array.array_push(doc.new_real(r)?)?;
        array.array_push(doc.new_real(g)?)?;
        array.array_push(doc.new_real(b)?)?;
        marker.dict_put("TextColor", array)?;
    }
    if let Some(source) = source {
        marker.dict_put("Source", doc.new_string(source)?)?;
    }
    let mut object = annot.object();
    object.dict_put("Marker", marker)?;
    object.dict_put("NM", doc.new_string(&format!("marker-{id}"))?)?;
    Ok(())
}

pub(crate) fn upsert(
    doc: &mut PdfDocument,
    page: &mut PdfPage,
    annot: &Annotation,
    math_pdf: Option<&[u8]>,
) -> Result<i32, Error> {
    if let Some(xref) = annot.xref {
        if let Some(mut existing) = find_annot(page, xref) {
            if apply_existing(doc, &mut existing, annot, math_pdf).is_ok() {
                return Ok(xref);
            }
            delete_xref(page, xref)?;
        }
    }
    create_annot(doc, page, annot, math_pdf)
}

pub(crate) fn find_annot(page: &PdfPage, xref: i32) -> Option<PdfAnnotation> {
    page.annotations()
        .find(|annot| annot.xref().ok() == Some(xref))
}

pub(crate) fn delete_xref(page: &mut PdfPage, xref: i32) -> Result<(), mupdf::Error> {
    if let Some(annot) = find_annot(page, xref) {
        page.delete_annotation(annot)?;
    }
    Ok(())
}

pub(crate) fn apply_existing(
    doc: &mut PdfDocument,
    annot: &mut PdfAnnotation,
    source: &Annotation,
    math_pdf: Option<&[u8]>,
) -> Result<(), Error> {
    annot.set_flags(AnnotationFlags::IS_PRINT)?;
    match &source.kind {
        AnnotKind::Highlight { quads, color } => {
            if annot.r#type()? != PdfAnnotationType::Highlight {
                return Err(mupdf::Error::InvalidArgument("type changed".into()).into());
            }
            let pdf_quads: Vec<Quad> = quads.iter().copied().map(rect_quad).collect();
            if pdf_quads.is_empty() {
                return Err(mupdf::Error::InvalidArgument("empty highlight".into()).into());
            }
            annot.set_quad_points(pdf_quads)?;
            annot.set_color(rgb_color(*color))?;
            annot.set_opacity(HIGHLIGHT_OPACITY)?;
            if let Some(bounds) = source.kind.bounds() {
                annot.set_rect(to_rect(bounds))?;
            }
            write_marker(
                doc,
                annot,
                MarkerKind::Highlight,
                source.id,
                None,
                Some(*color),
                None,
            )?;
            annot.update()?;
        }
        AnnotKind::Markup {
            style,
            quads,
            color,
        } => {
            if annot.r#type()? != pdf_markup_type(*style) {
                return Err(mupdf::Error::InvalidArgument("type changed".into()).into());
            }
            write_markup_quads(annot, quads, *color)?;
            write_marker(
                doc,
                annot,
                style
                    .marker_name()
                    .parse()
                    .expect("markup style has a MarkerKind"),
                source.id,
                None,
                Some(*color),
                None,
            )?;
            annot.update()?;
        }
        AnnotKind::Text {
            rect,
            content,
            size,
            color,
        } => {
            if annot.r#type()? != PdfAnnotationType::FreeText {
                return Err(mupdf::Error::InvalidArgument("type changed".into()).into());
            }
            let rich = crate::math_spans::has_math(content);
            let visible = if rich {
                crate::math_spans::prose_only(content)
            } else {
                content.clone()
            };
            annot.set_rect(to_rect(*rect))?;
            annot.set_contents(&visible)?;
            annot.set_default_appearance("Helv", *size, Some(rgb_color(*color)))?;
            annot.set_quadding(AnnotationTextAlign::Left)?;
            write_marker(
                doc,
                annot,
                MarkerKind::Text,
                source.id,
                Some(*size),
                Some(*color),
                rich.then_some(content.as_str()),
            )?;
            annot.update()?;
        }
        AnnotKind::Note {
            rect,
            content,
            color,
        } => {
            if annot.r#type()? != PdfAnnotationType::Text {
                return Err(mupdf::Error::InvalidArgument("type changed".into()).into());
            }
            annot.set_rect(to_rect(*rect))?;
            annot.set_contents(content)?;
            annot.set_color(rgb_color(*color))?;
            write_marker(
                doc,
                annot,
                MarkerKind::Note,
                source.id,
                None,
                Some(*color),
                None,
            )?;
            annot.update()?;
        }
        AnnotKind::Shape {
            kind,
            rect,
            start,
            end,
            stroke,
            fill,
            width,
        } => {
            apply_shape(annot, *kind, *rect, *start, *end, *stroke, *fill, *width)?;
            write_marker(
                doc,
                annot,
                MarkerKind::Shape,
                source.id,
                None,
                Some(*stroke),
                None,
            )?;
            annot.update()?;
        }
        AnnotKind::Ink {
            strokes,
            color,
            width,
        } => {
            apply_ink(annot, strokes, *color, *width)?;
            write_marker(
                doc,
                annot,
                MarkerKind::Ink,
                source.id,
                None,
                Some(*color),
                None,
            )?;
            annot.update()?;
        }
        AnnotKind::Math {
            rect,
            source: latex,
            size,
            color,
            ..
        } => {
            if annot.r#type()? != PdfAnnotationType::Stamp {
                return Err(mupdf::Error::InvalidArgument("type changed".into()).into());
            }
            annot.set_rect(to_rect(*rect))?;
            annot.set_contents(latex)?;
            write_marker(
                doc,
                annot,
                MarkerKind::Math,
                source.id,
                Some(*size),
                Some(*color),
                Some(latex),
            )?;
            if let Some(bytes) = math_pdf {
                install_math_appearance(doc, annot, bytes)?;
            }
        }
        AnnotKind::Image {
            rect,
            rgba,
            width,
            height,
        } => {
            if annot.r#type()? != PdfAnnotationType::Stamp {
                return Err(mupdf::Error::InvalidArgument("type changed".into()).into());
            }
            annot.set_rect(to_rect(*rect))?;
            // Same pixels: keep the existing image stream and appearance.
            // A new Arc after reload still matches via the stored hash.
            let hash = pixel_hash(rgba);
            if read_marker(&annot.object()).pixel_hash == Some(hash) {
                annot.update()?;
                return Ok(());
            }
            write_image_marker(doc, annot, source.id, rgba, *width, *height)?;
            install_image_appearance(doc, annot, rgba, *width, *height)?;
        }
        // Leave the original object untouched. Rewriting it would drop the other app's annot.
        AnnotKind::Foreign { .. } => {}
    }
    Ok(())
}

pub(crate) fn write_markup_quads(
    annot: &mut PdfAnnotation,
    quads: &[PdfRect],
    color: Rgb,
) -> Result<(), mupdf::Error> {
    let pdf_quads: Vec<Quad> = quads.iter().copied().map(rect_quad).collect();
    if pdf_quads.is_empty() {
        return Err(mupdf::Error::InvalidArgument("empty markup".into()));
    }
    // Text markup annots have no Rect property; MuPDF derives it from the quads.
    annot.set_quad_points(pdf_quads)?;
    annot.set_color(rgb_color(color))?;
    annot.set_opacity(1.0)?;
    Ok(())
}

// RF7 packs shape geometry/style into a struct.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_shape(
    annot: &mut PdfAnnotation,
    kind: ShapeKind,
    rect: PdfRect,
    start: PdfPoint,
    end: PdfPoint,
    stroke: Rgb,
    fill: Option<Rgb>,
    width: f32,
) -> Result<(), mupdf::Error> {
    let expected = match kind {
        ShapeKind::Rect => PdfAnnotationType::Square,
        ShapeKind::Ellipse => PdfAnnotationType::Circle,
        ShapeKind::Line | ShapeKind::Arrow => PdfAnnotationType::Line,
    };
    if annot.r#type()? != expected {
        return Err(mupdf::Error::InvalidArgument("type changed".into()));
    }
    annot.set_color(rgb_color(stroke))?;
    annot.set_border_width(width.max(0.25))?;
    if let Some(fill) = fill {
        annot.set_interior_color(rgb_color(fill))?;
    } else {
        // Drop /IC so toggling fill off does not leave a stale interior color.
        let _ = annot.object().dict_delete("IC");
    }
    match kind {
        ShapeKind::Line | ShapeKind::Arrow => {
            annot.set_line(to_point(start), to_point(end))?;
            let end_style = if kind == ShapeKind::Arrow {
                LineEndingStyle::ClosedArrow
            } else {
                LineEndingStyle::None
            };
            annot.set_line_ending_styles(LineEndingStyle::None, end_style)?;
        }
        ShapeKind::Rect | ShapeKind::Ellipse => annot.set_rect(to_rect(rect))?,
    }
    Ok(())
}

pub(crate) fn apply_ink(
    annot: &mut PdfAnnotation,
    strokes: &[Vec<PdfPoint>],
    color: Rgb,
    width: f32,
) -> Result<(), mupdf::Error> {
    if annot.r#type()? != PdfAnnotationType::Ink {
        return Err(mupdf::Error::InvalidArgument("type changed".into()));
    }
    if strokes.is_empty() || strokes.iter().all(|stroke| stroke.is_empty()) {
        return Err(mupdf::Error::InvalidArgument("empty ink".into()));
    }
    annot.set_ink_list(
        strokes
            .iter()
            .map(|stroke| stroke.iter().copied().map(to_point)),
    )?;
    annot.set_color(rgb_color(color))?;
    annot.set_border_width(width.max(0.25))?;
    // Ink annots have no Rect property; MuPDF derives bounds from the ink list.
    Ok(())
}

pub(crate) fn create_annot(
    doc: &mut PdfDocument,
    page: &mut PdfPage,
    source: &Annotation,
    math_pdf: Option<&[u8]>,
) -> Result<i32, Error> {
    let mut annot = match &source.kind {
        AnnotKind::Highlight { quads, color } => {
            let pdf_quads: Vec<Quad> = quads.iter().copied().map(rect_quad).collect();
            if pdf_quads.is_empty() {
                return Err("Highlight is empty.".into());
            }
            let mut annot = page.add_highlight_annotation(pdf_quads)?;
            annot.set_color(rgb_color(*color))?;
            annot.set_opacity(HIGHLIGHT_OPACITY)?;
            write_marker(
                doc,
                &annot,
                MarkerKind::Highlight,
                source.id,
                None,
                Some(*color),
                None,
            )?;
            annot
        }
        AnnotKind::Markup {
            style,
            quads,
            color,
        } => {
            let pdf_quads: Vec<Quad> = quads.iter().copied().map(rect_quad).collect();
            if pdf_quads.is_empty() {
                return Err("Markup is empty.".into());
            }
            let mut annot = page.create_annotation(pdf_markup_type(*style))?;
            annot.set_quad_points(pdf_quads)?;
            annot.set_color(rgb_color(*color))?;
            annot.set_opacity(1.0)?;
            write_marker(
                doc,
                &annot,
                style
                    .marker_name()
                    .parse()
                    .expect("markup style has a MarkerKind"),
                source.id,
                None,
                Some(*color),
                None,
            )?;
            annot
        }
        AnnotKind::Text {
            rect,
            content,
            size,
            color,
        } => {
            let rich = crate::math_spans::has_math(content);
            let visible = if rich {
                crate::math_spans::prose_only(content)
            } else {
                content.clone()
            };
            let mut annot = page.add_free_text_annotation(to_rect(*rect), &visible)?;
            annot.set_default_appearance("Helv", *size, Some(rgb_color(*color)))?;
            annot.set_quadding(AnnotationTextAlign::Left)?;
            write_marker(
                doc,
                &annot,
                MarkerKind::Text,
                source.id,
                Some(*size),
                Some(*color),
                rich.then_some(content.as_str()),
            )?;
            annot
        }
        AnnotKind::Note {
            rect,
            content,
            color,
        } => {
            let mut annot = page.add_text_annotation(to_rect(*rect), content)?;
            annot.set_color(rgb_color(*color))?;
            annot.set_icon_name("Comment")?;
            write_marker(
                doc,
                &annot,
                MarkerKind::Note,
                source.id,
                None,
                Some(*color),
                None,
            )?;
            annot
        }
        AnnotKind::Shape {
            kind,
            rect,
            start,
            end,
            stroke,
            fill,
            width,
        } => {
            let mut annot = match kind {
                ShapeKind::Rect => page.add_square_annotation(to_rect(*rect))?,
                ShapeKind::Ellipse => page.add_circle_annotation(to_rect(*rect))?,
                ShapeKind::Line | ShapeKind::Arrow => {
                    page.add_line_annotation(to_point(*start), to_point(*end))?
                }
            };
            annot.set_color(rgb_color(*stroke))?;
            annot.set_border_width(width.max(0.25))?;
            if let Some(fill) = fill {
                annot.set_interior_color(rgb_color(*fill))?;
            }
            if kind.is_line_like() {
                let end_style = if *kind == ShapeKind::Arrow {
                    LineEndingStyle::ClosedArrow
                } else {
                    LineEndingStyle::None
                };
                annot.set_line_ending_styles(LineEndingStyle::None, end_style)?;
            }
            write_marker(
                doc,
                &annot,
                MarkerKind::Shape,
                source.id,
                None,
                Some(*stroke),
                None,
            )?;
            annot
        }
        AnnotKind::Ink {
            strokes,
            color,
            width,
        } => {
            if strokes.is_empty() || strokes.iter().all(|stroke| stroke.is_empty()) {
                return Err("Ink stroke is empty.".into());
            }
            let mut annot = page.add_ink_annotation(
                strokes
                    .iter()
                    .map(|stroke| stroke.iter().copied().map(to_point)),
            )?;
            annot.set_color(rgb_color(*color))?;
            annot.set_border_width(width.max(0.25))?;
            // Ink annots have no Rect property; MuPDF derives bounds from the ink list.
            write_marker(
                doc,
                &annot,
                MarkerKind::Ink,
                source.id,
                None,
                Some(*color),
                None,
            )?;
            annot
        }
        AnnotKind::Math {
            rect,
            source: latex,
            size,
            color,
            ..
        } => {
            let mut annot = page.create_annotation(PdfAnnotationType::Stamp)?;
            annot.set_rect(to_rect(*rect))?;
            annot.set_contents(latex)?;
            write_marker(
                doc,
                &annot,
                MarkerKind::Math,
                source.id,
                Some(*size),
                Some(*color),
                Some(latex),
            )?;
            if let Some(bytes) = math_pdf {
                install_math_appearance(doc, &mut annot, bytes)?;
            }
            annot.set_flags(AnnotationFlags::IS_PRINT)?;
            return Ok(annot.xref()?);
        }
        AnnotKind::Image {
            rect,
            rgba,
            width,
            height,
        } => {
            let mut annot = page.create_annotation(PdfAnnotationType::Stamp)?;
            annot.set_rect(to_rect(*rect))?;
            write_image_marker(doc, &annot, source.id, rgba, *width, *height)?;
            install_image_appearance(doc, &mut annot, rgba, *width, *height)?;
            annot.set_flags(AnnotationFlags::IS_PRINT)?;
            return Ok(annot.xref()?);
        }
        AnnotKind::Foreign { .. } => {
            return Err("Foreign annotations are read-only.".into());
        }
    };
    annot.set_flags(AnnotationFlags::IS_PRINT)?;
    annot.update()?;
    Ok(annot.xref()?)
}

pub(crate) fn delete_inline_math_children(
    page: &mut PdfPage,
    parent_id: u64,
) -> Result<(), mupdf::Error> {
    let doomed: Vec<i32> = page
        .annotations()
        .filter_map(|annot| {
            let marker = read_marker(&annot.object());
            if marker.kind == Some(MarkerKind::InlineMath) && marker.parent_id == Some(parent_id) {
                annot.xref().ok()
            } else {
                None
            }
        })
        .collect();
    for xref in doomed {
        delete_xref(page, xref)?;
    }
    Ok(())
}

pub(crate) fn create_inline_math(
    doc: &mut PdfDocument,
    page: &mut PdfPage,
    inline: &InlineMathSave,
) -> Result<(), Error> {
    // Synthetic id: keep ParentId for reload skip; Id just needs to be unique-ish.
    let synth_id = inline
        .parent_id
        .saturating_mul(1_000_003)
        .wrapping_add(u64::from(inline.index) + 1);
    let mut annot = page.create_annotation(PdfAnnotationType::Stamp)?;
    annot.set_rect(to_rect(inline.rect))?;
    annot.set_contents(&inline.source)?;
    write_marker_full(
        doc,
        &annot,
        MarkerKind::InlineMath,
        synth_id,
        Some(inline.parent_id),
        Some(inline.size),
        Some(inline.color),
        Some(&inline.source),
    )?;
    if let Some(bytes) = inline.pdf.as_deref() {
        install_math_appearance(doc, &mut annot, bytes)?;
    }
    annot.set_flags(AnnotationFlags::IS_PRINT)?;
    Ok(())
}
