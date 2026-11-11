//! Fuzzes every public parser in wb-css. None of them may panic, overflow
//! the stack, or loop forever on any input.
#![no_main]

use libfuzzer_sys::fuzz_target;
use wb_css::{
    ComponentValue, CssRule, DeclarationKind, MediaContext, PropertyId, charset_from_bytes,
    parse_component_values, parse_media_query_list, parse_property_value,
    parse_selector_list, parse_style_attribute, parse_stylesheet, serialize,
    substitute_variables,
};

fuzz_target!(|data: &[u8]| {
    let _ = charset_from_bytes(data);
    let Ok(css) = std::str::from_utf8(data) else {
        return;
    };

    let sheet = parse_stylesheet(css);
    let ctx = MediaContext::default();
    for rule in &sheet.rules {
        if let CssRule::Media(m) = rule {
            let _ = m.queries.matches(&ctx);
        }
    }

    let block = parse_style_attribute(css);
    for decl in &block.declarations {
        if let DeclarationKind::WithVariables { property, value } = &decl.kind {
            let mut lookup = |_: &str| Some(parse_component_values("1px red"));
            if let Some(tokens) = substitute_variables(value, &mut lookup) {
                let _ = parse_property_value(*property, &tokens);
            }
        }
    }

    let _ = parse_selector_list(css);
    let _ = parse_media_query_list(css).matches(&ctx);

    // Serialization must not panic either.
    let values: Vec<ComponentValue> = parse_component_values(css);
    let _ = serialize(&values);

    // Treat the input as a value for a few properties.
    for name in ["margin", "background", "font", "color", "width", "flex", "content"] {
        if let Some(id) = PropertyId::from_name(name) {
            let _ = parse_property_value(id, &values);
        }
    }
});
