//! Shared native Computer Use engine. No desktop shell or transport dependencies.
mod computer_use;
pub use computer_use::{detect_capabilities, execute, open_input_settings, release_ax, ComputerUseCapabilities, ExecuteRequest};
