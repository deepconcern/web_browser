//! Fuzzes document parsing (including encoding sniffing) and fragment
//! parsing, then walks and dumps the resulting tree.
#![no_main]

use libfuzzer_sys::fuzz_target;
use wb_html::{FragmentContext, ParseOptions, dump_tree, parse_document_bytes, parse_fragment};

const CONTEXTS: [&str; 8] = [
    "body", "div", "table", "tr", "td", "select", "template", "title",
];

fuzz_target!(|data: &[u8]| {
    let Some((&selector, rest)) = data.split_first() else {
        return;
    };
    let options = ParseOptions {
        scripting: selector & 1 != 0,
    };
    let parsed = parse_document_bytes(rest, None, &options);
    let document = &parsed.document;
    let _ = dump_tree(document, document.root());
    for node in document.descendants(document.root()) {
        let _ = document.text_content(node);
    }

    let input = String::from_utf8_lossy(rest);
    let context = CONTEXTS[usize::from(selector >> 1) % CONTEXTS.len()];
    let fragment = parse_fragment(&input, &FragmentContext::html(context), &options);
    let _ = dump_tree(&fragment.document, fragment.fragment);
});
