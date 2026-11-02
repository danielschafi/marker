//! Marker GUI binary. Headless modules live in the [`marker`] library crate;
//! thin re-exports keep existing `crate::…` paths working in binary modules.

mod annot {
    pub use marker::annot::*;
}
mod assistant {
    pub use marker::assistant::*;
}
mod cite {
    pub use marker::cite::*;
}
mod export {
    pub use marker::export::*;
}
mod geom {
    pub use marker::geom::*;
}
mod instance {
    pub use marker::instance::*;
}
mod math {
    pub use marker::math::*;
}
mod math_spans {
    pub use marker::math_spans::*;
}
mod pdf {
    pub use marker::pdf::*;
}
mod rich_text {
    pub use marker::rich_text::*;
}
mod settings {
    pub use marker::settings::*;
}
mod undo {
    pub use marker::undo::*;
}
mod worker {
    pub use marker::worker::*;
}

mod app;
mod assistant_md;
mod icon;
mod platform;
mod rich_text_edit;
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

fn run_ui(paths: Vec<std::path::PathBuf>, inbox: Option<instance::IpcInbox>) -> eframe::Result<()> {
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([1280.0, 840.0])
        .with_min_inner_size([760.0, 480.0])
        .with_title("Marker")
        .with_drag_and_drop(true)
        .with_icon(icon::window_icon());

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
