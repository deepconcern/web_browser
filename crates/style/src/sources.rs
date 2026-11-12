//! Finding a document's stylesheets (HTML §4.2.4 `<link>`, §4.2.6
//! `<style>`, CSSOM §6.3 "the document's CSS style sheets").

use wb_css::{MediaQueryList, parse_media_query_list};
use wb_dom::{Document, Namespace, NodeData, NodeId};

/// Where a document stylesheet comes from.
#[derive(Clone, Debug, PartialEq)]
pub enum StyleSourceKind {
    /// A `<style>` element; the text is its child text content.
    Inline(String),
    /// A `<link rel=stylesheet>`; the caller fetches `href` (resolved
    /// against the document's base URL) and parses it.
    External {
        /// The `href` attribute as written.
        href: String,
    },
}

/// A `<style>` or `<link rel=stylesheet>` element that contributes a
/// stylesheet.
#[derive(Clone, Debug, PartialEq)]
pub struct StyleSource {
    /// The element.
    pub node: NodeId,
    /// The sheet's text or URL.
    pub kind: StyleSourceKind,
    /// The `media` attribute (empty for all media). Pass it as
    /// [`crate::SheetInput::media`].
    pub media: MediaQueryList,
}

/// The stylesheet-contributing elements of `doc`, in tree order (which is
/// the order their sheets apply in).
///
/// - `<style>` (HTML or SVG) applies when its `type` is absent, empty or
///   `text/css` (HTML §4.2.6).
/// - `<link>` applies when its `rel` contains `stylesheet` but not
///   `alternate`, its `href` is not empty, it has no `disabled` attribute,
///   and its `type` is absent or `text/css` (HTML §4.6.7.17).
pub fn document_style_sources(doc: &Document) -> Vec<StyleSource> {
    let mut out = Vec::new();
    for node in doc.descendants(doc.root()) {
        let Some(el) = doc.element(node) else {
            continue;
        };
        let is_style = match el.namespace {
            Namespace::Html | Namespace::Svg => el.local_name == "style",
            _ => false,
        };
        let media = el
            .attribute("media")
            .map(parse_media_query_list)
            .unwrap_or_default();
        if is_style {
            if !type_is_css(el.attribute("type")) {
                continue;
            }
            let mut text = String::new();
            for child in doc.children(node) {
                if let Some(NodeData::Text(t)) = doc.data(child) {
                    text.push_str(t);
                }
            }
            out.push(StyleSource {
                node,
                kind: StyleSourceKind::Inline(text),
                media,
            });
        } else if el.is_html("link") {
            let rel = el.attribute("rel").unwrap_or("");
            let has = |token: &str| {
                rel.split_ascii_whitespace()
                    .any(|t| t.eq_ignore_ascii_case(token))
            };
            if !has("stylesheet") || has("alternate") || el.has_attribute("disabled") {
                continue;
            }
            if !type_is_css(el.attribute("type")) {
                continue;
            }
            let href = el.attribute("href").unwrap_or("").trim();
            if href.is_empty() {
                continue;
            }
            out.push(StyleSource {
                node,
                kind: StyleSourceKind::External {
                    href: href.to_string(),
                },
                media,
            });
        }
    }
    out
}

fn type_is_css(ty: Option<&str>) -> bool {
    match ty {
        None => true,
        Some(t) => {
            let t = t.trim();
            // Parameters such as `; charset=utf-8` are allowed.
            let essence = t.split(';').next().unwrap_or("").trim();
            t.is_empty() || essence.eq_ignore_ascii_case("text/css")
        }
    }
}
