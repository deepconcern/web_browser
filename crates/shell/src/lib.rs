//! The browser application.
//!
//! - [`engine`]: the headless engine: loads a page through the network
//!   stack (or a local fetcher), styles, lays out and rasterizes it. The
//!   reference-rendering harness and the UI both build on it.
#![warn(missing_docs)]

pub mod engine;
