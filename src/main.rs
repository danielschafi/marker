mod annot;
mod app;
mod assistant;
mod geom;
mod math;
mod pdf;
mod settings;
mod theme;
mod ui;
mod view;

use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let paths = pdf_args();
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([1280.0, 840.0])
        .with_min_inner_size([760.0, 480.0])
        .with_title("Marker")
        .with_drag_and_drop(true);

    let options = eframe::NativeOptions {
        viewport,
        // Hyprland does not deliver frame callbacks to a window on a hidden
        // workspace. Waiting for vsync there blocks the event loop, so the
        // compositor reports the app as not responding until you switch back.
        // Frame pacing is owned by app-level request_repaint_after / idle sleep.
        vsync: false,
        ..Default::default()
    };

    eframe::run_native(
        "Marker",
        options,
        Box::new(move |cc| Ok(Box::new(app::MarkerApp::new(cc, paths)))),
    )
}

fn pdf_args() -> Vec<PathBuf> {
    std::env::args()
        .skip(1)
        .map(PathBuf::from)
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
        })
        .collect()
}
