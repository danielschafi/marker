//! Headless core of Marker: annotations, PDF, math, settings, and export.
//!
//! The GUI binary (`main.rs`) depends on this library. Integration tests can
//! link it without starting egui.

pub mod annot;
pub mod assistant;
pub mod cite;
pub mod export;
pub mod geom;
pub mod instance;
pub mod math;
pub mod math_spans;
pub mod pdf;
pub mod rich_text;
pub mod settings;
pub mod undo;
pub mod worker;
