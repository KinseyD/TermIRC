//! Terminal presentation, input, and view state.
pub mod input;
pub mod layout;
pub mod mouse;
pub mod render;
pub mod view;
pub use view::{App, Focus};
pub mod events;
