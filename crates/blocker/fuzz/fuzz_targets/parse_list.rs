//! Fuzzes list parsing, engine building, request checks and cosmetic
//! queries: none of them may panic on any input.

#![no_main]

use libfuzzer_sys::fuzz_target;
use wb_blocker::{Engine, Request, ResourceType};

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let (engine, _report) = Engine::from_list(&text);
    let types = [
        ResourceType::Document,
        ResourceType::Script,
        ResourceType::Image,
        ResourceType::XmlHttpRequest,
    ];
    // Use the input's own lines as URLs too, so patterns meet text they
    // were derived from.
    let urls = text.lines().take(16).chain([
        "https://ads.example.com/banner/ad.js?x=1",
        "https://a.b.example/",
    ]);
    for (i, url) in urls.enumerate() {
        let ty = types[i % types.len()];
        for source in ["https://www.example.com/page", "example.com", "", url] {
            let _ = engine.check(&Request::new(url, source, ty, i % 2 == 0));
        }
        let _ = engine.cosmetic_selectors(url);
    }
    let _ = engine.cosmetic_selectors("https://www.example.com/");
});
