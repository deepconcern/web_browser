//! The DOM tree.
//!
//! An arena-based implementation of the parts of the [DOM Standard] that the
//! rest of the engine needs in M1: a [`Document`] owns every node, and nodes
//! refer to each other by [`NodeId`] (an index into the arena), so there are
//! no reference-counted cycles.
//!
//! The tree holds documents, document fragments, doctypes, elements, text,
//! comments and processing instructions (DOM §4.4–§4.14). Elements carry a
//! [`Namespace`] (HTML, SVG or MathML) and a list of [`Attribute`]s. The small
//! query API covers navigation (parent, children, siblings), attributes,
//! lookup by id, class and tag name, and `textContent`.
//!
//! Nodes are never freed: removing a node detaches it from its parent, and it
//! stays in the arena until the [`Document`] is dropped. That keeps every
//! [`NodeId`] valid for the lifetime of its document.
//!
//! [DOM Standard]: https://dom.spec.whatwg.org/

use std::fmt;

/// Identifies a node inside one [`Document`].
///
/// A `NodeId` is only meaningful for the document that created it. Passing
/// it to another document is not undefined behavior: lookups simply return
/// `None` or refer to an unrelated node.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(u32);

impl NodeId {
    /// The position of this node in its document's arena.
    #[must_use]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeId({})", self.0)
    }
}

/// A namespace that elements and attributes can live in (Infra §8).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Namespace {
    /// `http://www.w3.org/1999/xhtml`
    Html,
    /// `http://www.w3.org/2000/svg`
    Svg,
    /// `http://www.w3.org/1998/Math/MathML`
    MathMl,
    /// `http://www.w3.org/1999/xlink` (attributes only)
    XLink,
    /// `http://www.w3.org/XML/1998/namespace` (attributes only)
    Xml,
    /// `http://www.w3.org/2000/xmlns/` (attributes only)
    Xmlns,
}

impl Namespace {
    /// The namespace URL.
    #[must_use]
    pub fn url(self) -> &'static str {
        match self {
            Namespace::Html => "http://www.w3.org/1999/xhtml",
            Namespace::Svg => "http://www.w3.org/2000/svg",
            Namespace::MathMl => "http://www.w3.org/1998/Math/MathML",
            Namespace::XLink => "http://www.w3.org/1999/xlink",
            Namespace::Xml => "http://www.w3.org/XML/1998/namespace",
            Namespace::Xmlns => "http://www.w3.org/2000/xmlns/",
        }
    }
}

/// A document's mode (DOM §4.5): set by the HTML parser from the doctype.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum QuirksMode {
    /// "no-quirks": standards mode.
    #[default]
    NoQuirks,
    /// "limited-quirks": almost-standards mode.
    LimitedQuirks,
    /// "quirks": quirks mode.
    Quirks,
}

/// An attribute on an element (DOM §4.9.2).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Attribute {
    /// The attribute's namespace; `None` for ordinary attributes.
    pub namespace: Option<Namespace>,
    /// The namespace prefix, e.g. `xlink` in `xlink:href`.
    pub prefix: Option<String>,
    /// The local name.
    pub local_name: String,
    /// The value.
    pub value: String,
}

impl Attribute {
    /// An attribute in no namespace.
    #[must_use]
    pub fn new(local_name: impl Into<String>, value: impl Into<String>) -> Attribute {
        Attribute {
            namespace: None,
            prefix: None,
            local_name: local_name.into(),
            value: value.into(),
        }
    }

    /// The qualified name: `prefix:local_name`, or the local name when there
    /// is no prefix.
    #[must_use]
    pub fn qualified_name(&self) -> String {
        match &self.prefix {
            Some(prefix) => format!("{prefix}:{}", self.local_name),
            None => self.local_name.clone(),
        }
    }

    fn has_qualified_name(&self, name: &str) -> bool {
        match &self.prefix {
            Some(prefix) => name
                .strip_prefix(prefix.as_str())
                .and_then(|rest| rest.strip_prefix(':'))
                .is_some_and(|local| local == self.local_name),
            None => self.local_name == name,
        }
    }
}

/// The element-specific part of an element node.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ElementData {
    /// The element's namespace.
    pub namespace: Namespace,
    /// The local name, e.g. `div` or `foreignObject`.
    pub local_name: String,
    /// Attributes in the order they were added.
    pub attributes: Vec<Attribute>,
    /// For an HTML `template` element, the document fragment holding its
    /// contents (HTML §4.12.3); `None` for every other element.
    pub template_contents: Option<NodeId>,
}

impl ElementData {
    /// Whether this is an HTML element with the given local name.
    #[must_use]
    pub fn is_html(&self, local_name: &str) -> bool {
        self.namespace == Namespace::Html && self.local_name == local_name
    }

    /// The value of the first attribute whose qualified name is `name`
    /// ("get an attribute by name", DOM §4.9).
    #[must_use]
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|attr| attr.has_qualified_name(name))
            .map(|attr| attr.value.as_str())
    }

    /// Whether an attribute with the qualified name `name` exists.
    #[must_use]
    pub fn has_attribute(&self, name: &str) -> bool {
        self.attribute(name).is_some()
    }

    /// Sets the value of the attribute in no namespace called `name`, adding
    /// it if it does not exist.
    pub fn set_attribute(&mut self, name: &str, value: impl Into<String>) {
        let value = value.into();
        match self
            .attributes
            .iter_mut()
            .find(|attr| attr.namespace.is_none() && attr.local_name == name)
        {
            Some(attr) => attr.value = value,
            None => self.attributes.push(Attribute::new(name, value)),
        }
    }

    /// The element's ID: the value of its `id` attribute, if any.
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        self.attribute("id")
    }

    /// The element's classes: its `class` attribute split on ASCII
    /// whitespace.
    pub fn classes(&self) -> impl Iterator<Item = &str> {
        self.attribute("class")
            .unwrap_or_default()
            .split(is_ascii_whitespace)
            .filter(|class| !class.is_empty())
    }
}

/// What a node is, with its type-specific data.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NodeData {
    /// The document node: the root of the tree.
    Document {
        /// The document's mode.
        quirks_mode: QuirksMode,
    },
    /// A document fragment, e.g. template contents or a parsed fragment.
    DocumentFragment,
    /// A doctype.
    Doctype {
        /// The doctype's name.
        name: String,
        /// The public identifier, or the empty string.
        public_id: String,
        /// The system identifier, or the empty string.
        system_id: String,
    },
    /// An element.
    Element(ElementData),
    /// A text node with its data.
    Text(String),
    /// A comment with its data.
    Comment(String),
    /// A processing instruction.
    ProcessingInstruction {
        /// The target, e.g. `xml-stylesheet`.
        target: String,
        /// The data.
        data: String,
    },
}

/// Why a tree mutation was refused (the DOM's "HierarchyRequestError" and
/// friends).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DomError {
    /// A node id that does not belong to this document.
    UnknownNode,
    /// The parent cannot have children (text, comment, doctype, ...), or the
    /// child would become its own ancestor, or the child is a document.
    HierarchyRequest,
    /// The reference child is not a child of the parent.
    NotFound,
}

impl fmt::Display for DomError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            DomError::UnknownNode => "the node does not belong to this document",
            DomError::HierarchyRequest => "the operation would yield an incorrect node tree",
            DomError::NotFound => "the reference node is not a child of the parent",
        })
    }
}

impl std::error::Error for DomError {}

#[derive(Clone, Debug)]
struct Node {
    parent: Option<NodeId>,
    first_child: Option<NodeId>,
    last_child: Option<NodeId>,
    previous_sibling: Option<NodeId>,
    next_sibling: Option<NodeId>,
    /// For a template's contents fragment, the template element (DOM's
    /// "host" of a template contents fragment).
    host: Option<NodeId>,
    data: NodeData,
}

/// A document: the arena that owns every node, with the document node at
/// [`Document::root`].
#[derive(Clone, Debug)]
pub struct Document {
    nodes: Vec<Node>,
}

impl Default for Document {
    fn default() -> Self {
        Document::new()
    }
}

impl Document {
    /// Creates an empty document in no-quirks mode.
    #[must_use]
    pub fn new() -> Document {
        Document {
            nodes: vec![Node {
                parent: None,
                first_child: None,
                last_child: None,
                previous_sibling: None,
                next_sibling: None,
                host: None,
                data: NodeData::Document {
                    quirks_mode: QuirksMode::NoQuirks,
                },
            }],
        }
    }

    /// The document node.
    #[must_use]
    pub fn root(&self) -> NodeId {
        NodeId(0)
    }

    /// The number of nodes ever created in this document (attached or not).
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// The document's mode.
    #[must_use]
    pub fn quirks_mode(&self) -> QuirksMode {
        match self.nodes.first().map(|n| &n.data) {
            Some(NodeData::Document { quirks_mode }) => *quirks_mode,
            _ => QuirksMode::NoQuirks,
        }
    }

    /// Sets the document's mode.
    pub fn set_quirks_mode(&mut self, mode: QuirksMode) {
        if let Some(Node {
            data: NodeData::Document { quirks_mode },
            ..
        }) = self.nodes.first_mut()
        {
            *quirks_mode = mode;
        }
    }

    // ----- Creating nodes -------------------------------------------------

    fn push(&mut self, data: NodeData) -> NodeId {
        // The arena would need four billion nodes to overflow; saturate
        // rather than panic if it ever does.
        let id = NodeId(u32::try_from(self.nodes.len()).unwrap_or(u32::MAX));
        self.nodes.push(Node {
            parent: None,
            first_child: None,
            last_child: None,
            previous_sibling: None,
            next_sibling: None,
            host: None,
            data,
        });
        id
    }

    /// Creates a detached element. An HTML `template` element also gets an
    /// empty document fragment for its contents.
    pub fn create_element(
        &mut self,
        namespace: Namespace,
        local_name: impl Into<String>,
        attributes: Vec<Attribute>,
    ) -> NodeId {
        let local_name = local_name.into();
        let template_contents = (namespace == Namespace::Html && local_name == "template")
            .then(|| self.push(NodeData::DocumentFragment));
        let element = self.push(NodeData::Element(ElementData {
            namespace,
            local_name,
            attributes,
            template_contents,
        }));
        if let Some(contents) = template_contents.and_then(|c| self.node_mut(c)) {
            contents.host = Some(element);
        }
        element
    }

    /// Creates a detached text node.
    pub fn create_text(&mut self, data: impl Into<String>) -> NodeId {
        self.push(NodeData::Text(data.into()))
    }

    /// Creates a detached comment.
    pub fn create_comment(&mut self, data: impl Into<String>) -> NodeId {
        self.push(NodeData::Comment(data.into()))
    }

    /// Creates a detached processing instruction.
    pub fn create_processing_instruction(
        &mut self,
        target: impl Into<String>,
        data: impl Into<String>,
    ) -> NodeId {
        self.push(NodeData::ProcessingInstruction {
            target: target.into(),
            data: data.into(),
        })
    }

    /// Creates a detached doctype.
    pub fn create_doctype(
        &mut self,
        name: impl Into<String>,
        public_id: impl Into<String>,
        system_id: impl Into<String>,
    ) -> NodeId {
        self.push(NodeData::Doctype {
            name: name.into(),
            public_id: public_id.into(),
            system_id: system_id.into(),
        })
    }

    /// Creates an empty, detached document fragment.
    pub fn create_document_fragment(&mut self) -> NodeId {
        self.push(NodeData::DocumentFragment)
    }

    // ----- Reading nodes --------------------------------------------------

    fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id.index())
    }

    fn node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.nodes.get_mut(id.index())
    }

    /// The node's type and data, or `None` for an unknown id.
    #[must_use]
    pub fn data(&self, id: NodeId) -> Option<&NodeData> {
        self.node(id).map(|n| &n.data)
    }

    /// The element data if `id` is an element.
    #[must_use]
    pub fn element(&self, id: NodeId) -> Option<&ElementData> {
        match self.data(id) {
            Some(NodeData::Element(element)) => Some(element),
            _ => None,
        }
    }

    /// Mutable element data if `id` is an element.
    pub fn element_mut(&mut self, id: NodeId) -> Option<&mut ElementData> {
        match self.node_mut(id).map(|n| &mut n.data) {
            Some(NodeData::Element(element)) => Some(element),
            _ => None,
        }
    }

    /// Whether `id` is an element.
    #[must_use]
    pub fn is_element(&self, id: NodeId) -> bool {
        self.element(id).is_some()
    }

    /// Whether `id` is an HTML element with the given local name.
    #[must_use]
    pub fn is_html_element(&self, id: NodeId, local_name: &str) -> bool {
        self.element(id).is_some_and(|e| e.is_html(local_name))
    }

    /// The data of a text node.
    #[must_use]
    pub fn text(&self, id: NodeId) -> Option<&str> {
        match self.data(id) {
            Some(NodeData::Text(text)) => Some(text),
            _ => None,
        }
    }

    /// Appends `data` to a text node. Returns false if `id` is not a text
    /// node.
    pub fn append_text(&mut self, id: NodeId, data: &str) -> bool {
        match self.node_mut(id).map(|n| &mut n.data) {
            Some(NodeData::Text(text)) => {
                text.push_str(data);
                true
            }
            _ => false,
        }
    }

    /// The value of an element's attribute by qualified name.
    #[must_use]
    pub fn attribute(&self, id: NodeId, name: &str) -> Option<&str> {
        self.element(id).and_then(|e| e.attribute(name))
    }

    /// The contents fragment of an HTML `template` element.
    #[must_use]
    pub fn template_contents(&self, id: NodeId) -> Option<NodeId> {
        self.element(id).and_then(|e| e.template_contents)
    }

    // ----- Navigation -----------------------------------------------------

    /// The parent node.
    #[must_use]
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).and_then(|n| n.parent)
    }

    /// The first child.
    #[must_use]
    pub fn first_child(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).and_then(|n| n.first_child)
    }

    /// The last child.
    #[must_use]
    pub fn last_child(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).and_then(|n| n.last_child)
    }

    /// The next sibling.
    #[must_use]
    pub fn next_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).and_then(|n| n.next_sibling)
    }

    /// The previous sibling.
    #[must_use]
    pub fn previous_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).and_then(|n| n.previous_sibling)
    }

    /// The children of `id`, in order.
    #[must_use]
    pub fn children(&self, id: NodeId) -> Children<'_> {
        Children {
            document: self,
            next: self.first_child(id),
        }
    }

    /// The element children of `id`, in order.
    pub fn element_children(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.children(id).filter(|&child| self.is_element(child))
    }

    /// The ancestors of `id`, nearest first (excluding `id` itself).
    #[must_use]
    pub fn ancestors(&self, id: NodeId) -> Ancestors<'_> {
        Ancestors {
            document: self,
            next: self.parent(id),
        }
    }

    /// The descendants of `id` in tree order (excluding `id` itself). Template
    /// contents are not descendants, as in the DOM.
    #[must_use]
    pub fn descendants(&self, id: NodeId) -> Descendants<'_> {
        Descendants {
            document: self,
            root: id,
            next: self.first_child(id),
        }
    }

    /// The host of a template contents fragment: its template element.
    #[must_use]
    pub fn host(&self, fragment: NodeId) -> Option<NodeId> {
        self.node(fragment).and_then(|n| n.host)
    }

    /// Whether `ancestor` is a "host-including inclusive ancestor" of `node`
    /// (DOM §4.2.2): an inclusive ancestor, following template contents
    /// fragments up to their template element.
    #[must_use]
    pub fn is_host_including_inclusive_ancestor(&self, ancestor: NodeId, node: NodeId) -> bool {
        let mut current = Some(node);
        while let Some(n) = current {
            if n == ancestor {
                return true;
            }
            current = self.parent(n).or_else(|| self.host(n));
        }
        false
    }

    /// Whether `ancestor` is `node` or one of its ancestors.
    #[must_use]
    pub fn is_inclusive_ancestor(&self, ancestor: NodeId, node: NodeId) -> bool {
        ancestor == node || self.ancestors(node).any(|a| a == ancestor)
    }

    /// The document element: the document's element child.
    #[must_use]
    pub fn document_element(&self) -> Option<NodeId> {
        self.element_children(self.root()).next()
    }

    /// The doctype child of the document, if any.
    #[must_use]
    pub fn doctype(&self) -> Option<NodeId> {
        self.children(self.root())
            .find(|&child| matches!(self.data(child), Some(NodeData::Doctype { .. })))
    }

    /// The first HTML element with this local name among the document
    /// element's children (e.g. `head` or `body`).
    #[must_use]
    pub fn html_child_of_document_element(&self, local_name: &str) -> Option<NodeId> {
        let html = self.document_element()?;
        self.element_children(html)
            .find(|&child| self.is_html_element(child, local_name))
    }

    // ----- Queries --------------------------------------------------------

    /// The first element in the document, in tree order, whose ID is `id`.
    #[must_use]
    pub fn element_by_id(&self, id: &str) -> Option<NodeId> {
        if id.is_empty() {
            return None;
        }
        self.descendants(self.root())
            .find(|&node| self.element(node).and_then(ElementData::id) == Some(id))
    }

    /// The elements under `root` (excluding `root`) that have every class in
    /// the whitespace-separated `class_names` (DOM §4.4
    /// `getElementsByClassName`, no-quirks matching).
    #[must_use]
    pub fn elements_by_class_name(&self, root: NodeId, class_names: &str) -> Vec<NodeId> {
        let wanted: Vec<&str> = class_names
            .split(is_ascii_whitespace)
            .filter(|c| !c.is_empty())
            .collect();
        if wanted.is_empty() {
            return Vec::new();
        }
        self.descendants(root)
            .filter(|&node| {
                self.element(node)
                    .is_some_and(|e| wanted.iter().all(|w| e.classes().any(|c| c == *w)))
            })
            .collect()
    }

    /// The elements under `root` (excluding `root`) whose qualified name is
    /// `name` (`*` matches every element). HTML elements match `name` ASCII
    /// case-insensitively.
    #[must_use]
    pub fn elements_by_tag_name(&self, root: NodeId, name: &str) -> Vec<NodeId> {
        self.descendants(root)
            .filter(|&node| {
                self.element(node).is_some_and(|e| {
                    name == "*"
                        || if e.namespace == Namespace::Html {
                            e.local_name.eq_ignore_ascii_case(name)
                        } else {
                            e.local_name == name
                        }
                })
            })
            .collect()
    }

    /// `textContent` (DOM §4.4): the concatenated data of every descendant
    /// text node for elements and fragments, the node's own data for text,
    /// comments and processing instructions, and `None` for documents and
    /// doctypes.
    #[must_use]
    pub fn text_content(&self, id: NodeId) -> Option<String> {
        match self.data(id)? {
            NodeData::Document { .. } | NodeData::Doctype { .. } => None,
            NodeData::Text(data) | NodeData::Comment(data) => Some(data.clone()),
            NodeData::ProcessingInstruction { data, .. } => Some(data.clone()),
            NodeData::Element(_) | NodeData::DocumentFragment => {
                let mut out = String::new();
                for node in self.descendants(id) {
                    if let Some(text) = self.text(node) {
                        out.push_str(text);
                    }
                }
                Some(out)
            }
        }
    }

    // ----- Mutation -------------------------------------------------------

    fn can_have_children(&self, id: NodeId) -> bool {
        matches!(
            self.data(id),
            Some(NodeData::Document { .. } | NodeData::DocumentFragment | NodeData::Element(_))
        )
    }

    /// Removes `child` from its parent, if it has one. The node stays valid
    /// and can be inserted again.
    pub fn detach(&mut self, child: NodeId) {
        let Some(node) = self.node(child) else {
            return;
        };
        let (parent, prev, next) = (node.parent, node.previous_sibling, node.next_sibling);
        let Some(parent) = parent else {
            return;
        };
        match prev {
            Some(prev) => {
                if let Some(p) = self.node_mut(prev) {
                    p.next_sibling = next;
                }
            }
            None => {
                if let Some(p) = self.node_mut(parent) {
                    p.first_child = next;
                }
            }
        }
        match next {
            Some(next) => {
                if let Some(n) = self.node_mut(next) {
                    n.previous_sibling = prev;
                }
            }
            None => {
                if let Some(p) = self.node_mut(parent) {
                    p.last_child = prev;
                }
            }
        }
        if let Some(c) = self.node_mut(child) {
            c.parent = None;
            c.previous_sibling = None;
            c.next_sibling = None;
        }
    }

    /// Appends `child` as the last child of `parent`, first removing it from
    /// its old parent.
    ///
    /// # Errors
    ///
    /// Fails if either id is unknown, `parent` cannot have children, `child`
    /// is a document, or `child` is an inclusive ancestor of `parent`.
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) -> Result<(), DomError> {
        self.insert_before(parent, child, None)
    }

    /// Inserts `child` into `parent` before `reference` (or last when
    /// `reference` is `None`), first removing it from its old parent.
    ///
    /// # Errors
    ///
    /// Fails like [`Document::append_child`], and also when `reference` is
    /// not a child of `parent`.
    pub fn insert_before(
        &mut self,
        parent: NodeId,
        child: NodeId,
        reference: Option<NodeId>,
    ) -> Result<(), DomError> {
        if self.node(parent).is_none() || self.node(child).is_none() {
            return Err(DomError::UnknownNode);
        }
        if !self.can_have_children(parent)
            || matches!(self.data(child), Some(NodeData::Document { .. }))
            || self.is_host_including_inclusive_ancestor(child, parent)
        {
            return Err(DomError::HierarchyRequest);
        }
        if let Some(reference) = reference {
            if self.parent(reference) != Some(parent) {
                return Err(DomError::NotFound);
            }
            if reference == child {
                return Ok(());
            }
        }
        self.detach(child);
        let prev = match reference {
            Some(reference) => self.previous_sibling(reference),
            None => self.last_child(parent),
        };
        if let Some(c) = self.node_mut(child) {
            c.parent = Some(parent);
            c.previous_sibling = prev;
            c.next_sibling = reference;
        }
        match prev {
            Some(prev) => {
                if let Some(p) = self.node_mut(prev) {
                    p.next_sibling = Some(child);
                }
            }
            None => {
                if let Some(p) = self.node_mut(parent) {
                    p.first_child = Some(child);
                }
            }
        }
        match reference {
            Some(reference) => {
                if let Some(r) = self.node_mut(reference) {
                    r.previous_sibling = Some(child);
                }
            }
            None => {
                if let Some(p) = self.node_mut(parent) {
                    p.last_child = Some(child);
                }
            }
        }
        Ok(())
    }

    /// Moves every child of `from` to the end of `to`, keeping their order.
    ///
    /// # Errors
    ///
    /// Fails like [`Document::append_child`]; children moved before the
    /// failure stay moved.
    pub fn reparent_children(&mut self, from: NodeId, to: NodeId) -> Result<(), DomError> {
        if from == to {
            return Ok(());
        }
        while let Some(child) = self.first_child(from) {
            self.append_child(to, child)?;
        }
        Ok(())
    }
}

/// Iterator over a node's children; see [`Document::children`].
#[derive(Debug)]
pub struct Children<'a> {
    document: &'a Document,
    next: Option<NodeId>,
}

impl Iterator for Children<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        let current = self.next?;
        self.next = self.document.next_sibling(current);
        Some(current)
    }
}

/// Iterator over a node's ancestors; see [`Document::ancestors`].
#[derive(Debug)]
pub struct Ancestors<'a> {
    document: &'a Document,
    next: Option<NodeId>,
}

impl Iterator for Ancestors<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        let current = self.next?;
        self.next = self.document.parent(current);
        Some(current)
    }
}

/// Pre-order iterator over a node's descendants; see
/// [`Document::descendants`].
#[derive(Debug)]
pub struct Descendants<'a> {
    document: &'a Document,
    root: NodeId,
    next: Option<NodeId>,
}

impl Iterator for Descendants<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        let current = self.next?;
        let doc = self.document;
        self.next = doc.first_child(current).or_else(|| {
            let mut node = current;
            loop {
                if node == self.root {
                    return None;
                }
                if let Some(sibling) = doc.next_sibling(node) {
                    return Some(sibling);
                }
                node = doc.parent(node)?;
            }
        });
        Some(current)
    }
}

/// ASCII whitespace (Infra §4.6): tab, LF, FF, CR and space.
fn is_ascii_whitespace(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{c}' | '\r' | ' ')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(doc: &mut Document, name: &str, attrs: &[(&str, &str)]) -> NodeId {
        doc.create_element(
            Namespace::Html,
            name,
            attrs.iter().map(|(k, v)| Attribute::new(*k, *v)).collect(),
        )
    }

    fn sample() -> (Document, NodeId, NodeId, NodeId, NodeId) {
        let mut doc = Document::new();
        let html = el(&mut doc, "html", &[]);
        let body = el(&mut doc, "body", &[("class", "main  page")]);
        let p = el(&mut doc, "p", &[("id", "first"), ("class", "page")]);
        let text = doc.create_text("Hello");
        doc.append_child(doc.root(), html).unwrap();
        doc.append_child(html, body).unwrap();
        doc.append_child(body, p).unwrap();
        doc.append_child(p, text).unwrap();
        (doc, html, body, p, text)
    }

    #[test]
    fn builds_and_navigates_a_tree() {
        let (doc, html, body, p, text) = sample();
        assert_eq!(doc.document_element(), Some(html));
        assert_eq!(doc.parent(body), Some(html));
        assert_eq!(doc.children(body).collect::<Vec<_>>(), vec![p]);
        assert_eq!(doc.first_child(p), Some(text));
        assert_eq!(doc.last_child(p), Some(text));
        assert_eq!(
            doc.ancestors(text).collect::<Vec<_>>(),
            vec![p, body, html, doc.root()]
        );
        assert_eq!(
            doc.descendants(doc.root()).collect::<Vec<_>>(),
            vec![html, body, p, text]
        );
        assert_eq!(doc.html_child_of_document_element("body"), Some(body));
    }

    #[test]
    fn queries_by_id_class_and_tag() {
        let (doc, _html, body, p, _text) = sample();
        assert_eq!(doc.element_by_id("first"), Some(p));
        assert_eq!(doc.element_by_id("missing"), None);
        assert_eq!(
            doc.elements_by_class_name(doc.root(), "page"),
            vec![body, p]
        );
        assert_eq!(
            doc.elements_by_class_name(doc.root(), "page main"),
            vec![body]
        );
        assert_eq!(doc.elements_by_tag_name(doc.root(), "P"), vec![p]);
        assert_eq!(doc.elements_by_tag_name(body, "*"), vec![p]);
        assert_eq!(doc.attribute(p, "id"), Some("first"));
        assert_eq!(doc.text_content(body).as_deref(), Some("Hello"));
        assert_eq!(doc.text_content(doc.root()), None);
    }

    #[test]
    fn insert_before_and_detach_keep_links_consistent() {
        let mut doc = Document::new();
        let parent = el(&mut doc, "div", &[]);
        let a = doc.create_text("a");
        let b = doc.create_text("b");
        let c = doc.create_text("c");
        doc.append_child(parent, a).unwrap();
        doc.append_child(parent, c).unwrap();
        doc.insert_before(parent, b, Some(c)).unwrap();
        assert_eq!(doc.children(parent).collect::<Vec<_>>(), vec![a, b, c]);
        assert_eq!(doc.previous_sibling(c), Some(b));
        doc.detach(b);
        assert_eq!(doc.children(parent).collect::<Vec<_>>(), vec![a, c]);
        assert_eq!(doc.next_sibling(a), Some(c));
        assert_eq!(doc.parent(b), None);
        // Moving a node to the front.
        doc.insert_before(parent, c, Some(a)).unwrap();
        assert_eq!(doc.children(parent).collect::<Vec<_>>(), vec![c, a]);
        assert_eq!(doc.last_child(parent), Some(a));
    }

    #[test]
    fn refuses_bad_hierarchies() {
        let (mut doc, html, body, p, text) = sample();
        assert_eq!(doc.append_child(p, html), Err(DomError::HierarchyRequest));
        assert_eq!(
            doc.append_child(text, body),
            Err(DomError::HierarchyRequest)
        );
        assert_eq!(
            doc.insert_before(body, text, Some(html)),
            Err(DomError::NotFound)
        );
        assert_eq!(doc.append_child(NodeId(999), p), Err(DomError::UnknownNode));
    }

    #[test]
    fn reparents_children_and_appends_text() {
        let (mut doc, _html, body, p, text) = sample();
        let div = el(&mut doc, "div", &[]);
        doc.reparent_children(p, div).unwrap();
        assert_eq!(doc.first_child(p), None);
        assert_eq!(doc.parent(text), Some(div));
        assert!(doc.append_text(text, ", world"));
        assert_eq!(doc.text(text), Some("Hello, world"));
        assert!(!doc.append_text(body, "x"));
    }

    #[test]
    fn templates_get_contents_and_attributes_work() {
        let mut doc = Document::new();
        let template = el(&mut doc, "template", &[]);
        let contents = doc.template_contents(template).unwrap();
        assert_eq!(doc.data(contents), Some(&NodeData::DocumentFragment));
        let element = doc.element_mut(template).unwrap();
        element.set_attribute("id", "t");
        element.set_attribute("id", "u");
        assert_eq!(element.id(), Some("u"));
        assert_eq!(element.attributes.len(), 1);

        let mut xlink = Attribute::new("href", "#a");
        xlink.namespace = Some(Namespace::XLink);
        xlink.prefix = Some("xlink".to_string());
        assert_eq!(xlink.qualified_name(), "xlink:href");
        let svg = doc.create_element(Namespace::Svg, "use", vec![xlink]);
        assert_eq!(doc.attribute(svg, "xlink:href"), Some("#a"));
        assert_eq!(doc.attribute(svg, "href"), None);
    }

    #[test]
    fn refuses_cycles_through_template_contents() {
        let mut doc = Document::new();
        let template = el(&mut doc, "template", &[]);
        let contents = doc.template_contents(template).unwrap();
        assert_eq!(doc.host(contents), Some(template));
        // A template inside its own contents.
        assert_eq!(
            doc.append_child(contents, template),
            Err(DomError::HierarchyRequest)
        );
        // A subtree holding the template, inserted into its contents.
        let div = el(&mut doc, "div", &[]);
        doc.append_child(div, template).unwrap();
        assert_eq!(
            doc.append_child(contents, div),
            Err(DomError::HierarchyRequest)
        );
        // Deeper: an element inside the contents of a nested template.
        let inner = el(&mut doc, "template", &[]);
        let inner_contents = doc.template_contents(inner).unwrap();
        doc.append_child(contents, inner).unwrap();
        assert_eq!(
            doc.append_child(inner_contents, div),
            Err(DomError::HierarchyRequest)
        );
        // Ordinary insertion into template contents still works.
        let p = el(&mut doc, "p", &[]);
        doc.append_child(inner_contents, p).unwrap();
        assert_eq!(doc.parent(p), Some(inner_contents));
    }

    #[test]
    fn reparenting_children_onto_the_same_node_is_a_no_op() {
        let (mut doc, _html, _body, p, text) = sample();
        doc.reparent_children(p, p).unwrap();
        assert_eq!(doc.children(p).collect::<Vec<_>>(), vec![text]);
        // Moving children into one of those children fails instead of
        // looping.
        let (mut doc, _html, body, p, _text) = sample();
        assert_eq!(
            doc.reparent_children(body, p),
            Err(DomError::HierarchyRequest)
        );
    }

    #[test]
    fn quirks_mode_round_trips() {
        let mut doc = Document::new();
        assert_eq!(doc.quirks_mode(), QuirksMode::NoQuirks);
        doc.set_quirks_mode(QuirksMode::Quirks);
        assert_eq!(doc.quirks_mode(), QuirksMode::Quirks);
        assert_eq!(Namespace::Svg.url(), "http://www.w3.org/2000/svg");
    }
}
