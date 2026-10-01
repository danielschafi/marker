use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use typst::foundations::{Dict, IntoValue};
use typst_as_lib::typst_kit_options::TypstKitFontOptions;
use typst_as_lib::{TypstAsLibError, TypstEngine};
use typst_layout::PagedDocument;

use crate::geom::{normalize_latex, Rgb};

const TEMPLATE: &str = r#"
#import sys: inputs
#set page(width: auto, height: auto, margin: (x: 6pt, y: 5pt), fill: none)
#set text(size: inputs.size * 1pt, fill: rgb(inputs.fill), top-edge: "ascender", bottom-edge: "descender")
#eval(str(inputs.wrapped), mode: "markup")
"#;

/// Cycle-able LaTeX starters for the math editor (Tab / Shift+Tab).
pub const MATH_TEMPLATES: &[&str] = &[
    "",
    r"\frac{a}{b}",
    r"x = \frac{-b \pm \sqrt{b^2 - 4ac}}{2a}",
    r"\begin{aligned}
a &= b \\
c &= d
\end{aligned}",
    r"\begin{bmatrix}
a & b \\
c & d
\end{bmatrix}",
    r"\nabla^2 f = \begin{bmatrix}
f_{11} & f_{12} & \dots & f_{1n} \\
\vdots & \vdots & \ddots & \vdots \\
f_{n1} & f_{n2} & \dots & f_{nn}
\end{bmatrix}",
];

pub fn cycle_math_template(current: &str) -> &'static str {
    cycle_math_template_by(current, 1)
}

/// Cycle math presets. Positive `delta` moves forward (Tab); negative moves
/// backward (Shift+Tab).
pub fn cycle_math_template_by(current: &str, delta: isize) -> &'static str {
    let trimmed = current.trim();
    let idx = MATH_TEMPLATES
        .iter()
        .position(|t| t.trim() == trimmed)
        .unwrap_or(0);
    let len = MATH_TEMPLATES.len() as isize;
    let next = (idx as isize + delta).rem_euclid(len) as usize;
    MATH_TEMPLATES[next]
}

pub fn wants_display_math(latex: &str) -> bool {
    let s = latex.trim();
    s.contains(r"\begin{")
        || s.contains(r"\\")
        || s.contains('\n')
        || s.starts_with(r"\[")
        || (s.starts_with("$$") && s.ends_with("$$"))
}

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
    pub fn spawn(ctx: egui::Context) -> Self {
        let (job_tx, job_rx) = mpsc::channel();
        let (reply_tx, reply_rx) = mpsc::channel();
        thread::Builder::new()
            .name("marker-math".into())
            .spawn(move || math_loop(ctx, job_rx, reply_tx))
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

fn reply(ctx: &egui::Context, replies: &Sender<MathRender>, msg: MathRender) {
    let _ = replies.send(msg);
    ctx.request_repaint();
}

fn math_loop(ctx: egui::Context, jobs: Receiver<Job>, replies: Sender<MathRender>) {
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
                            reply(&ctx, &replies, to_reply(gen, id, req, rendered));
                        }
                    }
                }
                let (gen, source, size, color, req) = current;
                let rendered = render_equation(
                    engine.get_or_insert_with(build_engine),
                    &source,
                    size,
                    color,
                );
                reply(&ctx, &replies, to_reply(gen, id, req, rendered));
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
    let display = wants_display_math(source) || wants_display_math(&latex);
    let expr = match mitex::convert_math(&latex, None) {
        Ok(expr) => mitex_to_typst(&expr),
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
    let wrapped = if display {
        format!("$ {expr} $")
    } else {
        format!("${expr}$")
    };

    let mut inputs = Dict::new();
    inputs.insert("wrapped".into(), wrapped.into_value());
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

/// MiTeX emits helpers (`mitexsqrt`, `bmatrix`, `zws`, `aligned`, …) that only
/// exist in the MiTeX Typst package. Rewrite them to native Typst math so we
/// stay offline.
fn mitex_to_typst(expr: &str) -> String {
    let mut s = expr.to_string();
    s = s.replace(" zws ", " ");
    s = s.replace("zws", "");
    s = s.replace("dots.h.c", "dots.c");
    s = s.replace("dots.h", "dots");
    // Roots: `\sqrt{x}` → mitexsqrt(x); `\sqrt[n]{x}` → mitexsqrt(\[n\], x).
    s = rewrite_call(&s, "mitexsqrt", rewrite_mitexsqrt);
    s = rewrite_call(&s, "mitexmathbf", |args| format!("bold(upright({args}))"));
    s = rewrite_call(&s, "mitexoverbrace", |args| format!("overbrace({args})"));
    s = rewrite_call(&s, "mitexunderbrace", |args| format!("underbrace({args})"));
    s = rewrite_call(&s, "operatorname", |args| {
        let name: String = args.split_whitespace().collect();
        format!("op(\"{name}\")")
    });
    // Prefer display-friendly matrix delimiters and unwrap alignment envs.
    s = rewrite_call(&s, "bmatrix", |args| format!("mat(delim: \"[\", {args})"));
    s = rewrite_call(&s, "Bmatrix", |args| format!("mat(delim: \"{{\", {args})"));
    s = rewrite_call(&s, "pmatrix", |args| format!("mat(delim: \"(\", {args})"));
    s = rewrite_call(&s, "vmatrix", |args| format!("mat(delim: \"|\", {args})"));
    s = rewrite_call(&s, "Vmatrix", |args| format!("mat(delim: \"||\", {args})"));
    s = rewrite_call(&s, "matrix", |args| format!("mat({args})"));
    s = rewrite_call(&s, "aligned", |args| args);
    s = rewrite_call(&s, "alignedat", |args| strip_first_arg(args));
    s = rewrite_call(&s, "align", |args| args);
    s = rewrite_call(&s, "alignat", |args| strip_first_arg(args));
    s = rewrite_call(&s, "gather", |args| args);
    s = rewrite_call(&s, "gathered", |args| args);
    s = rewrite_call(&s, "split", |args| args);
    collapse_ws(&s)
}

fn rewrite_mitexsqrt(args: String) -> String {
    let trimmed = args.trim();
    // Optional index from `\sqrt[n]{...}` arrives as `\[n\]`, radicand.
    if let Some(rest) = trimmed.strip_prefix(r"\[") {
        if let Some((idx_body, after)) = rest.split_once(r"\]") {
            let idx = idx_body.trim();
            let radicand = after
                .trim()
                .strip_prefix(',')
                .map(str::trim)
                .unwrap_or("")
                .to_string();
            return format!("root({idx}, {radicand})");
        }
    }
    format!("sqrt({trimmed})")
}

fn strip_first_arg(args: String) -> String {
    // alignedat/alignat lead with a column count: `2, a &= b \ c &= d`
    if let Some((_, rest)) = args.split_once(',') {
        rest.trim().to_string()
    } else {
        args
    }
}

fn rewrite_call(input: &str, name: &str, map: impl Fn(String) -> String) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    let name_bytes = name.as_bytes();
    while i < bytes.len() {
        if bytes[i..].starts_with(name_bytes) {
            let after = i + name_bytes.len();
            let boundary_ok = after >= bytes.len()
                || !bytes[after].is_ascii_alphanumeric() && bytes[after] != b'_';
            if boundary_ok {
                let mut j = after;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'(' {
                    if let Some((args, end)) = take_parens(input, j) {
                        out.push_str(&map(args));
                        i = end;
                        continue;
                    }
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn take_parens(input: &str, open: usize) -> Option<(String, usize)> {
    let bytes = input.as_bytes();
    if open >= bytes.len() || bytes[open] != b'(' {
        return None;
    }
    let mut depth = 0i32;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    let args = input[open + 1..i].trim().to_string();
                    return Some((args, i + 1));
                }
            }
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim().to_string()
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

    #[test]
    fn renders_bmatrix_with_ellipsis() {
        let latex = r"\nabla^2 f = \begin{bmatrix} f_{11} & f_{12} & \dots & f_{1n} \\ \vdots & \vdots & \ddots & \vdots \\ f_{n1} & f_{n2} & \dots & f_{nn} \end{bmatrix}";
        let rendered = render_equation_blocking(latex, 11.0, Rgb::new(200, 40, 40));
        assert!(
            rendered.error.is_none(),
            "render error: {:?}",
            rendered.error
        );
        assert!(rendered.width_pt > 10.0 && rendered.height_pt > 10.0);
        let preview = rendered.preview.expect("preview");
        let opaque = preview
            .pixels
            .chunks_exact(4)
            .filter(|px| px[3] > 20)
            .count();
        assert!(opaque > 200, "matrix should paint visible ink, got {opaque}");
    }

    #[test]
    fn renders_aligned_multiline() {
        let latex = r"\begin{aligned} a &= b \\ c &= d \end{aligned}";
        let rendered = render_equation_blocking(latex, 14.0, Rgb::new(0, 0, 0));
        assert!(
            rendered.error.is_none(),
            "render error: {:?}",
            rendered.error
        );
        assert!(rendered.height_pt > rendered.width_pt * 0.2);
    }

    #[test]
    fn mitex_rewrite_maps_matrix_helpers() {
        let expr = r"bmatrix( f _(1 1 ) zws , dots.h  zws ; dots.v  zws , dots.down  )";
        let out = mitex_to_typst(expr);
        assert!(out.contains("mat(delim: \"[\""));
        assert!(!out.contains("bmatrix"));
        assert!(!out.contains("zws"));
        assert!(out.contains("dots"));
    }

    #[test]
    fn mitex_rewrite_maps_sqrt_helpers() {
        assert_eq!(mitex_to_typst("mitexsqrt(x )"), "sqrt(x)");
        assert_eq!(
            mitex_to_typst(r"mitexsqrt(\[3 \],x )"),
            "root(3, x)"
        );
        // Commas inside the radicand must not be treated as an optional index.
        assert_eq!(
            mitex_to_typst("mitexsqrt(f(a ,b ))"),
            "sqrt(f(a ,b ))"
        );
    }

    #[test]
    fn renders_sqrt_and_nth_root() {
        for latex in [r"\sqrt{x}", r"\sqrt{b^2 - 4ac}", r"\sqrt[3]{8}"] {
            let rendered = render_equation_blocking(latex, 16.0, Rgb::new(0, 0, 0));
            assert!(
                rendered.error.is_none(),
                "{latex}: {:?}",
                rendered.error
            );
            assert!(rendered.width_pt > 1.0 && rendered.height_pt > 1.0);
            assert!(rendered.pdf.is_some());
        }
    }

    #[test]
    fn renders_quadratic_formula_with_sqrt() {
        let latex = r"x = \frac{-b \pm \sqrt{b^2 - 4ac}}{2a}";
        let rendered = render_equation_blocking(latex, 14.0, Rgb::new(0, 0, 0));
        assert!(
            rendered.error.is_none(),
            "render error: {:?}",
            rendered.error
        );
        assert!(rendered.width_pt > 10.0);
    }

    #[test]
    fn renders_pmatrix() {
        let latex = r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}";
        let rendered = render_equation_blocking(latex, 14.0, Rgb::new(0, 0, 0));
        assert!(
            rendered.error.is_none(),
            "render error: {:?}",
            rendered.error
        );
        assert!(rendered.height_pt > 5.0);
    }

    #[test]
    fn renders_mathbf_and_operatorname() {
        for latex in [r"\mathbf{v}", r"\operatorname{tr}(A)"] {
            let rendered = render_equation_blocking(latex, 16.0, Rgb::new(0, 0, 0));
            assert!(
                rendered.error.is_none(),
                "{latex}: {:?}",
                rendered.error
            );
        }
    }

    #[test]
    fn template_cycle_wraps() {
        assert_eq!(cycle_math_template(""), MATH_TEMPLATES[1]);
        let last = MATH_TEMPLATES.last().unwrap();
        assert_eq!(cycle_math_template(last), MATH_TEMPLATES[0]);
        assert_eq!(cycle_math_template_by(MATH_TEMPLATES[1], -1), MATH_TEMPLATES[0]);
        assert_eq!(cycle_math_template_by("", -1), *last);
    }
}
