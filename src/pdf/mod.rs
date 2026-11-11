mod annot_read;
mod annot_write;
mod appearance;
mod convert;
mod engine;
mod raster;
mod repair;
mod save_transaction;
mod worker;

/// Identity of one open document slot (UI ↔ PDF worker).
///
/// Distinct from annotation ids and from PDF object generation numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DocId(pub u64);

/// Cache / wanted-set key for one page tile at a render scale.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub page: usize,
    pub scale_bits: u32,
    pub col: i32,
    pub row: i32,
}

impl std::fmt::Display for DocId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

pub use engine::{
    Error, InlineMathSave, OutlineNode, PageInfo, SaveSnapshot, SavedXref, TileImage,
    TILE_PX,
};
pub use repair::repaired_sidecar_path as repaired_sidecar_for;
pub use worker::{OpenedDoc, PdfReply, PdfWorker};
