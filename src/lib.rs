//! Library crate root for the `text_reader` binary.
//!
//! Modules are split by concern; see each module for details. The actions
//! defined here are re-exported so both the binary (`src/main.rs`) and the
//! internal modules can use them for keybindings and listeners.

use gpui::actions;

pub mod editor;
pub mod find;
pub mod find_ui;
pub mod render;
pub mod util;
pub mod viewer;

actions!(text_reader, [Find, FindClose, FindPrev, ZoomIn, ZoomOut]);

pub use editor::{TabContent, TabEntry, TextEditor};
