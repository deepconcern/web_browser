//! Serializes a tree in the html5lib tree-construction test format.

use std::fmt::Write as _;

use wb_dom::{Document, Namespace, NodeData, NodeId};

/// Writes the children of `node` (and their descendants) in the format used
/// by the html5lib tree-construction tests: one `| `-prefixed line per node,
/// indented two spaces per level, attributes sorted by name, and template
/// contents under a `content` line.
#[must_use]
pub fn dump_tree(document: &Document, node: NodeId) -> String {
    let mut out = String::new();
    // An explicit stack instead of recursion, so deep trees cannot overflow
    // the call stack. `None` entries stand for a template's `content` line.
    let mut stack: Vec<(Option<NodeId>, usize)> = Vec::new();
    push_children(document, node, 0, &mut stack);
    while let Some((entry, depth)) = stack.pop() {
        match entry {
            Some(node) => dump_node(document, node, depth, &mut out, &mut stack),
            None => {
                line(&mut out, depth);
                out.push_str("content\n");
            }
        }
    }
    out
}

/// Pushes the children of `node` so that they pop in document order.
fn push_children(
    document: &Document,
    node: NodeId,
    depth: usize,
    stack: &mut Vec<(Option<NodeId>, usize)>,
) {
    let children: Vec<NodeId> = document.children(node).collect();
    stack.extend(children.into_iter().rev().map(|child| (Some(child), depth)));
}

fn line(out: &mut String, depth: usize) {
    out.push_str("| ");
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn dump_node(
    document: &Document,
    node: NodeId,
    depth: usize,
    out: &mut String,
    stack: &mut Vec<(Option<NodeId>, usize)>,
) {
    let Some(data) = document.data(node) else {
        return;
    };
    line(out, depth);
    match data {
        NodeData::Document { .. } | NodeData::DocumentFragment => {
            out.push_str("#document\n");
        }
        NodeData::Doctype {
            name,
            public_id,
            system_id,
        } => {
            if public_id.is_empty() && system_id.is_empty() {
                let _ = writeln!(out, "<!DOCTYPE {name}>");
            } else {
                let _ = writeln!(out, "<!DOCTYPE {name} \"{public_id}\" \"{system_id}\">");
            }
        }
        NodeData::Text(text) => {
            let _ = writeln!(out, "\"{text}\"");
        }
        NodeData::Comment(text) => {
            let _ = writeln!(out, "<!-- {text} -->");
        }
        NodeData::ProcessingInstruction { target, data } => {
            let _ = writeln!(out, "<?{target} {data}?>");
        }
        NodeData::Element(element) => {
            let prefix = match element.namespace {
                Namespace::Svg => "svg ",
                Namespace::MathMl => "math ",
                _ => "",
            };
            let _ = writeln!(out, "<{prefix}{}>", element.local_name);
            let mut attributes: Vec<(String, &str)> = element
                .attributes
                .iter()
                .map(|a| {
                    let name = match a.namespace {
                        Some(Namespace::XLink) => format!("xlink {}", a.local_name),
                        Some(Namespace::Xml) => format!("xml {}", a.local_name),
                        Some(Namespace::Xmlns) => format!("xmlns {}", a.local_name),
                        _ => a.local_name.clone(),
                    };
                    (name, a.value.as_str())
                })
                .collect();
            attributes.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
            for (name, value) in attributes {
                line(out, depth + 1);
                let _ = writeln!(out, "{name}=\"{value}\"");
            }
            // Children are dumped after the template contents.
            push_children(document, node, depth + 1, stack);
            if let Some(contents) = element.template_contents {
                push_children(document, contents, depth + 2, stack);
                stack.push((None, depth + 1));
            }
            return;
        }
    }
    push_children(document, node, depth + 1, stack);
}
