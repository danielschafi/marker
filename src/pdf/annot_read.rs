use std::collections::HashSet;

use mupdf::pdf::{
    AnnotationDefaultAppearance, LineEndingStyle, PdfAnnotation, PdfAnnotationType, PdfDocument,
    PdfObject,
};

use crate::annot::{AnnotKind, Annotation, ForeignKind, MarkupStyle, ShapeKind};
use crate::geom::{PdfPoint, PdfRect, Rgb};

use super::convert::{color_rgb, from_rect, quad_bounds};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MarkerKind {
    Highlight,
    Underline,
    StrikeOut,
    Squiggly,
    Text,
    Note,
    Shape,
    Ink,
    Math,
    Image,
    InlineMath,
}

impl MarkerKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Highlight => "Highlight",
            Self::Underline => "Underline",
            Self::StrikeOut => "StrikeOut",
            Self::Squiggly => "Squiggly",
            Self::Text => "Text",
            Self::Note => "Note",
            Self::Shape => "Shape",
            Self::Ink => "Ink",
            Self::Math => "Math",
            Self::Image => "Image",
            Self::InlineMath => "InlineMath",
        }
    }
}

impl std::fmt::Display for MarkerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for MarkerKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "Highlight" => Self::Highlight,
            "Underline" => Self::Underline,
            "StrikeOut" => Self::StrikeOut,
            "Squiggly" => Self::Squiggly,
            "Text" => Self::Text,
            "Note" => Self::Note,
            "Shape" => Self::Shape,
            "Ink" => Self::Ink,
            "Math" => Self::Math,
            "Image" => Self::Image,
            "InlineMath" => Self::InlineMath,
            _ => return Err(()),
        })
    }
}

#[derive(Default)]
pub(crate) struct MarkerMeta {
    pub(crate) kind: Option<MarkerKind>,
    pub(crate) id: Option<u64>,
    pub(crate) parent_id: Option<u64>,
    pub(crate) text_size: Option<f32>,
    pub(crate) text_color: Option<Rgb>,
    pub(crate) source: Option<String>,
    pub(crate) png: Option<Vec<u8>>,
    /// Hash of the image's raw pixels. Absent on files written before reuse.
    pub(crate) pixel_hash: Option<u64>,
}

pub(crate) fn read_marker(object: &PdfObject) -> MarkerMeta {
    let Ok(Some(marker)) = object.get_dict("Marker") else {
        return MarkerMeta {
            kind: None,
            id: None,
            parent_id: None,
            text_size: None,
            text_color: None,
            source: None,
            png: None,
            pixel_hash: None,
        };
    };
    let kind = marker
        .get_dict("Kind")
        .ok()
        .flatten()
        .and_then(|name| name.as_name().ok())
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|name| name.parse().ok());
    let id = marker
        .get_dict("Id")
        .ok()
        .flatten()
        .and_then(|value| value.as_int().ok())
        .and_then(|value| u64::try_from(value).ok());
    let parent_id = marker
        .get_dict("ParentId")
        .ok()
        .flatten()
        .and_then(|value| value.as_int().ok())
        .and_then(|value| u64::try_from(value).ok());
    let text_size = marker
        .get_dict("TextSize")
        .ok()
        .flatten()
        .and_then(|value| value.as_float().ok())
        .filter(|size| size.is_finite() && *size > 0.0);
    let text_color = marker
        .get_dict("TextColor")
        .ok()
        .flatten()
        .and_then(|array| {
            let r = array.get_array(0).ok().flatten()?.as_float().ok()?;
            let g = array.get_array(1).ok().flatten()?.as_float().ok()?;
            let b = array.get_array(2).ok().flatten()?.as_float().ok()?;
            Some(Rgb::from_unit([r, g, b]))
        });
    let source = marker
        .get_dict("Source")
        .ok()
        .flatten()
        .and_then(|value| value.as_string().ok());
    // Read the stream on the dictionary value itself. Resolving an indirect
    // stream first yields a plain dict in MuPDF, and read_stream then fails
    // with "object is not a stream" — which dropped pasted images on reload.
    let png = marker
        .get_dict("Png")
        .ok()
        .flatten()
        .and_then(|obj| obj.read_stream().ok());
    let pixel_hash = marker
        .get_dict("PixelHash")
        .ok()
        .flatten()
        .and_then(|value| value.as_string().ok())
        .and_then(|text| u64::from_str_radix(text.trim(), 16).ok());
    MarkerMeta {
        kind,
        id,
        parent_id,
        text_size,
        text_color,
        source,
        png,
        pixel_hash,
    }
}

pub(crate) fn read_nm_id(object: &PdfObject) -> Option<u64> {
    let name = object.get_dict("NM").ok().flatten()?.as_string().ok()?;
    name.strip_prefix("marker-")?.parse().ok()
}

pub(crate) fn is_native_markup(kind_name: PdfAnnotationType, annot: &PdfAnnotation) -> bool {
    let expected = match kind_name {
        PdfAnnotationType::Underline => MarkerKind::Underline,
        PdfAnnotationType::StrikeOut => MarkerKind::StrikeOut,
        PdfAnnotationType::Squiggly => MarkerKind::Squiggly,
        _ => return false,
    };
    read_marker(&annot.object()).kind == Some(expected)
}

pub(crate) fn is_native_ink(kind_name: PdfAnnotationType, annot: &PdfAnnotation) -> bool {
    kind_name == PdfAnnotationType::Ink
        && read_marker(&annot.object()).kind == Some(MarkerKind::Ink)
}

pub(crate) fn markup_style_of(kind_name: PdfAnnotationType) -> Option<MarkupStyle> {
    match kind_name {
        PdfAnnotationType::Underline => Some(MarkupStyle::Underline),
        PdfAnnotationType::StrikeOut => Some(MarkupStyle::StrikeOut),
        PdfAnnotationType::Squiggly => Some(MarkupStyle::Squiggly),
        _ => None,
    }
}

pub(crate) fn pdf_markup_type(style: MarkupStyle) -> PdfAnnotationType {
    match style {
        MarkupStyle::Underline => PdfAnnotationType::Underline,
        MarkupStyle::StrikeOut => PdfAnnotationType::StrikeOut,
        MarkupStyle::Squiggly => PdfAnnotationType::Squiggly,
    }
}

pub(crate) fn import_foreign(
    kind_name: PdfAnnotationType,
    annot: &PdfAnnotation,
) -> Result<Option<AnnotKind>, mupdf::Error> {
    let kind = match kind_name {
        PdfAnnotationType::Underline => ForeignKind::Underline,
        PdfAnnotationType::StrikeOut => ForeignKind::StrikeOut,
        PdfAnnotationType::Squiggly => ForeignKind::Squiggly,
        PdfAnnotationType::Ink => ForeignKind::Ink,
        PdfAnnotationType::Polygon => ForeignKind::Polygon,
        PdfAnnotationType::PolyLine => ForeignKind::PolyLine,
        PdfAnnotationType::Caret => ForeignKind::Caret,
        PdfAnnotationType::FileAttachment => ForeignKind::FileAttachment,
        _ => return Ok(None),
    };
    let rect = from_rect(annot.rect()?);
    let color = annot
        .color()?
        .and_then(color_rgb)
        .unwrap_or(Rgb::new(32, 32, 32));
    let contents = annot.contents()?.unwrap_or("").to_string();
    let quads = annot
        .quads()
        .unwrap_or_default()
        .into_iter()
        .map(quad_bounds)
        .collect();
    let strokes = match kind_name {
        PdfAnnotationType::Ink => annot
            .ink_list()
            .unwrap_or_default()
            .into_iter()
            .map(|stroke| {
                stroke
                    .into_iter()
                    .map(|point| PdfPoint::new(point.x, point.y))
                    .collect()
            })
            .collect(),
        PdfAnnotationType::Polygon | PdfAnnotationType::PolyLine => {
            let points: Vec<PdfPoint> = annot
                .vertices()
                .unwrap_or_default()
                .into_iter()
                .map(|point| PdfPoint::new(point.x, point.y))
                .collect();
            if points.is_empty() {
                Vec::new()
            } else {
                vec![points]
            }
        }
        _ => Vec::new(),
    };
    Ok(Some(AnnotKind::Foreign {
        kind,
        rect,
        quads,
        strokes,
        color,
        contents,
    }))
}

pub(crate) fn import_annotations(doc: &PdfDocument) -> Result<Vec<Annotation>, mupdf::Error> {
    let count = doc.page_count()?;
    let mut annotations = Vec::new();
    let mut used = HashSet::new();
    for index in 0..count {
        let page = doc.load_pdf_page(index)?;
        for annot in page.annotations() {
            let kind_name = annot.r#type()?;
            // Marker-native underline / strikeout / squiggly / ink carry a Marker dict.
            // Unmarked copies stay foreign (read-only).
            if !is_native_markup(kind_name, &annot) && !is_native_ink(kind_name, &annot) {
                if let Some(kind) = import_foreign(kind_name, &annot)? {
                    let preferred = read_nm_id(&annot.object());
                    let id = allocate_id(preferred, &mut used);
                    annotations.push(Annotation {
                        id,
                        page: index as usize,
                        xref: annot.xref().ok(),
                        dirty: false,
                        revision: 0,
                        kind,
                    });
                    continue;
                }
            }
            if !matches!(
                kind_name,
                PdfAnnotationType::Highlight
                    | PdfAnnotationType::FreeText
                    | PdfAnnotationType::Text
                    | PdfAnnotationType::Square
                    | PdfAnnotationType::Circle
                    | PdfAnnotationType::Line
                    | PdfAnnotationType::Ink
                    | PdfAnnotationType::Stamp
                    | PdfAnnotationType::Underline
                    | PdfAnnotationType::StrikeOut
                    | PdfAnnotationType::Squiggly
            ) {
                continue;
            }
            let marker = read_marker(&annot.object());
            // Inline math stamps are reconstituted from the parent Text Source.
            if marker.kind == Some(MarkerKind::InlineMath) {
                continue;
            }
            if kind_name == PdfAnnotationType::Stamp
                && !matches!(marker.kind, Some(MarkerKind::Math | MarkerKind::Image))
            {
                continue;
            }
            let Some(kind) = import_kind(kind_name, &annot, &marker)? else {
                continue;
            };
            let preferred = marker.id.or_else(|| read_nm_id(&annot.object()));
            let id = allocate_id(preferred, &mut used);
            annotations.push(Annotation {
                id,
                page: index as usize,
                xref: annot.xref().ok(),
                dirty: false,
                revision: 0,
                kind,
            });
        }
    }
    Ok(annotations)
}

pub(crate) fn allocate_id(preferred: Option<u64>, used: &mut HashSet<u64>) -> u64 {
    if let Some(id) = preferred {
        if id > 0 && used.insert(id) {
            return id;
        }
    }
    let mut id = 1;
    while !used.insert(id) {
        id += 1;
    }
    id
}

pub(crate) fn import_kind(
    kind_name: PdfAnnotationType,
    annot: &PdfAnnotation,
    marker: &MarkerMeta,
) -> Result<Option<AnnotKind>, mupdf::Error> {
    let stroke = annot
        .color()?
        .and_then(color_rgb)
        .unwrap_or(Rgb::new(255, 214, 0));
    match kind_name {
        PdfAnnotationType::Highlight => {
            let Some(quads) = text_markup_quads(annot)? else {
                return Ok(None);
            };
            Ok(Some(AnnotKind::Highlight {
                quads,
                color: stroke,
            }))
        }
        PdfAnnotationType::Underline
        | PdfAnnotationType::StrikeOut
        | PdfAnnotationType::Squiggly => {
            let Some(style) = markup_style_of(kind_name) else {
                return Ok(None);
            };
            let Some(quads) = text_markup_quads(annot)? else {
                return Ok(None);
            };
            Ok(Some(AnnotKind::Markup {
                style,
                quads,
                color: stroke,
            }))
        }
        PdfAnnotationType::FreeText => {
            let rect = from_rect(annot.rect()?);
            let contents = annot.contents()?.unwrap_or("").to_string();
            // Rich text stores the full `$...$` source in Marker.Source.
            let content = marker
                .source
                .clone()
                .filter(|source| crate::math_spans::has_math(source))
                .unwrap_or(contents);
            let appearance: Option<AnnotationDefaultAppearance> = annot.default_appearance()?;
            let size = marker
                .text_size
                .or(appearance.as_ref().map(|value| value.size))
                .filter(|size| *size > 0.0)
                .unwrap_or(12.0);
            let color = marker
                .text_color
                .or_else(|| appearance.and_then(|value| value.color).and_then(color_rgb))
                .unwrap_or(Rgb::new(24, 24, 24));
            Ok(Some(AnnotKind::Text {
                rect,
                content,
                size,
                color,
            }))
        }
        PdfAnnotationType::Text => Ok(Some(AnnotKind::Note {
            rect: from_rect(annot.rect()?),
            content: annot.contents()?.unwrap_or("").to_string(),
            color: stroke,
        })),
        PdfAnnotationType::Square | PdfAnnotationType::Circle => {
            let rect = from_rect(annot.rect()?);
            Ok(Some(AnnotKind::Shape {
                kind: if kind_name == PdfAnnotationType::Square {
                    ShapeKind::Rect
                } else {
                    ShapeKind::Ellipse
                },
                rect,
                start: PdfPoint::new(rect.x0, rect.y0),
                end: PdfPoint::new(rect.x1, rect.y1),
                stroke,
                fill: annot.interior_color()?.and_then(color_rgb),
                width: annot.border_width().unwrap_or(1.0).max(0.25),
            }))
        }
        PdfAnnotationType::Line => {
            let (start, end) = annot.line()?;
            let start = PdfPoint::new(start.x, start.y);
            let end = PdfPoint::new(end.x, end.y);
            let kind = match annot.line_ending_styles() {
                Ok((_, LineEndingStyle::ClosedArrow | LineEndingStyle::OpenArrow)) => {
                    ShapeKind::Arrow
                }
                Ok((LineEndingStyle::ClosedArrow | LineEndingStyle::OpenArrow, _)) => {
                    // Arrowhead on the start endpoint — flip so Marker always paints at `end`.
                    return Ok(Some(AnnotKind::Shape {
                        kind: ShapeKind::Arrow,
                        rect: PdfRect::from_points(end, start),
                        start: end,
                        end: start,
                        stroke,
                        fill: None,
                        width: annot.border_width().unwrap_or(1.0).max(0.25),
                    }));
                }
                _ => ShapeKind::Line,
            };
            Ok(Some(AnnotKind::Shape {
                kind,
                rect: PdfRect::from_points(start, end),
                start,
                end,
                stroke,
                fill: None,
                width: annot.border_width().unwrap_or(1.0).max(0.25),
            }))
        }
        PdfAnnotationType::Ink => {
            let strokes: Vec<Vec<PdfPoint>> = annot
                .ink_list()
                .unwrap_or_default()
                .into_iter()
                .map(|stroke| {
                    stroke
                        .into_iter()
                        .map(|point| PdfPoint::new(point.x, point.y))
                        .collect()
                })
                .filter(|stroke: &Vec<PdfPoint>| !stroke.is_empty())
                .collect();
            if strokes.is_empty() {
                return Ok(None);
            }
            Ok(Some(AnnotKind::Ink {
                strokes,
                color: stroke,
                width: annot.border_width().unwrap_or(1.5).max(0.25),
            }))
        }
        PdfAnnotationType::Stamp => {
            if marker.kind == Some(MarkerKind::Image) {
                let Some(png) = marker.png.as_ref() else {
                    return Ok(None);
                };
                let Ok(decoded) = image::load_from_memory(png) else {
                    return Ok(None);
                };
                let rgba = decoded.to_rgba8();
                let width = rgba.width();
                let height = rgba.height();
                if width == 0 || height == 0 {
                    return Ok(None);
                }
                return Ok(Some(AnnotKind::Image {
                    rect: from_rect(annot.rect()?),
                    rgba: std::sync::Arc::from(rgba.into_raw()),
                    width,
                    height,
                }));
            }
            Ok(Some(AnnotKind::Math {
                rect: from_rect(annot.rect()?),
                source: marker
                    .source
                    .clone()
                    .or_else(|| annot.contents().ok().flatten().map(str::to_string))
                    .unwrap_or_default(),
                size: marker.text_size.unwrap_or(14.0),
                color: marker.text_color.unwrap_or(Rgb::new(24, 24, 24)),
                auto_size: false,
            }))
        }
        _ => Ok(None),
    }
}

pub(crate) fn text_markup_quads(
    annot: &PdfAnnotation,
) -> Result<Option<Vec<PdfRect>>, mupdf::Error> {
    let mut quads: Vec<PdfRect> = annot
        .quad_points()?
        .into_iter()
        .map(quad_bounds)
        .filter(|rect| !rect.is_empty())
        .collect();
    if quads.is_empty() {
        let rect = from_rect(annot.rect()?);
        if rect.is_empty() {
            return Ok(None);
        }
        quads.push(rect);
    }
    Ok(Some(quads))
}
