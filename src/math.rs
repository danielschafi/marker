use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use typst::foundations::{Dict, IntoValue};
use typst_as_lib::typst_kit_options::TypstKitFontOptions;
use typst_as_lib::{TypstAsLibError, TypstEngine};
use typst_layout::PagedDocument;

use crate::geom::{normalize_latex, Rgb};

const TEMPLATE: &str = r#"
#import sys: inputs
#set page(width: auto, height: auto, margin: (x: 2pt, y: 3pt), fill: none)
#set text(size: inputs.size * 1pt, fill: rgb(inputs.fill), top-edge: "ascender", bottom-edge: "descender")
#eval("$" + str(inputs.expr) + "$", mode: "markup")
"#;

pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

pub struct MathRender {
    pub gen: u64,
    pub id: u64,
    pub req: u64,
    pub preview: Option<RgbaImage>,
    pub pdf: Option<Vec<u8>>,
    pub width_pt: f32,
    pub height_pt: f32,
    pub error: Option<String>,
}

enum Job {
    Render {
        gen: u64,
        id: u64,
        req: u64,
        source: String,
        size: f32,
        color: Rgb,
    },
    Shutdown,
}

pub struct MathWorker {
    jobs: Sender<Job>,
    replies: Receiver<MathRender>,
}

impl MathWorker {
    pub fn spawn() -> Self {
        let (job_tx, job_rx) = mpsc::channel();
        let (reply_tx, reply_rx) = mpsc::channel();
        thread::Builder::new()
            .name("marker-math".into())
            .spawn(move || math_loop(job_rx, reply_tx))
            .expect("math thread");
        Self {
            jobs: job_tx,
            replies: reply_rx,
        }
    }

    pub fn request(&self, gen: u64, id: u64, req: u64, source: String, size: f32, color: Rgb) {
        let _ = self.jobs.send(Job::Render {
            gen,
            id,
            req,
            source,
            size,
            color,
        });
    }

    pub fn poll(&self) -> Vec<MathRender> {
        let mut out = Vec::new();
        while let Ok(reply) = self.replies.try_recv() {
            out.push(reply);
        }
        out
    }
}

impl Drop for MathWorker {
    fn drop(&mut self) {
        let _ = self.jobs.send(Job::Shutdown);
    }
}

fn math_loop(jobs: Receiver<Job>, replies: Sender<MathRender>) {
    let mut engine = None;
    while let Ok(job) = jobs.recv() {
        match job {
            Job::Shutdown => break,
            Job::Render {
                gen,
                id,
                req,
                source,
                size,
                color,
            } => {
                let mut current = (gen, source, size, color, req);
                while let Ok(next) = jobs.try_recv() {
                    match next {
                        Job::Shutdown => return,
                        Job::Render {
                            gen,
                            id: next_id,
                            req,
                            source,
                            size,
                            color,
                        } if next_id == id => {
                            current = (gen, source, size, color, req);
                        }
                        Job::Render {
                            gen,
                            id,
                            req,
                            source,
                            size,
                            color,
                        } => {
                            let rendered = render_equation(
                                engine.get_or_insert_with(build_engine),
                                &source,
                                size,
                                color,
                            );
                            let _ = replies.send(to_reply(gen, id, req, rendered));
                        }
                    }
                }
                let (gen, source, size, color, req) = current;
                let rendered =
                    render_equation(engine.get_or_insert_with(build_engine), &source, size, color);
                let _ = replies.send(to_reply(gen, id, req, rendered));
            }
        }
    }
}

fn to_reply(gen: u64, id: u64, req: u64, rendered: Rendered) -> MathRender {
    MathRender {
        gen,
        id,
        req,
        preview: rendered.preview,
        pdf: rendered.pdf,
        width_pt: rendered.width_pt,
        height_pt: rendered.height_pt,
        error: rendered.error,
    }
}

struct Rendered {
    preview: Option<RgbaImage>,
    pdf: Option<Vec<u8>>,
    width_pt: f32,
    height_pt: f32,
    error: Option<String>,
}

fn build_engine() -> TypstEngine<typst_as_lib::TypstTemplateMainFile> {
    TypstEngine::builder()
        .main_file(TEMPLATE)
        .search_fonts_with(
            TypstKitFontOptions::new()
                .include_system_fonts(true)
                .include_embedded_fonts(true),
        )
        .build()
}

#[cfg(test)]
fn render_equation_blocking(source: &str, size: f32, color: Rgb) -> Rendered {
    let engine = build_engine();
    render_equation(&engine, source, size, color)
}

fn render_equation(
    engine: &TypstEngine<typst_as_lib::TypstTemplateMainFile>,
    source: &str,
    size: f32,
    color: Rgb,
) -> Rendered {
    let latex = normalize_latex(source);
    if latex.is_empty() {
        return Rendered {
            preview: None,
            pdf: None,
            width_pt: 0.0,
            height_pt: 0.0,
            error: None,
        };
    }
    let expr = match mitex::convert_math(&latex, None) {
        Ok(expr) => expr,
        Err(err) => {
            return Rendered {
                preview: None,
                pdf: None,
                width_pt: 0.0,
                height_pt: 0.0,
                error: Some(err),
            };
        }
    };

    let mut inputs = Dict::new();
    inputs.insert("expr".into(), expr.into_value());
    inputs.insert("size".into(), f64::from(size.max(6.0)).into_value());
    inputs.insert("fill".into(), color.hex().into_value());

    let warned = engine.compile_with_input(inputs);
    let doc: PagedDocument = match warned.output {
        Ok(doc) => doc,
        Err(err) => {
            return Rendered {
                preview: None,
                pdf: None,
                width_pt: 0.0,
                height_pt: 0.0,
                error: Some(format_typst(&err)),
            };
        }
    };
    let Some(page) = doc.pages().first() else {
        return Rendered {
            preview: None,
            pdf: None,
            width_pt: 0.0,
            height_pt: 0.0,
            error: Some("equation produced no pages".into()),
        };
    };

    let svg = typst_svg::svg(
        page,
        &typst_svg::SvgOptions {
            render_bleed: false,
            pretty: false,
        },
    );
    let pdf = match typst_pdf::pdf(&doc, &typst_pdf::PdfOptions::default()) {
        Ok(bytes) => Some(bytes),
        Err(err) => {
            let (preview, width_pt, height_pt) = rasterize_svg(&svg);
            return Rendered {
                preview,
                pdf: None,
                width_pt,
                height_pt,
                error: Some(format_typst_diags(&err)),
            };
        }
    };

    let (preview, width_pt, height_pt) = rasterize_svg(&svg);
    Rendered {
        preview,
        pdf,
        width_pt,
        height_pt,
        error: None,
    }
}

fn rasterize_svg(svg: &str) -> (Option<RgbaImage>, f32, f32) {
    let Ok(tree) = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()) else {
        return (None, 0.0, 0.0);
    };
    let size = tree.size();
    // usvg reports CSS pixels (96 dpi). Convert to PDF points so the overlay matches.
    let width_pt = size.width() * 72.0 / 96.0;
    let height_pt = size.height() * 72.0 / 96.0;
    let scale = 3.0;
    let width = (size.width() * scale).ceil().max(1.0) as u32;
    let height = (size.height() * scale).ceil().max(1.0) as u32;
    let Some(mut pixmap) = resvg::tiny_skia::Pixmap::new(width, height) else {
        return (None, width_pt, height_pt);
    };
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    (
        Some(RgbaImage {
            width,
            height,
            pixels: pixmap.take(),
        }),
        width_pt.max(1.0),
        height_pt.max(1.0),
    )
}

fn format_typst(err: &TypstAsLibError) -> String {
    match err {
        TypstAsLibError::TypstSource(diags) => format_typst_diags(diags),
        other => other.to_string(),
    }
}

fn format_typst_diags(diags: &[typst::diag::SourceDiagnostic]) -> String {
    if diags.is_empty() {
        return "equation failed to compile".into();
    }
    diags
        .iter()
        .map(|diag| diag.message.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_fraction_to_pdf_and_preview() {
        let rendered = render_equation_blocking(r"\frac{1}{2}", 16.0, Rgb::new(0, 0, 0));
        assert!(
            rendered.error.is_none(),
            "render error: {:?}",
            rendered.error
        );
        let pdf = rendered.pdf.expect("pdf bytes");
        assert!(pdf.starts_with(b"%PDF"));
        assert!(rendered.width_pt > 1.0 && rendered.height_pt > 1.0);
        let preview = rendered.preview.expect("preview");
        assert!(preview.width > 0 && preview.height > 0);
        let transparent = preview.pixels.chunks_exact(4).any(|px| px[3] == 0);
        assert!(transparent, "equation background should be transparent");
        let image_aspect = preview.width as f32 / preview.height as f32;
        let box_aspect = rendered.width_pt / rendered.height_pt;
        assert!(
            (image_aspect - box_aspect).abs() < 0.08,
            "preview aspect {image_aspect} diverges from {box_aspect}"
        );
    }
}
