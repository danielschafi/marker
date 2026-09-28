use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::path::{Path, PathBuf};

use mupdf::color::AnnotationColor;
use mupdf::pdf::{
    AnnotationDefaultAppearance, AnnotationFlags, AnnotationTextAlign, PdfAnnotation, PdfAnnotationType,
    PdfDocument, PdfObject, PdfPage, PdfWriteOptions,
};
use mupdf::{
    Colorspace, DestinationKind, Device, DisplayList, IRect, Matrix, Outline, Pixmap, Point, Quad, Rect,
    StructuredText, TextBlockContent, TextPageFlags,
};

use crate::annot::{AnnotKind, Annotation, Glyph, ShapeKind, Word};
use crate::geom::{PdfPoint, PdfRect, Rgb};

pub const TILE_PX: i32 = 1024;
const LIST_CACHE: usize = 8;

#[derive(Clone, Copy, Debug)]
pub struct PageInfo {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl PageInfo {
    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }
}

#[derive(Clone, Debug)]
pub struct OutlineNode {
    pub title: String,
    pub page: Option<usize>,
    pub y: Option<f32>,
    pub children: Vec<OutlineNode>,
}

pub struct TileImage {
    pub page: usize,
    pub scale: f32,
    pub col: i32,
    pub row: i32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

pub struct SaveSnapshot {
    pub upserts: Vec<Annotation>,
    pub deletes: Vec<(usize, i32)>,
    pub math_pdfs: HashMap<u64, Vec<u8>>,
}

#[derive(Clone, Copy, Debug)]
pub struct SavedXref {
    pub id: u64,
    pub page: usize,
    pub xref: i32,
}

pub struct LoadedPdf {
    pub engine: DocumentEngine,
    pub annotations: Vec<Annotation>,
}

pub struct DocumentEngine {
    doc: PdfDocument,
    path: PathBuf,
    pages: Vec<PageInfo>,
    outline: Vec<OutlineNode>,
    lists: HashMap<usize, DisplayList>,
    list_order: VecDeque<usize>,
}

impl DocumentEngine {
    pub fn open(path: &Path) -> Result<LoadedPdf, String> {
        let doc = PdfDocument::open(path).map_err(show)?;
        if doc.needs_password().unwrap_or(false) {
            return Err("This PDF is password protected.".into());
        }
        let count = doc.page_count().map_err(show)?;
        let mut pages = Vec::with_capacity(count as usize);
        for index in 0..count {
            let page = doc.load_pdf_page(index).map_err(show)?;
            let bounds = page.bounds().map_err(show)?;
            pages.push(PageInfo {
                x0: bounds.x0,
                y0: bounds.y0,
                x1: bounds.x1,
                y1: bounds.y1,
            });
        }
        let outline = doc
            .outlines()
            .map(convert_outline)
            .unwrap_or_default();
        let annotations = import_annotations(&doc).map_err(show)?;
        Ok(LoadedPdf {
            engine: Self {
                doc,
                path: path.to_path_buf(),
                pages,
                outline,
                lists: HashMap::new(),
                list_order: VecDeque::new(),
            },
            annotations,
        })
    }

    pub fn pages(&self) -> &[PageInfo] {
        &self.pages
    }

    pub fn outline(&self) -> &[OutlineNode] {
        &self.outline
    }

    pub fn words(&self, page: usize) -> Result<Vec<Word>, String> {
        Ok(self
            .glyphs(page)?
            .into_iter()
            .map(|glyph| Word {
                bounds: glyph.bounds,
            })
            .collect())
    }

    pub fn glyphs(&self, page: usize) -> Result<Vec<Glyph>, String> {
        let pdf_page = self.doc.load_pdf_page(page as i32).map_err(show)?;
        let text = pdf_page
            .to_text_page(TextPageFlags::ACCURATE_BBOXES)
            .map_err(show)?;
        Ok(glyphs_from_structured(&text.structured()))
    }

    pub fn search_page(&self, page: usize, needle: &str) -> Result<Vec<PdfRect>, String> {
        if needle.is_empty() || page >= self.pages.len() {
            return Ok(Vec::new());
        }
        let pdf_page = self.doc.load_pdf_page(page as i32).map_err(show)?;
        let text = pdf_page
            .to_text_page(TextPageFlags::empty())
            .map_err(show)?;
        Ok(text
            .search(needle)
            .map_err(show)?
            .into_iter()
            .map(quad_bounds)
            .collect())
    }

    /// Rasterize one contents-only tile. Annotations stay in the overlay.
    pub fn render_tile(
        &mut self,
        page: usize,
        scale: f32,
        col: i32,
        row: i32,
    ) -> Result<Option<TileImage>, String> {
        let info = *self.pages.get(page).ok_or("Page is out of range.")?;
        let scale = scale.max(0.05);
        let (tx0, ty0, tx1, ty1) = tile_device_rect(info, scale, col, row);
        if tx1 <= tx0 || ty1 <= ty0 {
            return Ok(None);
        }

        let ctm = Matrix::new_scale(scale, scale);
        let irect = IRect::new(tx0, ty0, tx1, ty1);
        let mut pixmap =
            Pixmap::new_with_rect(&Colorspace::device_rgb(), irect, false).map_err(show)?;
        pixmap.clear_with(255).map_err(show)?;
        let device = Device::from_pixmap(&pixmap).map_err(show)?;
        let scissor = Rect::new(tx0 as f32, ty0 as f32, tx1 as f32, ty1 as f32);
        self.display_list(page)?
            .run(&device, &ctm, scissor)
            .map_err(show)?;
        drop(device);

        let width = pixmap.width();
        let height = pixmap.height();
        let pixels = rgba_from_pixmap(&pixmap);
        Ok(Some(TileImage {
            page,
            scale,
            col,
            row,
            x: tx0,
            y: ty0,
            width,
            height,
            pixels,
        }))
    }

    pub fn save(&mut self, snapshot: &SaveSnapshot) -> Result<Vec<SavedXref>, String> {
        let saved = (|| {
            let xrefs = self.apply(snapshot)?;
            self.persist()?;
            Ok(xrefs)
        })();
        if saved.is_err() {
            let path = self.path.clone();
            if let Ok(reloaded) = Self::open(&path) {
                *self = reloaded.engine;
            }
        }
        saved
    }

    fn display_list(&mut self, page: usize) -> Result<&DisplayList, String> {
        if !self.lists.contains_key(&page) {
            let pdf_page = self.doc.load_pdf_page(page as i32).map_err(show)?;
            let list = pdf_page.to_display_list(false).map_err(show)?;
            self.lists.insert(page, list);
            self.list_order.push_back(page);
            while self.list_order.len() > LIST_CACHE {
                if let Some(old) = self.list_order.pop_front() {
                    if old != page {
                        self.lists.remove(&old);
                    }
                }
            }
        }
        self.lists.get(&page).ok_or_else(|| "Missing page cache.".into())
    }

    fn apply(&mut self, snapshot: &SaveSnapshot) -> Result<Vec<SavedXref>, String> {
        for &(page, xref) in &snapshot.deletes {
            let mut pdf_page = self.doc.load_pdf_page(page as i32).map_err(show)?;
            delete_xref(&mut pdf_page, xref).map_err(show)?;
        }
        let mut saved = Vec::new();
        for annot in &snapshot.upserts {
            if !annot.dirty {
                continue;
            }
            let math = snapshot.math_pdfs.get(&annot.id).map(Vec::as_slice);
            let mut pdf_page = self.doc.load_pdf_page(annot.page as i32).map_err(show)?;
            let xref = upsert(&mut self.doc, &mut pdf_page, annot, math)?;
            saved.push(SavedXref {
                id: annot.id,
                page: annot.page,
                xref,
            });
        }
        Ok(saved)
    }

    fn persist(&mut self) -> Result<(), String> {
        let path = self.path.clone();
        let Some(path_str) = path.to_str() else {
            return Err("The PDF path is not valid UTF-8.".into());
        };
        if self.doc.can_be_saved_incrementally() {
            let mut options = PdfWriteOptions::default();
            options.set_incremental(true);
            options.set_appearance(false);
            if self.doc.save_with_options(path_str, options).is_ok() {
                return Ok(());
            }
        }
        self.rewrite_all(&path)
    }

    fn rewrite_all(&mut self, path: &Path) -> Result<(), String> {
        let tmp = temp_path(path);
        {
            let mut file = File::create(&tmp).map_err(|err| err.to_string())?;
            self.doc.write_to(&mut file).map_err(show)?;
            file.sync_all().map_err(|err| err.to_string())?;
        }
        std::fs::rename(&tmp, path).map_err(|err| err.to_string())?;
        self.doc = PdfDocument::open(path).map_err(show)?;
        self.lists.clear();
        self.list_order.clear();
        Ok(())
    }
}

fn temp_path(path: &Path) -> PathBuf {
    let mut tmp = path.to_path_buf();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("document.pdf");
    tmp.set_file_name(format!("{name}.marker-tmp"));
    tmp
}

fn tile_device_rect(info: PageInfo, scale: f32, col: i32, row: i32) -> (i32, i32, i32, i32) {
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

fn rgba_from_pixmap(pixmap: &Pixmap) -> Vec<u8> {
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
                out[dest] = row[source];
                out[dest + 1] = row[source + 1];
                out[dest + 2] = row[source + 2];
            }
        }
    }
    out
}

fn show(err: impl ToString) -> String {
    err.to_string()
}

fn to_rect(rect: PdfRect) -> Rect {
    Rect::new(rect.x0, rect.y0, rect.x1, rect.y1)
}

fn from_rect(rect: Rect) -> PdfRect {
    PdfRect::new(rect.x0, rect.y0, rect.x1, rect.y1)
}

fn glyphs_from_structured(text: &StructuredText) -> Vec<Glyph> {
    let mut glyphs = Vec::new();
    let mut line_id = 0u32;
    let mut word_id = 0u32;
    for block in &text.blocks {
        let TextBlockContent::Text { lines } = &block.content else {
            continue;
        };
        for line in lines {
            let line_bounds = from_rect(line.bounds);
            let mut in_word = false;
            for ch in &line.chars {
                if ch.ch.is_whitespace() {
                    in_word = false;
                    continue;
                }
                if !in_word {
                    word_id = word_id.saturating_add(1);
                    in_word = true;
                }
                glyphs.push(Glyph {
                    ch: ch.ch,
                    bounds: quad_bounds(ch.quad.clone()),
                    line_bounds,
                    line: line_id,
                    word: word_id,
                });
            }
            line_id = line_id.saturating_add(1);
        }
    }
    glyphs
}

fn to_point(point: PdfPoint) -> Point {
    Point::new(point.x, point.y)
}

fn rect_quad(rect: PdfRect) -> Quad {
    Quad::new(
        Point::new(rect.x0, rect.y0),
        Point::new(rect.x1, rect.y0),
        Point::new(rect.x0, rect.y1),
        Point::new(rect.x1, rect.y1),
    )
}

fn quad_bounds(quad: Quad) -> PdfRect {
    PdfRect::from_minmax(
        [quad.ul.x, quad.ur.x, quad.ll.x, quad.lr.x],
        [quad.ul.y, quad.ur.y, quad.ll.y, quad.lr.y],
    )
}

fn rgb_color(color: Rgb) -> AnnotationColor {
    let [red, green, blue] = color.to_unit();
    AnnotationColor::Rgb { red, green, blue }
}

fn color_rgb(color: AnnotationColor) -> Option<Rgb> {
    match color {
        AnnotationColor::Rgb { red, green, blue } => Some(Rgb::from_unit([red, green, blue])),
        AnnotationColor::Gray(value) => Some(Rgb::from_unit([value, value, value])),
        AnnotationColor::Cmyk { .. } => None,
    }
}

fn convert_outline(nodes: Vec<Outline>) -> Vec<OutlineNode> {
    nodes.into_iter().map(convert_outline_node).collect()
}

fn convert_outline_node(node: Outline) -> OutlineNode {
    let (page, y) = match node.dest {
        Some(dest) => {
            let y = match dest.kind {
                DestinationKind::XYZ { top, .. } => top,
                DestinationKind::FitH { top } | DestinationKind::FitBH { top } => top,
                DestinationKind::FitR { top, .. } => Some(top),
                _ => None,
            };
            (Some(dest.loc.page_number as usize), y)
        }
        None => (None, None),
    };
    OutlineNode {
        title: node.title,
        page,
        y,
        children: convert_outline(node.down),
    }
}

struct MarkerMeta {
    kind: Option<String>,
    id: Option<u64>,
    text_size: Option<f32>,
    text_color: Option<Rgb>,
    source: Option<String>,
}

fn read_marker(object: &PdfObject) -> MarkerMeta {
    let Ok(Some(marker)) = object.get_dict("Marker") else {
        return MarkerMeta {
            kind: None,
            id: None,
            text_size: None,
            text_color: None,
            source: None,
        };
    };
    let kind = marker
        .get_dict("Kind")
        .ok()
        .flatten()
        .and_then(|name| name.as_name().ok())
        .and_then(|bytes| String::from_utf8(bytes).ok());
    let id = marker
        .get_dict("Id")
        .ok()
        .flatten()
        .and_then(|value| value.as_int().ok())
        .map(|value| value as u64);
    let text_size = marker
        .get_dict("TextSize")
        .ok()
        .flatten()
        .and_then(|value| value.as_float().ok())
        .filter(|size| size.is_finite() && *size > 0.0);
    let text_color = marker.get_dict("TextColor").ok().flatten().and_then(|array| {
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
    MarkerMeta {
        kind,
        id,
        text_size,
        text_color,
        source,
    }
}

fn read_nm_id(object: &PdfObject) -> Option<u64> {
    let name = object.get_dict("NM").ok().flatten()?.as_string().ok()?;
    name.strip_prefix("marker-")?.parse().ok()
}

fn write_marker(
    doc: &PdfDocument,
    annot: &PdfAnnotation,
    kind: &str,
    id: u64,
    text_size: Option<f32>,
    text_color: Option<Rgb>,
    source: Option<&str>,
) -> Result<(), mupdf::Error> {
    let mut marker = doc.new_dict()?;
    marker.dict_put("Kind", doc.new_name(kind)?)?;
    marker.dict_put("Id", doc.new_int(id as i32)?)?;
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

fn import_annotations(doc: &PdfDocument) -> Result<Vec<Annotation>, mupdf::Error> {
    let count = doc.page_count()?;
    let mut annotations = Vec::new();
    let mut used = HashSet::new();
    for index in 0..count {
        let page = doc.load_pdf_page(index)?;
        for annot in page.annotations() {
            let kind_name = annot.r#type()?;
            if !matches!(
                kind_name,
                PdfAnnotationType::Highlight
                    | PdfAnnotationType::FreeText
                    | PdfAnnotationType::Text
                    | PdfAnnotationType::Square
                    | PdfAnnotationType::Circle
                    | PdfAnnotationType::Line
                    | PdfAnnotationType::Stamp
            ) {
                continue;
            }
            let marker = read_marker(&annot.object());
            if kind_name == PdfAnnotationType::Stamp && marker.kind.as_deref() != Some("Math") {
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

fn allocate_id(preferred: Option<u64>, used: &mut HashSet<u64>) -> u64 {
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

fn import_kind(
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
            Ok(Some(AnnotKind::Highlight {
                quads,
                color: stroke,
            }))
        }
        PdfAnnotationType::FreeText => {
            let rect = from_rect(annot.rect()?);
            let content = annot.contents()?.unwrap_or("").to_string();
            let appearance: Option<AnnotationDefaultAppearance> = annot.default_appearance()?;
            let size = marker
                .text_size
                .or(appearance.as_ref().map(|value| value.size))
                .filter(|size| *size > 0.0)
                .unwrap_or(12.0);
            let color = marker.text_color.or_else(|| {
                appearance
                    .and_then(|value| value.color)
                    .and_then(color_rgb)
            }).unwrap_or(Rgb::new(24, 24, 24));
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
            Ok(Some(AnnotKind::Shape {
                kind: ShapeKind::Line,
                rect: PdfRect::from_points(start, end),
                start,
                end,
                stroke,
                fill: None,
                width: annot.border_width().unwrap_or(1.0).max(0.25),
            }))
        }
        PdfAnnotationType::Stamp => Ok(Some(AnnotKind::Math {
            rect: from_rect(annot.rect()?),
            source: marker
                .source
                .clone()
                .or_else(|| annot.contents().ok().flatten().map(str::to_string))
                .unwrap_or_default(),
            size: marker.text_size.unwrap_or(14.0),
            color: marker.text_color.unwrap_or(Rgb::new(24, 24, 24)),
            auto_size: false,
        })),
        _ => Ok(None),
    }
}

fn upsert(
    doc: &mut PdfDocument,
    page: &mut PdfPage,
    annot: &Annotation,
    math_pdf: Option<&[u8]>,
) -> Result<i32, String> {
    if let Some(xref) = annot.xref {
        if let Some(mut existing) = find_annot(page, xref) {
            if apply_existing(doc, &mut existing, annot, math_pdf).is_ok() {
                return Ok(xref);
            }
            delete_xref(page, xref).map_err(show)?;
        }
    }
    create_annot(doc, page, annot, math_pdf)
}

fn find_annot(page: &PdfPage, xref: i32) -> Option<PdfAnnotation> {
    page.annotations().find(|annot| annot.xref().ok() == Some(xref))
}

fn delete_xref(page: &mut PdfPage, xref: i32) -> Result<(), mupdf::Error> {
    if let Some(annot) = find_annot(page, xref) {
        page.delete_annotation(annot)?;
    }
    Ok(())
}

fn apply_existing(
    doc: &mut PdfDocument,
    annot: &mut PdfAnnotation,
    source: &Annotation,
    math_pdf: Option<&[u8]>,
) -> Result<(), mupdf::Error> {
    annot.set_flags(AnnotationFlags::IS_PRINT)?;
    match &source.kind {
        AnnotKind::Highlight { quads, color } => {
            if annot.r#type()? != PdfAnnotationType::Highlight {
                return Err(mupdf::Error::InvalidArgument("type changed".into()));
            }
            let pdf_quads: Vec<Quad> = quads.iter().copied().map(rect_quad).collect();
            if pdf_quads.is_empty() {
                return Err(mupdf::Error::InvalidArgument("empty highlight".into()));
            }
            annot.set_quad_points(pdf_quads)?;
            annot.set_color(rgb_color(*color))?;
            annot.set_opacity(0.45)?;
            if let Some(bounds) = source.kind.bounds() {
                annot.set_rect(to_rect(bounds))?;
            }
            write_marker(doc, annot, "Highlight", source.id, None, Some(*color), None)?;
            annot.update()?;
        }
        AnnotKind::Text { rect, content, size, color } => {
            if annot.r#type()? != PdfAnnotationType::FreeText {
                return Err(mupdf::Error::InvalidArgument("type changed".into()));
            }
            annot.set_rect(to_rect(*rect))?;
            annot.set_contents(content)?;
            annot.set_default_appearance("Helv", *size, Some(rgb_color(*color)))?;
            annot.set_quadding(AnnotationTextAlign::Left)?;
            write_marker(doc, annot, "Text", source.id, Some(*size), Some(*color), None)?;
            annot.update()?;
        }
        AnnotKind::Note { rect, content, color } => {
            if annot.r#type()? != PdfAnnotationType::Text {
                return Err(mupdf::Error::InvalidArgument("type changed".into()));
            }
            annot.set_rect(to_rect(*rect))?;
            annot.set_contents(content)?;
            annot.set_color(rgb_color(*color))?;
            write_marker(doc, annot, "Note", source.id, None, Some(*color), None)?;
            annot.update()?;
        }
        AnnotKind::Shape { kind, rect, start, end, stroke, fill, width } => {
            apply_shape(annot, *kind, *rect, *start, *end, *stroke, *fill, *width)?;
            write_marker(doc, annot, "Shape", source.id, None, Some(*stroke), None)?;
            annot.update()?;
        }
        AnnotKind::Math { rect, source: latex, size, color, .. } => {
            if annot.r#type()? != PdfAnnotationType::Stamp {
                return Err(mupdf::Error::InvalidArgument("type changed".into()));
            }
            annot.set_rect(to_rect(*rect))?;
            annot.set_contents(latex)?;
            write_marker(
                doc,
                annot,
                "Math",
                source.id,
                Some(*size),
                Some(*color),
                Some(latex),
            )?;
            if let Some(bytes) = math_pdf {
                install_math_appearance(doc, annot, bytes)?;
            }
        }
        AnnotKind::Future(_) => {
            return Err(mupdf::Error::NotYetImplemented("future annotation".into()));
        }
    }
    Ok(())
}

fn apply_shape(
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
        ShapeKind::Line => PdfAnnotationType::Line,
    };
    if annot.r#type()? != expected {
        return Err(mupdf::Error::InvalidArgument("type changed".into()));
    }
    annot.set_color(rgb_color(stroke))?;
    annot.set_border_width(width.max(0.25))?;
    if let Some(fill) = fill {
        annot.set_interior_color(rgb_color(fill))?;
    }
    match kind {
        ShapeKind::Line => annot.set_line(to_point(start), to_point(end))?,
        ShapeKind::Rect | ShapeKind::Ellipse => annot.set_rect(to_rect(rect))?,
    }
    Ok(())
}

fn create_annot(
    doc: &mut PdfDocument,
    page: &mut PdfPage,
    source: &Annotation,
    math_pdf: Option<&[u8]>,
) -> Result<i32, String> {
    let mut annot = match &source.kind {
        AnnotKind::Highlight { quads, color } => {
            let pdf_quads: Vec<Quad> = quads.iter().copied().map(rect_quad).collect();
            if pdf_quads.is_empty() {
                return Err("Highlight is empty.".into());
            }
            let mut annot = page.add_highlight_annotation(pdf_quads).map_err(show)?;
            annot.set_color(rgb_color(*color)).map_err(show)?;
            annot.set_opacity(0.45).map_err(show)?;
            write_marker(doc, &annot, "Highlight", source.id, None, Some(*color), None).map_err(show)?;
            annot
        }
        AnnotKind::Text { rect, content, size, color } => {
            let mut annot = page
                .add_free_text_annotation(to_rect(*rect), content)
                .map_err(show)?;
            annot
                .set_default_appearance("Helv", *size, Some(rgb_color(*color)))
                .map_err(show)?;
            annot.set_quadding(AnnotationTextAlign::Left).map_err(show)?;
            write_marker(doc, &annot, "Text", source.id, Some(*size), Some(*color), None)
                .map_err(show)?;
            annot
        }
        AnnotKind::Note { rect, content, color } => {
            let mut annot = page
                .add_text_annotation(to_rect(*rect), content)
                .map_err(show)?;
            annot.set_color(rgb_color(*color)).map_err(show)?;
            annot.set_icon_name("Comment").map_err(show)?;
            write_marker(doc, &annot, "Note", source.id, None, Some(*color), None).map_err(show)?;
            annot
        }
        AnnotKind::Shape { kind, rect, start, end, stroke, fill, width } => {
            let mut annot = match kind {
                ShapeKind::Rect => page.add_square_annotation(to_rect(*rect)).map_err(show)?,
                ShapeKind::Ellipse => page.add_circle_annotation(to_rect(*rect)).map_err(show)?,
                ShapeKind::Line => page
                    .add_line_annotation(to_point(*start), to_point(*end))
                    .map_err(show)?,
            };
            annot.set_color(rgb_color(*stroke)).map_err(show)?;
            annot.set_border_width(width.max(0.25)).map_err(show)?;
            if let Some(fill) = fill {
                annot.set_interior_color(rgb_color(*fill)).map_err(show)?;
            }
            write_marker(doc, &annot, "Shape", source.id, None, Some(*stroke), None).map_err(show)?;
            annot
        }
        AnnotKind::Math { rect, source: latex, size, color, .. } => {
            let mut annot = page
                .create_annotation(PdfAnnotationType::Stamp)
                .map_err(show)?;
            annot.set_rect(to_rect(*rect)).map_err(show)?;
            annot.set_contents(latex).map_err(show)?;
            write_marker(
                doc,
                &annot,
                "Math",
                source.id,
                Some(*size),
                Some(*color),
                Some(latex),
            )
            .map_err(show)?;
            if let Some(bytes) = math_pdf {
                install_math_appearance(doc, &mut annot, bytes).map_err(show)?;
            }
            annot.set_flags(AnnotationFlags::IS_PRINT).map_err(show)?;
            return annot.xref().map_err(show);
        }
        AnnotKind::Future(_) => return Err("That annotation type is not available yet.".into()),
    };
    annot.set_flags(AnnotationFlags::IS_PRINT).map_err(show)?;
    annot.update().map_err(show)?;
    annot.xref().map_err(show)
}

fn install_math_appearance(
    dest: &mut PdfDocument,
    annot: &mut PdfAnnotation,
    pdf_bytes: &[u8],
) -> Result<(), mupdf::Error> {
    let src = PdfDocument::from_bytes(pdf_bytes)?;
    let src_page = src.load_pdf_page(0)?;
    let media = src_page.media_box()?;
    let Some(contents) = src_page.contents()? else {
        return Err(mupdf::Error::InvalidArgument(
            "equation PDF has no content".into(),
        ));
    };
    let resources = dest.graft_object(&src_page.resources()?)?;
    let form = form_xobject(dest, &contents, resources, media)?;
    let mut appearance = dest.new_dict()?;
    appearance.dict_put("N", form)?;
    annot.object().dict_put("AP", appearance)?;
    Ok(())
}

fn form_xobject(
    dest: &mut PdfDocument,
    contents: &PdfObject,
    resources: PdfObject,
    media: Rect,
) -> Result<PdfObject, mupdf::Error> {
    let resolved = contents.resolve()?.unwrap_or(contents.try_clone()?);
    let mut form = if resolved.is_array()? {
        let mut bytes = Vec::new();
        for index in 0..resolved.len()? {
            let Some(item) = resolved.get_array(index as i32)? else {
                continue;
            };
            let item = item.resolve()?.unwrap_or(item);
            if item.is_stream()? {
                bytes.extend(item.read_stream()?);
                bytes.push(b'\n');
            }
        }
        let dict = dest.new_dict()?;
        let buffer = mupdf::Buffer::from_bytes(&bytes)?;
        dest.add_stream(&buffer, Some(&dict), true)?
    } else {
        dest.graft_object(contents)?
    };
    let mut bbox = dest.new_array()?;
    media.encode_into(&mut bbox)?;
    form.dict_put("Type", dest.new_name("XObject")?)?;
    form.dict_put("Subtype", dest.new_name("Form")?)?;
    form.dict_put("BBox", bbox)?;
    form.dict_put("Resources", resources)?;
    Ok(form)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mupdf::pdf::PdfDocument;
    use mupdf::shape::{Shape, TextOptions};
    use mupdf::{Point, Size};

    fn sample_pdf(path: &Path) {
        let mut doc = PdfDocument::new();
        let mut page = doc.new_page(Size::A4).unwrap();
        let mut shape = Shape::new(&mut page).unwrap();
        shape
            .insert_text(
                Point::new(72.0, 96.0),
                "Hello Marker world",
                &TextOptions {
                    fontsize: 22.0,
                    ..TextOptions::default()
                },
            )
            .unwrap()
            .commit(&mut doc, true)
            .unwrap();
        doc.save(path.to_str().unwrap()).unwrap();
    }

    #[test]
    fn annotations_roundtrip_and_tiles_skip_them() {
        let dir = std::env::temp_dir().join(format!("marker-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.pdf");
        sample_pdf(&path);

        let loaded = DocumentEngine::open(&path).unwrap();
        let mut engine = loaded.engine;
        assert_eq!(engine.pages().len(), 1);
        let words = engine.words(0).unwrap();
        assert!(!words.is_empty(), "expected words on the sample page");

        let before = engine.render_tile(0, 1.5, 0, 0).unwrap().unwrap();
        let yellow_before = count_yellow(&before.pixels);

        let mut session = crate::annot::Session::from_imported(loaded.annotations);
        let quads: Vec<PdfRect> = words.iter().map(|word| word.bounds).collect();
        session.insert(
            0,
            AnnotKind::Highlight {
                quads,
                color: Rgb::new(255, 214, 0),
            },
        );
        session.insert(
            0,
            AnnotKind::Text {
                rect: PdfRect::new(72.0, 140.0, 260.0, 180.0),
                content: "inline note".into(),
                size: 16.0,
                color: Rgb::new(20, 20, 20),
            },
        );
        session.insert(
            0,
            AnnotKind::Note {
                rect: PdfRect::new(300.0, 80.0, 324.0, 104.0),
                content: "popup".into(),
                color: Rgb::new(255, 196, 0),
            },
        );
        session.insert(
            0,
            AnnotKind::Shape {
                kind: ShapeKind::Rect,
                rect: PdfRect::new(72.0, 220.0, 180.0, 280.0),
                start: PdfPoint::new(72.0, 220.0),
                end: PdfPoint::new(180.0, 280.0),
                stroke: Rgb::new(28, 78, 186),
                fill: None,
                width: 1.5,
            },
        );
        session.insert(
            0,
            AnnotKind::Shape {
                kind: ShapeKind::Ellipse,
                rect: PdfRect::new(200.0, 220.0, 280.0, 280.0),
                start: PdfPoint::new(200.0, 220.0),
                end: PdfPoint::new(280.0, 280.0),
                stroke: Rgb::new(186, 36, 36),
                fill: None,
                width: 1.5,
            },
        );
        session.insert(
            0,
            AnnotKind::Shape {
                kind: ShapeKind::Line,
                rect: PdfRect::from_points(PdfPoint::new(72.0, 320.0), PdfPoint::new(220.0, 360.0)),
                start: PdfPoint::new(72.0, 320.0),
                end: PdfPoint::new(220.0, 360.0),
                stroke: Rgb::new(22, 122, 58),
                fill: None,
                width: 2.0,
            },
        );

        let snapshot = SaveSnapshot {
            upserts: session.annotations.clone(),
            deletes: Vec::new(),
            math_pdfs: HashMap::new(),
        };
        engine.save(&snapshot).unwrap();

        let mut loaded = DocumentEngine::open(&path).unwrap();
        let mut kinds = loaded
            .annotations
            .iter()
            .map(|annot| match &annot.kind {
                AnnotKind::Highlight { .. } => "highlight",
                AnnotKind::Text { content, size, .. } => {
                    assert_eq!(content, "inline note");
                    assert!((*size - 16.0).abs() < 0.2, "size {size}");
                    "text"
                }
                AnnotKind::Note { content, .. } => {
                    assert_eq!(content, "popup");
                    "note"
                }
                AnnotKind::Shape { kind, .. } => match kind {
                    ShapeKind::Rect => "rect",
                    ShapeKind::Ellipse => "ellipse",
                    ShapeKind::Line => "line",
                },
                AnnotKind::Math { .. } => "math",
                AnnotKind::Future(_) => "future",
            })
            .collect::<Vec<_>>();
        kinds.sort_unstable();
        assert_eq!(
            kinds,
            ["ellipse", "highlight", "line", "note", "rect", "text"]
        );

        let after = loaded
            .engine
            .render_tile(0, 1.5, 0, 0)
            .unwrap()
            .unwrap();
        let yellow_after = count_yellow(&after.pixels);
        assert!(
            yellow_after < yellow_before + 40,
            "contents render picked up highlight pixels: before {yellow_before} after {yellow_after}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn count_yellow(pixels: &[u8]) -> usize {
        pixels
            .chunks_exact(4)
            .filter(|px| px[0] > 220 && px[1] > 180 && px[2] < 120)
            .count()
    }
}
