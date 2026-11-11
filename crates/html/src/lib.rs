//! WHATWG HTML tokenizer and tree builder.
//!
//! An implementation of HTML parsing as specified in the [HTML Standard]
//! §13.2 ("Parsing HTML documents") and §13.4 ("Parsing HTML fragments"),
//! written from the specification. It produces a [`wb_dom::Document`].
//!
//! - [`parse_document`] parses a string as a whole document.
//! - [`parse_document_bytes`] first determines the character encoding (BOM,
//!   the transport `charset`, then the `<meta>` prescan, §13.2.3.2), decodes,
//!   and parses; a `<meta charset>` found while parsing that disagrees with a
//!   tentative guess restarts the parse with the declared encoding.
//! - [`parse_fragment`] runs the fragment parsing algorithm with a context
//!   element (as `innerHTML` does).
//! - [`tokenizer`] exposes the tokenizer on its own.
//!
//! Parse errors never stop parsing: they are collected as [`ParseError`]s
//! and the parser recovers the way the spec says. Scripts are not executed
//! (M1 has no JavaScript engine); [`ParseOptions::scripting`] only selects
//! how `<noscript>` is parsed.
//!
//! [HTML Standard]: https://html.spec.whatwg.org/multipage/parsing.html

mod dump;
mod encoding;
mod entities;
pub mod tokenizer;
mod treebuilder;

use wb_dom::{Attribute, Document, Namespace, NodeId, QuirksMode};

pub use dump::dump_tree;
pub use encoding::{Confidence, SniffedEncoding, sniff_encoding};

use tokenizer::{Token, Tokenizer};
use treebuilder::TreeBuilder;

/// A parse error. Parsing always continues after one.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ParseError {
    /// The error code. Tokenizer errors use the codes defined in HTML
    /// §13.2.2 (e.g. `eof-in-tag`); tree construction errors, which the
    /// spec does not name, use descriptive codes such as
    /// `unexpected-end-tag`.
    pub code: &'static str,
    /// The 1-based line where the error was found.
    pub line: u32,
    /// The column on that line.
    pub column: u32,
}

/// Options for the parser.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ParseOptions {
    /// The scripting flag (HTML §13.2.4.5). When set, `<noscript>` content
    /// is parsed as raw text, as in a browser with scripting enabled.
    pub scripting: bool,
}

/// The result of parsing a document.
#[derive(Debug)]
pub struct ParsedDocument {
    /// The document.
    pub document: Document,
    /// Every parse error, in the order found.
    pub errors: Vec<ParseError>,
    /// The name of the character encoding used to decode the input (always
    /// `UTF-8` for [`parse_document`]).
    pub encoding: &'static str,
}

/// The context element for fragment parsing.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FragmentContext {
    /// The context element's namespace.
    pub namespace: Namespace,
    /// The context element's local name, e.g. `div` or `tr`.
    pub local_name: String,
    /// The context element's attributes (they matter for MathML
    /// `annotation-xml`, which is an HTML integration point depending on
    /// its `encoding` attribute).
    pub attributes: Vec<Attribute>,
    /// The mode of the document the context element belongs to.
    pub quirks_mode: QuirksMode,
}

impl FragmentContext {
    /// A context element in the HTML namespace with no attributes, in a
    /// no-quirks document.
    #[must_use]
    pub fn html(local_name: &str) -> FragmentContext {
        FragmentContext {
            namespace: Namespace::Html,
            local_name: local_name.to_string(),
            attributes: Vec::new(),
            quirks_mode: QuirksMode::NoQuirks,
        }
    }
}

/// The result of parsing a fragment.
#[derive(Debug)]
pub struct ParsedFragment {
    /// The document that owns the fragment's nodes.
    pub document: Document,
    /// A document fragment node holding the parsed nodes.
    pub fragment: NodeId,
    /// Every parse error, in the order found.
    pub errors: Vec<ParseError>,
}

/// Feeds every token from `tokenizer` to `builder` until parsing stops.
fn run(tokenizer: &mut Tokenizer, builder: &mut TreeBuilder) {
    loop {
        tokenizer.set_cdata_allowed(builder.adjusted_current_node_is_foreign());
        let token = tokenizer.next_token();
        let eof = token == Token::Eof;
        builder.set_position(tokenizer.position());
        builder.process(token);
        if let Some(state) = builder.tokenizer_state.take() {
            tokenizer.set_state(state);
        }
        if eof || builder.stopped {
            break;
        }
    }
}

fn merge_errors(tokenizer: &mut Tokenizer, builder: &mut TreeBuilder) -> Vec<ParseError> {
    let mut errors = tokenizer.take_errors();
    errors.append(&mut builder.errors);
    errors.sort_by_key(|e| (e.line, e.column));
    errors
}

/// Parses `input` as an HTML document (HTML §13.2).
#[must_use]
pub fn parse_document(input: &str, options: &ParseOptions) -> ParsedDocument {
    let (document, errors, _) = parse_str(input, options, false);
    ParsedDocument {
        document,
        errors,
        encoding: "UTF-8",
    }
}

fn parse_str(
    input: &str,
    options: &ParseOptions,
    watch_meta_charset: bool,
) -> (Document, Vec<ParseError>, Option<String>) {
    let mut tokenizer = Tokenizer::new(input);
    let mut builder = TreeBuilder::new(options.scripting);
    builder.watch_meta_charset = watch_meta_charset;
    run(&mut tokenizer, &mut builder);
    let errors = merge_errors(&mut tokenizer, &mut builder);
    let meta = builder.meta_charset.take();
    (builder.document, errors, meta)
}

/// Determines the encoding of `bytes` (HTML §13.2.3.2), decodes them and
/// parses the result as a document. `transport_charset` is the `charset`
/// parameter of the resource's `Content-Type`, if any.
///
/// If the encoding was only a guess and the document declares another one
/// in a `<meta>` element, the document is parsed again with the declared
/// encoding (§13.2.3.4 "changing the encoding while parsing").
#[must_use]
pub fn parse_document_bytes(
    bytes: &[u8],
    transport_charset: Option<&str>,
    options: &ParseOptions,
) -> ParsedDocument {
    let (encoding, confidence) = encoding::sniff(bytes, transport_charset);
    let text = encoding::decode(bytes, encoding);
    // §13.2.3.4 "changing the encoding while parsing": if the encoding in
    // use is UTF-16BE/LE, the confidence becomes certain and a declaration
    // in the document is ignored. A declared UTF-16 means UTF-8 and
    // x-user-defined means windows-1252 (`adjust_declared` below); a
    // declaration of the encoding already in use changes nothing.
    let watch = confidence == Confidence::Tentative && !encoding::is_utf16(encoding);
    let (document, errors, meta) = parse_str(&text, options, watch);
    if let Some(declared) = meta.as_deref().and_then(encoding::lookup) {
        let declared = encoding::adjust_declared(declared);
        if declared != encoding {
            let text = encoding::decode(bytes, declared);
            let (document, errors, _) = parse_str(&text, options, false);
            return ParsedDocument {
                document,
                errors,
                encoding: declared.name(),
            };
        }
    }
    ParsedDocument {
        document,
        errors,
        encoding: encoding.name(),
    }
}

/// Parses `input` with the HTML fragment parsing algorithm (HTML §13.4)
/// in the given context. The parsed nodes are the children of
/// [`ParsedFragment::fragment`].
#[must_use]
pub fn parse_fragment(
    input: &str,
    context: &FragmentContext,
    options: &ParseOptions,
) -> ParsedFragment {
    let mut tokenizer = Tokenizer::new(input);
    let mut builder = TreeBuilder::new(options.scripting);
    let (state, root) = builder.init_fragment(
        context.namespace,
        &context.local_name,
        context.attributes.clone(),
        context.quirks_mode,
    );
    tokenizer.set_state(state);
    run(&mut tokenizer, &mut builder);
    let errors = merge_errors(&mut tokenizer, &mut builder);
    let mut document = builder.document;
    let fragment = document.create_document_fragment();
    let _ = document.reparent_children(root, fragment);
    document.detach(root);
    ParsedFragment {
        document,
        fragment,
        errors,
    }
}

#[cfg(test)]
mod tests;
