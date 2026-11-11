//! The browser application.
//!
//! - [`engine`]: the headless engine: loads a page through the network
//!   stack (or a local fetcher), styles, lays out and rasterizes it. The
//!   reference-rendering harness and the UI both build on it.
//! - [`history`], [`omnibox`], [`forms`]: UI-independent state machines
//!   (session history, URL bar input, form controls and submission).
#![warn(missing_docs)]

pub mod content;
pub mod engine;
pub mod filters;
pub mod forms;
pub mod history;
pub mod omnibox;
pub mod profile;
pub mod selection;
