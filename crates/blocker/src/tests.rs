//! Engine-level tests: whole lists, requests and pages.

use std::time::{Duration, Instant};

use crate::{Engine, EngineBuilder, Request, ResourceType, Verdict};

fn engine(list: &str) -> Engine {
    Engine::from_list(list).0
}

fn check<'e>(
    e: &'e Engine,
    url: &str,
    source: &str,
    ty: ResourceType,
    same_site: bool,
) -> Verdict<'e> {
    e.check(&Request::new(url, source, ty, same_site))
}

fn blocked(e: &Engine, url: &str, source: &str, ty: ResourceType, same_site: bool) -> bool {
    check(e, url, source, ty, same_site).is_blocked()
}

const PAGE: &str = "https://news.test/article";

#[test]
fn basic_block_and_exception() {
    let e = engine("||ads.example^\n@@||ads.example/allowed/\n");
    assert_eq!(
        check(
            &e,
            "https://ads.example/x.js",
            PAGE,
            ResourceType::Script,
            false
        ),
        Verdict::Blocked {
            filter: "||ads.example^"
        }
    );
    assert_eq!(
        check(
            &e,
            "https://ads.example/allowed/x.js",
            PAGE,
            ResourceType::Script,
            false
        ),
        Verdict::Allowed {
            filter: "@@||ads.example/allowed/"
        }
    );
    assert_eq!(
        check(
            &e,
            "https://news.test/x.js",
            PAGE,
            ResourceType::Script,
            true
        ),
        Verdict::NoMatch
    );
}

#[test]
fn documents_need_explicit_type() {
    let e = engine("||ads.example^\n||tracker.example^$document\n");
    assert!(!blocked(
        &e,
        "https://ads.example/",
        "https://ads.example/",
        ResourceType::Document,
        true
    ));
    assert!(blocked(
        &e,
        "https://ads.example/",
        PAGE,
        ResourceType::Subdocument,
        false
    ));
    assert!(blocked(
        &e,
        "https://tracker.example/",
        "https://tracker.example/",
        ResourceType::Document,
        true
    ));
    assert!(!blocked(
        &e,
        "https://tracker.example/a.js",
        PAGE,
        ResourceType::Script,
        false
    ));
}

#[test]
fn resource_types_and_party() {
    let e = engine(
        "/banner/*$image,~third-party\n\
         ||cdn.example^$script,3p\n\
         /track.js$~script\n",
    );
    assert!(blocked(
        &e,
        "https://news.test/banner/a.png",
        PAGE,
        ResourceType::Image,
        true
    ));
    assert!(!blocked(
        &e,
        "https://x.test/banner/a.png",
        PAGE,
        ResourceType::Image,
        false
    ));
    assert!(!blocked(
        &e,
        "https://news.test/banner/a.js",
        PAGE,
        ResourceType::Script,
        true
    ));
    assert!(blocked(
        &e,
        "https://cdn.example/a.js",
        PAGE,
        ResourceType::Script,
        false
    ));
    assert!(!blocked(
        &e,
        "https://cdn.example/a.js",
        PAGE,
        ResourceType::Script,
        true
    ));
    assert!(!blocked(
        &e,
        "https://x.test/track.js",
        PAGE,
        ResourceType::Script,
        true
    ));
    assert!(blocked(
        &e,
        "https://x.test/track.js",
        PAGE,
        ResourceType::XmlHttpRequest,
        true
    ));
}

#[test]
fn domain_option() {
    let e = engine("/ads.js$domain=news.test|~sports.news.test\n*$script,domain=evil.test\n");
    assert!(blocked(
        &e,
        "https://x.test/ads.js",
        PAGE,
        ResourceType::Script,
        false
    ));
    assert!(blocked(
        &e,
        "https://x.test/ads.js",
        "https://www.news.test/",
        ResourceType::Script,
        false
    ));
    assert!(!blocked(
        &e,
        "https://x.test/ads.js",
        "https://sports.news.test/",
        ResourceType::Script,
        false
    ));
    assert!(!blocked(
        &e,
        "https://x.test/ads.js",
        "https://other.test/",
        ResourceType::Script,
        false
    ));
    assert!(!blocked(
        &e,
        "https://x.test/ads.js",
        "",
        ResourceType::Script,
        false
    ));
    // Token-less filter indexed by domain.
    assert!(blocked(
        &e,
        "https://any.test/anything",
        "https://a.evil.test/",
        ResourceType::Script,
        false
    ));
    assert!(!blocked(
        &e,
        "https://any.test/anything",
        "https://a.evil.test/",
        ResourceType::Image,
        false
    ));
    assert!(!blocked(
        &e,
        "https://any.test/anything",
        PAGE,
        ResourceType::Script,
        false
    ));
}

#[test]
fn important_beats_exceptions() {
    let e = engine("||ads.example^$important\n@@||ads.example^\n||t.example^\n@@||t.example^\n");
    assert!(blocked(
        &e,
        "https://ads.example/x",
        PAGE,
        ResourceType::Image,
        false
    ));
    assert!(!blocked(
        &e,
        "https://t.example/x",
        PAGE,
        ResourceType::Image,
        false
    ));
}

#[test]
fn document_exception_allows_whole_page() {
    let e = engine("||ads.example^\n@@||trusted.test^$document\n");
    assert!(blocked(
        &e,
        "https://ads.example/x",
        PAGE,
        ResourceType::Image,
        false
    ));
    assert_eq!(
        check(
            &e,
            "https://ads.example/x",
            "https://www.trusted.test/p",
            ResourceType::Image,
            false
        ),
        Verdict::Allowed {
            filter: "@@||trusted.test^$document"
        }
    );
}

#[test]
fn badfilter_across_lists() {
    let mut b = EngineBuilder::new();
    let r1 = b.add_list("||ads.example^$script,image\n||keep.example^\n");
    let r2 = b.add_list("||ads.example^$image,script,badfilter\n");
    assert_eq!(r1.stats.network, 2);
    assert_eq!(r2.stats.badfilters, 1);
    let e = b.build();
    assert_eq!(e.network_filter_count(), 1);
    assert!(!blocked(
        &e,
        "https://ads.example/x.js",
        PAGE,
        ResourceType::Script,
        false
    ));
    assert!(blocked(
        &e,
        "https://keep.example/x.js",
        PAGE,
        ResourceType::Script,
        false
    ));
}

#[test]
fn regex_filters_in_engine() {
    let e = engine("/^https?:\\/\\/[a-z]+\\.adnet\\.test\\/[0-9]+$/$script\n");
    assert!(blocked(
        &e,
        "https://foo.adnet.test/123",
        PAGE,
        ResourceType::Script,
        false
    ));
    assert!(!blocked(
        &e,
        "https://foo.adnet.test/123x",
        PAGE,
        ResourceType::Script,
        false
    ));
    assert!(!blocked(
        &e,
        "https://foo.adnet.test/123",
        PAGE,
        ResourceType::Image,
        false
    ));
}

#[test]
fn site_disable_list() {
    let mut e = engine("||ads.example^\n##.ad\n");
    e.set_site_disabled("News.Test", true);
    assert!(e.is_site_disabled("www.news.test"));
    assert!(!e.is_site_disabled("othernews.test"));
    assert_eq!(
        check(
            &e,
            "https://ads.example/x",
            PAGE,
            ResourceType::Image,
            false
        ),
        Verdict::SiteDisabled
    );
    assert!(e.cosmetic_selectors(PAGE).is_empty());
    assert_eq!(e.disabled_sites().collect::<Vec<_>>(), ["news.test"]);
    e.set_site_disabled("news.test", false);
    assert!(blocked(
        &e,
        "https://ads.example/x",
        PAGE,
        ResourceType::Image,
        false
    ));
    assert_eq!(e.cosmetic_selectors(PAGE).generic, [".ad"]);
}

#[test]
fn cosmetic_page_exceptions() {
    let e = engine(
        "##.ad\n\
         news.test##.promo\n\
         @@||news.test^$generichide\n\
         @@||quiet.test^$elemhide\n\
         @@||plain.test^$specifichide\n\
         plain.test##.x\n",
    );
    let s = e.cosmetic_selectors("https://news.test/");
    assert!(s.generic.is_empty());
    assert_eq!(s.specific, [".promo"]);
    assert!(e.cosmetic_selectors("https://quiet.test/").is_empty());
    let s = e.cosmetic_selectors("plain.test");
    assert_eq!(s.generic, [".ad"]);
    assert!(s.specific.is_empty());
    let s = e.cosmetic_selectors("https://else.test/");
    assert_eq!(s.generic, [".ad"]);
    // Page-level exceptions do not unblock requests.
    let e = engine("||ads.example^\n@@||news.test^$generichide\n");
    assert!(blocked(
        &e,
        "https://ads.example/",
        PAGE,
        ResourceType::Image,
        false
    ));
}

#[test]
fn list_parsing_stats() {
    let list = "\u{feff}[Adblock Plus 2.0]\n\
        ! Title: Test List\n\
        ! Expires: 2 days (update frequency)\n\
        ! Homepage: https://example.org/\n\
        !\n\
        ||ads.example^\n\
        @@||ads.example/ok^\n\
        ##.ad\n\
        example.com#@#.ad\n\
        ! Title: not metadata any more\n\
        # hosts-style comment\n\
        ||x.example^$popup\n\
        ||y.example^$redirect=noop.js\n\
        example.com##+js(abort)\n\
        ##div:has-text(Ad)\n\
        /unclosed(/\n\
        has space\n\
        !#if env_firefox\n\
        ||firefox-only.example^\n\
        !#else\n\
        ||not-firefox.example^\n\
        !#endif\n\
        !#if !ext_ubol\n\
        ||kept.example^\n\
        !#endif\n\
        ||z.example^$badfilter\n\
        !#include other.txt\n";
    let mut b = EngineBuilder::new();
    let r = b.add_list(list);
    assert_eq!(r.metadata.title.as_deref(), Some("Test List"));
    assert_eq!(r.metadata.expires, Some(Duration::from_secs(2 * 86_400)));
    assert_eq!(r.metadata.homepage.as_deref(), Some("https://example.org/"));
    let s = &r.stats;
    assert_eq!(s.network, 4, "{s:?}");
    assert_eq!(s.cosmetic, 2);
    assert_eq!(s.badfilters, 1);
    assert_eq!(s.unsupported_network, 2);
    assert_eq!(s.unsupported_cosmetic, 2);
    assert_eq!(s.invalid_regex, 1);
    assert_eq!(s.invalid, 1);
    assert_eq!(s.skipped_conditional, 1);
    assert_eq!(s.comments, 13);
    assert_eq!(s.dropped(), 6);
    let e = b.build();
    assert!(!blocked(
        &e,
        "https://firefox-only.example/",
        PAGE,
        ResourceType::Image,
        false
    ));
    assert!(blocked(
        &e,
        "https://not-firefox.example/",
        PAGE,
        ResourceType::Image,
        false
    ));
    assert!(blocked(
        &e,
        "https://kept.example/",
        PAGE,
        ResourceType::Image,
        false
    ));
    assert!(
        e.cosmetic_selectors("https://example.com/")
            .generic
            .is_empty()
    );
    assert_eq!(e.cosmetic_selectors("https://news.test/").generic, [".ad"]);
}

#[test]
fn empty_engine() {
    let e = Engine::default();
    assert_eq!(
        check(&e, "https://ads.example/", PAGE, ResourceType::Image, false),
        Verdict::NoMatch
    );
    assert!(e.cosmetic_selectors(PAGE).is_empty());
    assert_eq!(e.network_filter_count(), 0);
    assert_eq!(e.cosmetic_filter_count(), 0);
}

#[test]
fn hostile_input_does_not_panic() {
    let list = "||\n|\n@@\n$\n@@$\n||$script\n/\n//\n///$\n#@#\n##\n$$\n!#if\n!#else\n\
                !#endif\n!#endif\n|*|\n^\n^^^|\n*$domain=~\n||é.example^\n##é\n\
                ~##.x\n,##.x\n\u{0}\n||a^$domain=a.com|b.com|~c.com,1p,match-case";
    let (e, _) = Engine::from_list(list);
    for url in [
        "",
        "https://",
        "https://é.example/",
        "nonsense",
        "https://[::1/",
        "a://@",
    ] {
        for ty in [ResourceType::Script, ResourceType::Document] {
            let _ = check(&e, url, url, ty, false);
        }
        let _ = e.cosmetic_selectors(url);
    }
}

/// Builds a synthetic 100k-rule list shaped like real lists: host-anchored
/// rules, path rules, rules with options, a few token-less and regex rules.
fn synthetic_list(n: usize) -> String {
    let mut s = String::with_capacity(n * 40);
    for i in 0..n {
        let line = match i % 10 {
            0..=3 => format!("||ad{i}.adhost{}.test^\n", i % 97),
            4 => format!("/banner{i}/*$image\n"),
            5 => format!("-track{i}.js$script,third-party\n"),
            6 => format!("||cdn.test/ads/slot{i}^$domain=site{}.test\n", i % 1000),
            7 => format!("@@||ad{}.adhost{}.test/ok{i}^\n", i - 7, (i - 7) % 97),
            8 => format!("site{}.test##.promo-{i}\n", i % 1000),
            _ => {
                if i % 1000 == 9 {
                    format!("/^https:\\/\\/x{i}\\.test\\/[0-9]+$/\n")
                } else {
                    format!("&adparam{i}=\n")
                }
            }
        };
        s.push_str(&line);
    }
    s
}

#[test]
fn speed_100k_rules_10k_requests() {
    let list = synthetic_list(100_000);
    let t0 = Instant::now();
    let (e, report) = Engine::from_list(&list);
    let build = t0.elapsed();
    assert_eq!(report.stats.dropped(), 0, "{:?}", report.stats);
    assert_eq!(
        e.network_filter_count() + e.cosmetic_filter_count(),
        100_000
    );

    let mut requests = Vec::with_capacity(10_000);
    for i in 0..10_000usize {
        let base = (i * 7919) % 10_000;
        let source = format!("https://site{}.test/", i % 1000);
        let (url, source, ty) = match i % 5 {
            0 => {
                let k = base * 10 + i % 4;
                let url = format!("https://ad{k}.adhost{}.test/x.js", k % 97);
                (url, source, ResourceType::Script)
            }
            1 => {
                let k = base * 10 + 4;
                (
                    format!("https://img.test/banner{k}/a.png"),
                    source,
                    ResourceType::Image,
                )
            }
            2 => {
                let k = base * 10 + 6;
                let url = format!("https://cdn.test/ads/slot{k}?r={i}");
                (
                    url,
                    format!("https://site{}.test/", k % 1000),
                    ResourceType::Script,
                )
            }
            3 => {
                let k = base * 10 + 9;
                let url = format!("https://news.test/a?b=1&adparam{k}=2");
                (url, source, ResourceType::XmlHttpRequest)
            }
            _ => {
                let url = format!("https://clean{i}.test/static/app.{i}.js?v={i}");
                (url, source, ResourceType::Script)
            }
        };
        requests.push((url, source, ty));
    }
    let t1 = Instant::now();
    let mut blocked_count = 0;
    for (url, source, ty) in &requests {
        if e.check(&Request::new(url, source, *ty, false)).is_blocked() {
            blocked_count += 1;
        }
    }
    let matching = t1.elapsed();
    eprintln!("build: {build:?}, 10k checks: {matching:?}, blocked {blocked_count}");
    assert!(blocked_count > 7_000, "{blocked_count}");
    // Generous bounds so that unoptimized debug builds on a busy CI machine
    // pass; a linear scan of 100k rules per request would take minutes.
    assert!(matching < Duration::from_secs(10), "{matching:?}");
    assert!(build < Duration::from_secs(30), "{build:?}");
}
