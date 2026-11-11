//! Network and cosmetic content filtering (uBlock Origin / EasyList syntax
//! subset).
//!
//! The syntax is implemented from the public filter documentation: Adblock
//! Plus "How to write filters" and uBlock Origin's static filter syntax wiki.
//! Downloading lists is not done here; the shell fetches them through the
//! network policy gate and passes the text in.
//!
//! # Usage
//!
//! ```
//! use wb_blocker::{EngineBuilder, Request, ResourceType};
//!
//! let mut builder = EngineBuilder::new();
//! let list = [
//!     "[Adblock Plus 2.0]",
//!     "! Title: Example list",
//!     "||ads.example^$third-party",
//!     "@@||ads.example/ok.js",
//!     "##.banner",
//! ];
//! let report = builder.add_list(&list.join("\n"));
//! assert_eq!(report.metadata.title.as_deref(), Some("Example list"));
//! let engine = builder.build();
//!
//! // `same_site` comes from the caller's registrable-domain comparison.
//! let req = Request::new(
//!     "https://ads.example/x.js",
//!     "https://news.test/",
//!     ResourceType::Script,
//!     false,
//! );
//! assert!(engine.check(&req).is_blocked());
//! let req = Request::new(
//!     "https://ads.example/ok.js",
//!     "https://news.test/",
//!     ResourceType::Script,
//!     false,
//! );
//! assert!(!engine.check(&req).is_blocked());
//!
//! let css = engine.cosmetic_selectors("https://news.test/");
//! assert_eq!(css.generic, [".banner"]);
//! ```
//!
//! # Supported syntax
//!
//! - Network filters: substrings, `*`, `^`, `|` start/end anchors, `||`
//!   host anchor, `/regex/`, `@@` exceptions.
//! - Options: `third-party`/`3p`, `first-party`/`1p` (and `~` forms),
//!   `domain=`/`from=`, resource types with `~` negation (`script`, `image`,
//!   `stylesheet`/`css`, `font`, `media`, `xmlhttprequest`/`xhr`,
//!   `subdocument`/`frame`, `document`/`doc`, `ping`/`beacon`, `websocket`,
//!   `other`, `object`), `all`, `match-case`, `important`, `badfilter`, and
//!   on exceptions `elemhide`/`ehide`, `generichide`/`ghide`,
//!   `specifichide`/`shide`. Any other option drops the filter (counted in
//!   [`ParseStats`]).
//! - A filter without type options applies to every type except
//!   [`ResourceType::Document`]; `$document` must be explicit.
//! - Cosmetic filters: `##sel`, `domains##sel`, `~domain##sel`, `#@#sel`.
//!   Procedural filters, scriptlets, HTML filters and snippets are dropped.
//! - `!` comments, `[Adblock Plus 2.0]` headers, metadata (`! Title:`,
//!   `! Expires:`, ...), and `!#if`/`!#else`/`!#endif` (every condition
//!   token is false for this browser).
//!
//! # Matching speed
//!
//! Each network filter is indexed under its rarest alphanumeric token that
//! is guaranteed to appear as a whole token in every matching URL; a request
//! only checks the filters stored under its own URL tokens, under its source
//! host's domains (for token-less `$domain=` filters), and a short fallback
//! list.

mod cosmetic;
mod domains;
mod engine;
mod hash;
mod list;
mod network;
mod request;

pub use engine::{CosmeticSelectors, Engine, EngineBuilder, Verdict};
pub use list::{ListMetadata, ListReport, ParseStats};
pub use request::{Request, ResourceType};

#[cfg(test)]
mod tests;
