//! ytfast: a native YouTube Music client. See docs/SPEC.md.

pub mod app;
pub mod auth;
pub mod backend;
pub mod colors;
pub mod covers;
#[cfg(feature = "e2e")]
pub mod e2e;
pub mod icons;
pub mod innertube;
pub mod lyrics;
pub mod model;
pub mod mpv;
pub mod parse;
pub mod paths;
pub mod resolver;
pub mod searches;
pub mod settings;
pub mod single_instance;
pub mod theme;
pub mod ui;
