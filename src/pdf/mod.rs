mod engine;
mod worker;

/// Identity of one open document slot (UI ↔ PDF worker).
///
/// Distinct from annotation ids and from PDF object generation numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DocId(pub u64);

impl std::fmt::Display for DocId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

pub use engine::{
    CropImage, Error, InlineMathSave, OutlineNode, PageInfo, SaveSnapshot, SavedXref, TileImage,
    TILE_PX,
};
pub use worker::{PdfReply, PdfWorker};
