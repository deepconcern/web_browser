//! The built-in user-agent stylesheets (HTML §15).

use std::sync::OnceLock;

use wb_css::{Stylesheet, parse_stylesheet};

/// The parsed UA sheets.
#[derive(Debug)]
pub(crate) struct UaSheets {
    /// Rules that always apply.
    pub(crate) normal: Stylesheet,
    /// Rules that apply only to quirks-mode documents.
    pub(crate) quirks: Stylesheet,
}

/// The UA sheets, parsed on first use.
pub(crate) fn ua_sheets() -> &'static UaSheets {
    static SHEETS: OnceLock<UaSheets> = OnceLock::new();
    SHEETS.get_or_init(|| UaSheets {
        normal: parse_stylesheet(include_str!("ua/html.css")),
        quirks: parse_stylesheet(include_str!("ua/quirks.css")),
    })
}
