//! Rule collection: flattening stylesheets into a list of selectors with
//! their cascade data (CSS Cascade 5 §6), and bucketing them by their
//! rightmost compound for fast lookup.

use std::collections::HashMap;
use std::sync::Arc;

use wb_css::{
    Combinator, CssRule, DeclarationBlock, MediaContext, PseudoElement, Selector, SimpleSelector,
    Specificity, Stylesheet,
};

use crate::bloom::{self, Kind};
use crate::{Origin, SheetInput};

/// `@import` nesting deeper than this is ignored.
const MAX_IMPORT_DEPTH: usize = 16;
/// Selectors with more compounds than this never match (they could only
/// come from adversarial input, and matching recurses once per compound).
pub(crate) const MAX_COMPOUNDS: usize = 256;
/// How many ancestor hashes are kept per selector for the Bloom filter.
const ANCESTOR_HASHES: usize = 4;

/// The declarations of one style rule, shared by all its selectors.
#[derive(Debug)]
pub(crate) struct RuleData {
    pub(crate) declarations: DeclarationBlock,
    pub(crate) origin: Origin,
    /// The rule's cascade layer rank within its origin; higher wins for
    /// normal declarations. Unlayered rules have the highest rank.
    pub(crate) layer: u32,
    /// The base URL for `url()`s in the declarations.
    pub(crate) base_url: Option<Arc<wb_url::Url>>,
    /// UA rules that only apply in quirks mode (HTML §15.3).
    pub(crate) quirks_only: bool,
}

/// One selector of a rule.
#[derive(Debug)]
pub(crate) struct RuleSelector {
    pub(crate) selector: Selector,
    pub(crate) rule: u32,
    /// Specificity packed as `ids << 20 | classes << 10 | types`, each
    /// saturated to 10 bits.
    pub(crate) specificity: u32,
    /// Global source order.
    pub(crate) order: u32,
    /// Ids, classes and tags some ancestor must have (0 = unused).
    pub(crate) ancestor_hashes: [u32; ANCESTOR_HASHES],
}

impl RuleSelector {
    pub(crate) fn pseudo(&self) -> Option<PseudoElement> {
        self.selector.pseudo_element
    }
}

/// All rules of all origins, bucketed.
#[derive(Debug, Default)]
pub(crate) struct RuleSet {
    pub(crate) rules: Vec<RuleData>,
    pub(crate) selectors: Vec<RuleSelector>,
    /// Keyed by the ASCII-lowercased id / class / tag of the rightmost
    /// compound.
    pub(crate) by_id: HashMap<String, Vec<u32>>,
    pub(crate) by_class: HashMap<String, Vec<u32>>,
    pub(crate) by_tag: HashMap<String, Vec<u32>>,
    pub(crate) universal: Vec<u32>,
}

/// The cascade layer tree of one origin (CSS Cascade 5 §6.4).
#[derive(Debug, Default)]
struct Layers {
    /// Node 0 is the implicit outer (unlayered) layer.
    nodes: Vec<LayerNode>,
    by_name: HashMap<String, usize>,
    anonymous: usize,
}

#[derive(Debug, Default)]
struct LayerNode {
    children: Vec<usize>,
    rank: u32,
}

impl Layers {
    fn new() -> Self {
        Layers {
            nodes: vec![LayerNode::default()],
            by_name: HashMap::new(),
            anonymous: 0,
        }
    }

    /// The layer named `name` (dotted, relative to `parent`), creating it
    /// and any missing ancestors in declaration order.
    fn get_or_create(&mut self, parent: usize, parent_name: &str, name: &str) -> (usize, String) {
        let mut node = parent;
        let mut full = parent_name.to_string();
        for part in name.split('.') {
            if !full.is_empty() {
                full.push('.');
            }
            full.push_str(part);
            node = match self.by_name.get(&full) {
                Some(&n) => n,
                None => {
                    let n = self.nodes.len();
                    self.nodes.push(LayerNode::default());
                    if let Some(p) = self.nodes.get_mut(node) {
                        p.children.push(n);
                    }
                    self.by_name.insert(full.clone(), n);
                    n
                }
            };
        }
        (node, full)
    }

    fn anonymous(&mut self, parent: usize, parent_name: &str) -> (usize, String) {
        self.anonymous += 1;
        // A name that cannot be written in CSS, so it is never shared.
        let name = format!(" anon{}", self.anonymous);
        self.get_or_create(parent, parent_name, &name)
    }

    /// Ranks layers in cascade order: a layer's sublayers come before the
    /// layer's own rules, siblings in declaration order, and the outer
    /// layer (unlayered rules) last (§6.4.3). Iterative post-order.
    fn assign_ranks(&mut self) {
        let mut rank = 0u32;
        let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
        while let Some((node, child)) = stack.pop() {
            let next = self
                .nodes
                .get(node)
                .and_then(|n| n.children.get(child))
                .copied();
            match next {
                Some(c) => {
                    stack.push((node, child + 1));
                    stack.push((c, 0));
                }
                None => {
                    if let Some(n) = self.nodes.get_mut(node) {
                        n.rank = rank;
                    }
                    rank = rank.saturating_add(1);
                }
            }
        }
    }

    fn rank(&self, node: usize) -> u32 {
        self.nodes.get(node).map_or(0, |n| n.rank)
    }
}

struct Builder<'a> {
    set: RuleSet,
    media: &'a MediaContext,
    layers: [Layers; 3],
    /// Layer node per rule, ranked once every rule is known.
    rule_layer_nodes: Vec<usize>,
    order: u32,
}

fn origin_index(o: Origin) -> usize {
    match o {
        Origin::UserAgent => 0,
        Origin::User => 1,
        Origin::Author => 2,
    }
}

struct SheetCx<'a> {
    origin: Origin,
    base_url: Option<Arc<wb_url::Url>>,
    quirks_only: bool,
    imports: &'a [Option<SheetInput>],
    depth: usize,
}

impl RuleSet {
    pub(crate) fn build(sheets: &[SheetInput], media: &MediaContext) -> RuleSet {
        let mut b = Builder {
            set: RuleSet::default(),
            media,
            layers: [Layers::new(), Layers::new(), Layers::new()],
            rule_layer_nodes: Vec::new(),
            order: 0,
        };
        let ua = crate::ua::ua_sheets();
        for (sheet, quirks_only) in [(&ua.normal, false), (&ua.quirks, true)] {
            let cx = SheetCx {
                origin: Origin::UserAgent,
                base_url: None,
                quirks_only,
                imports: &[],
                depth: 0,
            };
            b.add_rules(&sheet.rules, &cx, 0, "");
        }
        for input in sheets {
            b.add_input(input, input.origin, 0, "", 0);
        }
        for l in &mut b.layers {
            l.assign_ranks();
        }
        let Builder {
            mut set,
            layers,
            rule_layer_nodes,
            ..
        } = b;
        for (rule, node) in set.rules.iter_mut().zip(rule_layer_nodes) {
            if let Some(l) = layers.get(origin_index(rule.origin)) {
                rule.layer = l.rank(node);
            }
        }
        set
    }

    pub(crate) fn selector_count(&self) -> usize {
        self.selectors.len()
    }
}

impl Builder<'_> {
    fn add_input(
        &mut self,
        input: &SheetInput,
        origin: Origin,
        layer: usize,
        layer_name: &str,
        depth: usize,
    ) {
        if depth > MAX_IMPORT_DEPTH || !input.media.matches(self.media) {
            return;
        }
        let cx = SheetCx {
            origin,
            base_url: input.base_url.clone().map(Arc::new),
            quirks_only: false,
            imports: &input.imports,
            depth,
        };
        self.add_sheet(&input.sheet, &cx, layer, layer_name);
    }

    fn add_sheet(&mut self, sheet: &Stylesheet, cx: &SheetCx<'_>, layer: usize, name: &str) {
        self.add_rules(&sheet.rules, cx, layer, name);
    }

    fn add_rules(&mut self, rules: &[CssRule], cx: &SheetCx<'_>, layer: usize, name: &str) {
        let oi = origin_index(cx.origin);
        let mut import_index = 0usize;
        for rule in rules {
            match rule {
                CssRule::Style(style) => self.add_style_rule(style, cx, layer),
                CssRule::Media(m) => {
                    if m.queries.matches(self.media) {
                        self.add_rules(&m.rules, cx, layer, name);
                    }
                }
                CssRule::Supports(s) => {
                    if s.condition {
                        self.add_rules(&s.rules, cx, layer, name);
                    }
                }
                CssRule::LayerBlock(block) => {
                    let Some(layers) = self.layers.get_mut(oi) else {
                        continue;
                    };
                    let (node, full) = match &block.name {
                        Some(n) => layers.get_or_create(layer, name, n),
                        None => layers.anonymous(layer, name),
                    };
                    self.add_rules(&block.rules, cx, node, &full);
                }
                CssRule::LayerStatement(names) => {
                    if let Some(layers) = self.layers.get_mut(oi) {
                        for n in names {
                            layers.get_or_create(layer, name, n);
                        }
                    }
                }
                CssRule::Import(import) => {
                    let entry = cx.imports.get(import_index);
                    import_index += 1;
                    let Some(Some(child)) = entry else {
                        continue;
                    };
                    if !import.supports || !import.media.matches(self.media) {
                        continue;
                    }
                    let (child_layer, child_name) = match (&import.layer, self.layers.get_mut(oi)) {
                        (Some(Some(n)), Some(layers)) => layers.get_or_create(layer, name, n),
                        (Some(None), Some(layers)) => layers.anonymous(layer, name),
                        _ => (layer, name.to_string()),
                    };
                    // The imported sheet keeps the importing sheet's origin.
                    let depth = cx.depth.saturating_add(1);
                    self.add_input(child, cx.origin, child_layer, &child_name, depth);
                }
            }
        }
    }

    fn add_style_rule(&mut self, style: &wb_css::StyleRule, cx: &SheetCx<'_>, layer: usize) {
        if style.declarations.declarations.is_empty() {
            return;
        }
        let Ok(rule_index) = u32::try_from(self.set.rules.len()) else {
            return;
        };
        self.set.rules.push(RuleData {
            declarations: style.declarations.clone(),
            origin: cx.origin,
            layer: 0,
            base_url: cx.base_url.clone(),
            quirks_only: cx.quirks_only,
        });
        self.rule_layer_nodes.push(layer);
        for selector in &style.selectors.0 {
            if selector.compounds.len() > MAX_COMPOUNDS {
                continue;
            }
            let Ok(index) = u32::try_from(self.set.selectors.len()) else {
                return;
            };
            self.order = self.order.saturating_add(1);
            let rs = RuleSelector {
                selector: selector.clone(),
                rule: rule_index,
                specificity: pack_specificity(selector.specificity),
                order: self.order,
                ancestor_hashes: ancestor_hashes(selector),
            };
            self.bucket(index, selector);
            self.set.selectors.push(rs);
        }
    }

    fn bucket(&mut self, index: u32, selector: &Selector) {
        let Some(subject) = selector.compounds.first() else {
            return;
        };
        let mut id = None;
        let mut class = None;
        let mut tag = None;
        for s in &subject.selectors {
            match s {
                SimpleSelector::Id(i) => id = Some(i),
                SimpleSelector::Class(c) => class = class.or(Some(c)),
                SimpleSelector::Type { lowercase_name, .. } => tag = Some(lowercase_name),
                _ => {}
            }
        }
        let set = &mut self.set;
        if let Some(i) = id {
            set.by_id
                .entry(i.to_ascii_lowercase())
                .or_default()
                .push(index);
        } else if let Some(c) = class {
            set.by_class
                .entry(c.to_ascii_lowercase())
                .or_default()
                .push(index);
        } else if let Some(t) = tag {
            set.by_tag.entry(t.clone()).or_default().push(index);
        } else {
            set.universal.push(index);
        }
    }
}

fn pack_specificity(s: Specificity) -> u32 {
    let c = |v: u32| v.min(1023);
    (c(s.ids) << 20) | (c(s.classes) << 10) | c(s.types)
}

/// Hashes of ids, classes and tags that must appear on ancestors: taken from
/// compounds reached from the subject only through child/descendant
/// combinators.
fn ancestor_hashes(selector: &Selector) -> [u32; ANCESTOR_HASHES] {
    let mut out = [0u32; ANCESTOR_HASHES];
    let mut n = 0usize;
    for pair in selector.compounds.windows(2) {
        let [right, left] = pair else {
            break;
        };
        match right.combinator {
            Some(Combinator::Child | Combinator::Descendant) => {}
            _ => break,
        }
        for s in &left.selectors {
            let h = match s {
                SimpleSelector::Id(i) => bloom::hash(Kind::Id, i),
                SimpleSelector::Class(c) => bloom::hash(Kind::Class, c),
                SimpleSelector::Type { lowercase_name, .. } => {
                    bloom::hash(Kind::Tag, lowercase_name)
                }
                _ => continue,
            };
            if let Some(slot) = out.get_mut(n) {
                *slot = h;
                n += 1;
            } else {
                return out;
            }
        }
    }
    out
}
