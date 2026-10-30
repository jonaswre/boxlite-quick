//! boxq: run an Agent against a Workspace inside an isolated Sandbox.

pub mod config;
pub mod reap;
pub mod runtime;
mod workspace;

pub use workspace::Workspace;
