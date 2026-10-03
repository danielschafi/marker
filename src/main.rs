mod annot;
mod app;
mod assistant;
mod geom;
mod instance;
mod math;
mod math_spans;
mod pdf;
mod settings;
mod theme;
mod ui;
mod view;

fn main() -> eframe::Result<()> {
    let args = instance::LaunchArgs::from_env();
    match instance::boot(args) {
        instance::Boot::HandedOff => Ok(()),
        instance::Boot::Run { paths, inbox } => run_ui(paths, inbox),
    }
}

fn run_ui(
    paths: Vec<std::path::PathBuf>,
    inbox: Option<instance::IpcInbox>,
) -> eframe::Result<()> {
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
        Box::new(move |cc| Ok(Box::new(app::MarkerApp::new(cc, paths, inbox)))),
    )
}
