use super::*;
use std::collections::HashMap;
use std::path::Path;

use mupdf::pdf::PdfDocument;
use mupdf::shape::{Shape, TextOptions};
use mupdf::{Point, Size};

use mupdf::color::AnnotationColor;
use mupdf::pdf::PdfAnnotationType;
use mupdf::Quad;

use crate::annot::{AnnotKind, ForeignKind, MarkupStyle, ShapeKind};
use crate::geom::{PdfPoint, PdfRect, Rgb};

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
fn open_rejects_pdf_without_readable_pages() {
    let dir = std::env::temp_dir().join(format!("marker-empty-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("empty.pdf");
    PdfDocument::new().save(path.to_str().unwrap()).unwrap();

    let result = DocumentEngine::open(&path);

    assert!(matches!(
        result,
        Err(Error::Message(ref message))
            if message == "This PDF has no readable pages. It may be damaged."
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn render_thumb_is_about_target_width() {
    let dir = std::env::temp_dir().join(format!(
        "marker-thumb-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.pdf");
    sample_pdf(&path);
    let mut engine = DocumentEngine::open(&path).unwrap().engine;
    let thumb = engine.render_thumb(0).unwrap().unwrap();
    assert_eq!(thumb.page, 0);
    assert!(
        (thumb.width as f32 - THUMB_WIDTH_PX).abs() <= 2.0,
        "thumb width {} should be near {}",
        thumb.width,
        THUMB_WIDTH_PX
    );
    assert!(thumb.height > 0);
    assert_eq!(
        thumb.pixels.len(),
        (thumb.width * thumb.height * 4) as usize
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn search_page_respects_case_flag_on_sample() {
    let dir = std::env::temp_dir().join(format!(
        "marker-search-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.pdf");
    sample_pdf(&path);
    let engine = DocumentEngine::open(&path).unwrap().engine;

    let insensitive = engine.search_page(0, "hello", false).unwrap();
    assert!(
        !insensitive.is_empty(),
        "case-insensitive search should match Hello"
    );

    let wrong_case = engine.search_page(0, "hello", true).unwrap();
    assert!(
        wrong_case.is_empty(),
        "case-sensitive search must not match Hello for needle hello"
    );

    let exact = engine.search_page(0, "Hello", true).unwrap();
    assert!(!exact.is_empty(), "exact case Hello should hit");
    assert!(
        (exact[0].y0 - insensitive[0].y0).abs() < 1.0,
        "exact and insensitive hits should share a y"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn first_persist_copies_pristine_bytes_to_bak() {
    let dir = std::env::temp_dir().join(format!(
        "marker-bak-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.pdf");
    sample_pdf(&path);
    let original = std::fs::read(&path).unwrap();
    let bak = bak_path(&path);

    let mut engine = DocumentEngine::open(&path).unwrap().engine;
    let empty = SaveSnapshot {
        upserts: Vec::new(),
        deletes: Vec::new(),
        math_pdfs: HashMap::new(),
        inline_math: Vec::new(),
        rich_text_parents: Vec::new(),
    };
    engine.save(&empty).unwrap();
    assert!(
        bak.exists(),
        "expected {}.bak after first save",
        path.display()
    );
    assert_eq!(std::fs::read(&bak).unwrap(), original);

    // Second save must not rotate or rewrite an existing bak.
    std::fs::write(&bak, b"frozen-bak").unwrap();
    engine.save(&empty).unwrap();
    assert_eq!(std::fs::read(&bak).unwrap(), b"frozen-bak");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn save_as_writes_copy_and_retargets_without_touching_original() {
    let dir = std::env::temp_dir().join(format!(
        "marker-saveas-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.pdf");
    let copy = dir.join("copy.pdf");
    sample_pdf(&path);
    let original = std::fs::read(&path).unwrap();
    let mtime_before = std::fs::metadata(&path).unwrap().modified().unwrap();

    let mut engine = DocumentEngine::open(&path).unwrap().engine;
    let empty = SaveSnapshot {
        upserts: Vec::new(),
        deletes: Vec::new(),
        math_pdfs: HashMap::new(),
        inline_math: Vec::new(),
        rich_text_parents: Vec::new(),
    };
    engine.save_as(&empty, &copy).unwrap();
    assert!(copy.exists());
    assert_eq!(engine.path, copy);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let mtime_after = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(mtime_before, mtime_after);
    assert!(
        !bak_path(&path).exists(),
        "Save As must not bak the original"
    );

    let _ = std::fs::remove_dir_all(&dir);
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
    let glyphs = engine.glyphs(0).unwrap();
    assert!(!glyphs.is_empty(), "expected glyphs on the sample page");

    let before = engine.render_tile(0, 1.5, 0, 0).unwrap().unwrap();
    let yellow_before = count_yellow(&before.pixels);

    let mut session = crate::annot::Session::from_imported(loaded.annotations);
    let quads: Vec<PdfRect> = glyphs.iter().map(|glyph| glyph.bounds).collect();
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
        upserts: session.cloned_annotations(),
        deletes: Vec::new(),
        math_pdfs: HashMap::new(),
        inline_math: Vec::new(),
        rich_text_parents: Vec::new(),
    };
    engine.save(&snapshot).unwrap();

    let mut loaded = DocumentEngine::open(&path).unwrap();
    let mut kinds = loaded
        .annotations
        .iter()
        .map(|annot| match &annot.kind {
            AnnotKind::Highlight { .. } => "highlight",
            AnnotKind::Markup { .. } => "markup",
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
                ShapeKind::Arrow => "arrow",
            },
            AnnotKind::Ink { .. } => "ink",
            AnnotKind::Math { .. } => "math",
            AnnotKind::Image { .. } => "image",
            AnnotKind::Foreign { .. } => "foreign",
        })
        .collect::<Vec<_>>();
    kinds.sort_unstable();
    assert_eq!(
        kinds,
        ["ellipse", "highlight", "line", "note", "rect", "text"]
    );

    let after = loaded.engine.render_tile(0, 1.5, 0, 0).unwrap().unwrap();
    let yellow_after = count_yellow(&after.pixels);
    assert!(
        yellow_after < yellow_before + 40,
        "contents render picked up highlight pixels: before {yellow_before} after {yellow_after}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn empty_note_roundtrip() {
    let dir = std::env::temp_dir().join(format!(
        "marker-note-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.pdf");
    sample_pdf(&path);

    let loaded = DocumentEngine::open(&path).unwrap();
    let mut engine = loaded.engine;
    let color = Rgb::new(255, 196, 0);
    let rect = PdfRect::new(120.0, 90.0, 120.0 + 24.0, 90.0 + 24.0);
    let mut session = crate::annot::Session::from_imported(loaded.annotations);
    session.insert(
        0,
        AnnotKind::Note {
            rect,
            content: String::new(),
            color,
        },
    );
    engine
        .save(&SaveSnapshot {
            upserts: session.cloned_annotations(),
            deletes: Vec::new(),
            math_pdfs: HashMap::new(),
            inline_math: Vec::new(),
            rich_text_parents: Vec::new(),
        })
        .unwrap();

    let again = DocumentEngine::open(&path).unwrap();
    let notes: Vec<_> = again
        .annotations
        .iter()
        .filter_map(|annot| match &annot.kind {
            AnnotKind::Note {
                content,
                color: c,
                rect: r,
            } => Some((content.clone(), *c, *r)),
            _ => None,
        })
        .collect();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].0, "");
    assert_eq!(notes[0].1, color);
    assert!((notes[0].2.x0 - rect.x0).abs() < 0.5);
    assert!((notes[0].2.y0 - rect.y0).abs() < 0.5);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn shape_fill_roundtrip() {
    let dir = std::env::temp_dir().join(format!(
        "marker-fill-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.pdf");
    sample_pdf(&path);

    let loaded = DocumentEngine::open(&path).unwrap();
    let mut engine = loaded.engine;
    let stroke = Rgb::new(28, 78, 186);
    let fill = Rgb::new(28, 78, 186);
    let mut session = crate::annot::Session::from_imported(loaded.annotations);
    session.insert(
        0,
        AnnotKind::Shape {
            kind: ShapeKind::Rect,
            rect: PdfRect::new(72.0, 200.0, 160.0, 260.0),
            start: PdfPoint::new(72.0, 200.0),
            end: PdfPoint::new(160.0, 260.0),
            stroke,
            fill: Some(fill),
            width: 1.5,
        },
    );
    session.insert(
        0,
        AnnotKind::Shape {
            kind: ShapeKind::Rect,
            rect: PdfRect::new(180.0, 200.0, 268.0, 260.0),
            start: PdfPoint::new(180.0, 200.0),
            end: PdfPoint::new(268.0, 260.0),
            stroke,
            fill: None,
            width: 1.5,
        },
    );
    engine
        .save(&SaveSnapshot {
            upserts: session.cloned_annotations(),
            deletes: Vec::new(),
            math_pdfs: HashMap::new(),
            inline_math: Vec::new(),
            rich_text_parents: Vec::new(),
        })
        .unwrap();

    let again = DocumentEngine::open(&path).unwrap();
    let mut fills: Vec<Option<Rgb>> = again
        .annotations
        .iter()
        .filter_map(|annot| match &annot.kind {
            AnnotKind::Shape {
                kind: ShapeKind::Rect,
                fill,
                ..
            } => Some(*fill),
            _ => None,
        })
        .collect();
    fills.sort_by_key(|f| f.is_some());
    assert_eq!(fills, [None, Some(fill)]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn arrow_line_ending_roundtrip() {
    let dir = std::env::temp_dir().join(format!(
        "marker-arrow-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.pdf");
    sample_pdf(&path);

    let loaded = DocumentEngine::open(&path).unwrap();
    let mut engine = loaded.engine;
    let start = PdfPoint::new(90.0, 140.0);
    let end = PdfPoint::new(210.0, 200.0);
    let stroke = Rgb::new(28, 78, 186);
    let width = 2.0;
    let mut session = crate::annot::Session::from_imported(loaded.annotations);
    session.insert(
        0,
        AnnotKind::Shape {
            kind: ShapeKind::Arrow,
            rect: PdfRect::from_points(start, end),
            start,
            end,
            stroke,
            fill: None,
            width,
        },
    );
    engine
        .save(&SaveSnapshot {
            upserts: session.cloned_annotations(),
            deletes: Vec::new(),
            math_pdfs: HashMap::new(),
            inline_math: Vec::new(),
            rich_text_parents: Vec::new(),
        })
        .unwrap();

    let again = DocumentEngine::open(&path).unwrap();
    let arrow = again
        .annotations
        .iter()
        .find_map(|annot| match &annot.kind {
            AnnotKind::Shape {
                kind: ShapeKind::Arrow,
                start: loaded_start,
                end: loaded_end,
                stroke: loaded_stroke,
                width: loaded_width,
                ..
            } => Some((
                *loaded_start,
                *loaded_end,
                *loaded_stroke,
                *loaded_width,
            )),
            _ => None,
        })
        .expect("arrow shape annotation");
    assert_eq!(arrow.2, stroke);
    assert!((arrow.3 - width).abs() < 0.05, "width {}", arrow.3);
    assert!(
        (arrow.0.x - start.x).abs() < 0.5 && (arrow.0.y - start.y).abs() < 0.5,
        "start {:?} vs {:?}",
        arrow.0,
        start
    );
    assert!(
        (arrow.1.x - end.x).abs() < 0.5 && (arrow.1.y - end.y).abs() < 0.5,
        "end {:?} vs {:?}",
        arrow.1,
        end
    );
    assert!(
        again.annotations.iter().all(|annot| {
            !matches!(
                annot.kind,
                AnnotKind::Shape {
                    kind: ShapeKind::Line,
                    ..
                }
            )
        }),
        "arrow must not reload as a plain line"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ink_stroke_roundtrip() {
    let dir = std::env::temp_dir().join(format!(
        "marker-ink-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.pdf");
    sample_pdf(&path);

    let loaded = DocumentEngine::open(&path).unwrap();
    let mut engine = loaded.engine;
    let color = Rgb::new(186, 36, 36);
    let width = 2.5;
    let strokes = vec![vec![
        PdfPoint::new(80.0, 120.0),
        PdfPoint::new(140.0, 160.0),
        PdfPoint::new(200.0, 110.0),
    ]];
    let mut session = crate::annot::Session::from_imported(loaded.annotations);
    session.insert(
        0,
        AnnotKind::Ink {
            strokes: strokes.clone(),
            color,
            width,
        },
    );
    engine
        .save(&SaveSnapshot {
            upserts: session.cloned_annotations(),
            deletes: Vec::new(),
            math_pdfs: HashMap::new(),
            inline_math: Vec::new(),
            rich_text_parents: Vec::new(),
        })
        .unwrap();

    let again = DocumentEngine::open(&path).unwrap();
    let ink = again
        .annotations
        .iter()
        .find_map(|annot| match &annot.kind {
            AnnotKind::Ink {
                strokes: loaded_strokes,
                color: loaded_color,
                width: loaded_width,
            } => Some((loaded_strokes, *loaded_color, *loaded_width)),
            _ => None,
        })
        .expect("native ink annotation");
    assert_eq!(ink.1, color);
    assert!((ink.2 - width).abs() < 0.05, "width {}", ink.2);
    assert_eq!(ink.0.len(), 1);
    assert_eq!(ink.0[0].len(), strokes[0].len());
    for (got, want) in ink.0[0].iter().zip(strokes[0].iter()) {
        assert!((got.x - want.x).abs() < 0.5, "x {} vs {}", got.x, want.x);
        assert!((got.y - want.y).abs() < 0.5, "y {} vs {}", got.y, want.y);
    }
    assert!(
        again
            .annotations
            .iter()
            .all(|annot| !matches!(annot.kind, AnnotKind::Foreign { .. })),
        "marker ink must not load as foreign"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn count_yellow(pixels: &[u8]) -> usize {
    pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[0] > 220 && px[1] > 180 && px[2] < 120)
        .count()
}

#[test]
fn image_stamp_roundtrip() {
    let dir = std::env::temp_dir().join(format!(
        "marker-img-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.pdf");
    sample_pdf(&path);

    let loaded = DocumentEngine::open(&path).unwrap();
    let mut engine = loaded.engine;
    let mut rgba = vec![0u8; 8 * 8 * 4];
    for px in rgba.chunks_exact_mut(4) {
        px[0] = 200;
        px[1] = 40;
        px[2] = 40;
        px[3] = 255;
    }
    let mut session = crate::annot::Session::from_imported(loaded.annotations);
    session.insert(
        0,
        AnnotKind::Image {
            rect: PdfRect::new(72.0, 72.0, 172.0, 172.0),
            rgba: std::sync::Arc::from(rgba),
            width: 8,
            height: 8,
        },
    );
    let snapshot = SaveSnapshot {
        upserts: session.cloned_annotations(),
        deletes: Vec::new(),
        math_pdfs: HashMap::new(),
        inline_math: Vec::new(),
        rich_text_parents: Vec::new(),
    };
    let xrefs = engine.save(&snapshot).unwrap();
    assert_eq!(xrefs.len(), 1, "expected one saved xref: {xrefs:?}");

    let reloaded = DocumentEngine::open(&path).unwrap();
    let images: Vec<_> = reloaded
        .annotations
        .iter()
        .filter(|annot| matches!(annot.kind, AnnotKind::Image { .. }))
        .collect();
    assert_eq!(
        images.len(),
        1,
        "expected image after reload, got {} annots",
        reloaded.annotations.len()
    );
    let AnnotKind::Image {
        width,
        height,
        rgba,
        rect,
    } = &images[0].kind
    else {
        unreachable!();
    };
    assert_eq!(*width, 8);
    assert_eq!(*height, 8);
    assert_eq!(rgba.len(), 8 * 8 * 4);
    assert!((rect.x0 - 72.0).abs() < 0.5);
    assert_eq!(rgba[0], 200);
    assert_eq!(rgba[1], 40);
    assert_eq!(rgba[2], 40);
    assert_eq!(rgba[3], 255);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn image_move_does_not_reembed_pixels() {
    let dir = std::env::temp_dir().join(format!(
        "marker-img-move-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.pdf");
    sample_pdf(&path);

    const W: u32 = 96;
    const H: u32 = 96;
    let mut rgba = vec![0u8; (W * H * 4) as usize];
    for (i, byte) in rgba.iter_mut().enumerate() {
        *byte = (i.wrapping_mul(1103515245).wrapping_add(12345) >> 16) as u8;
    }
    rgba.chunks_exact_mut(4).for_each(|px| px[3] = 255);

    let loaded = DocumentEngine::open(&path).unwrap();
    let mut engine = loaded.engine;
    let mut session = crate::annot::Session::from_imported(loaded.annotations);
    let id = session.insert(
        0,
        AnnotKind::Image {
            rect: PdfRect::new(72.0, 72.0, 260.0, 260.0),
            rgba: std::sync::Arc::from(rgba.clone()),
            width: W,
            height: H,
        },
    );
    let xrefs = engine
        .save(&SaveSnapshot {
            upserts: session.cloned_annotations(),
            deletes: Vec::new(),
            math_pdfs: HashMap::new(),
            inline_math: Vec::new(),
            rich_text_parents: Vec::new(),
        })
        .unwrap();
    let saved = xrefs.iter().find(|entry| entry.id == id).unwrap();
    let annot = session.get_mut(id).unwrap();
    annot.xref = Some(saved.xref);
    annot.dirty = true;
    if let AnnotKind::Image { rect, .. } = &mut annot.kind {
        *rect = PdfRect::new(120.0, 90.0, 308.0, 278.0);
    }
    let before = std::fs::metadata(&path).unwrap().len();
    engine
        .save(&SaveSnapshot {
            upserts: session.cloned_annotations(),
            deletes: Vec::new(),
            math_pdfs: HashMap::new(),
            inline_math: Vec::new(),
            rich_text_parents: Vec::new(),
        })
        .unwrap();
    let after = std::fs::metadata(&path).unwrap().len();
    let growth = after.saturating_sub(before);
    assert!(
        growth < 16 * 1024,
        "moving an image re-embedded pixels: file grew by {growth} bytes ({before} -> {after})"
    );

    let reloaded = DocumentEngine::open(&path).unwrap();
    let images: Vec<_> = reloaded
        .annotations
        .iter()
        .filter(|annot| matches!(annot.kind, AnnotKind::Image { .. }))
        .collect();
    assert_eq!(images.len(), 1);
    let AnnotKind::Image {
        rect, rgba: got, ..
    } = &images[0].kind
    else {
        unreachable!();
    };
    assert!((rect.x0 - 120.0).abs() < 0.5, "rect snapped back: {rect:?}");
    assert_eq!(got.as_ref(), rgba.as_slice());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn foreign_underline_survives_marker_save() {
    let dir = std::env::temp_dir().join(format!(
        "marker-foreign-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.pdf");
    sample_pdf(&path);

    {
        let doc = PdfDocument::open(path.to_str().unwrap()).unwrap();
        let mut page = doc.load_pdf_page(0).unwrap();
        let mut underline = page
            .create_annotation(PdfAnnotationType::Underline)
            .unwrap();
        let quad = Quad::new(
            Point::new(72.0, 80.0),
            Point::new(220.0, 80.0),
            Point::new(72.0, 100.0),
            Point::new(220.0, 100.0),
        );
        underline.set_quad_points([quad]).unwrap();
        underline
            .set_color(AnnotationColor::Rgb {
                red: 0.1,
                green: 0.2,
                blue: 0.9,
            })
            .unwrap();
        underline.update().unwrap();
        doc.save(path.to_str().unwrap()).unwrap();
    }

    let loaded = DocumentEngine::open(&path).unwrap();
    let foreign: Vec<_> = loaded
        .annotations
        .iter()
        .filter(|annot| matches!(annot.kind, AnnotKind::Foreign { .. }))
        .collect();
    assert_eq!(foreign.len(), 1, "underline should import");
    assert!(!foreign[0].dirty);
    let AnnotKind::Foreign {
        kind: ForeignKind::Underline,
        quads,
        ..
    } = &foreign[0].kind
    else {
        panic!("expected underline, got {:?}", foreign[0].kind);
    };
    assert!(!quads.is_empty(), "underline quads should be kept");

    let mut engine = loaded.engine;
    let mut session = crate::annot::Session::from_imported(loaded.annotations);
    session.insert(
        0,
        AnnotKind::Highlight {
            quads: vec![PdfRect::new(72.0, 400.0, 140.0, 414.0)],
            color: Rgb::new(255, 214, 0),
        },
    );
    engine
        .save(&SaveSnapshot {
            upserts: session.cloned_annotations(),
            deletes: Vec::new(),
            math_pdfs: HashMap::new(),
            inline_math: Vec::new(),
            rich_text_parents: Vec::new(),
        })
        .unwrap();

    let again = DocumentEngine::open(&path).unwrap();
    let mut labels: Vec<_> = again
        .annotations
        .iter()
        .map(|annot| match &annot.kind {
            AnnotKind::Foreign {
                kind: ForeignKind::Underline,
                ..
            } => "underline",
            AnnotKind::Highlight { .. } => "highlight",
            _ => "other",
        })
        .collect();
    labels.sort_unstable();
    assert_eq!(labels, ["highlight", "underline"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn markup_underline_strikeout_squiggly_roundtrip() {
    let dir = std::env::temp_dir().join(format!(
        "marker-markup-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.pdf");
    sample_pdf(&path);

    let loaded = DocumentEngine::open(&path).unwrap();
    let mut engine = loaded.engine;
    let before = engine.render_tile(0, 1.5, 0, 0).unwrap().unwrap();
    let blue_before = count_channel(&before.pixels, 2);

    let underline_quads = vec![PdfRect::new(72.0, 80.0, 220.0, 102.0)];
    let strike_quads = vec![PdfRect::new(72.0, 120.0, 240.0, 142.0)];
    let squiggly_quads = vec![
        PdfRect::new(72.0, 160.0, 180.0, 182.0),
        PdfRect::new(190.0, 160.0, 300.0, 182.0),
    ];
    let underline_color = Rgb::new(28, 78, 186);
    let strike_color = Rgb::new(186, 36, 36);
    let squiggly_color = Rgb::new(22, 122, 58);

    let mut session = crate::annot::Session::from_imported(loaded.annotations);
    session.insert(
        0,
        AnnotKind::Markup {
            style: MarkupStyle::Underline,
            quads: underline_quads.clone(),
            color: underline_color,
        },
    );
    session.insert(
        0,
        AnnotKind::Markup {
            style: MarkupStyle::StrikeOut,
            quads: strike_quads.clone(),
            color: strike_color,
        },
    );
    session.insert(
        0,
        AnnotKind::Markup {
            style: MarkupStyle::Squiggly,
            quads: squiggly_quads.clone(),
            color: squiggly_color,
        },
    );
    engine
        .save(&SaveSnapshot {
            upserts: session.cloned_annotations(),
            deletes: Vec::new(),
            math_pdfs: HashMap::new(),
            inline_math: Vec::new(),
            rich_text_parents: Vec::new(),
        })
        .unwrap();

    let mut again = DocumentEngine::open(&path).unwrap();
    let mut found = Vec::new();
    for annot in &again.annotations {
        assert!(!annot.dirty, "imported native markup should be clean");
        let AnnotKind::Markup {
            style,
            quads,
            color,
        } = &annot.kind
        else {
            panic!("expected native markup, got {:?}", annot.kind);
        };
        found.push((*style, quads.clone(), *color));
    }
    found.sort_by_key(|(style, _, _)| match style {
        MarkupStyle::Underline => 0,
        MarkupStyle::StrikeOut => 1,
        MarkupStyle::Squiggly => 2,
    });
    assert_eq!(found.len(), 3);
    assert_eq!(found[0].0, MarkupStyle::Underline);
    assert_eq!(found[1].0, MarkupStyle::StrikeOut);
    assert_eq!(found[2].0, MarkupStyle::Squiggly);
    assert_eq!(found[0].2, underline_color);
    assert_eq!(found[1].2, strike_color);
    assert_eq!(found[2].2, squiggly_color);
    quads_near(&found[0].1, &underline_quads);
    quads_near(&found[1].1, &strike_quads);
    quads_near(&found[2].1, &squiggly_quads);

    let after = again.engine.render_tile(0, 1.5, 0, 0).unwrap().unwrap();
    let blue_after = count_channel(&after.pixels, 2);
    assert!(
        blue_after < blue_before + 40,
        "tile render picked up underline pixels: before {blue_before} after {blue_after}"
    );

    // Editing the color rewrites the same annotation instead of leaving it foreign.
    let mut session = crate::annot::Session::from_imported(again.annotations);
    let edited = Rgb::new(10, 20, 30);
    let underline_id = session
        .annotations
        .iter()
        .find(|annot| {
            matches!(
                annot.kind,
                AnnotKind::Markup {
                    style: MarkupStyle::Underline,
                    ..
                }
            )
        })
        .map(|annot| annot.id)
        .unwrap();
    let underline = session.get_mut(underline_id).unwrap();
    if let AnnotKind::Markup { color, .. } = &mut underline.kind {
        *color = edited;
    }
    underline.dirty = true;
    let underline = underline.clone();
    let mut engine = again.engine;
    engine
        .save(&SaveSnapshot {
            upserts: vec![underline],
            deletes: Vec::new(),
            math_pdfs: HashMap::new(),
            inline_math: Vec::new(),
            rich_text_parents: Vec::new(),
        })
        .unwrap();
    let edited_doc = DocumentEngine::open(&path).unwrap();
    let colors: Vec<_> = edited_doc
        .annotations
        .iter()
        .map(|annot| match &annot.kind {
            AnnotKind::Markup {
                style: MarkupStyle::Underline,
                color,
                ..
            } => *color,
            AnnotKind::Markup { .. } => Rgb::new(0, 0, 0),
            other => panic!("markup became {other:?}"),
        })
        .collect();
    assert!(
        colors.contains(&edited),
        "re-saved underline color {colors:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn quads_near(got: &[PdfRect], expect: &[PdfRect]) {
    assert_eq!(got.len(), expect.len(), "quads {got:?} vs {expect:?}");
    for (got, expect) in got.iter().zip(expect) {
        for (a, b) in [
            (got.x0, expect.x0),
            (got.y0, expect.y0),
            (got.x1, expect.x1),
            (got.y1, expect.y1),
        ] {
            assert!((a - b).abs() < 0.8, "{got:?} vs {expect:?}");
        }
    }
}

fn count_channel(pixels: &[u8], channel: usize) -> usize {
    pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[channel] > 180 && px[(channel + 1) % 3] < 80 && px[(channel + 2) % 3] < 80)
        .count()
}

#[test]
fn saved_highlight_appearance_uses_multiply_blend() {
    let dir = std::env::temp_dir().join(format!("marker-hl-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("highlight.pdf");
    sample_pdf(&path);

    let loaded = DocumentEngine::open(&path).unwrap();
    let mut engine = loaded.engine;
    let glyphs = engine.glyphs(0).unwrap();
    let mut session = crate::annot::Session::from_imported(loaded.annotations);
    session.insert(
        0,
        AnnotKind::Highlight {
            quads: glyphs.iter().map(|g| g.bounds).collect(),
            color: Rgb::new(255, 214, 0),
        },
    );
    engine
        .save(&SaveSnapshot {
            upserts: session.cloned_annotations(),
            deletes: Vec::new(),
            math_pdfs: HashMap::new(),
            inline_math: Vec::new(),
            rich_text_parents: Vec::new(),
        })
        .unwrap();

    let bytes = std::fs::read(&path).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("/Multiply") || text.contains("/BM/Multiply"),
        "saved Highlight appearance should use Multiply blend mode"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
