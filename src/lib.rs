//! boxq: run an Agent against a Workspace inside an isolated Sandbox.

pub mod claude;
pub mod config;
pub mod reap;
pub mod runtime;
pub mod sandbox;
pub mod session;
mod workspace;

pub use workspace::Workspace;
