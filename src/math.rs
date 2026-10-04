use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use typst::foundations::{Dict, IntoValue};
use typst_as_lib::typst_kit_options::TypstKitFontOptions;
use typst_as_lib::{TypstAsLibError, TypstEngine};
use typst_layout::PagedDocument;

use crate::geom::{normalize_latex, Rgb};

#[path = "rich_text.rs"]
pub mod rich_text;

/// Live-lane trailing debounce. Latest edit wins; this only limits texture churn.
const LIVE_DEBOUNCE: Duration = Duration::from_millis(40);
/// Base CSS-pixel multiplier for the first preview raster (√2 buckets step up from here).
const RASTER_SCALE: f32 = 3.0;
/// Re-raster when `zoom × ppp` exceeds the cached scale by this factor.
const RASTER_UPGRADE: f32 = 1.3;

pub const MATH_CACHE_CAP: usize = 256;
pub const MATH_CACHE_BYTE_CAP: usize = 32 * 1024 * 1024;

/// Cache key for one equation. No annotation id, so identical spans share a render.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MathKey {
    pub source: String,
    pub display: bool,
    size_milli: u32,
    color: (u8, u8, u8),
}

impl MathKey {
    pub fn new(source: &str, display: bool, size_pt: f32, color: Rgb) -> Self {
        Self {
            source: normalize_latex(source),
            display,
            size_milli: (size_pt.max(0.0) * 1000.0).round() as u32,
            color: (color.r, color.g, color.b),
        }
    }
}

/// What a save should do with one equation's PDF bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdfPlan {
    /// Raster or error is enough, and PDF bytes are already stored (or the
    /// equation failed and will not produce any).
    Have,
    /// Preview is ready and the PDF still has to be requested.
    Request,
    /// Preview has not finished; wait, do not start a second preview job.
    Wait,
}

pub fn pdf_plan(pending: bool, ready: bool, has_pdf: bool, failed: bool) -> PdfPlan {
    if failed || has_pdf {
        PdfPlan::Have
    } else if pending || !ready {
        PdfPlan::Wait
    } else {
        PdfPlan::Request
    }
}

/// Pick a √2 raster bucket at least as sharp as `needed` (`zoom × pixels_per_point`).
pub fn raster_scale_for(needed: f32) -> f32 {
    let needed = needed.max(0.5);
    if needed <= RASTER_SCALE {
        return RASTER_SCALE;
    }
    // steps of √2 above the base: 3, 3√2, 6, 6√2, …
    let steps = ((needed / RASTER_SCALE).log2() * 2.0).ceil().max(0.0) as i32;
    RASTER_SCALE * 2f32.powf(steps as f32 / 2.0)
}

/// True when the cached raster is too soft for the current screen scale.
pub fn needs_sharper_raster(cached: f32, needed: f32) -> bool {
    needed > cached.max(0.5) * RASTER_UPGRADE
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MathLane {
    /// The span currently being edited. A newer job replaces the older one.
    Live,
    /// Visible equations. FIFO, and a key already queued does not run twice.
    Background,
}

#[derive(Clone, Debug)]
pub struct ScheduledJob<T> {
    pub key: MathKey,
    pub lane: MathLane,
    pub payload: T,
}

/// Two lanes in front of the math thread. Tested without Typst.
pub struct MathScheduler<T> {
    live: Option<ScheduledJob<T>>,
    live_ready: Option<Instant>,
    background: VecDeque<ScheduledJob<T>>,
    background_keys: HashSet<MathKey>,
    debounce: Duration,
}

impl<T> MathScheduler<T> {
    pub fn new(debounce: Duration) -> Self {
        Self {
            live: None,
            live_ready: None,
            background: VecDeque::new(),
            background_keys: HashSet::new(),
            debounce,
        }
    }

    pub fn push(&mut self, job: ScheduledJob<T>, now: Instant) {
        match job.lane {
            MathLane::Live => {
                self.live = Some(job);
                self.live_ready = Some(now + self.debounce);
            }
            MathLane::Background => {
                if !self.background_keys.insert(job.key.clone()) {
                    return;
                }
                self.background.push_back(job);
            }
        }
    }

    /// Next job that should run at `now`.
    ///
    /// A due live job wins. Until then, background jobs keep draining so a
    /// keystroke cannot cancel equations that are not being edited.
    pub fn poll(&mut self, now: Instant) -> Option<ScheduledJob<T>> {
        if self.live_ready.is_some_and(|ready| now >= ready) {
            self.live_ready = None;
            return self.live.take();
        }
        let job = self.background.pop_front()?;
        self.background_keys.remove(&job.key);
        Some(job)
    }

    pub fn live_deadline(&self) -> Option<Instant> {
        self.live_ready
    }
}

struct Entry<T> {
    last_used: Cell<u64>,
    kind: EntryKind<T>,
}

#[derive(Debug)]
pub enum EntryKind<T> {
    Pending { req: u64 },
    Ready { value: T, bytes: usize },
    Error { message: String },
}

/// Bounded render cache. `T` is the ready payload (texture lives in the app).
pub struct MathCache<T> {
    entries: HashMap<MathKey, Entry<T>>,
    clock: Cell<u64>,
    bytes: usize,
    revision: u64,
    cap: usize,
    byte_cap: usize,
}

impl<T> MathCache<T> {
    pub fn new() -> Self {
        Self::with_limits(MATH_CACHE_CAP, MATH_CACHE_BYTE_CAP)
    }

    pub fn with_limits(cap: usize, byte_cap: usize) -> Self {
        Self {
            entries: HashMap::new(),
            clock: Cell::new(0),
            bytes: 0,
            revision: 0,
            cap: cap.max(1),
            byte_cap,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn get(&self, key: &MathKey) -> Option<&EntryKind<T>> {
        let entry = self.entries.get(key)?;
        self.touch(entry);
        Some(&entry.kind)
    }

    pub fn insert_pending(&mut self, key: MathKey, req: u64) {
        self.bump_revision();
        self.remove_bytes(&key);
        let entry = Entry {
            last_used: Cell::new(0),
            kind: EntryKind::Pending { req },
        };
        self.touch(&entry);
        self.entries.insert(key, entry);
        self.evict();
    }

    pub fn insert_ready(&mut self, key: MathKey, value: T, bytes: usize) {
        self.bump_revision();
        self.remove_bytes(&key);
        self.bytes += bytes;
        let entry = Entry {
            last_used: Cell::new(0),
            kind: EntryKind::Ready { value, bytes },
        };
        self.touch(&entry);
        self.entries.insert(key, entry);
        self.evict();
    }

    pub fn insert_error(&mut self, key: MathKey, message: String) {
        self.bump_revision();
        self.remove_bytes(&key);
        let entry = Entry {
            last_used: Cell::new(0),
            kind: EntryKind::Error { message },
        };
        self.touch(&entry);
        self.entries.insert(key, entry);
        self.evict();
    }

    pub fn ready_mut(&mut self, key: &MathKey) -> Option<&mut T> {
        let entry = self.entries.get_mut(key)?;
        match &mut entry.kind {
            EntryKind::Ready { value, .. } => Some(value),
            EntryKind::Pending { .. } | EntryKind::Error { .. } => None,
        }
    }

    /// Update the byte weight of a ready entry after an in-place texture replace.
    pub fn set_ready_bytes(&mut self, key: &MathKey, new_bytes: usize) {
        let Some(entry) = self.entries.get_mut(key) else {
            return;
        };
        let EntryKind::Ready { bytes, .. } = &mut entry.kind else {
            return;
        };
        self.bytes = self.bytes.saturating_sub(*bytes).saturating_add(new_bytes);
        *bytes = new_bytes;
        self.evict();
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    fn touch(&self, entry: &Entry<T>) {
        let next = self.clock.get().wrapping_add(1);
        self.clock.set(next);
        entry.last_used.set(next);
    }

    fn remove_bytes(&mut self, key: &MathKey) {
        let Some(entry) = self.entries.get(key) else {
            return;
        };
        if let EntryKind::Ready { bytes, .. } = &entry.kind {
            self.bytes = self.bytes.saturating_sub(*bytes);
        }
    }

    fn evict(&mut self) {
        while self.over_cap() {
            let Some(oldest) = self.oldest_key() else {
                break;
            };
            // A single oversized texture stays; dropping it would render nothing.
            if self.entries.len() == 1 {
                break;
            }
            self.remove_bytes(&oldest);
            self.entries.remove(&oldest);
            self.bump_revision();
        }
    }

    fn over_cap(&self) -> bool {
        self.entries.len() > self.cap || (self.bytes > self.byte_cap && self.entries.len() > 1)
    }

    fn oldest_key(&self) -> Option<MathKey> {
        self.entries
            .iter()
            .min_by_key(|(_, entry)| entry.last_used.get())
            .map(|(key, _)| key.clone())
    }
}

impl<T> Default for MathCache<T> {
    fn default() -> Self {
        Self::new()
    }
}

const TEMPLATE: &str = r#"
#import sys: inputs
#set page(width: auto, height: auto, margin: (x: 6pt, y: 5pt), fill: none)
#set text(size: inputs.size * 1pt, fill: rgb(inputs.fill), top-edge: "ascender", bottom-edge: "descender")
#eval(str(inputs.wrapped), mode: "markup")
"#;

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
    /// When set, this render belongs to an inline `$...$` island inside a Text annot.
    pub span_key: Option<u64>,
    pub req: u64,
    pub key: MathKey,
    pub preview: Option<RgbaImage>,
    pub svg: Option<String>,
    pub pdf: Option<Vec<u8>>,
    pub width_pt: f32,
    pub height_pt: f32,
    /// Distance from the top of the raster to the equation baseline, when Typst set one.
    pub baseline_pt: Option<f32>,
    pub raster_scale: f32,
    pub error: Option<String>,
    pub want_pdf: bool,
}

struct RenderRequest {
    gen: u64,
    id: u64,
    span_key: Option<u64>,
    req: u64,
    source: String,
    size: f32,
    color: Rgb,
    display: bool,
    lane: MathLane,
    want_pdf: bool,
}

/// Re-raster an existing SVG at a sharper scale. No Typst, no PDF.
struct RerasterRequest {
    gen: u64,
    id: u64,
    span_key: Option<u64>,
    req: u64,
    key: MathKey,
    svg: String,
    raster_scale: f32,
    width_pt: f32,
    height_pt: f32,
    baseline_pt: Option<f32>,
}

enum Job {
    Render(RenderRequest),
    Reraster(RerasterRequest),
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
        let display = wants_display_math(&source);
        self.request_span(
            gen,
            id,
            None,
            req,
            source,
            size,
            color,
            display,
            MathLane::Live,
            false,
        );
    }

    /// `lane` selects live vs background scheduling. `want_pdf` skips Typst PDF
    /// export when false so previews stay cheap until a save asks for bytes.
    #[allow(clippy::too_many_arguments)]
    pub fn request_span(
        &self,
        gen: u64,
        id: u64,
        span_key: Option<u64>,
        req: u64,
        source: String,
        size: f32,
        color: Rgb,
        display: bool,
        lane: MathLane,
        want_pdf: bool,
    ) {
        let _ = self.jobs.send(Job::Render(RenderRequest {
            gen,
            id,
            span_key,
            req,
            source,
            size,
            color,
            display,
            lane,
            want_pdf,
        }));
    }

    /// Re-raster a cached SVG at `raster_scale`. Coalesced latest-wins per key on
    /// the worker so a zoom burst does not enqueue one job per frame.
    #[allow(clippy::too_many_arguments)]
    pub fn request_reraster(
        &self,
        gen: u64,
        id: u64,
        span_key: Option<u64>,
        req: u64,
        key: MathKey,
        svg: String,
        raster_scale: f32,
        width_pt: f32,
        height_pt: f32,
        baseline_pt: Option<f32>,
    ) {
        let _ = self.jobs.send(Job::Reraster(RerasterRequest {
            gen,
            id,
            span_key,
            req,
            key,
            svg,
            raster_scale,
            width_pt,
            height_pt,
            baseline_pt,
        }));
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
    // Pay for fonts and the engine before the first equation, not during it.
    let engine = build_engine();
    let mut sched: MathScheduler<RenderRequest> = MathScheduler::new(LIVE_DEBOUNCE);
    // Latest sharper raster per equation key; zoom bursts replace, not stack.
    let mut reraster: HashMap<MathKey, RerasterRequest> = HashMap::new();
    loop {
        if let Some(job) = sched.poll(Instant::now()) {
            run_job(&ctx, &replies, &engine, job.payload);
            continue;
        }
        if let Some(key) = reraster.keys().next().cloned() {
            if let Some(request) = reraster.remove(&key) {
                run_reraster(&ctx, &replies, request);
                continue;
            }
        }
        let incoming = if let Some(deadline) = sched.live_deadline() {
            let wait = deadline.saturating_duration_since(Instant::now());
            match jobs.recv_timeout(wait) {
                Ok(job) => Some(job),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else if !reraster.is_empty() {
            // Drain sharper rasters without blocking new Typst jobs.
            match jobs.try_recv() {
                Ok(job) => Some(job),
                Err(_) => None,
            }
        } else {
            match jobs.recv() {
                Ok(job) => Some(job),
                Err(_) => break,
            }
        };
        let Some(incoming) = incoming else {
            continue;
        };
        match incoming {
            Job::Shutdown => break,
            Job::Render(request) => {
                let key = MathKey::new(&request.source, request.display, request.size, request.color);
                let lane = request.lane;
                sched.push(
                    ScheduledJob {
                        key,
                        lane,
                        payload: request,
                    },
                    Instant::now(),
                );
            }
            Job::Reraster(request) => {
                reraster.insert(request.key.clone(), request);
            }
        }
    }
}

fn run_job(
    ctx: &egui::Context,
    replies: &Sender<MathRender>,
    engine: &TypstEngine<typst_as_lib::TypstTemplateMainFile>,
    request: RenderRequest,
) {
    let rendered = render_equation(
        engine,
        &request.source,
        request.size,
        request.color,
        request.want_pdf,
        RASTER_SCALE,
    );
    reply(ctx, replies, to_reply(request, rendered));
}

fn run_reraster(ctx: &egui::Context, replies: &Sender<MathRender>, request: RerasterRequest) {
    let (preview, width_pt, height_pt) = rasterize_svg(&request.svg, request.raster_scale);
    // Keep the Typst page size from the first render; only the bitmap changes.
    let width_pt = if request.width_pt > 0.0 {
        request.width_pt
    } else {
        width_pt
    };
    let height_pt = if request.height_pt > 0.0 {
        request.height_pt
    } else {
        height_pt
    };
    reply(
        ctx,
        replies,
        MathRender {
            gen: request.gen,
            id: request.id,
            span_key: request.span_key,
            req: request.req,
            key: request.key,
            preview,
            svg: Some(request.svg),
            pdf: None,
            width_pt,
            height_pt,
            baseline_pt: request.baseline_pt,
            raster_scale: request.raster_scale,
            error: None,
            want_pdf: false,
        },
    );
}

fn to_reply(request: RenderRequest, rendered: Rendered) -> MathRender {
    let key = MathKey::new(
        &request.source,
        request.display,
        request.size,
        request.color,
    );
    MathRender {
        gen: request.gen,
        id: request.id,
        span_key: request.span_key,
        req: request.req,
        key,
        preview: rendered.preview,
        svg: rendered.svg,
        pdf: rendered.pdf,
        width_pt: rendered.width_pt,
        height_pt: rendered.height_pt,
        baseline_pt: rendered.baseline_pt,
        raster_scale: rendered.raster_scale,
        error: rendered.error,
        want_pdf: request.want_pdf,
    }
}

struct Rendered {
    preview: Option<RgbaImage>,
    svg: Option<String>,
    pdf: Option<Vec<u8>>,
    width_pt: f32,
    height_pt: f32,
    baseline_pt: Option<f32>,
    raster_scale: f32,
    error: Option<String>,
}

fn blank_render() -> Rendered {
    Rendered {
        preview: None,
        svg: None,
        pdf: None,
        width_pt: 0.0,
        height_pt: 0.0,
        baseline_pt: None,
        raster_scale: 0.0,
        error: None,
    }
}

fn failed_render(error: impl Into<String>) -> Rendered {
    Rendered {
        error: Some(error.into()),
        ..blank_render()
    }
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
    render_equation(&engine, source, size, color, true, RASTER_SCALE)
}

fn render_equation(
    engine: &TypstEngine<typst_as_lib::TypstTemplateMainFile>,
    source: &str,
    size: f32,
    color: Rgb,
    want_pdf: bool,
    raster_scale: f32,
) -> Rendered {
    let latex = normalize_latex(source);
    if latex.is_empty() {
        return blank_render();
    }
    let display = wants_display_math(source) || wants_display_math(&latex);
    let expr = match mitex::convert_math(&latex, None) {
        Ok(expr) => mitex_to_typst(&expr),
        Err(err) => return failed_render(err),
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
        Err(err) => return failed_render(format_typst(&err)),
    };
    let Some(page) = doc.pages().first() else {
        return failed_render("equation produced no pages");
    };

    let svg = typst_svg::svg(
        page,
        &typst_svg::SvgOptions {
            render_bleed: false,
            pretty: false,
        },
    );
    let baseline_pt = equation_baseline_pt(page);
    let scale = raster_scale.max(RASTER_SCALE);
    let pdf = if want_pdf {
        match typst_pdf::pdf(&doc, &typst_pdf::PdfOptions::default()) {
            Ok(bytes) => Some(bytes),
            Err(err) => {
                let (preview, width_pt, height_pt) = rasterize_svg(&svg, scale);
                return Rendered {
                    preview,
                    svg: Some(svg),
                    pdf: None,
                    width_pt,
                    height_pt,
                    baseline_pt,
                    raster_scale: scale,
                    error: Some(format_typst_diags(&err)),
                };
            }
        }
    } else {
        None
    };

    let (preview, width_pt, height_pt) = rasterize_svg(&svg, scale);
    Rendered {
        preview,
        svg: Some(svg),
        pdf,
        width_pt,
        height_pt,
        baseline_pt,
        raster_scale: scale,
        error: None,
    }
}

/// Baseline from the top of the page frame, in PDF points. `None` when Typst
/// left the frame on its default (bottom) baseline — paint then centers.
fn equation_baseline_pt(page: &typst_layout::Page) -> Option<f32> {
    let frame = &page.frame;
    if !frame.has_baseline() {
        return None;
    }
    let baseline = frame.baseline().to_pt() as f32;
    let height = frame.size().y.to_pt() as f32;
    if baseline > 0.5 && baseline < height - 0.25 {
        Some(baseline)
    } else {
        None
    }
}

/// MiTeX emits helpers (`mitexsqrt`, `bmatrix`, `zws`, `aligned`, …) that only
/// exist in the MiTeX Typst package, plus occasional raw LaTeX escapes (`\,`).
/// Rewrite them to native Typst math so we stay offline.
fn mitex_to_typst(expr: &str) -> String {
    let mut s = expr.to_string();
    // MiTeX often turns a literal comma into LaTeX thin-space `\,`, which Typst
    // rejects ("unknown symbol modifier"). Restore a comma; other TeX spaces map
    // to Typst math spacers.
    s = s.replace(r"\,", ",");
    s = s.replace(r"\;", " thick ");
    s = s.replace(r"\:", " med ");
    s = s.replace(r"\>", " med ");
    s = s.replace(r"\!", "");
    s = s.replace(r"\ ", " ");
    s = s.replace(r"\quad", " quad ");
    s = s.replace(r"\qquad", " wide ");
    s = s.replace("negthinspace", "");
    s = s.replace(" zws ", " ");
    s = s.replace("zws", "");
    s = s.replace("dots.h.c", "dots.c");
    s = s.replace("dots.h", "dots");
    // `angle` is a Typst unit type (90deg), so MiTeX's `angle.l` / `angle.r`
    // resolve to that type and fail with "unknown symbol modifier". Use glyphs.
    s = s.replace("angle.l", "⟨");
    s = s.replace("angle.r", "⟩");
    // MiTeX's `diff` is not always in scope; Typst's partial symbol is.
    s = replace_ident(&s, "diff", "partial");
    // Delimiters if raw TeX escapes leak through.
    s = s.replace(r"\langle", "⟨");
    s = s.replace(r"\rangle", "⟩");
    s = s.replace(r"\lvert", "|");
    s = s.replace(r"\rvert", "|");
    s = s.replace(r"\lVert", "||");
    s = s.replace(r"\rVert", "||");
    s = s.replace(r"\lfloor", "⌊");
    s = s.replace(r"\rfloor", "⌋");
    s = s.replace(r"\lceil", "⌈");
    s = s.replace(r"\rceil", "⌉");
    // Roots: `\sqrt{x}` → mitexsqrt(x); `\sqrt[n]{x}` → mitexsqrt(\[n\], x).
    s = rewrite_call(&s, "mitexsqrt", rewrite_mitexsqrt);
    s = rewrite_call(&s, "mitexmathbf", |args| format!("bold(upright({args}))"));
    s = rewrite_call(&s, "mitexdisplaystyle", |args| args);
    s = rewrite_call(&s, "mitexdisplay", |args| args);
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
    // Strip leftover TeX command escapes that would poison Typst parsing.
    s = strip_tex_backslashes(&s);
    collapse_ws(&s)
}

/// Replace a bare identifier, not a prefix of a longer name.
fn replace_ident(input: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(at) = rest.find(from) {
        let before = &rest[..at];
        let after = &rest[at + from.len()..];
        let left_ok = before
            .chars()
            .last()
            .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_');
        let right_ok = after
            .chars()
            .next()
            .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_');
        out.push_str(before);
        if left_ok && right_ok {
            out.push_str(to);
        } else {
            out.push_str(from);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Drop a leading `\` before ASCII letters / punctuation that is still TeX-shaped.
/// Keeps Typst escapes like `\"` inside strings alone by only touching `\X` forms
/// that remain after MiTeX (e.g. `\]`, `\{`).
fn strip_tex_backslashes(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.peek().copied() {
                Some(next) if next == '"' || next == '\\' => {
                    // Typst string escape — keep.
                    out.push(ch);
                }
                Some(next) if next.is_ascii_alphabetic() => {
                    // Unknown TeX command residue: drop the slash, keep the name
                    // (often already invalid; better than "unknown symbol modifier").
                    continue;
                }
                Some(next) if matches!(next, '{' | '}' | '[' | ']' | '|' | ',' | ';' | ':' | '!' | ' ') =>
                {
                    // `\{` → `{`, `\,` already handled above; keep the following char.
                    continue;
                }
                _ => out.push(ch),
            }
        } else {
            out.push(ch);
        }
    }
    out
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
    let mut i = 0;
    while i < input.len() {
        if input[i..].starts_with(name) {
            let after = i + name.len();
            let boundary_ok = input[after..]
                .chars()
                .next()
                .is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_');
            if boundary_ok {
                let mut j = after;
                while let Some(ch) = input[j..].chars().next() {
                    if !ch.is_ascii_whitespace() {
                        break;
                    }
                    j += ch.len_utf8();
                }
                if input[j..].starts_with('(') {
                    if let Some((args, end)) = take_parens(input, j) {
                        out.push_str(&map(args));
                        i = end;
                        continue;
                    }
                }
            }
        }
        let ch = input[i..].chars().next().expect("i in range");
        out.push(ch);
        i += ch.len_utf8();
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

fn rasterize_svg(svg: &str, raster_scale: f32) -> (Option<RgbaImage>, f32, f32) {
    let Ok(tree) = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()) else {
        return (None, 0.0, 0.0);
    };
    let size = tree.size();
    // usvg reports CSS pixels (96 dpi). Convert to PDF points so the overlay matches.
    let width_pt = size.width() * 72.0 / 96.0;
    let height_pt = size.height() * 72.0 / 96.0;
    let scale = raster_scale.max(0.5);
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
        other => map_typst_error(&other.to_string()),
    }
}

fn format_typst_diags(diags: &[typst::diag::SourceDiagnostic]) -> String {
    if diags.is_empty() {
        return "equation failed to compile".into();
    }
    diags
        .iter()
        .map(|diag| map_typst_error(&diag.message.to_string()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Map Typst / MiTeX-flavored diagnostics back to LaTeX wording.
///
/// `unknown variable: mitexsqrt` → `unknown command \sqrt`. Unmapped text passes through.
pub fn map_typst_error(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return trimmed.to_string();
    }
    // Multi-line diagnostics: map each line independently.
    if trimmed.contains('\n') {
        return trimmed
            .lines()
            .map(map_typst_error)
            .collect::<Vec<_>>()
            .join("\n");
    }
    let Some(name) = trimmed.strip_prefix("unknown variable:") else {
        return trimmed.to_string();
    };
    let name = name
        .trim()
        .trim_matches(|ch: char| ch == '`' || ch == '"' || ch == '\'');
    if name.is_empty() {
        return trimmed.to_string();
    }
    let command = latex_command_for_typst_name(name);
    if command.is_empty() {
        return trimmed.to_string();
    }
    format!("unknown command \\{command}")
}

/// Reverse of the helpers [`mitex_to_typst`] rewrites, plus a plain name fallback.
fn latex_command_for_typst_name(name: &str) -> &str {
    match name {
        "mitexsqrt" => "sqrt",
        "mitexmathbf" => "mathbf",
        "mitexdisplaystyle" | "mitexdisplay" => "displaystyle",
        "mitexoverbrace" => "overbrace",
        "mitexunderbrace" => "underbrace",
        "operatorname" => "operatorname",
        "bmatrix" | "Bmatrix" | "pmatrix" | "vmatrix" | "Vmatrix" | "matrix" => name,
        "aligned" | "alignedat" | "align" | "alignat" | "gather" | "gathered" | "split" => name,
        "partial" => "partial",
        other => other.strip_prefix("mitex").unwrap_or(other),
    }
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
    fn renders_common_latex_math() {
        let cases = [
            r"\langle x \rangle",
            r"\langle x, y \rangle",
            r"\left\langle x \right\rangle",
            r"x \cdot y",
            r"a \times b",
            r"\mathrm{d}x",
            r"\vec{v}",
            r"\hat{x}",
            r"\overline{x}",
            r"\sum_{i=1}^{n} i",
            r"\int_0^1 x\,dx",
            r"\partial",
            r"\infty",
            r"\neq",
            r"\leq",
            r"\geq",
            r"\in",
            r"\subset",
            r"\rightarrow",
            r"\Rightarrow",
            r"\forall",
            r"\exists",
            r"\alpha + \beta",
            r"\sin x + \cos y",
            r"\lvert x \rvert",
            r"\lfloor x \rfloor",
            r"\lceil x \rceil",
            r"\mathbb{R}",
            r"\mathcal{L}",
            r"\|v\|",
            r"\nabla",
            r"\pm",
        ];
        for latex in cases {
            let rendered = render_equation_blocking(latex, 14.0, Rgb::new(0, 0, 0));
            assert!(
                rendered.error.is_none(),
                "{latex}: mitex={:?} typst={:?} err={:?}",
                mitex::convert_math(latex, None),
                mitex::convert_math(latex, None)
                    .ok()
                    .map(|expr| mitex_to_typst(&expr)),
                rendered.error
            );
        }
    }

    #[test]

    fn mitex_rewrite_maps_langle_to_glyphs() {
        let expr = r"angle.l  x \, y  angle.r";
        assert_eq!(mitex_to_typst(expr), "⟨ x , y ⟩");
    }

    #[test]
    fn rewrite_call_preserves_unicode_delimiters() {
        let out = mitex_to_typst("⌊ x ⌋");
        assert_eq!(out, "⌊ x ⌋");
        assert!(!out.contains('Ã'));
    }

    fn key(source: &str) -> MathKey {
        MathKey::new(source, false, 12.0, Rgb::new(0, 0, 0))
    }

    #[test]
    fn newer_live_job_cancels_older() {
        let mut sched = MathScheduler::new(Duration::from_millis(40));
        let t0 = Instant::now();
        sched.push(
            ScheduledJob {
                key: key("a"),
                lane: MathLane::Live,
                payload: "a",
            },
            t0,
        );
        sched.push(
            ScheduledJob {
                key: key("b"),
                lane: MathLane::Live,
                payload: "b",
            },
            t0 + Duration::from_millis(10),
        );
        assert!(sched.poll(t0 + Duration::from_millis(30)).is_none());
        let job = sched.poll(t0 + Duration::from_millis(50)).expect("live job");
        assert_eq!(job.payload, "b");
        assert!(sched.poll(t0 + Duration::from_millis(80)).is_none());
    }

    #[test]
    fn background_duplicate_does_not_run_twice() {
        let mut sched = MathScheduler::new(Duration::from_millis(40));
        let now = Instant::now();
        let shared = key("a");
        sched.push(
            ScheduledJob {
                key: shared.clone(),
                lane: MathLane::Background,
                payload: 1,
            },
            now,
        );
        sched.push(
            ScheduledJob {
                key: shared,
                lane: MathLane::Background,
                payload: 2,
            },
            now,
        );
        assert_eq!(sched.poll(now).expect("first").payload, 1);
        assert!(sched.poll(now).is_none());
    }

    #[test]
    fn live_keystroke_does_not_drop_background() {
        let mut sched = MathScheduler::new(Duration::from_millis(40));
        let now = Instant::now();
        sched.push(
            ScheduledJob {
                key: key("bg"),
                lane: MathLane::Background,
                payload: "bg",
            },
            now,
        );
        sched.push(
            ScheduledJob {
                key: key("old"),
                lane: MathLane::Live,
                payload: "old",
            },
            now,
        );
        sched.push(
            ScheduledJob {
                key: key("new"),
                lane: MathLane::Live,
                payload: "new",
            },
            now,
        );
        assert_eq!(sched.poll(now).expect("background").payload, "bg");
        assert!(sched.poll(now + Duration::from_millis(20)).is_none());
        assert_eq!(
            sched.poll(now + Duration::from_millis(40))
                .expect("latest live")
                .payload,
            "new"
        );
    }

    #[test]
    fn identical_equations_share_one_cache_entry() {
        let mut cache = MathCache::<()>::new();
        let key = key("x^2");
        cache.insert_pending(key.clone(), 1);
        cache.insert_pending(MathKey::new(" x^2 ", false, 12.0, Rgb::new(0, 0, 0)), 2);
        assert_eq!(cache.len(), 1);
        match cache.get(&key) {
            Some(EntryKind::Pending { req: 2 }) => {}
            other => panic!("expected the latest pending req, got {other:?}"),
        }
    }

    #[test]
    fn lru_evicts_oldest_past_cap() {
        let mut cache = MathCache::with_limits(2, usize::MAX);
        let a = key("a");
        let b = key("b");
        let c = key("c");
        cache.insert_ready(a.clone(), (), 10);
        cache.insert_ready(b.clone(), (), 10);
        cache.insert_ready(c.clone(), (), 10);
        assert!(cache.get(&a).is_none());
        assert!(cache.get(&b).is_some());
        assert!(cache.get(&c).is_some());
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn lru_evicts_when_texture_bytes_exceed_cap() {
        let mut cache = MathCache::with_limits(16, 100);
        let a = key("a");
        let b = key("b");
        cache.insert_ready(a.clone(), (), 80);
        cache.insert_ready(b.clone(), (), 80);
        assert!(cache.get(&a).is_none(), "oldest texture should leave");
        assert!(cache.get(&b).is_some());
        assert_eq!(cache.bytes(), 80);
    }

    #[test]
    fn preview_without_pdf_asks_save_to_request_it() {
        assert_eq!(pdf_plan(false, true, false, false), PdfPlan::Request);
        assert_eq!(pdf_plan(false, true, true, false), PdfPlan::Have);
        assert_eq!(pdf_plan(false, false, false, true), PdfPlan::Have);
        assert_eq!(pdf_plan(true, false, false, false), PdfPlan::Wait);
        assert_eq!(pdf_plan(false, false, false, false), PdfPlan::Wait);
    }

    #[test]
    fn preview_omits_pdf_until_asked() {
        let engine = build_engine();
        let preview = render_equation(&engine, r"x^2", 14.0, Rgb::new(0, 0, 0), false, RASTER_SCALE);
        assert!(preview.error.is_none(), "{:?}", preview.error);
        assert!(preview.pdf.is_none(), "preview must not build a PDF");
        assert!(preview.svg.is_some());
        assert!(preview.preview.is_some());
        assert!(preview.width_pt > 1.0);
        assert!((preview.raster_scale - RASTER_SCALE).abs() < f32::EPSILON);
        let with_pdf = render_equation(&engine, r"x^2", 14.0, Rgb::new(0, 0, 0), true, RASTER_SCALE);
        assert!(with_pdf.pdf.is_some());
        // Page frames in this Typst version may or may not publish a baseline.
        // Paint centers when this is None; it aligns when it is Some.
        if let Some(baseline) = preview.baseline_pt {
            assert!(baseline > 0.0 && baseline < preview.height_pt);
        }
    }

    #[test]
    fn raster_scale_buckets_are_sqrt2_steps() {
        assert_eq!(raster_scale_for(1.0), RASTER_SCALE);
        assert_eq!(raster_scale_for(RASTER_SCALE), RASTER_SCALE);
        // 1.2× base sits between 3 and 3√2 → first step up.
        let next = raster_scale_for(RASTER_SCALE * 1.2);
        let expected = RASTER_SCALE * std::f32::consts::SQRT_2;
        assert!(
            (next - expected).abs() < 1e-3,
            "got {next}, expected ~{expected}"
        );
        // Exactly 2× base lands on the 6× bucket.
        let double = raster_scale_for(RASTER_SCALE * 2.0);
        assert!(
            (double - RASTER_SCALE * 2.0).abs() < 1e-3,
            "got {double}, expected {}",
            RASTER_SCALE * 2.0
        );
        // Just above 2× needs the next √2 step (6√2).
        let above = raster_scale_for(RASTER_SCALE * 2.0 + 0.01);
        let expect_above = RASTER_SCALE * 2.0 * std::f32::consts::SQRT_2;
        assert!(
            (above - expect_above).abs() < 1e-2,
            "got {above}, expected ~{expect_above}"
        );
    }

    #[test]
    fn sharper_raster_uses_hysteresis() {
        assert!(!needs_sharper_raster(3.0, 3.0));
        assert!(!needs_sharper_raster(3.0, 3.0 * 1.2));
        assert!(needs_sharper_raster(3.0, 3.0 * 1.3 + 0.01));
    }

    #[test]
    fn map_typst_error_uses_latex_commands() {
        assert_eq!(
            map_typst_error("unknown variable: mitexsqrt"),
            "unknown command \\sqrt"
        );
        assert_eq!(
            map_typst_error("unknown variable: foo"),
            "unknown command \\foo"
        );
        assert_eq!(
            map_typst_error("unknown variable: `bmatrix`"),
            "unknown command \\bmatrix"
        );
        assert_eq!(
            map_typst_error("unknown variable: mitexmathbf"),
            "unknown command \\mathbf"
        );
        assert_eq!(
            map_typst_error("unbalanced delimiters"),
            "unbalanced delimiters"
        );
        assert_eq!(
            map_typst_error("unknown variable: mitexsqrt\nunknown variable: foo"),
            "unknown command \\sqrt\nunknown command \\foo"
        );
    }

    #[test]
    fn reraster_from_svg_skips_typst_and_pdf() {
        let engine = build_engine();
        let first = render_equation(&engine, r"\frac{a}{b}", 14.0, Rgb::new(0, 0, 0), false, RASTER_SCALE);
        assert!(first.error.is_none(), "{:?}", first.error);
        let svg = first.svg.expect("svg");
        let sharp = RASTER_SCALE * std::f32::consts::SQRT_2;
        let (preview, w, h) = rasterize_svg(&svg, sharp);
        let preview = preview.expect("sharper preview");
        let base = first.preview.expect("base preview");
        assert!(preview.width > base.width || preview.height > base.height);
        assert!((w - first.width_pt).abs() < 0.5);
        assert!((h - first.height_pt).abs() < 0.5);
    }

    /// Manual artifact: `LT6_ZOOM_PNG=/path/to.png cargo test --bin marker write_lt6_zoom_png -- --exact --nocapture`
    #[test]
    fn write_lt6_zoom_png() {
        let Ok(path) = std::env::var("LT6_ZOOM_PNG") else {
            return;
        };
        // ~400% zoom: ZOOM_100≈1.333 → scale≈5.33; bucket above base×1.3.
        let needed = crate::geom::ZOOM_100 * 4.0;
        let scale = raster_scale_for(needed);
        assert!(needs_sharper_raster(RASTER_SCALE, needed));
        let engine = build_engine();
        let rendered = render_equation(
            &engine,
            r"x = \frac{-b \pm \sqrt{b^2 - 4ac}}{2a}",
            18.0,
            Rgb::new(20, 20, 20),
            false,
            scale,
        );
        assert!(rendered.error.is_none(), "{:?}", rendered.error);
        let preview = rendered.preview.expect("preview");
        image::save_buffer(
            &path,
            &preview.pixels,
            preview.width,
            preview.height,
            image::ColorType::Rgba8,
        )
        .expect("write png");
        eprintln!(
            "wrote {path} at raster_scale={scale} ({}×{})",
            preview.width, preview.height
        );
    }
}
