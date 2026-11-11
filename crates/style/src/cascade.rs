//! The cascade (stub; implementation in progress).
use std::sync::Arc;

use crate::{ComputedStyle, StyleOptions, StyledDocument, Stylist};
use wb_dom::Document;

pub(crate) fn style_document(_s: &Stylist, doc: &Document, _o: &StyleOptions) -> StyledDocument {
    let initial = Arc::new(ComputedStyle::initial());
    let n = doc.node_count();
    let mut styles = vec![None; n];
    let mut parents = vec![None; n];
    for node in doc.descendants(doc.root()) {
        if let Some(slot) = parents.get_mut(node.index()) {
            *slot = doc.parent(node);
        }
        if doc.is_element(node)
            && let Some(slot) = styles.get_mut(node.index())
        {
            *slot = Some(initial.clone());
        }
    }
    StyledDocument {
        styles,
        pseudos: vec![None; n],
        parents,
        root: doc.document_element(),
        initial,
    }
}
