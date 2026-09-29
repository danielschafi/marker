//! Cursor Agent CLI learning assistant (issue #12).

mod adapter;
mod bundle;
mod crop;
mod text;
mod types;
mod worker;

pub use adapter::{AgentCapability, probe_agent};
pub use bundle::{
    build_bundle, cleanup_bundle, BundleImageAttach, BundleInput, BundlePaths, BundleTextAttach,
    MAX_TEXT_CHARS,
};
pub use crop::{
    clamp_crop_rect, crop_scale, CROP_DPI, CROP_MAX_EDGE_PX, CROP_MAX_PNG_BYTES,
};
pub use text::{reconstruct_text, truncate_text};
pub use types::{
    AssistantAttachment, AssistantEvent, AssistantRole, AssistantTurn, CaptureMode,
    LearningSelection, PendingCrop, TabAssistant,
};
pub use worker::{AssistantRequest, AssistantWorker};
