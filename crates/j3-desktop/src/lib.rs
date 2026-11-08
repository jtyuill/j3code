//! The j3 desktop app on Rune: its own in-process j3 server (the local environment) and
//! remote j3 servers over the t3 wire protocol, in one window. `main.rs` starts the process;
//! `app` holds the root state and the window.

// The server-config fixtures in unit tests are large `json!` literals.
#![recursion_limit = "256"]

pub mod actions;
pub mod app;
pub mod archived;
pub mod assist;
pub mod attention;
pub mod completion;
pub mod composer;
pub mod dialogs;
pub mod environment;
pub mod environments;
pub mod feed;
pub mod files;
pub mod git;
pub mod goto;
pub mod hub;
pub mod icons;
pub mod images;
pub mod inbox;
pub mod keywords;
pub mod local_server;
pub mod logos;
pub mod math;
pub mod mention;
pub mod menus;
pub mod message_text;
pub mod models;
pub mod motion;
pub mod new_thread;
pub mod prediction;
pub mod preferences;
pub mod services;
pub mod settings;
pub mod sidebar;
pub mod speech;
pub mod syntax;
pub mod terminal;
pub mod theme;
pub mod thread;
pub mod toasts;
pub mod ui_settings;
#[cfg(test)]
mod ui_tests;
pub mod usage;
