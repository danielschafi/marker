use crate::geom::PdfRect;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CaptureMode {
    #[default]
    None,
    /// Ephemeral text selection for assistant attach (does not create annotations).
    LearningText,
    /// Drag a PDF-space rectangle for a screenshot crop.
    Region,
}

#[derive(Clone, Debug)]
pub struct LearningSelection {
    pub page: usize,
    pub glyph_lo: usize,
    pub glyph_hi: usize,
}

#[derive(Clone)]
pub enum AssistantAttachment {
    Text {
        page: usize,
        text: String,
        truncated: bool,
    },
    Image {
        page: usize,
        rect: PdfRect,
        png: Vec<u8>,
        width: u32,
        height: u32,
        texture: Option<egui::TextureHandle>, // reserved for chip thumbnails
    },
}

impl AssistantAttachment {
    pub fn label(&self) -> String {
        match self {
            Self::Text {
                page,
                text,
                truncated,
            } => {
                let n = text.chars().count();
                if *truncated {
                    format!("Text · page {} · {n} characters (truncated)", page + 1)
                } else {
                    format!("Text · page {} · {n} characters", page + 1)
                }
            }
            Self::Image { page, .. } => format!("Image · page {}", page + 1),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssistantRole {
    User,
    Assistant,
}

#[derive(Clone, Debug)]
pub struct AssistantTurn {
    pub role: AssistantRole,
    pub text: String,
    pub incomplete: bool,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct PendingCrop {
    pub gen: u64,
    pub seq: u64,
    pub page: usize,
    pub rect: PdfRect,
}

#[derive(Default)]
pub struct TabAssistant {
    pub chat_id: Option<String>,
    pub turns: Vec<AssistantTurn>,
    pub draft: String,
    pub attachments: Vec<AssistantAttachment>,
    pub learning: Option<LearningSelection>,
    pub streaming: bool,
    pub request_seq: u64,
    pub error: Option<String>,
    /// User acknowledged first-send Cursor disclosure for this tab.
    pub disclosed: bool,
    /// Opt-in Cursor agent mode (tools / writes). Default is ask (read-only Q&A).
    pub agent_mode: bool,
    pub status_line: Option<String>,
    pub pending_crop: Option<PendingCrop>,
    crop_seq: u64,
}

impl TabAssistant {
    pub fn new_chat(&mut self) {
        self.chat_id = None;
        self.turns.clear();
        self.draft.clear();
        self.attachments.clear();
        self.streaming = false;
        self.request_seq = self.request_seq.wrapping_add(1);
        self.error = None;
        self.status_line = None;
        self.pending_crop = None;
    }

    pub fn bump_seq(&mut self) -> u64 {
        self.request_seq = self.request_seq.wrapping_add(1);
        self.request_seq
    }

    pub fn bump_crop_seq(&mut self) -> u64 {
        self.crop_seq = self.crop_seq.wrapping_add(1);
        self.crop_seq
    }
}

/// Events from the assistant worker to the UI thread.
#[derive(Debug)]
pub enum AssistantEvent {
    Started {
        gen: u64,
        seq: u64,
        chat_id: String,
    },
    Delta {
        gen: u64,
        seq: u64,
        text: String,
    },
    Completed {
        gen: u64,
        seq: u64,
        text: String,
    },
    Cancelled {
        gen: u64,
        seq: u64,
    },
    Failed {
        gen: u64,
        seq: u64,
        message: String,
    },
    AuthRequired {
        gen: u64,
        seq: u64,
        message: String,
    },
}
