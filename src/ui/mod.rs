//! UI layer: layout, scroll maths, list rendering, footer and mouse hit-testing.
//!
//! Everything here is a pure function of `(state, area)` so it can be tested
//! with a `ratatui` buffer and no terminal. Colours are never written inline in
//! a widget: they come from [`palette`].

pub mod footer;
pub mod layout;
pub mod list;
pub mod mouse;
pub mod palette;
pub mod render;
pub mod scroll;
pub mod truncate;

pub use layout::{areas, Areas};
pub use mouse::{hit_test, HitContext, MouseAction};
pub use render::{draw, render_lines};
pub use scroll::{Scroll, Thumb, WHEEL_ROWS};
