//! Tree construction (HTML §13.2.6).
//!
//! The tree builder receives tokens one at a time from the tokenizer and
//! builds a [`Document`]. Every insertion mode, foster parenting, the list of
//! active formatting elements, the adoption agency algorithm, template
//! contents and the fragment case are implemented as specified. Scripts are
//! never executed (there is no JavaScript engine yet), so the parts of the
//! algorithm that run script are skipped; the scripting flag only changes
//! how `noscript` is parsed.

use wb_dom::{Attribute as DomAttribute, Document, Namespace, NodeData, NodeId, QuirksMode};

use crate::ParseError;
use crate::tokenizer::{Attribute, Doctype, State, Tag, Token, is_ascii_whitespace};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mode {
    Initial,
    BeforeHtml,
    BeforeHead,
    InHead,
    InHeadNoscript,
    AfterHead,
    InBody,
    Text,
    InTable,
    InTableText,
    InCaption,
    InColumnGroup,
    InTableBody,
    InRow,
    InCell,
    InTemplate,
    AfterBody,
    InFrameset,
    AfterFrameset,
    AfterAfterBody,
    AfterAfterFrameset,
}

/// An entry in the list of active formatting elements (§13.2.4.3).
#[derive(Clone, Debug)]
enum Formatting {
    Marker,
    Element(NodeId, Tag),
}

/// The kinds of "has an element in ... scope" (§13.2.4.2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scope {
    Default,
    ListItem,
    Button,
    Table,
}

/// An insertion location: a parent and the child to insert before.
type Location = (NodeId, Option<NodeId>);

const SVG_TAG_ADJUSTMENTS: [(&str, &str); 37] = [
    ("altglyph", "altGlyph"),
    ("altglyphdef", "altGlyphDef"),
    ("altglyphitem", "altGlyphItem"),
    ("animatecolor", "animateColor"),
    ("animatemotion", "animateMotion"),
    ("animatetransform", "animateTransform"),
    ("clippath", "clipPath"),
    ("feblend", "feBlend"),
    ("fecolormatrix", "feColorMatrix"),
    ("fecomponenttransfer", "feComponentTransfer"),
    ("fecomposite", "feComposite"),
    ("feconvolvematrix", "feConvolveMatrix"),
    ("fediffuselighting", "feDiffuseLighting"),
    ("fedisplacementmap", "feDisplacementMap"),
    ("fedistantlight", "feDistantLight"),
    ("fedropshadow", "feDropShadow"),
    ("feflood", "feFlood"),
    ("fefunca", "feFuncA"),
    ("fefuncb", "feFuncB"),
    ("fefuncg", "feFuncG"),
    ("fefuncr", "feFuncR"),
    ("fegaussianblur", "feGaussianBlur"),
    ("feimage", "feImage"),
    ("femerge", "feMerge"),
    ("femergenode", "feMergeNode"),
    ("femorphology", "feMorphology"),
    ("feoffset", "feOffset"),
    ("fepointlight", "fePointLight"),
    ("fespecularlighting", "feSpecularLighting"),
    ("fespotlight", "feSpotLight"),
    ("fetile", "feTile"),
    ("feturbulence", "feTurbulence"),
    ("foreignobject", "foreignObject"),
    ("glyphref", "glyphRef"),
    ("lineargradient", "linearGradient"),
    ("radialgradient", "radialGradient"),
    ("textpath", "textPath"),
];

const SVG_ATTRIBUTE_ADJUSTMENTS: [(&str, &str); 58] = [
    ("attributename", "attributeName"),
    ("attributetype", "attributeType"),
    ("basefrequency", "baseFrequency"),
    ("baseprofile", "baseProfile"),
    ("calcmode", "calcMode"),
    ("clippathunits", "clipPathUnits"),
    ("diffuseconstant", "diffuseConstant"),
    ("edgemode", "edgeMode"),
    ("filterunits", "filterUnits"),
    ("glyphref", "glyphRef"),
    ("gradienttransform", "gradientTransform"),
    ("gradientunits", "gradientUnits"),
    ("kernelmatrix", "kernelMatrix"),
    ("kernelunitlength", "kernelUnitLength"),
    ("keypoints", "keyPoints"),
    ("keysplines", "keySplines"),
    ("keytimes", "keyTimes"),
    ("lengthadjust", "lengthAdjust"),
    ("limitingconeangle", "limitingConeAngle"),
    ("markerheight", "markerHeight"),
    ("markerunits", "markerUnits"),
    ("markerwidth", "markerWidth"),
    ("maskcontentunits", "maskContentUnits"),
    ("maskunits", "maskUnits"),
    ("numoctaves", "numOctaves"),
    ("pathlength", "pathLength"),
    ("patterncontentunits", "patternContentUnits"),
    ("patterntransform", "patternTransform"),
    ("patternunits", "patternUnits"),
    ("pointsatx", "pointsAtX"),
    ("pointsaty", "pointsAtY"),
    ("pointsatz", "pointsAtZ"),
    ("preservealpha", "preserveAlpha"),
    ("preserveaspectratio", "preserveAspectRatio"),
    ("primitiveunits", "primitiveUnits"),
    ("refx", "refX"),
    ("refy", "refY"),
    ("repeatcount", "repeatCount"),
    ("repeatdur", "repeatDur"),
    ("requiredextensions", "requiredExtensions"),
    ("requiredfeatures", "requiredFeatures"),
    ("specularconstant", "specularConstant"),
    ("specularexponent", "specularExponent"),
    ("spreadmethod", "spreadMethod"),
    ("startoffset", "startOffset"),
    ("stddeviation", "stdDeviation"),
    ("stitchtiles", "stitchTiles"),
    ("surfacescale", "surfaceScale"),
    ("systemlanguage", "systemLanguage"),
    ("tablevalues", "tableValues"),
    ("targetx", "targetX"),
    ("targety", "targetY"),
    ("textlength", "textLength"),
    ("viewbox", "viewBox"),
    ("viewtarget", "viewTarget"),
    ("xchannelselector", "xChannelSelector"),
    ("ychannelselector", "yChannelSelector"),
    ("zoomandpan", "zoomAndPan"),
];

/// Public identifier prefixes that put a document in quirks mode (§13.2.6.4.1).
const QUIRKY_PUBLIC_PREFIXES: [&str; 55] = [
    "+//silmaril//dtd html pro v0r11 19970101//",
    "-//as//dtd html 3.0 aswedit + extensions//",
    "-//advasoft ltd//dtd html 3.0 aswedit + extensions//",
    "-//ietf//dtd html 2.0 level 1//",
    "-//ietf//dtd html 2.0 level 2//",
    "-//ietf//dtd html 2.0 strict level 1//",
    "-//ietf//dtd html 2.0 strict level 2//",
    "-//ietf//dtd html 2.0 strict//",
    "-//ietf//dtd html 2.0//",
    "-//ietf//dtd html 2.1e//",
    "-//ietf//dtd html 3.0//",
    "-//ietf//dtd html 3.2 final//",
    "-//ietf//dtd html 3.2//",
    "-//ietf//dtd html 3//",
    "-//ietf//dtd html level 0//",
    "-//ietf//dtd html level 1//",
    "-//ietf//dtd html level 2//",
    "-//ietf//dtd html level 3//",
    "-//ietf//dtd html strict level 0//",
    "-//ietf//dtd html strict level 1//",
    "-//ietf//dtd html strict level 2//",
    "-//ietf//dtd html strict level 3//",
    "-//ietf//dtd html strict//",
    "-//ietf//dtd html//",
    "-//metrius//dtd metrius presentational//",
    "-//microsoft//dtd internet explorer 2.0 html strict//",
    "-//microsoft//dtd internet explorer 2.0 html//",
    "-//microsoft//dtd internet explorer 2.0 tables//",
    "-//microsoft//dtd internet explorer 3.0 html strict//",
    "-//microsoft//dtd internet explorer 3.0 html//",
    "-//microsoft//dtd internet explorer 3.0 tables//",
    "-//netscape comm. corp.//dtd html//",
    "-//netscape comm. corp.//dtd strict html//",
    "-//o'reilly and associates//dtd html 2.0//",
    "-//o'reilly and associates//dtd html extended 1.0//",
    "-//o'reilly and associates//dtd html extended relaxed 1.0//",
    "-//sq//dtd html 2.0 hotmetal + extensions//",
    "-//softquad software//dtd hotmetal pro 6.0::19990601::extensions to html 4.0//",
    "-//softquad//dtd hotmetal pro 4.0::19971010::extensions to html 4.0//",
    "-//spyglass//dtd html 2.0 extended//",
    "-//sun microsystems corp.//dtd hotjava html//",
    "-//sun microsystems corp.//dtd hotjava strict html//",
    "-//w3c//dtd html 3 1995-03-24//",
    "-//w3c//dtd html 3.2 draft//",
    "-//w3c//dtd html 3.2 final//",
    "-//w3c//dtd html 3.2//",
    "-//w3c//dtd html 3.2s draft//",
    "-//w3c//dtd html 4.0 frameset//",
    "-//w3c//dtd html 4.0 transitional//",
    "-//w3c//dtd html experimental 19960712//",
    "-//w3c//dtd html experimental 970421//",
    "-//w3c//dtd w3 html//",
    "-//w3o//dtd w3 html 3.0//",
    "-//webtechs//dtd mozilla html 2.0//",
    "-//webtechs//dtd mozilla html//",
];

/// The tree builder state (§13.2.4).
#[derive(Debug)]
pub(crate) struct TreeBuilder {
    pub(crate) document: Document,
    pub(crate) errors: Vec<ParseError>,
    /// A state the tokenizer must switch to before the next token.
    pub(crate) tokenizer_state: Option<State>,
    /// The `charset` declared by the first `meta` element in `head` whose
    /// declaration names an encoding (§13.2.6.4.4), when asked to watch.
    pub(crate) meta_charset: Option<String>,
    pub(crate) watch_meta_charset: bool,
    pub(crate) stopped: bool,
    mode: Mode,
    original_mode: Mode,
    template_modes: Vec<Mode>,
    open: Vec<NodeId>,
    active: Vec<Formatting>,
    head: Option<NodeId>,
    form: Option<NodeId>,
    frameset_ok: bool,
    foster_parenting: bool,
    scripting: bool,
    context: Option<NodeId>,
    pending_table_chars: Vec<char>,
    ignore_next_lf: bool,
    self_closing_pending: bool,
    position: (u32, u32),
}

/// Whether `tag` is a start tag with one of these names.
fn is_start(token: &Token, names: &[&str]) -> bool {
    matches!(token, Token::StartTag(tag) if names.contains(&tag.name.as_str()))
}

/// Whether `tag` is an end tag with one of these names.
fn is_end(token: &Token, names: &[&str]) -> bool {
    matches!(token, Token::EndTag(tag) if names.contains(&tag.name.as_str()))
}

fn tag_of(token: &Token) -> Option<&Tag> {
    match token {
        Token::StartTag(tag) | Token::EndTag(tag) => Some(tag),
        _ => None,
    }
}

impl TreeBuilder {
    pub(crate) fn new(scripting: bool) -> TreeBuilder {
        TreeBuilder {
            document: Document::new(),
            errors: Vec::new(),
            tokenizer_state: None,
            meta_charset: None,
            watch_meta_charset: false,
            stopped: false,
            mode: Mode::Initial,
            original_mode: Mode::Initial,
            template_modes: Vec::new(),
            open: Vec::new(),
            active: Vec::new(),
            head: None,
            form: None,
            frameset_ok: true,
            foster_parenting: false,
            scripting,
            context: None,
            pending_table_chars: Vec::new(),
            ignore_next_lf: false,
            self_closing_pending: false,
            position: (0, 0),
        }
    }

    /// Sets up the fragment case (§13.4 "initialize an HTML fragment
    /// parser"). Returns the tokenizer state to start in and the root `html`
    /// element whose children become the fragment.
    pub(crate) fn init_fragment(
        &mut self,
        namespace: Namespace,
        local_name: &str,
        attributes: Vec<DomAttribute>,
        quirks_mode: QuirksMode,
    ) -> (State, NodeId) {
        self.document.set_quirks_mode(quirks_mode);
        let context = self
            .document
            .create_element(namespace, local_name, attributes);
        self.context = Some(context);
        let state = if namespace == Namespace::Html {
            match local_name {
                "title" | "textarea" => State::Rcdata,
                "style" | "xmp" | "iframe" | "noembed" | "noframes" => State::Rawtext,
                "script" => State::ScriptData,
                "noscript" if self.scripting => State::Rawtext,
                "plaintext" => State::Plaintext,
                _ => State::Data,
            }
        } else {
            State::Data
        };
        let root = self
            .document
            .create_element(Namespace::Html, "html", Vec::new());
        let document_node = self.document.root();
        self.append(document_node, root);
        self.open.push(root);
        if self.document.is_html_element(context, "template") {
            self.template_modes.push(Mode::InTemplate);
        }
        self.reset_insertion_mode();
        if self.document.is_html_element(context, "form") {
            self.form = Some(context);
        }
        (state, root)
    }

    /// Whether the adjusted current node exists and is not an HTML element
    /// (decides whether `<![CDATA[` opens a CDATA section).
    pub(crate) fn adjusted_current_node_is_foreign(&self) -> bool {
        self.adjusted_current_node()
            .and_then(|node| self.document.element(node))
            .is_some_and(|e| e.namespace != Namespace::Html)
    }

    pub(crate) fn set_position(&mut self, position: (u32, u32)) {
        self.position = position;
    }

    fn error(&mut self, code: &'static str) {
        self.errors.push(ParseError {
            code,
            line: self.position.0,
            column: self.position.1,
        });
    }

    // ----- The dispatcher (§13.2.6) ---------------------------------------

    /// Processes one token from the tokenizer.
    pub(crate) fn process(&mut self, token: Token) {
        if self.stopped {
            return;
        }
        if std::mem::take(&mut self.ignore_next_lf) && token == Token::Character('\n') {
            return;
        }
        self.self_closing_pending = matches!(&token, Token::StartTag(tag) if tag.self_closing);
        let mut next = Some(token);
        while let Some(token) = next {
            next = if self.use_html_rules(&token) {
                self.process_in_mode(self.mode, token)
            } else {
                self.foreign_content(token)
            };
        }
        if self.self_closing_pending {
            self.self_closing_pending = false;
            self.error("non-void-html-element-start-tag-with-trailing-solidus");
        }
    }

    fn acknowledge_self_closing(&mut self) {
        self.self_closing_pending = false;
    }

    fn use_html_rules(&self, token: &Token) -> bool {
        let Some(node) = self.adjusted_current_node() else {
            return true;
        };
        let Some(element) = self.document.element(node) else {
            return true;
        };
        if element.namespace == Namespace::Html {
            return true;
        }
        if self.is_mathml_text_integration_point(node) {
            match token {
                Token::StartTag(tag) if tag.name != "mglyph" && tag.name != "malignmark" => {
                    return true;
                }
                Token::Character(_) => return true,
                _ => {}
            }
        }
        if element.namespace == Namespace::MathMl
            && element.local_name == "annotation-xml"
            && is_start(token, &["svg"])
        {
            return true;
        }
        if self.is_html_integration_point(node)
            && matches!(token, Token::StartTag(_) | Token::Character(_))
        {
            return true;
        }
        matches!(token, Token::Eof)
    }

    fn process_in_mode(&mut self, mode: Mode, token: Token) -> Option<Token> {
        match mode {
            Mode::Initial => self.initial(token),
            Mode::BeforeHtml => self.before_html(token),
            Mode::BeforeHead => self.before_head(token),
            Mode::InHead => self.in_head(token),
            Mode::InHeadNoscript => self.in_head_noscript(token),
            Mode::AfterHead => self.after_head(token),
            Mode::InBody => self.in_body(token),
            Mode::Text => self.text(token),
            Mode::InTable => self.in_table(token),
            Mode::InTableText => self.in_table_text(token),
            Mode::InCaption => self.in_caption(token),
            Mode::InColumnGroup => self.in_column_group(token),
            Mode::InTableBody => self.in_table_body(token),
            Mode::InRow => self.in_row(token),
            Mode::InCell => self.in_cell(token),
            Mode::InTemplate => self.in_template(token),
            Mode::AfterBody => self.after_body(token),
            Mode::InFrameset => self.in_frameset(token),
            Mode::AfterFrameset => self.after_frameset(token),
            Mode::AfterAfterBody => self.after_after_body(token),
            Mode::AfterAfterFrameset => self.after_after_frameset(token),
        }
    }

    // ----- Element and stack helpers --------------------------------------

    fn current_node(&self) -> Option<NodeId> {
        self.open.last().copied()
    }

    fn adjusted_current_node(&self) -> Option<NodeId> {
        match self.context {
            Some(context) if self.open.len() == 1 => Some(context),
            _ => self.current_node(),
        }
    }

    fn is_html(&self, node: NodeId, name: &str) -> bool {
        self.document.is_html_element(node, name)
    }

    fn is_html_one_of(&self, node: NodeId, names: &[&str]) -> bool {
        self.document.element(node).is_some_and(|e| {
            e.namespace == Namespace::Html && names.contains(&e.local_name.as_str())
        })
    }

    fn current_is_html(&self, name: &str) -> bool {
        self.current_node().is_some_and(|n| self.is_html(n, name))
    }

    fn current_is_html_one_of(&self, names: &[&str]) -> bool {
        self.current_node()
            .is_some_and(|n| self.is_html_one_of(n, names))
    }

    fn is_mathml_text_integration_point(&self, node: NodeId) -> bool {
        self.document.element(node).is_some_and(|e| {
            e.namespace == Namespace::MathMl
                && matches!(e.local_name.as_str(), "mi" | "mo" | "mn" | "ms" | "mtext")
        })
    }

    fn is_html_integration_point(&self, node: NodeId) -> bool {
        self.document
            .element(node)
            .is_some_and(|e| match e.namespace {
                Namespace::MathMl => {
                    e.local_name == "annotation-xml"
                        && e.attributes.iter().any(|a| {
                            a.namespace.is_none()
                                && a.local_name == "encoding"
                                && (a.value.eq_ignore_ascii_case("text/html")
                                    || a.value.eq_ignore_ascii_case("application/xhtml+xml"))
                        })
                }
                Namespace::Svg => {
                    matches!(e.local_name.as_str(), "foreignObject" | "desc" | "title")
                }
                _ => false,
            })
    }

    /// The special category (§13.2.4.2).
    fn is_special(&self, node: NodeId) -> bool {
        self.document
            .element(node)
            .is_some_and(|e| match e.namespace {
                Namespace::Html => matches!(
                    e.local_name.as_str(),
                    "address"
                        | "applet"
                        | "area"
                        | "article"
                        | "aside"
                        | "base"
                        | "basefont"
                        | "bgsound"
                        | "blockquote"
                        | "body"
                        | "br"
                        | "button"
                        | "caption"
                        | "center"
                        | "col"
                        | "colgroup"
                        | "dd"
                        | "details"
                        | "dir"
                        | "div"
                        | "dl"
                        | "dt"
                        | "embed"
                        | "fieldset"
                        | "figcaption"
                        | "figure"
                        | "footer"
                        | "form"
                        | "frame"
                        | "frameset"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                        | "head"
                        | "header"
                        | "hgroup"
                        | "hr"
                        | "html"
                        | "iframe"
                        | "img"
                        | "input"
                        | "keygen"
                        | "li"
                        | "link"
                        | "listing"
                        | "main"
                        | "marquee"
                        | "menu"
                        | "meta"
                        | "nav"
                        | "noembed"
                        | "noframes"
                        | "noscript"
                        | "object"
                        | "ol"
                        | "p"
                        | "param"
                        | "plaintext"
                        | "pre"
                        | "script"
                        | "search"
                        | "section"
                        | "select"
                        | "source"
                        | "style"
                        | "summary"
                        | "table"
                        | "tbody"
                        | "td"
                        | "template"
                        | "textarea"
                        | "tfoot"
                        | "th"
                        | "thead"
                        | "title"
                        | "tr"
                        | "track"
                        | "ul"
                        | "wbr"
                        | "xmp"
                ),
                Namespace::MathMl => matches!(
                    e.local_name.as_str(),
                    "mi" | "mo" | "mn" | "ms" | "mtext" | "annotation-xml"
                ),
                Namespace::Svg => {
                    matches!(e.local_name.as_str(), "foreignObject" | "desc" | "title")
                }
                _ => false,
            })
    }

    fn is_scope_boundary(&self, node: NodeId, scope: Scope) -> bool {
        let Some(e) = self.document.element(node) else {
            return false;
        };
        let name = e.local_name.as_str();
        match (scope, e.namespace) {
            (Scope::Table, Namespace::Html) => matches!(name, "html" | "table" | "template"),
            (Scope::Table, _) => false,
            (_, Namespace::Html) => {
                matches!(
                    name,
                    "applet"
                        | "caption"
                        | "html"
                        | "table"
                        | "td"
                        | "th"
                        | "marquee"
                        | "object"
                        | "select"
                        | "template"
                ) || (scope == Scope::ListItem && matches!(name, "ol" | "ul"))
                    || (scope == Scope::Button && name == "button")
            }
            (_, Namespace::MathMl) => {
                matches!(name, "mi" | "mo" | "mn" | "ms" | "mtext" | "annotation-xml")
            }
            (_, Namespace::Svg) => matches!(name, "foreignObject" | "desc" | "title"),
            _ => false,
        }
    }

    /// "Has an element in scope" for HTML elements with any of `names`.
    fn in_scope_any(&self, names: &[&str], scope: Scope) -> bool {
        for &node in self.open.iter().rev() {
            if self.is_html_one_of(node, names) {
                return true;
            }
            if self.is_scope_boundary(node, scope) {
                return false;
            }
        }
        false
    }

    fn in_scope(&self, name: &str, scope: Scope) -> bool {
        self.in_scope_any(&[name], scope)
    }

    /// "Has an element in scope" for one particular node.
    fn node_in_scope(&self, target: NodeId) -> bool {
        for &node in self.open.iter().rev() {
            if node == target {
                return true;
            }
            if self.is_scope_boundary(node, Scope::Default) {
                return false;
            }
        }
        false
    }

    fn has_open_html(&self, name: &str) -> bool {
        self.open.iter().any(|&n| self.is_html(n, name))
    }

    /// "Parsing template contents" (§13.2.4.4): a template is open or the
    /// fragment context element is a template.
    fn parsing_template_contents(&self) -> bool {
        self.has_open_html("template") || self.context.is_some_and(|c| self.is_html(c, "template"))
    }

    fn pop(&mut self) -> Option<NodeId> {
        self.open.pop()
    }

    /// Pops until an HTML element with one of `names` has been popped.
    fn pop_until_any(&mut self, names: &[&str]) {
        while let Some(node) = self.pop() {
            if self.is_html_one_of(node, names) {
                break;
            }
        }
    }

    fn pop_until(&mut self, name: &str) {
        self.pop_until_any(&[name]);
    }

    fn remove_from_stack(&mut self, target: NodeId) {
        if let Some(index) = self.open.iter().rposition(|&n| n == target) {
            self.open.remove(index);
        }
    }

    /// §13.2.6.3 Generate implied end tags (except for `except`).
    fn generate_implied_end_tags(&mut self, except: Option<&str>) {
        const IMPLIED: [&str; 10] = [
            "dd", "dt", "li", "optgroup", "option", "p", "rb", "rp", "rt", "rtc",
        ];
        while let Some(node) = self.current_node() {
            let pop = self.document.element(node).is_some_and(|e| {
                e.namespace == Namespace::Html
                    && IMPLIED.contains(&e.local_name.as_str())
                    && Some(e.local_name.as_str()) != except
            });
            if !pop {
                break;
            }
            self.pop();
        }
    }

    /// §13.2.6.3 Generate all implied end tags thoroughly.
    fn generate_all_implied_end_tags_thoroughly(&mut self) {
        while self.current_is_html_one_of(&[
            "caption", "colgroup", "dd", "dt", "li", "optgroup", "option", "p", "rb", "rp", "rt",
            "rtc", "tbody", "td", "tfoot", "th", "thead", "tr",
        ]) {
            self.pop();
        }
    }

    /// §13.2.4.1 Reset the insertion mode appropriately.
    fn reset_insertion_mode(&mut self) {
        let mut index = self.open.len();
        while index > 0 {
            index -= 1;
            let last = index == 0;
            let Some(mut node) = self.open.get(index).copied() else {
                break;
            };
            if last && let Some(context) = self.context {
                node = context;
            }
            let Some(element) = self.document.element(node) else {
                continue;
            };
            if element.namespace == Namespace::Html {
                let mode = match element.local_name.as_str() {
                    "td" | "th" if !last => Some(Mode::InCell),
                    "tr" => Some(Mode::InRow),
                    "tbody" | "thead" | "tfoot" => Some(Mode::InTableBody),
                    "caption" => Some(Mode::InCaption),
                    "colgroup" => Some(Mode::InColumnGroup),
                    "table" => Some(Mode::InTable),
                    "template" => Some(
                        self.template_modes
                            .last()
                            .copied()
                            .unwrap_or(Mode::InTemplate),
                    ),
                    "head" if !last => Some(Mode::InHead),
                    "body" => Some(Mode::InBody),
                    "frameset" => Some(Mode::InFrameset),
                    "html" => Some(if self.head.is_none() {
                        Mode::BeforeHead
                    } else {
                        Mode::AfterHead
                    }),
                    _ => None,
                };
                if let Some(mode) = mode {
                    self.mode = mode;
                    return;
                }
            }
            if last {
                self.mode = Mode::InBody;
                return;
            }
        }
        self.mode = Mode::InBody;
    }

    // ----- Creating and inserting nodes (§13.2.6.1) -----------------------

    fn append(&mut self, parent: NodeId, child: NodeId) {
        // Insertion failures can only come from impossible hierarchies,
        // which the algorithm avoids; ignore them rather than panic.
        let _ = self.document.append_child(parent, child);
    }

    fn template_contents_or_self(&self, node: NodeId) -> NodeId {
        if self.is_html(node, "template") {
            self.document.template_contents(node).unwrap_or(node)
        } else {
            node
        }
    }

    /// The appropriate place for inserting a node.
    fn appropriate_place(&self, override_target: Option<NodeId>) -> Option<Location> {
        let target = override_target.or_else(|| self.current_node())?;
        if self.foster_parenting
            && self.is_html_one_of(target, &["table", "tbody", "tfoot", "thead", "tr"])
        {
            let last_template = self.open.iter().rposition(|&n| self.is_html(n, "template"));
            let last_table = self.open.iter().rposition(|&n| self.is_html(n, "table"));
            match (last_template, last_table) {
                (Some(template), table) if table.is_none_or(|table| template > table) => {
                    let template = self.open.get(template).copied()?;
                    return Some((self.template_contents_or_self(template), None));
                }
                (_, None) => return self.open.first().map(|&html| (html, None)),
                (_, Some(table_index)) => {
                    let table = self.open.get(table_index).copied()?;
                    if let Some(parent) = self.document.parent(table) {
                        return Some((parent, Some(table)));
                    }
                    let previous = self.open.get(table_index.checked_sub(1)?).copied()?;
                    return Some((self.template_contents_or_self(previous), None));
                }
            }
        }
        Some((self.template_contents_or_self(target), None))
    }

    fn insert_at(&mut self, (parent, before): Location, node: NodeId) {
        if matches!(self.document.data(parent), Some(NodeData::Document { .. }))
            && self.document.is_element(node)
            && self.document.document_element().is_some()
        {
            return;
        }
        let _ = self.document.insert_before(parent, node, before);
    }

    fn create_element(
        &mut self,
        namespace: Namespace,
        name: &str,
        attributes: Vec<DomAttribute>,
    ) -> NodeId {
        self.document.create_element(namespace, name, attributes)
    }

    fn html_attributes(tag: &Tag) -> Vec<DomAttribute> {
        tag.attributes
            .iter()
            .map(|a| DomAttribute::new(a.name.clone(), a.value.clone()))
            .collect()
    }

    /// Insert an HTML element for a token.
    fn insert_html_element(&mut self, tag: &Tag) -> NodeId {
        let element = self.create_element(Namespace::Html, &tag.name, Self::html_attributes(tag));
        if let Some(location) = self.appropriate_place(None) {
            self.insert_at(location, element);
        }
        self.open.push(element);
        element
    }

    fn insert_html_element_named(&mut self, name: &str) -> NodeId {
        self.insert_html_element(&Tag {
            name: name.to_string(),
            ..Tag::default()
        })
    }

    /// Insert a foreign element: adjusts attributes for `namespace`.
    fn insert_foreign_element(&mut self, tag: &Tag, namespace: Namespace) -> NodeId {
        let attributes = adjust_attributes(&tag.attributes, namespace);
        let element = self.create_element(namespace, &tag.name, attributes);
        if let Some(location) = self.appropriate_place(None) {
            self.insert_at(location, element);
        }
        self.open.push(element);
        element
    }

    /// Insert a character.
    fn insert_char(&mut self, c: char) {
        let mut buffer = [0; 4];
        self.insert_text(c.encode_utf8(&mut buffer));
    }

    fn insert_text(&mut self, data: &str) {
        let Some((parent, before)) = self.appropriate_place(None) else {
            return;
        };
        if matches!(self.document.data(parent), Some(NodeData::Document { .. })) {
            return;
        }
        let previous = match before {
            Some(before) => self.document.previous_sibling(before),
            None => self.document.last_child(parent),
        };
        if let Some(previous) = previous
            && self.document.append_text(previous, data)
        {
            return;
        }
        let text = self.document.create_text(data);
        let _ = self.document.insert_before(parent, text, before);
    }

    fn insert_comment(&mut self, data: String, location: Option<Location>) {
        let Some(location) = location.or_else(|| self.appropriate_place(None)) else {
            return;
        };
        let comment = self.document.create_comment(data);
        self.insert_at(location, comment);
    }

    fn insert_processing_instruction(
        &mut self,
        target: String,
        data: String,
        location: Option<Location>,
    ) {
        let Some(location) = location.or_else(|| self.appropriate_place(None)) else {
            return;
        };
        let pi = self.document.create_processing_instruction(target, data);
        self.insert_at(location, pi);
    }

    fn document_location(&self) -> Location {
        (self.document.root(), None)
    }

    // ----- The list of active formatting elements (§13.2.4.3) -------------

    fn push_formatting(&mut self, element: NodeId, tag: &Tag) {
        // The Noah's Ark clause: at most three matching entries after the
        // last marker.
        let mut matching = Vec::new();
        for (index, entry) in self.active.iter().enumerate().rev() {
            match entry {
                Formatting::Marker => break,
                Formatting::Element(_, other) => {
                    if other.name == tag.name && same_attributes(&other.attributes, &tag.attributes)
                    {
                        matching.push(index);
                    }
                }
            }
        }
        if matching.len() >= 3
            && let Some(&earliest) = matching.last()
        {
            self.active.remove(earliest);
        }
        self.active.push(Formatting::Element(element, tag.clone()));
    }

    fn push_marker(&mut self) {
        self.active.push(Formatting::Marker);
    }

    fn clear_formatting_to_last_marker(&mut self) {
        while let Some(entry) = self.active.pop() {
            if matches!(entry, Formatting::Marker) {
                break;
            }
        }
    }

    fn formatting_index(&self, node: NodeId) -> Option<usize> {
        self.active
            .iter()
            .position(|e| matches!(e, Formatting::Element(n, _) if *n == node))
    }

    fn is_open(&self, node: NodeId) -> bool {
        self.open.contains(&node)
    }

    /// Reconstruct the active formatting elements.
    fn reconstruct_formatting(&mut self) {
        let Some(last) = self.active.last() else {
            return;
        };
        match last {
            Formatting::Marker => return,
            Formatting::Element(node, _) if self.is_open(*node) => return,
            Formatting::Element(..) => {}
        }
        let mut index = self.active.len() - 1;
        // Rewind.
        while index > 0 {
            let open = match self.active.get(index - 1) {
                Some(Formatting::Marker) => true,
                Some(Formatting::Element(node, _)) => self.is_open(*node),
                None => true,
            };
            if open {
                break;
            }
            index -= 1;
        }
        // Advance and create.
        while index < self.active.len() {
            let Some(Formatting::Element(_, tag)) = self.active.get(index).cloned() else {
                index += 1;
                continue;
            };
            let element = self.insert_html_element(&tag);
            if let Some(entry) = self.active.get_mut(index) {
                *entry = Formatting::Element(element, tag);
            }
            index += 1;
        }
    }

    // ----- Shared algorithms ----------------------------------------------

    /// Close a p element.
    fn close_p_element(&mut self) {
        self.generate_implied_end_tags(Some("p"));
        if !self.current_is_html("p") {
            self.error("unexpected-end-tag");
        }
        self.pop_until("p");
    }

    fn close_p_if_in_button_scope(&mut self) {
        if self.in_scope("p", Scope::Button) {
            self.close_p_element();
        }
    }

    /// The generic raw text and RCDATA element parsing algorithms.
    fn parse_text_element(&mut self, tag: &Tag, state: State) {
        self.insert_html_element(tag);
        self.tokenizer_state = Some(state);
        self.original_mode = self.mode;
        self.mode = Mode::Text;
    }

    /// The "any other end tag" steps of "in body".
    fn any_other_end_tag(&mut self, name: &str) {
        let mut index = self.open.len();
        while index > 0 {
            index -= 1;
            let Some(node) = self.open.get(index).copied() else {
                return;
            };
            if self.is_html(node, name) {
                self.generate_implied_end_tags(Some(name));
                if self.current_node() != Some(node) {
                    self.error("unexpected-end-tag");
                }
                self.open.truncate(index);
                return;
            }
            if self.is_special(node) {
                self.error("unexpected-end-tag");
                return;
            }
        }
    }

    /// The adoption agency algorithm (§13.2.6.4.7).
    fn adoption_agency(&mut self, subject: &str) {
        if let Some(current) = self.current_node()
            && self.is_html(current, subject)
            && self.formatting_index(current).is_none()
        {
            self.pop();
            return;
        }
        for _ in 0..8 {
            // Find the formatting element.
            let mut found = None;
            for (index, entry) in self.active.iter().enumerate().rev() {
                match entry {
                    Formatting::Marker => break,
                    Formatting::Element(node, tag) if tag.name == subject => {
                        found = Some((index, *node));
                        break;
                    }
                    Formatting::Element(..) => {}
                }
            }
            let Some((formatting_index, formatting_element)) = found else {
                self.any_other_end_tag(subject);
                return;
            };
            let Some(stack_index) = self.open.iter().position(|&n| n == formatting_element) else {
                self.error("adoption-agency-1.2");
                self.active.remove(formatting_index);
                return;
            };
            if !self.node_in_scope(formatting_element) {
                self.error("adoption-agency-4.4");
                return;
            }
            if self.current_node() != Some(formatting_element) {
                self.error("adoption-agency-1.3");
            }
            let furthest = self
                .open
                .iter()
                .enumerate()
                .skip(stack_index + 1)
                .find(|(_, n)| self.is_special(**n))
                .map(|(i, n)| (i, *n));
            let Some((furthest_index, furthest_block)) = furthest else {
                self.open.truncate(stack_index);
                self.active.remove(formatting_index);
                return;
            };
            let Some(common_ancestor) = stack_index
                .checked_sub(1)
                .and_then(|i| self.open.get(i).copied())
            else {
                return;
            };
            let mut bookmark = formatting_index;
            let mut node_index = furthest_index;
            let mut last_node = furthest_block;
            let mut inner = 0;
            let mut bookmark_moved = false;
            loop {
                inner += 1;
                node_index -= 1;
                let Some(mut node) = self.open.get(node_index).copied() else {
                    break;
                };
                if node == formatting_element {
                    break;
                }
                let mut active_index = self.formatting_index(node);
                if inner > 3
                    && let Some(i) = active_index
                {
                    self.active.remove(i);
                    if i < bookmark {
                        bookmark -= 1;
                    }
                    active_index = None;
                }
                let Some(active_index) = active_index else {
                    self.open.remove(node_index);
                    continue;
                };
                let Some(Formatting::Element(_, tag)) = self.active.get(active_index).cloned()
                else {
                    break;
                };
                let new_element =
                    self.create_element(Namespace::Html, &tag.name, Self::html_attributes(&tag));
                if let Some(entry) = self.active.get_mut(active_index) {
                    *entry = Formatting::Element(new_element, tag);
                }
                if let Some(slot) = self.open.get_mut(node_index) {
                    *slot = new_element;
                }
                node = new_element;
                if !bookmark_moved {
                    bookmark = active_index + 1;
                    bookmark_moved = true;
                }
                self.document.detach(last_node);
                self.append(node, last_node);
                last_node = node;
            }
            self.document.detach(last_node);
            if let Some(location) = self.appropriate_place(Some(common_ancestor)) {
                self.insert_at(location, last_node);
            }
            let Some(Formatting::Element(_, tag)) = self
                .formatting_index(formatting_element)
                .and_then(|i| self.active.get(i).cloned())
            else {
                return;
            };
            let new_element =
                self.create_element(Namespace::Html, &tag.name, Self::html_attributes(&tag));
            let _ = self.document.reparent_children(furthest_block, new_element);
            self.append(furthest_block, new_element);
            if let Some(index) = self.formatting_index(formatting_element) {
                self.active.remove(index);
                if index < bookmark {
                    bookmark -= 1;
                }
            }
            let bookmark = bookmark.min(self.active.len());
            self.active
                .insert(bookmark, Formatting::Element(new_element, tag));
            self.remove_from_stack(formatting_element);
            if let Some(position) = self.open.iter().position(|&n| n == furthest_block) {
                self.open.insert(position + 1, new_element);
            }
        }
    }

    // ----- The insertion modes (§13.2.6.4) --------------------------------

    /// §13.2.6.4.1
    fn initial(&mut self, token: Token) -> Option<Token> {
        match token {
            Token::Character(c) if is_ascii_whitespace(c) => None,
            Token::Comment(data) => {
                let location = self.document_location();
                self.insert_comment(data, Some(location));
                None
            }
            Token::ProcessingInstruction { target, data } => {
                let location = self.document_location();
                self.insert_processing_instruction(target, data, Some(location));
                None
            }
            Token::Doctype(doctype) => {
                self.doctype(&doctype);
                self.mode = Mode::BeforeHtml;
                None
            }
            token => {
                self.error("expected-doctype-but-got-other");
                self.document.set_quirks_mode(QuirksMode::Quirks);
                self.mode = Mode::BeforeHtml;
                Some(token)
            }
        }
    }

    fn doctype(&mut self, doctype: &Doctype) {
        let name = doctype.name.as_deref().unwrap_or_default();
        if name != "html"
            || doctype.public_id.is_some()
            || doctype
                .system_id
                .as_deref()
                .is_some_and(|s| s != "about:legacy-compat")
        {
            self.error("unknown-doctype");
        }
        let node = self.document.create_doctype(
            name,
            doctype.public_id.as_deref().unwrap_or_default(),
            doctype.system_id.as_deref().unwrap_or_default(),
        );
        let root = self.document.root();
        if self.document.doctype().is_none() && self.document.document_element().is_none() {
            self.append(root, node);
        }
        let public = doctype
            .public_id
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let system = doctype.system_id.as_deref();
        let system_lower = system.map(str::to_ascii_lowercase);
        let system_missing = system.is_none();
        let quirks = doctype.force_quirks
            || name != "html"
            || public == "-//w3o//dtd w3 html strict 3.0//en//"
            || public == "-/w3c/dtd html 4.0 transitional/en"
            || public == "html"
            || system_lower.as_deref()
                == Some("http://www.ibm.com/data/dtd/v11/ibmxhtml1-transitional.dtd")
            || QUIRKY_PUBLIC_PREFIXES
                .iter()
                .any(|prefix| public.starts_with(prefix))
            || (system_missing
                && (public.starts_with("-//w3c//dtd html 4.01 frameset//")
                    || public.starts_with("-//w3c//dtd html 4.01 transitional//")));
        let limited = !quirks
            && (public.starts_with("-//w3c//dtd xhtml 1.0 frameset//")
                || public.starts_with("-//w3c//dtd xhtml 1.0 transitional//")
                || (!system_missing
                    && (public.starts_with("-//w3c//dtd html 4.01 frameset//")
                        || public.starts_with("-//w3c//dtd html 4.01 transitional//"))));
        if quirks {
            self.document.set_quirks_mode(QuirksMode::Quirks);
        } else if limited {
            self.document.set_quirks_mode(QuirksMode::LimitedQuirks);
        }
    }

    /// §13.2.6.4.2
    fn before_html(&mut self, token: Token) -> Option<Token> {
        match token {
            Token::Doctype(_) => {
                self.error("unexpected-doctype");
                None
            }
            Token::Comment(data) => {
                let location = self.document_location();
                self.insert_comment(data, Some(location));
                None
            }
            Token::ProcessingInstruction { target, data } => {
                let location = self.document_location();
                self.insert_processing_instruction(target, data, Some(location));
                None
            }
            Token::Character(c) if is_ascii_whitespace(c) => None,
            Token::StartTag(tag) if tag.name == "html" => {
                let element =
                    self.create_element(Namespace::Html, "html", Self::html_attributes(&tag));
                let location = self.document_location();
                self.insert_at(location, element);
                self.open.push(element);
                self.mode = Mode::BeforeHead;
                None
            }
            Token::EndTag(ref tag)
                if !matches!(tag.name.as_str(), "head" | "body" | "html" | "br") =>
            {
                self.error("unexpected-end-tag");
                None
            }
            token => {
                let element = self.create_element(Namespace::Html, "html", Vec::new());
                let location = self.document_location();
                self.insert_at(location, element);
                self.open.push(element);
                self.mode = Mode::BeforeHead;
                Some(token)
            }
        }
    }

    /// §13.2.6.4.3
    fn before_head(&mut self, token: Token) -> Option<Token> {
        match token {
            Token::Character(c) if is_ascii_whitespace(c) => None,
            Token::Comment(data) => {
                self.insert_comment(data, None);
                None
            }
            Token::ProcessingInstruction { target, data } => {
                self.insert_processing_instruction(target, data, None);
                None
            }
            Token::Doctype(_) => {
                self.error("unexpected-doctype");
                None
            }
            Token::StartTag(ref tag) if tag.name == "html" => self.in_body(token),
            Token::StartTag(tag) if tag.name == "head" => {
                let head = self.insert_html_element(&tag);
                self.head = Some(head);
                self.mode = Mode::InHead;
                None
            }
            Token::EndTag(ref tag)
                if !matches!(tag.name.as_str(), "head" | "body" | "html" | "br") =>
            {
                self.error("unexpected-end-tag");
                None
            }
            token => {
                let head = self.insert_html_element_named("head");
                self.head = Some(head);
                self.mode = Mode::InHead;
                Some(token)
            }
        }
    }

    /// §13.2.6.4.4
    fn in_head(&mut self, token: Token) -> Option<Token> {
        match token {
            Token::Character(c) if is_ascii_whitespace(c) => {
                self.insert_char(c);
                None
            }
            Token::Comment(data) => {
                self.insert_comment(data, None);
                None
            }
            Token::ProcessingInstruction { target, data } => {
                self.insert_processing_instruction(target, data, None);
                None
            }
            Token::Doctype(_) => {
                self.error("unexpected-doctype");
                None
            }
            Token::StartTag(tag) => match tag.name.as_str() {
                "html" => self.in_body(Token::StartTag(tag)),
                "base" | "basefont" | "bgsound" | "link" => {
                    self.insert_html_element(&tag);
                    self.pop();
                    self.acknowledge_self_closing();
                    None
                }
                "meta" => {
                    self.insert_html_element(&tag);
                    self.pop();
                    self.acknowledge_self_closing();
                    self.check_meta_charset(&tag);
                    None
                }
                "title" => {
                    self.parse_text_element(&tag, State::Rcdata);
                    None
                }
                "noscript" if self.scripting => {
                    self.parse_text_element(&tag, State::Rawtext);
                    None
                }
                "noframes" | "style" => {
                    self.parse_text_element(&tag, State::Rawtext);
                    None
                }
                "noscript" => {
                    self.insert_html_element(&tag);
                    self.mode = Mode::InHeadNoscript;
                    None
                }
                "script" => {
                    // Scripts are never run, so the script element's
                    // parser-specific flags are not tracked.
                    self.insert_html_element(&tag);
                    self.tokenizer_state = Some(State::ScriptData);
                    self.original_mode = self.mode;
                    self.mode = Mode::Text;
                    None
                }
                "template" => {
                    // Declarative shadow roots are not supported yet (the
                    // parser's "allow declarative shadow roots" is false), so
                    // a template is always an ordinary template element.
                    self.push_marker();
                    self.frameset_ok = false;
                    self.mode = Mode::InTemplate;
                    self.template_modes.push(Mode::InTemplate);
                    self.insert_html_element(&tag);
                    None
                }
                "head" => {
                    self.error("unexpected-start-tag");
                    None
                }
                _ => self.in_head_anything_else(Token::StartTag(tag)),
            },
            Token::EndTag(tag) => match tag.name.as_str() {
                "head" => {
                    self.pop();
                    self.mode = Mode::AfterHead;
                    None
                }
                "body" | "html" | "br" => self.in_head_anything_else(Token::EndTag(tag)),
                "template" => {
                    self.end_template();
                    None
                }
                _ => {
                    self.error("unexpected-end-tag");
                    None
                }
            },
            token => self.in_head_anything_else(token),
        }
    }

    fn in_head_anything_else(&mut self, token: Token) -> Option<Token> {
        self.pop();
        self.mode = Mode::AfterHead;
        Some(token)
    }

    fn end_template(&mut self) {
        if !self.has_open_html("template") {
            self.error("unexpected-end-tag");
            return;
        }
        self.generate_all_implied_end_tags_thoroughly();
        if !self.current_is_html("template") {
            self.error("unexpected-end-tag");
        }
        self.pop_until("template");
        self.clear_formatting_to_last_marker();
        self.template_modes.pop();
        self.reset_insertion_mode();
    }

    fn check_meta_charset(&mut self, tag: &Tag) {
        if !self.watch_meta_charset || self.meta_charset.is_some() {
            return;
        }
        if let Some(charset) = tag.attribute("charset") {
            if crate::encoding::is_known_label(charset) {
                self.meta_charset = Some(charset.to_string());
            }
        } else if tag
            .attribute("http-equiv")
            .is_some_and(|v| v.eq_ignore_ascii_case("content-type"))
            && let Some(label) = tag
                .attribute("content")
                .and_then(crate::encoding::extract_from_meta_content)
        {
            self.meta_charset = Some(label);
        }
    }

    /// §13.2.6.4.5
    fn in_head_noscript(&mut self, token: Token) -> Option<Token> {
        match token {
            Token::Doctype(_) => {
                self.error("unexpected-doctype");
                None
            }
            Token::StartTag(ref tag) if tag.name == "html" => self.in_body(token),
            Token::EndTag(ref tag) if tag.name == "noscript" => {
                self.pop();
                self.mode = Mode::InHead;
                None
            }
            Token::Character(c) if is_ascii_whitespace(c) => self.in_head(token),
            Token::Comment(_) | Token::ProcessingInstruction { .. } => self.in_head(token),
            Token::StartTag(ref tag)
                if matches!(
                    tag.name.as_str(),
                    "basefont" | "bgsound" | "link" | "meta" | "noframes" | "style"
                ) =>
            {
                self.in_head(token)
            }
            Token::StartTag(ref tag) if matches!(tag.name.as_str(), "head" | "noscript") => {
                self.error("unexpected-start-tag");
                None
            }
            Token::EndTag(ref tag) if tag.name != "br" => {
                self.error("unexpected-end-tag");
                None
            }
            token => {
                self.error("unexpected-token-in-noscript");
                self.pop();
                self.mode = Mode::InHead;
                Some(token)
            }
        }
    }

    /// §13.2.6.4.6
    fn after_head(&mut self, token: Token) -> Option<Token> {
        match token {
            Token::Character(c) if is_ascii_whitespace(c) => {
                self.insert_char(c);
                None
            }
            Token::Comment(data) => {
                self.insert_comment(data, None);
                None
            }
            Token::ProcessingInstruction { target, data } => {
                self.insert_processing_instruction(target, data, None);
                None
            }
            Token::Doctype(_) => {
                self.error("unexpected-doctype");
                None
            }
            Token::StartTag(tag) => match tag.name.as_str() {
                "html" => self.in_body(Token::StartTag(tag)),
                "body" => {
                    self.insert_html_element(&tag);
                    self.frameset_ok = false;
                    self.mode = Mode::InBody;
                    None
                }
                "frameset" => {
                    self.insert_html_element(&tag);
                    self.mode = Mode::InFrameset;
                    None
                }
                "base" | "basefont" | "bgsound" | "link" | "meta" | "noframes" | "script"
                | "style" | "template" | "title" => {
                    self.error("unexpected-start-tag");
                    let Some(head) = self.head else {
                        return self.in_head(Token::StartTag(tag));
                    };
                    self.open.push(head);
                    let result = self.in_head(Token::StartTag(tag));
                    self.remove_from_stack(head);
                    result
                }
                "head" => {
                    self.error("unexpected-start-tag");
                    None
                }
                _ => self.after_head_anything_else(Token::StartTag(tag)),
            },
            Token::EndTag(tag) => match tag.name.as_str() {
                "template" => self.in_head(Token::EndTag(tag)),
                "body" | "html" | "br" => self.after_head_anything_else(Token::EndTag(tag)),
                _ => {
                    self.error("unexpected-end-tag");
                    None
                }
            },
            token => self.after_head_anything_else(token),
        }
    }

    fn after_head_anything_else(&mut self, token: Token) -> Option<Token> {
        self.insert_html_element_named("body");
        self.frameset_ok = true;
        self.mode = Mode::InBody;
        Some(token)
    }

    /// Whether some open element is not one that may be left open at the
    /// end of the body.
    fn check_body_end_open_elements(&mut self) {
        let bad = self.open.iter().any(|&n| {
            !self.is_html_one_of(
                n,
                &[
                    "dd", "dt", "li", "optgroup", "option", "p", "rb", "rp", "rt", "rtc", "tbody",
                    "td", "tfoot", "th", "thead", "tr", "body", "html",
                ],
            )
        });
        if bad {
            self.error("expected-closing-tag-but-got-eof");
        }
    }

    /// §13.2.6.4.7
    #[allow(clippy::too_many_lines)]
    fn in_body(&mut self, token: Token) -> Option<Token> {
        match token {
            Token::Character('\0') => {
                self.error("unexpected-null-character");
                None
            }
            Token::Character(c) => {
                self.reconstruct_formatting();
                self.insert_char(c);
                if !is_ascii_whitespace(c) {
                    self.frameset_ok = false;
                }
                None
            }
            Token::Comment(data) => {
                self.insert_comment(data, None);
                None
            }
            Token::ProcessingInstruction { target, data } => {
                self.insert_processing_instruction(target, data, None);
                None
            }
            Token::Doctype(_) => {
                self.error("unexpected-doctype");
                None
            }
            Token::StartTag(tag) => self.in_body_start_tag(tag),
            Token::EndTag(tag) => self.in_body_end_tag(tag),
            Token::Eof => {
                if !self.template_modes.is_empty() {
                    return self.in_template(Token::Eof);
                }
                self.check_body_end_open_elements();
                self.stopped = true;
                None
            }
        }
    }

    fn add_missing_attributes(&mut self, node: NodeId, tag: &Tag) {
        if let Some(element) = self.document.element_mut(node) {
            for attribute in &tag.attributes {
                if !element
                    .attributes
                    .iter()
                    .any(|a| a.namespace.is_none() && a.local_name == attribute.name)
                {
                    element.attributes.push(DomAttribute::new(
                        attribute.name.clone(),
                        attribute.value.clone(),
                    ));
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn in_body_start_tag(&mut self, mut tag: Tag) -> Option<Token> {
        match tag.name.as_str() {
            "html" => {
                self.error("unexpected-start-tag");
                if !self.has_open_html("template")
                    && let Some(&html) = self.open.first()
                {
                    self.add_missing_attributes(html, &tag);
                }
                None
            }
            "base" | "basefont" | "bgsound" | "link" | "meta" | "noframes" | "script" | "style"
            | "template" | "title" => self.in_head(Token::StartTag(tag)),
            "body" => {
                self.error("unexpected-start-tag");
                let second = self.open.get(1).copied();
                match second {
                    Some(body) if self.is_html(body, "body") && !self.has_open_html("template") => {
                        self.frameset_ok = false;
                        self.add_missing_attributes(body, &tag);
                    }
                    _ => {}
                }
                None
            }
            "frameset" => {
                self.error("unexpected-start-tag");
                let second = self.open.get(1).copied();
                let body = second.filter(|&b| self.is_html(b, "body"))?;
                if !self.frameset_ok {
                    return None;
                }
                self.document.detach(body);
                self.open.truncate(1);
                self.insert_html_element(&tag);
                self.mode = Mode::InFrameset;
                None
            }
            "address" | "article" | "aside" | "blockquote" | "center" | "details" | "dialog"
            | "dir" | "div" | "dl" | "fieldset" | "figcaption" | "figure" | "footer" | "header"
            | "hgroup" | "main" | "menu" | "nav" | "ol" | "p" | "search" | "section"
            | "summary" | "ul" => {
                self.close_p_if_in_button_scope();
                self.insert_html_element(&tag);
                None
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.close_p_if_in_button_scope();
                if self.current_is_html_one_of(&["h1", "h2", "h3", "h4", "h5", "h6"]) {
                    self.error("unexpected-start-tag");
                    self.pop();
                }
                self.insert_html_element(&tag);
                None
            }
            "pre" | "listing" => {
                self.close_p_if_in_button_scope();
                self.insert_html_element(&tag);
                self.ignore_next_lf = true;
                self.frameset_ok = false;
                None
            }
            "form" => {
                let in_template = self.parsing_template_contents();
                if self.form.is_some() && !in_template {
                    self.error("unexpected-start-tag");
                    return None;
                }
                self.close_p_if_in_button_scope();
                let form = self.insert_html_element(&tag);
                if !in_template {
                    self.form = Some(form);
                }
                None
            }
            "li" => {
                self.list_item_start(&["li"]);
                self.insert_html_element(&tag);
                None
            }
            "dd" | "dt" => {
                self.list_item_start(&["dd", "dt"]);
                self.insert_html_element(&tag);
                None
            }
            "plaintext" => {
                self.close_p_if_in_button_scope();
                self.insert_html_element(&tag);
                self.tokenizer_state = Some(State::Plaintext);
                None
            }
            "button" => {
                if self.in_scope("button", Scope::Default) {
                    self.error("unexpected-start-tag");
                    self.generate_implied_end_tags(None);
                    self.pop_until("button");
                }
                self.reconstruct_formatting();
                self.insert_html_element(&tag);
                self.frameset_ok = false;
                None
            }
            "a" => {
                let existing = self
                    .active
                    .iter()
                    .rev()
                    .take_while(|e| !matches!(e, Formatting::Marker))
                    .find_map(|e| match e {
                        Formatting::Element(node, t) if t.name == "a" => Some(*node),
                        _ => None,
                    });
                if let Some(existing) = existing {
                    self.error("unexpected-start-tag");
                    self.adoption_agency("a");
                    if let Some(index) = self.formatting_index(existing) {
                        self.active.remove(index);
                    }
                    self.remove_from_stack(existing);
                }
                self.reconstruct_formatting();
                let element = self.insert_html_element(&tag);
                self.push_formatting(element, &tag);
                None
            }
            "b" | "big" | "code" | "em" | "font" | "i" | "s" | "small" | "strike" | "strong"
            | "tt" | "u" => {
                self.reconstruct_formatting();
                let element = self.insert_html_element(&tag);
                self.push_formatting(element, &tag);
                None
            }
            "nobr" => {
                self.reconstruct_formatting();
                if self.in_scope("nobr", Scope::Default) {
                    self.error("unexpected-start-tag");
                    self.adoption_agency("nobr");
                    self.reconstruct_formatting();
                }
                let element = self.insert_html_element(&tag);
                self.push_formatting(element, &tag);
                None
            }
            "applet" | "marquee" | "object" => {
                self.reconstruct_formatting();
                self.insert_html_element(&tag);
                self.push_marker();
                self.frameset_ok = false;
                None
            }
            "table" => {
                if self.document.quirks_mode() != QuirksMode::Quirks {
                    self.close_p_if_in_button_scope();
                }
                self.insert_html_element(&tag);
                self.frameset_ok = false;
                self.mode = Mode::InTable;
                None
            }
            "area" | "br" | "embed" | "img" | "keygen" | "wbr" => {
                self.reconstruct_formatting();
                self.insert_html_element(&tag);
                self.pop();
                self.acknowledge_self_closing();
                self.frameset_ok = false;
                None
            }
            "input" => {
                if self.context.is_some_and(|c| self.is_html(c, "select")) {
                    self.error("unexpected-start-tag");
                    return None;
                }
                if self.in_scope("select", Scope::Default) {
                    self.error("unexpected-start-tag");
                    self.pop_until("select");
                }
                self.reconstruct_formatting();
                self.insert_html_element(&tag);
                self.pop();
                self.acknowledge_self_closing();
                if !tag
                    .attribute("type")
                    .is_some_and(|t| t.eq_ignore_ascii_case("hidden"))
                {
                    self.frameset_ok = false;
                }
                None
            }
            "param" | "source" | "track" => {
                self.insert_html_element(&tag);
                self.pop();
                self.acknowledge_self_closing();
                None
            }
            "hr" => {
                self.close_p_if_in_button_scope();
                if self.in_scope("select", Scope::Default) {
                    self.generate_implied_end_tags(None);
                    if self.in_scope("option", Scope::Default)
                        || self.in_scope("optgroup", Scope::Default)
                    {
                        self.error("unexpected-start-tag");
                    }
                }
                self.insert_html_element(&tag);
                self.pop();
                self.acknowledge_self_closing();
                self.frameset_ok = false;
                None
            }
            "image" => {
                self.error("unexpected-start-tag");
                tag.name = "img".to_string();
                Some(Token::StartTag(tag))
            }
            "textarea" => {
                self.insert_html_element(&tag);
                self.ignore_next_lf = true;
                self.tokenizer_state = Some(State::Rcdata);
                self.original_mode = self.mode;
                self.frameset_ok = false;
                self.mode = Mode::Text;
                None
            }
            "xmp" => {
                self.close_p_if_in_button_scope();
                self.reconstruct_formatting();
                self.frameset_ok = false;
                self.parse_text_element(&tag, State::Rawtext);
                None
            }
            "iframe" => {
                self.frameset_ok = false;
                self.parse_text_element(&tag, State::Rawtext);
                None
            }
            "noembed" => {
                self.parse_text_element(&tag, State::Rawtext);
                None
            }
            "noscript" if self.scripting => {
                self.parse_text_element(&tag, State::Rawtext);
                None
            }
            "select" => {
                if self.context.is_some_and(|c| self.is_html(c, "select")) {
                    self.error("unexpected-start-tag");
                } else if self.in_scope("select", Scope::Default) {
                    self.error("unexpected-start-tag");
                    self.pop_until("select");
                } else {
                    self.reconstruct_formatting();
                    self.insert_html_element(&tag);
                    self.frameset_ok = false;
                }
                None
            }
            "option" => {
                if self.in_scope("select", Scope::Default) {
                    self.generate_implied_end_tags(Some("optgroup"));
                    if self.in_scope("option", Scope::Default) {
                        self.error("unexpected-start-tag");
                    }
                } else if self.current_is_html("option") {
                    self.pop();
                }
                self.reconstruct_formatting();
                self.insert_html_element(&tag);
                None
            }
            "optgroup" => {
                if self.in_scope("select", Scope::Default) {
                    self.generate_implied_end_tags(None);
                    if self.in_scope("option", Scope::Default)
                        || self.in_scope("optgroup", Scope::Default)
                    {
                        self.error("unexpected-start-tag");
                    }
                } else if self.current_is_html("option") {
                    self.pop();
                }
                self.reconstruct_formatting();
                self.insert_html_element(&tag);
                None
            }
            "rb" | "rtc" => {
                if self.in_scope("ruby", Scope::Default) {
                    self.generate_implied_end_tags(None);
                    if !self.current_is_html("ruby") {
                        self.error("unexpected-start-tag");
                    }
                }
                self.insert_html_element(&tag);
                None
            }
            "rp" | "rt" => {
                if self.in_scope("ruby", Scope::Default) {
                    self.generate_implied_end_tags(Some("rtc"));
                    if !self.current_is_html_one_of(&["rtc", "ruby"]) {
                        self.error("unexpected-start-tag");
                    }
                }
                self.insert_html_element(&tag);
                None
            }
            "math" => {
                self.reconstruct_formatting();
                self.insert_foreign_element(&tag, Namespace::MathMl);
                if tag.self_closing {
                    self.pop();
                    self.acknowledge_self_closing();
                }
                None
            }
            "svg" => {
                self.reconstruct_formatting();
                self.insert_foreign_element(&tag, Namespace::Svg);
                if tag.self_closing {
                    self.pop();
                    self.acknowledge_self_closing();
                }
                None
            }
            "caption" | "col" | "colgroup" | "frame" | "head" | "tbody" | "td" | "tfoot" | "th"
            | "thead" | "tr" => {
                self.error("unexpected-start-tag");
                None
            }
            _ => {
                self.reconstruct_formatting();
                self.insert_html_element(&tag);
                None
            }
        }
    }

    /// The shared steps for `li`, `dd` and `dt` start tags.
    fn list_item_start(&mut self, names: &[&str]) {
        self.frameset_ok = false;
        let mut index = self.open.len();
        while index > 0 {
            index -= 1;
            let Some(node) = self.open.get(index).copied() else {
                break;
            };
            let matched = self
                .document
                .element(node)
                .filter(|e| {
                    e.namespace == Namespace::Html && names.contains(&e.local_name.as_str())
                })
                .map(|e| e.local_name.clone());
            if let Some(name) = matched {
                self.generate_implied_end_tags(Some(&name));
                if !self.current_is_html(&name) {
                    self.error("unexpected-start-tag");
                }
                self.pop_until(&name);
                break;
            }
            if self.is_special(node) && !self.is_html_one_of(node, &["address", "div", "p"]) {
                break;
            }
        }
        self.close_p_if_in_button_scope();
    }

    #[allow(clippy::too_many_lines)]
    fn in_body_end_tag(&mut self, tag: Tag) -> Option<Token> {
        match tag.name.as_str() {
            "template" => self.in_head(Token::EndTag(tag)),
            "body" | "html" => {
                if !self.in_scope("body", Scope::Default) {
                    self.error("unexpected-end-tag");
                    return None;
                }
                self.check_body_end_open_elements();
                self.mode = Mode::AfterBody;
                (tag.name == "html").then_some(Token::EndTag(tag))
            }
            "address" | "article" | "aside" | "blockquote" | "button" | "center" | "details"
            | "dialog" | "dir" | "div" | "dl" | "fieldset" | "figcaption" | "figure" | "footer"
            | "header" | "hgroup" | "listing" | "main" | "menu" | "nav" | "ol" | "pre"
            | "search" | "section" | "select" | "summary" | "ul" => {
                if !self.in_scope(&tag.name, Scope::Default) {
                    self.error("unexpected-end-tag");
                    return None;
                }
                self.generate_implied_end_tags(None);
                if !self.current_is_html(&tag.name) {
                    self.error("unexpected-end-tag");
                }
                self.pop_until(&tag.name);
                None
            }
            "form" => {
                if self.parsing_template_contents() {
                    if !self.in_scope("form", Scope::Default) {
                        self.error("unexpected-end-tag");
                        return None;
                    }
                    self.generate_implied_end_tags(None);
                    if !self.current_is_html("form") {
                        self.error("unexpected-end-tag");
                    }
                    self.pop_until("form");
                } else {
                    let node = self.form.take();
                    let Some(node) = node.filter(|&n| self.node_in_scope(n)) else {
                        self.error("unexpected-end-tag");
                        return None;
                    };
                    self.generate_implied_end_tags(None);
                    if self.current_node() != Some(node) {
                        self.error("unexpected-end-tag");
                    }
                    self.remove_from_stack(node);
                }
                None
            }
            "p" => {
                if !self.in_scope("p", Scope::Button) {
                    self.error("unexpected-end-tag");
                    self.insert_html_element_named("p");
                }
                self.close_p_element();
                None
            }
            "li" => {
                if !self.in_scope("li", Scope::ListItem) {
                    self.error("unexpected-end-tag");
                    return None;
                }
                self.generate_implied_end_tags(Some("li"));
                if !self.current_is_html("li") {
                    self.error("unexpected-end-tag");
                }
                self.pop_until("li");
                None
            }
            "dd" | "dt" => {
                if !self.in_scope(&tag.name, Scope::Default) {
                    self.error("unexpected-end-tag");
                    return None;
                }
                self.generate_implied_end_tags(Some(&tag.name));
                if !self.current_is_html(&tag.name) {
                    self.error("unexpected-end-tag");
                }
                self.pop_until(&tag.name);
                None
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                const HEADINGS: [&str; 6] = ["h1", "h2", "h3", "h4", "h5", "h6"];
                if !self.in_scope_any(&HEADINGS, Scope::Default) {
                    self.error("unexpected-end-tag");
                    return None;
                }
                self.generate_implied_end_tags(None);
                if !self.current_is_html(&tag.name) {
                    self.error("unexpected-end-tag");
                }
                self.pop_until_any(&HEADINGS);
                None
            }
            "a" | "b" | "big" | "code" | "em" | "font" | "i" | "nobr" | "s" | "small"
            | "strike" | "strong" | "tt" | "u" => {
                self.adoption_agency(&tag.name);
                None
            }
            "applet" | "marquee" | "object" => {
                if !self.in_scope(&tag.name, Scope::Default) {
                    self.error("unexpected-end-tag");
                    return None;
                }
                self.generate_implied_end_tags(None);
                if !self.current_is_html(&tag.name) {
                    self.error("unexpected-end-tag");
                }
                self.pop_until(&tag.name);
                self.clear_formatting_to_last_marker();
                None
            }
            "br" => {
                self.error("unexpected-end-tag");
                self.in_body_start_tag(Tag {
                    name: "br".to_string(),
                    ..Tag::default()
                })
            }
            _ => {
                self.any_other_end_tag(&tag.name);
                None
            }
        }
    }

    /// §13.2.6.4.8
    fn text(&mut self, token: Token) -> Option<Token> {
        match token {
            Token::Character(c) => {
                self.insert_char(c);
                None
            }
            Token::Eof => {
                self.error("eof-in-text");
                self.pop();
                self.mode = self.original_mode;
                Some(Token::Eof)
            }
            _ => {
                // Script end tags would prepare the script here; nothing is
                // executed in M1.
                self.pop();
                self.mode = self.original_mode;
                None
            }
        }
    }

    /// §13.2.6.4.9
    fn in_table(&mut self, token: Token) -> Option<Token> {
        match token {
            Token::Character(_)
                if self.current_is_html_one_of(&[
                    "table", "tbody", "template", "tfoot", "thead", "tr",
                ]) =>
            {
                self.pending_table_chars.clear();
                self.original_mode = self.mode;
                self.mode = Mode::InTableText;
                Some(token)
            }
            Token::Comment(data) => {
                self.insert_comment(data, None);
                None
            }
            Token::ProcessingInstruction { target, data } => {
                self.insert_processing_instruction(target, data, None);
                None
            }
            Token::Doctype(_) => {
                self.error("unexpected-doctype");
                None
            }
            Token::StartTag(tag) => match tag.name.as_str() {
                "caption" => {
                    self.clear_stack_back_to(&["table", "template", "html"]);
                    self.push_marker();
                    self.insert_html_element(&tag);
                    self.mode = Mode::InCaption;
                    None
                }
                "colgroup" => {
                    self.clear_stack_back_to(&["table", "template", "html"]);
                    self.insert_html_element(&tag);
                    self.mode = Mode::InColumnGroup;
                    None
                }
                "col" => {
                    self.clear_stack_back_to(&["table", "template", "html"]);
                    self.insert_html_element_named("colgroup");
                    self.mode = Mode::InColumnGroup;
                    Some(Token::StartTag(tag))
                }
                "tbody" | "tfoot" | "thead" => {
                    self.clear_stack_back_to(&["table", "template", "html"]);
                    self.insert_html_element(&tag);
                    self.mode = Mode::InTableBody;
                    None
                }
                "td" | "th" | "tr" => {
                    self.clear_stack_back_to(&["table", "template", "html"]);
                    self.insert_html_element_named("tbody");
                    self.mode = Mode::InTableBody;
                    Some(Token::StartTag(tag))
                }
                "table" => {
                    self.error("unexpected-start-tag");
                    if !self.in_scope("table", Scope::Table) {
                        return None;
                    }
                    self.pop_until("table");
                    self.reset_insertion_mode();
                    Some(Token::StartTag(tag))
                }
                "style" | "script" | "template" => self.in_head(Token::StartTag(tag)),
                "input"
                    if tag
                        .attribute("type")
                        .is_some_and(|t| t.eq_ignore_ascii_case("hidden")) =>
                {
                    self.error("unexpected-start-tag");
                    self.insert_html_element(&tag);
                    self.pop();
                    self.acknowledge_self_closing();
                    None
                }
                "form" => {
                    self.error("unexpected-start-tag");
                    let in_template = self.parsing_template_contents();
                    if self.form.is_some() && !in_template {
                        return None;
                    }
                    let form = self.insert_html_element(&tag);
                    if !in_template {
                        self.form = Some(form);
                    }
                    self.pop();
                    None
                }
                _ => self.in_table_anything_else(Token::StartTag(tag)),
            },
            Token::EndTag(tag) => match tag.name.as_str() {
                "table" => {
                    if !self.in_scope("table", Scope::Table) {
                        self.error("unexpected-end-tag");
                        return None;
                    }
                    self.pop_until("table");
                    self.reset_insertion_mode();
                    None
                }
                "body" | "caption" | "col" | "colgroup" | "html" | "tbody" | "td" | "tfoot"
                | "th" | "thead" | "tr" => {
                    self.error("unexpected-end-tag");
                    None
                }
                "template" => self.in_head(Token::EndTag(tag)),
                _ => self.in_table_anything_else(Token::EndTag(tag)),
            },
            Token::Eof => self.in_body(Token::Eof),
            token => self.in_table_anything_else(token),
        }
    }

    fn in_table_anything_else(&mut self, token: Token) -> Option<Token> {
        self.error("unexpected-token-in-table");
        self.foster_parenting = true;
        let result = self.in_body(token);
        self.foster_parenting = false;
        result
    }

    fn clear_stack_back_to(&mut self, names: &[&str]) {
        while let Some(node) = self.current_node() {
            if self.is_html_one_of(node, names) {
                break;
            }
            self.pop();
        }
    }

    /// §13.2.6.4.10
    fn in_table_text(&mut self, token: Token) -> Option<Token> {
        match token {
            Token::Character('\0') => {
                self.error("unexpected-null-character");
                None
            }
            Token::Character(c) => {
                self.pending_table_chars.push(c);
                None
            }
            token => {
                let pending = std::mem::take(&mut self.pending_table_chars);
                if pending.iter().any(|&c| !is_ascii_whitespace(c)) {
                    for c in pending {
                        // Character tokens never need reprocessing here.
                        let _ = self.in_table_anything_else(Token::Character(c));
                    }
                } else {
                    let text: String = pending.into_iter().collect();
                    self.insert_text(&text);
                }
                self.mode = self.original_mode;
                Some(token)
            }
        }
    }

    /// §13.2.6.4.11
    fn in_caption(&mut self, token: Token) -> Option<Token> {
        let closes_caption = is_end(&token, &["caption", "table"])
            || is_start(
                &token,
                &[
                    "caption", "col", "colgroup", "tbody", "td", "tfoot", "th", "thead", "tr",
                ],
            );
        if closes_caption {
            if !self.in_scope("caption", Scope::Table) {
                self.error("unexpected-token-in-caption");
                return None;
            }
            self.generate_implied_end_tags(None);
            if !self.current_is_html("caption") {
                self.error("unexpected-token-in-caption");
            }
            self.pop_until("caption");
            self.clear_formatting_to_last_marker();
            self.mode = Mode::InTable;
            return if is_end(&token, &["caption"]) {
                None
            } else {
                Some(token)
            };
        }
        if is_end(
            &token,
            &[
                "body", "col", "colgroup", "html", "tbody", "td", "tfoot", "th", "thead", "tr",
            ],
        ) {
            self.error("unexpected-end-tag");
            return None;
        }
        self.in_body(token)
    }

    /// §13.2.6.4.12
    fn in_column_group(&mut self, token: Token) -> Option<Token> {
        match token {
            Token::Character(c) if is_ascii_whitespace(c) => {
                self.insert_char(c);
                None
            }
            Token::Comment(data) => {
                self.insert_comment(data, None);
                None
            }
            Token::ProcessingInstruction { target, data } => {
                self.insert_processing_instruction(target, data, None);
                None
            }
            Token::Doctype(_) => {
                self.error("unexpected-doctype");
                None
            }
            Token::StartTag(ref tag) if tag.name == "html" => self.in_body(token),
            Token::StartTag(ref tag) if tag.name == "col" => {
                if let Token::StartTag(tag) = &token {
                    self.insert_html_element(tag);
                }
                self.pop();
                self.acknowledge_self_closing();
                None
            }
            Token::EndTag(ref tag) if tag.name == "colgroup" => {
                if !self.current_is_html("colgroup") {
                    self.error("unexpected-end-tag");
                    return None;
                }
                self.pop();
                self.mode = Mode::InTable;
                None
            }
            Token::EndTag(ref tag) if tag.name == "col" => {
                self.error("unexpected-end-tag");
                None
            }
            Token::StartTag(ref tag) | Token::EndTag(ref tag) if tag.name == "template" => {
                self.in_head(token)
            }
            Token::Eof => self.in_body(token),
            token => {
                if !self.current_is_html("colgroup") {
                    self.error("unexpected-token-in-column-group");
                    return None;
                }
                self.pop();
                self.mode = Mode::InTable;
                Some(token)
            }
        }
    }

    /// §13.2.6.4.13
    fn in_table_body(&mut self, token: Token) -> Option<Token> {
        const CONTEXT: [&str; 5] = ["tbody", "tfoot", "thead", "template", "html"];
        match token {
            Token::StartTag(tag) if tag.name == "tr" => {
                self.clear_stack_back_to(&CONTEXT);
                self.insert_html_element(&tag);
                self.mode = Mode::InRow;
                None
            }
            Token::StartTag(tag) if matches!(tag.name.as_str(), "th" | "td") => {
                self.error("unexpected-start-tag");
                self.clear_stack_back_to(&CONTEXT);
                self.insert_html_element_named("tr");
                self.mode = Mode::InRow;
                Some(Token::StartTag(tag))
            }
            Token::EndTag(tag) if matches!(tag.name.as_str(), "tbody" | "tfoot" | "thead") => {
                if !self.in_scope(&tag.name, Scope::Table) {
                    self.error("unexpected-end-tag");
                    return None;
                }
                self.clear_stack_back_to(&CONTEXT);
                self.pop();
                self.mode = Mode::InTable;
                None
            }
            token
                if is_start(
                    &token,
                    &["caption", "col", "colgroup", "tbody", "tfoot", "thead"],
                ) || is_end(&token, &["table"]) =>
            {
                if !self.in_scope_any(&["tbody", "thead", "tfoot"], Scope::Table) {
                    self.error("unexpected-token-in-table-body");
                    return None;
                }
                self.clear_stack_back_to(&CONTEXT);
                self.pop();
                self.mode = Mode::InTable;
                Some(token)
            }
            token
                if is_end(
                    &token,
                    &[
                        "body", "caption", "col", "colgroup", "html", "td", "th", "tr",
                    ],
                ) =>
            {
                self.error("unexpected-end-tag");
                None
            }
            token => self.in_table(token),
        }
    }

    /// §13.2.6.4.14
    fn in_row(&mut self, token: Token) -> Option<Token> {
        const CONTEXT: [&str; 3] = ["tr", "template", "html"];
        match token {
            Token::StartTag(tag) if matches!(tag.name.as_str(), "th" | "td") => {
                self.clear_stack_back_to(&CONTEXT);
                self.insert_html_element(&tag);
                self.mode = Mode::InCell;
                self.push_marker();
                None
            }
            Token::EndTag(ref tag) if tag.name == "tr" => {
                if !self.in_scope("tr", Scope::Table) {
                    self.error("unexpected-end-tag");
                    return None;
                }
                self.clear_stack_back_to(&CONTEXT);
                self.pop();
                self.mode = Mode::InTableBody;
                None
            }
            token
                if is_start(
                    &token,
                    &[
                        "caption", "col", "colgroup", "tbody", "tfoot", "thead", "tr",
                    ],
                ) || is_end(&token, &["table"]) =>
            {
                if !self.in_scope("tr", Scope::Table) {
                    self.error("unexpected-token-in-row");
                    return None;
                }
                self.clear_stack_back_to(&CONTEXT);
                self.pop();
                self.mode = Mode::InTableBody;
                Some(token)
            }
            Token::EndTag(ref tag) if matches!(tag.name.as_str(), "tbody" | "tfoot" | "thead") => {
                if !self.in_scope(&tag.name, Scope::Table) {
                    self.error("unexpected-end-tag");
                    return None;
                }
                if !self.in_scope("tr", Scope::Table) {
                    return None;
                }
                self.clear_stack_back_to(&CONTEXT);
                self.pop();
                self.mode = Mode::InTableBody;
                Some(token)
            }
            token
                if is_end(
                    &token,
                    &["body", "caption", "col", "colgroup", "html", "td", "th"],
                ) =>
            {
                self.error("unexpected-end-tag");
                None
            }
            token => self.in_table(token),
        }
    }

    /// §13.2.6.4.15
    fn in_cell(&mut self, token: Token) -> Option<Token> {
        match token {
            Token::EndTag(tag) if matches!(tag.name.as_str(), "td" | "th") => {
                if !self.in_scope(&tag.name, Scope::Table) {
                    self.error("unexpected-end-tag");
                    return None;
                }
                self.generate_implied_end_tags(None);
                if !self.current_is_html(&tag.name) {
                    self.error("unexpected-end-tag");
                }
                self.pop_until(&tag.name);
                self.clear_formatting_to_last_marker();
                self.mode = Mode::InRow;
                None
            }
            token
                if is_start(
                    &token,
                    &[
                        "caption", "col", "colgroup", "tbody", "td", "tfoot", "th", "thead", "tr",
                    ],
                ) =>
            {
                if !self.in_scope_any(&["td", "th"], Scope::Table) {
                    self.error("unexpected-start-tag");
                    return None;
                }
                self.close_cell();
                Some(token)
            }
            token if is_end(&token, &["body", "caption", "col", "colgroup", "html"]) => {
                self.error("unexpected-end-tag");
                None
            }
            token if is_end(&token, &["table", "tbody", "tfoot", "thead", "tr"]) => {
                let name = tag_of(&token).map(|t| t.name.clone()).unwrap_or_default();
                if !self.in_scope(&name, Scope::Table) {
                    self.error("unexpected-end-tag");
                    return None;
                }
                self.close_cell();
                Some(token)
            }
            token => self.in_body(token),
        }
    }

    fn close_cell(&mut self) {
        self.generate_implied_end_tags(None);
        if !self.current_is_html_one_of(&["td", "th"]) {
            self.error("unexpected-cell-end");
        }
        self.pop_until_any(&["td", "th"]);
        self.clear_formatting_to_last_marker();
        self.mode = Mode::InRow;
    }

    /// §13.2.6.4.16
    fn in_template(&mut self, token: Token) -> Option<Token> {
        match token {
            Token::Character(_)
            | Token::Comment(_)
            | Token::ProcessingInstruction { .. }
            | Token::Doctype(_) => self.in_body(token),
            token
                if is_start(
                    &token,
                    &[
                        "base", "basefont", "bgsound", "link", "meta", "noframes", "script",
                        "style", "template", "title",
                    ],
                ) || is_end(&token, &["template"]) =>
            {
                self.in_head(token)
            }
            Token::StartTag(ref tag) => {
                let mode = match tag.name.as_str() {
                    "caption" | "colgroup" | "tbody" | "tfoot" | "thead" => Mode::InTable,
                    "col" => Mode::InColumnGroup,
                    "tr" => Mode::InTableBody,
                    "td" | "th" => Mode::InRow,
                    _ => Mode::InBody,
                };
                self.template_modes.pop();
                self.template_modes.push(mode);
                self.mode = mode;
                Some(token)
            }
            Token::EndTag(_) => {
                self.error("unexpected-end-tag");
                None
            }
            Token::Eof => {
                if !self.has_open_html("template") {
                    self.stopped = true;
                    return None;
                }
                self.error("eof-in-template");
                self.pop_until("template");
                self.clear_formatting_to_last_marker();
                self.template_modes.pop();
                self.reset_insertion_mode();
                Some(Token::Eof)
            }
        }
    }

    /// §13.2.6.4.17
    fn after_body(&mut self, token: Token) -> Option<Token> {
        match token {
            Token::Character(c) if is_ascii_whitespace(c) => self.in_body(token),
            Token::Comment(data) => {
                let location = self.open.first().map(|&html| (html, None));
                self.insert_comment(data, location);
                None
            }
            Token::ProcessingInstruction { target, data } => {
                let location = self.open.first().map(|&html| (html, None));
                self.insert_processing_instruction(target, data, location);
                None
            }
            Token::Doctype(_) => {
                self.error("unexpected-doctype");
                None
            }
            Token::StartTag(ref tag) if tag.name == "html" => self.in_body(token),
            Token::EndTag(ref tag) if tag.name == "html" => {
                if self.context.is_some() {
                    self.error("unexpected-end-tag");
                } else {
                    self.mode = Mode::AfterAfterBody;
                }
                None
            }
            Token::Eof => {
                self.stopped = true;
                None
            }
            token => {
                self.error("unexpected-token-after-body");
                self.mode = Mode::InBody;
                Some(token)
            }
        }
    }

    /// §13.2.6.4.18
    fn in_frameset(&mut self, token: Token) -> Option<Token> {
        match token {
            Token::Character(c) if is_ascii_whitespace(c) => {
                self.insert_char(c);
                None
            }
            Token::Comment(data) => {
                self.insert_comment(data, None);
                None
            }
            Token::ProcessingInstruction { target, data } => {
                self.insert_processing_instruction(target, data, None);
                None
            }
            Token::Doctype(_) => {
                self.error("unexpected-doctype");
                None
            }
            Token::StartTag(tag) => match tag.name.as_str() {
                "html" => self.in_body(Token::StartTag(tag)),
                "frameset" => {
                    self.insert_html_element(&tag);
                    None
                }
                "frame" => {
                    self.insert_html_element(&tag);
                    self.pop();
                    self.acknowledge_self_closing();
                    None
                }
                "noframes" => self.in_head(Token::StartTag(tag)),
                _ => {
                    self.error("unexpected-start-tag");
                    None
                }
            },
            Token::EndTag(ref tag) if tag.name == "frameset" => {
                if self.open.len() <= 1 {
                    self.error("unexpected-end-tag");
                    return None;
                }
                self.pop();
                if self.context.is_none() && !self.current_is_html("frameset") {
                    self.mode = Mode::AfterFrameset;
                }
                None
            }
            Token::Eof => {
                if self.open.len() > 1 {
                    self.error("eof-in-frameset");
                }
                self.stopped = true;
                None
            }
            _ => {
                self.error("unexpected-token-in-frameset");
                None
            }
        }
    }

    /// §13.2.6.4.19
    fn after_frameset(&mut self, token: Token) -> Option<Token> {
        match token {
            Token::Character(c) if is_ascii_whitespace(c) => {
                self.insert_char(c);
                None
            }
            Token::Comment(data) => {
                self.insert_comment(data, None);
                None
            }
            Token::ProcessingInstruction { target, data } => {
                self.insert_processing_instruction(target, data, None);
                None
            }
            Token::Doctype(_) => {
                self.error("unexpected-doctype");
                None
            }
            Token::StartTag(ref tag) if tag.name == "html" => self.in_body(token),
            Token::EndTag(ref tag) if tag.name == "html" => {
                self.mode = Mode::AfterAfterFrameset;
                None
            }
            Token::StartTag(ref tag) if tag.name == "noframes" => self.in_head(token),
            Token::Eof => {
                self.stopped = true;
                None
            }
            _ => {
                self.error("unexpected-token-after-frameset");
                None
            }
        }
    }

    /// §13.2.6.4.20
    fn after_after_body(&mut self, token: Token) -> Option<Token> {
        match token {
            Token::Comment(data) => {
                let location = self.document_location();
                self.insert_comment(data, Some(location));
                None
            }
            Token::ProcessingInstruction { target, data } => {
                let location = self.document_location();
                self.insert_processing_instruction(target, data, Some(location));
                None
            }
            Token::Doctype(_) => self.in_body(token),
            Token::Character(c) if is_ascii_whitespace(c) => self.in_body(token),
            Token::StartTag(ref tag) if tag.name == "html" => self.in_body(token),
            Token::Eof => {
                self.stopped = true;
                None
            }
            token => {
                self.error("unexpected-token-after-after-body");
                self.mode = Mode::InBody;
                Some(token)
            }
        }
    }

    /// §13.2.6.4.21
    fn after_after_frameset(&mut self, token: Token) -> Option<Token> {
        match token {
            Token::Comment(data) => {
                let location = self.document_location();
                self.insert_comment(data, Some(location));
                None
            }
            Token::ProcessingInstruction { target, data } => {
                let location = self.document_location();
                self.insert_processing_instruction(target, data, Some(location));
                None
            }
            Token::Doctype(_) => self.in_body(token),
            Token::Character(c) if is_ascii_whitespace(c) => self.in_body(token),
            Token::StartTag(ref tag) if tag.name == "html" => self.in_body(token),
            Token::Eof => {
                self.stopped = true;
                None
            }
            Token::StartTag(ref tag) if tag.name == "noframes" => self.in_head(token),
            _ => {
                self.error("unexpected-token-after-after-frameset");
                None
            }
        }
    }

    /// §13.2.6.5 The rules for parsing tokens in foreign content.
    fn foreign_content(&mut self, token: Token) -> Option<Token> {
        match token {
            Token::Character('\0') => {
                self.error("unexpected-null-character");
                self.insert_char('\u{FFFD}');
                None
            }
            Token::Character(c) => {
                self.insert_char(c);
                if !is_ascii_whitespace(c) {
                    self.frameset_ok = false;
                }
                None
            }
            Token::Comment(data) => {
                self.insert_comment(data, None);
                None
            }
            Token::ProcessingInstruction { target, data } => {
                self.insert_processing_instruction(target, data, None);
                None
            }
            Token::Doctype(_) => {
                self.error("unexpected-doctype");
                None
            }
            Token::StartTag(ref tag) if breaks_out_of_foreign_content(tag) => {
                self.error("unexpected-html-element-in-foreign-content");
                self.pop_to_html_or_integration_point();
                self.process_in_mode(self.mode, token)
            }
            Token::EndTag(ref tag) if matches!(tag.name.as_str(), "br" | "p") => {
                self.error("unexpected-html-element-in-foreign-content");
                self.pop_to_html_or_integration_point();
                self.process_in_mode(self.mode, token)
            }
            Token::StartTag(mut tag) => {
                let namespace = self
                    .adjusted_current_node()
                    .and_then(|n| self.document.element(n))
                    .map_or(Namespace::Html, |e| e.namespace);
                if namespace == Namespace::Svg
                    && let Some((_, adjusted)) = SVG_TAG_ADJUSTMENTS
                        .iter()
                        .find(|(lower, _)| *lower == tag.name)
                {
                    tag.name = (*adjusted).to_string();
                }
                self.insert_foreign_element(&tag, namespace);
                if tag.self_closing {
                    // An SVG script would be processed here; scripts never
                    // run in M1, so both branches just pop.
                    self.pop();
                    self.acknowledge_self_closing();
                }
                None
            }
            Token::EndTag(tag) => {
                let svg_script = self.current_node().is_some_and(|n| {
                    self.document
                        .element(n)
                        .is_some_and(|e| e.namespace == Namespace::Svg && e.local_name == "script")
                });
                if tag.name == "script" && svg_script {
                    self.pop();
                    return None;
                }
                self.foreign_end_tag(tag)
            }
            Token::Eof => self.process_in_mode(self.mode, Token::Eof),
        }
    }

    fn pop_to_html_or_integration_point(&mut self) {
        while let Some(node) = self.current_node() {
            let html = self
                .document
                .element(node)
                .is_none_or(|e| e.namespace == Namespace::Html);
            if html
                || self.is_mathml_text_integration_point(node)
                || self.is_html_integration_point(node)
            {
                break;
            }
            self.pop();
        }
    }

    fn foreign_end_tag(&mut self, tag: Tag) -> Option<Token> {
        let lower_name = |builder: &Self, node: NodeId| {
            builder
                .document
                .element(node)
                .map(|e| e.local_name.to_ascii_lowercase())
        };
        let mut index = self.open.len().checked_sub(1)?;
        let first = self.open.get(index).copied()?;
        if lower_name(self, first).as_deref() != Some(tag.name.as_str()) {
            self.error("unexpected-end-tag");
        }
        loop {
            if index == 0 {
                return None;
            }
            let node = self.open.get(index).copied()?;
            if lower_name(self, node).as_deref() == Some(tag.name.as_str()) {
                self.open.truncate(index);
                return None;
            }
            index -= 1;
            let previous = self.open.get(index).copied()?;
            if self
                .document
                .element(previous)
                .is_some_and(|e| e.namespace == Namespace::Html)
            {
                return self.process_in_mode(self.mode, Token::EndTag(tag));
            }
        }
    }
}

fn breaks_out_of_foreign_content(tag: &Tag) -> bool {
    matches!(
        tag.name.as_str(),
        "b" | "big"
            | "blockquote"
            | "body"
            | "br"
            | "center"
            | "code"
            | "dd"
            | "div"
            | "dl"
            | "dt"
            | "em"
            | "embed"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "head"
            | "hr"
            | "i"
            | "img"
            | "li"
            | "listing"
            | "menu"
            | "meta"
            | "nobr"
            | "ol"
            | "p"
            | "pre"
            | "ruby"
            | "s"
            | "small"
            | "span"
            | "strong"
            | "strike"
            | "sub"
            | "sup"
            | "table"
            | "tt"
            | "u"
            | "ul"
            | "var"
    ) || (tag.name == "font"
        && tag
            .attributes
            .iter()
            .any(|a| matches!(a.name.as_str(), "color" | "face" | "size")))
}

/// Two attribute lists are the same if they pair up by name and value.
fn same_attributes(a: &[Attribute], b: &[Attribute]) -> bool {
    a.len() == b.len()
        && a.iter()
            .all(|x| b.iter().any(|y| y.name == x.name && y.value == x.value))
}

/// Adjust MathML/SVG attributes and foreign attributes (§13.2.6.1) and turn
/// the token's attributes into DOM attributes.
fn adjust_attributes(attributes: &[Attribute], namespace: Namespace) -> Vec<DomAttribute> {
    attributes
        .iter()
        .map(|a| {
            let mut name = a.name.as_str();
            if namespace == Namespace::MathMl && name == "definitionurl" {
                name = "definitionURL";
            }
            if namespace == Namespace::Svg
                && let Some((_, adjusted)) = SVG_ATTRIBUTE_ADJUSTMENTS
                    .iter()
                    .find(|(lower, _)| *lower == name)
            {
                name = adjusted;
            }
            let foreign = match name {
                "xlink:actuate" | "xlink:arcrole" | "xlink:href" | "xlink:role" | "xlink:show"
                | "xlink:title" | "xlink:type" => Some((
                    Some("xlink"),
                    name.get(6..).unwrap_or_default(),
                    Namespace::XLink,
                )),
                "xml:lang" | "xml:space" => Some((
                    Some("xml"),
                    name.get(4..).unwrap_or_default(),
                    Namespace::Xml,
                )),
                "xmlns" => Some((None, "xmlns", Namespace::Xmlns)),
                "xmlns:xlink" => Some((Some("xmlns"), "xlink", Namespace::Xmlns)),
                _ => None,
            };
            match foreign {
                Some((prefix, local, ns)) => DomAttribute {
                    namespace: Some(ns),
                    prefix: prefix.map(str::to_string),
                    local_name: local.to_string(),
                    value: a.value.clone(),
                },
                None => DomAttribute::new(name, a.value.clone()),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjusts_foreign_attributes() {
        let attributes = vec![
            Attribute {
                name: "xlink:href".into(),
                value: "#a".into(),
            },
            Attribute {
                name: "viewbox".into(),
                value: "0 0 1 1".into(),
            },
        ];
        let adjusted = adjust_attributes(&attributes, Namespace::Svg);
        assert_eq!(adjusted[0].namespace, Some(Namespace::XLink));
        assert_eq!(adjusted[0].local_name, "href");
        assert_eq!(adjusted[1].local_name, "viewBox");
    }
}
