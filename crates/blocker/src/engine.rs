//! The compiled filter engine and its token index.

use std::collections::HashSet;

use crate::cosmetic::{self, Cosmetic, CosmeticFilter, CosmeticIndex};
use crate::domains::{Reject, normalize_host, parent_domains};
use crate::hash::IdMap;
use crate::list::{Conditions, ListReport, read_metadata};
use crate::network::{
    self, NetworkFilter, PAGE_DOCUMENT, PAGE_ELEMHIDE, PAGE_GENERICHIDE, PAGE_SPECIFICHIDE,
    domain_hash,
};
use crate::request::{Request, Target};

/// Network filters indexed for fast lookup.
///
/// Each filter is stored under at most one key:
/// 1. its rarest whole token (see [`NetworkFilter::index_tokens`]), looked
///    up from the URL's tokens; else
/// 2. each of its `$domain=` includes, looked up from the source host and
///    its parents; else
/// 3. a short fallback list checked for every request (regex filters and
///    patterns without a usable token).
#[derive(Debug, Default)]
struct FilterSet {
    filters: Vec<NetworkFilter>,
    by_token: IdMap<Vec<u32>>,
    by_domain: IdMap<Vec<u32>>,
    fallback: Vec<u32>,
}

impl FilterSet {
    fn build(filters: Vec<NetworkFilter>) -> Self {
        let tokens: Vec<Vec<u64>> = filters.iter().map(NetworkFilter::index_tokens).collect();
        let mut freq: IdMap<u32> = IdMap::default();
        for t in tokens.iter().flatten() {
            let n = freq.entry(*t).or_insert(0);
            *n = n.saturating_add(1);
        }
        let mut set = Self::default();
        for (i, (f, toks)) in filters.iter().zip(&tokens).enumerate() {
            let Ok(i) = u32::try_from(i) else {
                break;
            };
            let rarest = toks
                .iter()
                .min_by_key(|t| freq.get(t).copied().unwrap_or(0));
            if let Some(t) = rarest {
                set.by_token.entry(*t).or_default().push(i);
            } else if let Some(d) = f.domains.as_ref().filter(|d| d.has_includes()) {
                for dom in d.includes() {
                    set.by_domain.entry(domain_hash(dom)).or_default().push(i);
                }
            } else {
                set.fallback.push(i);
            }
        }
        set.filters = filters;
        set
    }

    /// The first filter that `accept`s and matches `target` requested from
    /// `source_host`.
    fn find(
        &self,
        target: &Target<'_>,
        source_host: &str,
        third_party: bool,
        accept: impl Fn(&NetworkFilter) -> bool,
    ) -> Option<&NetworkFilter> {
        if self.filters.is_empty() {
            return None;
        }
        let check = |i: &u32| {
            self.filters.get(*i as usize).filter(|f| {
                accept(f) && f.matches_context(source_host, third_party) && f.matches_url(target)
            })
        };
        let by_token = target
            .tokens
            .iter()
            .filter_map(|t| self.by_token.get(t))
            .flatten();
        let by_domain = parent_domains(source_host)
            .filter_map(|d| self.by_domain.get(&domain_hash(d)))
            .flatten();
        by_token
            .chain(by_domain)
            .chain(self.fallback.iter())
            .find_map(check)
    }
}

/// The outcome of checking a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict<'e> {
    /// No blocking filter matched.
    NoMatch,
    /// Block the request. `filter` is the text of the matching filter.
    Blocked {
        /// The blocking filter's text.
        filter: &'e str,
    },
    /// A blocking filter matched but an exception (`@@`) overrode it.
    Allowed {
        /// The exception filter's text.
        filter: &'e str,
    },
    /// Filtering is disabled for the source site.
    SiteDisabled,
}

impl Verdict<'_> {
    /// Whether the request should be blocked.
    pub fn is_blocked(&self) -> bool {
        matches!(self, Self::Blocked { .. })
    }
}

/// The cosmetic selectors to hide on a page.
///
/// Each string is one selector exactly as written in the list; the caller
/// parses it with the CSS engine and drops selectors that fail to parse.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CosmeticSelectors<'e> {
    /// Generic selectors (`##sel` and `~domain##sel`), minus exceptions for
    /// this page. Mostly the same for every page.
    pub generic: Vec<&'e str>,
    /// Selectors for this page's host and its parent domains
    /// (`domain##sel`), minus exceptions.
    pub specific: Vec<&'e str>,
}

impl CosmeticSelectors<'_> {
    /// Whether there is nothing to hide.
    pub fn is_empty(&self) -> bool {
        self.generic.is_empty() && self.specific.is_empty()
    }
}

/// Collects filter lists, then compiles them into an [`Engine`].
#[derive(Debug, Default)]
pub struct EngineBuilder {
    network: Vec<NetworkFilter>,
    badfilters: HashSet<String>,
    cosmetic: Vec<CosmeticFilter>,
}

impl EngineBuilder {
    /// An empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses one filter list (EasyList / uBlock Origin syntax subset) and
    /// adds its filters. Unsupported or malformed lines are dropped and
    /// counted in the report; parsing never fails.
    pub fn add_list(&mut self, text: &str) -> ListReport {
        let mut report = ListReport::default();
        let stats = &mut report.stats;
        let mut conditions = Conditions::default();
        let mut in_header = true;
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("!#")
                && conditions.directive(rest)
            {
                stats.comments += 1;
                continue;
            }
            if !conditions.active() {
                stats.skipped_conditional += 1;
                continue;
            }
            if let Some(comment) = line.strip_prefix('!') {
                if in_header {
                    read_metadata(comment, &mut report.metadata);
                }
                stats.comments += 1;
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                // `[Adblock Plus 2.0]` and similar headers.
                stats.comments += 1;
                continue;
            }
            in_header = false;
            match cosmetic::parse(line) {
                Cosmetic::Filter(f) => {
                    stats.cosmetic += 1;
                    self.cosmetic.push(f);
                    continue;
                }
                Cosmetic::Rejected(Reject::Invalid) => {
                    stats.invalid += 1;
                    continue;
                }
                Cosmetic::Rejected(_) => {
                    stats.unsupported_cosmetic += 1;
                    continue;
                }
                Cosmetic::NotThisSyntax => {}
            }
            if line.starts_with('#') {
                // `# comment` (hosts-file style comments used by some lists).
                stats.comments += 1;
                continue;
            }
            match network::parse(line) {
                Ok(p) if p.badfilter => {
                    stats.badfilters += 1;
                    self.badfilters.insert(network::badfilter_key(line));
                }
                Ok(p) => {
                    stats.network += 1;
                    self.network.push(p.filter);
                }
                Err(Reject::Unsupported) => stats.unsupported_network += 1,
                Err(Reject::BadRegex) => stats.invalid_regex += 1,
                Err(Reject::Invalid) => stats.invalid += 1,
            }
        }
        report
    }

    /// Compiles the collected lists. `$badfilter` lines from any list
    /// disable the matching filter from any list.
    pub fn build(self) -> Engine {
        let mut important = Vec::new();
        let mut blocks = Vec::new();
        let mut exceptions = Vec::new();
        let mut page = Vec::new();
        let mut network_count = 0;
        for f in self.network {
            if !self.badfilters.is_empty()
                && self.badfilters.contains(&network::badfilter_key(&f.raw))
            {
                continue;
            }
            network_count += 1;
            if f.exception {
                if f.page_flags != 0 {
                    page.push(f.clone());
                }
                if f.types != 0 {
                    exceptions.push(f);
                }
            } else if f.important {
                important.push(f);
            } else {
                blocks.push(f);
            }
        }
        Engine {
            important: FilterSet::build(important),
            blocks: FilterSet::build(blocks),
            exceptions: FilterSet::build(exceptions),
            page: FilterSet::build(page),
            cosmetic: CosmeticIndex::build(self.cosmetic),
            disabled_sites: HashSet::new(),
            network_count,
        }
    }
}

/// A compiled set of filter lists plus the user's per-site disable list.
///
/// Build one with [`EngineBuilder`]. Checking is read-only (`&self`), so one
/// engine can be shared between threads.
#[derive(Debug, Default)]
pub struct Engine {
    important: FilterSet,
    blocks: FilterSet,
    exceptions: FilterSet,
    /// Exceptions with page-level options (`$document`, `$elemhide`,
    /// `$generichide`, `$specifichide`), matched against the page URL.
    page: FilterSet,
    cosmetic: CosmeticIndex,
    disabled_sites: HashSet<String>,
    network_count: usize,
}

impl Engine {
    /// Compiles one list; a shortcut for [`EngineBuilder`].
    pub fn from_list(text: &str) -> (Self, ListReport) {
        let mut b = EngineBuilder::new();
        let report = b.add_list(text);
        (b.build(), report)
    }

    /// Decides whether to block a request.
    ///
    /// Order: a disabled site allows everything; an `$important` blocking
    /// filter blocks even if an exception matches; otherwise a blocking
    /// filter blocks unless an exception for the request, or a `$document`
    /// exception for the source page, matches.
    pub fn check(&self, req: &Request<'_>) -> Verdict<'_> {
        if self.is_site_disabled(&req.source_host) {
            return Verdict::SiteDisabled;
        }
        let bit = req.resource_type.bit();
        let types = |f: &NetworkFilter| f.types & bit != 0;
        let host = req.source_host.as_str();
        if let Some(f) = self.important.find(&req.url, host, req.third_party, types) {
            return Verdict::Blocked { filter: &f.raw };
        }
        let Some(block) = self.blocks.find(&req.url, host, req.third_party, types) else {
            return Verdict::NoMatch;
        };
        let page_ok = |f: &NetworkFilter| f.page_flags & PAGE_DOCUMENT != 0;
        if let Some(f) = self.page.find(&req.source, host, false, page_ok) {
            return Verdict::Allowed { filter: &f.raw };
        }
        if let Some(f) = self.exceptions.find(&req.url, host, req.third_party, types) {
            return Verdict::Allowed { filter: &f.raw };
        }
        Verdict::Blocked { filter: &block.raw }
    }

    /// The element-hiding selectors for a page.
    ///
    /// `page` is the page URL (or just its host). `$elemhide`,
    /// `$generichide` and `$specifichide` exceptions are matched against it.
    pub fn cosmetic_selectors(&self, page: &str) -> CosmeticSelectors<'_> {
        let probe = Request::new(page, page, crate::ResourceType::Document, true);
        let host = probe.source_host.as_str();
        if host.is_empty() || self.is_site_disabled(host) {
            return CosmeticSelectors::default();
        }
        let flags = |flag: u8| {
            self.page
                .find(&probe.source, host, false, |f| f.page_flags & flag != 0)
                .is_some()
        };
        if flags(PAGE_ELEMHIDE) {
            return CosmeticSelectors::default();
        }
        let (generic, specific) =
            self.cosmetic
                .query(host, !flags(PAGE_GENERICHIDE), !flags(PAGE_SPECIFICHIDE));
        CosmeticSelectors { generic, specific }
    }

    /// Turns filtering off (`disabled = true`) or back on for `host` and its
    /// subdomains. The host is compared case-insensitively.
    pub fn set_site_disabled(&mut self, host: &str, disabled: bool) {
        let host = normalize_host(host);
        if host.is_empty() {
            return;
        }
        if disabled {
            self.disabled_sites.insert(host);
        } else {
            self.disabled_sites.remove(&host);
        }
    }

    /// Whether filtering is off for `host` (it or a parent domain is on the
    /// disable list).
    pub fn is_site_disabled(&self, host: &str) -> bool {
        if self.disabled_sites.is_empty() {
            return false;
        }
        let host = normalize_host(host);
        parent_domains(&host).any(|d| self.disabled_sites.contains(d))
    }

    /// The hosts on the disable list, in no particular order.
    pub fn disabled_sites(&self) -> impl Iterator<Item = &str> {
        self.disabled_sites.iter().map(String::as_str)
    }

    /// Number of network filters in the engine (after `$badfilter`).
    pub fn network_filter_count(&self) -> usize {
        self.network_count
    }

    /// Number of cosmetic filters in the engine.
    pub fn cosmetic_filter_count(&self) -> usize {
        self.cosmetic.len()
    }
}
