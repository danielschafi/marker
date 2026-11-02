//! Window / taskbar icon loaded from the packaged Marker mark.

use egui::IconData;

/// 256×256 RGBA window icon (rasterized from `packaging/marker.svg`).
pub(crate) fn window_icon() -> IconData {
    let image = image::load_from_memory(include_bytes!("../assets/icons/marker-256.png"))
        .expect("embedded marker icon must decode")
        .into_rgba8();
    let (width, height) = image.dimensions();
    IconData {
        rgba: image.into_raw(),
        width,
        height,
    }
}
