#![recursion_limit = "512"]

#[allow(dead_code)]
#[path = "main.rs"]
mod app;

pub use app::{set_playground_theme, Workspace};
