//! Cursor Agent CLI learning assistant (issue #12).

mod adapter;
mod bundle;
mod crop;
mod markdown;
mod text;
mod types;
mod worker;

pub use bundle::{BundleImageAttach, BundleInput, BundleTextAttach, MAX_TEXT_CHARS};
pub use crop::{clamp_crop_rect, crop_scale, CROP_DPI, CROP_MAX_PNG_BYTES};
pub use markdown::show as show_markdown;
pub use text::{glyphs_intersecting_rects, reconstruct_text, truncate_text};
pub use types::{
    AssistantAttachment, AssistantEvent, AssistantRole, AssistantTurn, CaptureMode,
    LearningSelection, PendingCrop, TabAssistant,
};
pub use worker::{AssistantRequest, AssistantWorker};
