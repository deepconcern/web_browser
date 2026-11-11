//! UI fonts: egui's bundled fonts are off (they are OFL/UFL licensed, not
//! on our allowlist), so the UI uses installed system fonts, found with
//! `fontdb` (the same discovery `wb-text` uses for pages).

use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};

/// Families tried for the proportional UI font, then as fallbacks.
const PROPORTIONAL: [&str; 8] = [
    "Cantarell",
    "Ubuntu",
    "Segoe UI",
    "Helvetica Neue",
    "Noto Sans",
    "DejaVu Sans",
    "Liberation Sans",
    "FreeSans",
];

/// Families tried for the monospace UI font.
const MONOSPACE: [&str; 5] = [
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "Liberation Mono",
    "Menlo",
    "Consolas",
];

/// Broad-coverage fallbacks appended to both families.
const FALLBACKS: [&str; 5] = [
    "Noto Sans CJK SC",
    "Noto Sans Symbols",
    "Noto Sans Symbols2",
    "DejaVu Sans",
    "Symbola",
];

/// Font definitions built from the installed fonts. Empty families when
/// nothing is installed (egui then draws no text, but doesn't crash).
#[must_use]
pub fn system_font_definitions() -> FontDefinitions {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let mut defs = FontDefinitions::empty();
    let mut load = |name: &str, family: fontdb::Family<'_>| -> Option<String> {
        let id = db.query(&fontdb::Query {
            families: &[family],
            ..fontdb::Query::default()
        })?;
        let key = format!("system:{name}");
        if defs.font_data.contains_key(&key) {
            return Some(key);
        }
        let data = db.with_face_data(id, |data, index| (data.to_vec(), index))?;
        let mut font = FontData::from_owned(data.0);
        font.index = data.1;
        defs.font_data.insert(key.clone(), Arc::new(font));
        Some(key)
    };
    let mut proportional: Vec<String> = Vec::new();
    for name in PROPORTIONAL {
        if let Some(k) = load(name, fontdb::Family::Name(name)) {
            proportional.push(k);
            break;
        }
    }
    if proportional.is_empty()
        && let Some(k) = load("sans-serif", fontdb::Family::SansSerif)
    {
        proportional.push(k);
    }
    let mut monospace: Vec<String> = Vec::new();
    for name in MONOSPACE {
        if let Some(k) = load(name, fontdb::Family::Name(name)) {
            monospace.push(k);
            break;
        }
    }
    if monospace.is_empty()
        && let Some(k) = load("monospace", fontdb::Family::Monospace)
    {
        monospace.push(k);
    }
    for name in FALLBACKS {
        if let Some(k) = load(name, fontdb::Family::Name(name)) {
            for list in [&mut proportional, &mut monospace] {
                if !list.contains(&k) {
                    list.push(k.clone());
                }
            }
        }
    }
    // Each family falls back to the other's fonts.
    let (p, m) = (proportional.clone(), monospace.clone());
    let m_before = m.clone();
    proportional.extend(m.into_iter().filter(|k| !p.contains(k)));
    monospace.extend(p.into_iter().filter(|k| !m_before.contains(k)));
    defs.families.insert(FontFamily::Proportional, proportional);
    defs.families.insert(FontFamily::Monospace, monospace);
    defs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_only_system_fonts() {
        let defs = system_font_definitions();
        assert!(defs.font_data.keys().all(|k| k.starts_with("system:")));
        let prop = defs.families.get(&FontFamily::Proportional).unwrap();
        if defs.font_data.is_empty() {
            assert!(prop.is_empty());
        } else {
            assert!(!prop.is_empty());
            assert!(prop.iter().all(|k| defs.font_data.contains_key(k)));
        }
    }
}
