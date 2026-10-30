//! boxq: run an Agent against a Workspace inside an isolated Sandbox.

pub mod auth;
pub mod config;
pub mod reap;
pub mod runtime;
pub mod sandbox;
mod workspace;

pub use workspace::Workspace;
