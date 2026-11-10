//! The font database: discovery, CSS font matching and per-character
//! fallback.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use unicode_script::Script;

use crate::matching::{FaceAttrs, best_match};
use crate::raster::GlyphCache;
use crate::style::{FontFamily, FontQuery, FontStretch, FontStyle, FontWeight, GenericFamily};

/// Raw font file bytes shared between faces of one file.
pub(crate) type FontBytes = Arc<dyn AsRef<[u8]> + Send + Sync>;

/// An opaque handle to one font face in a [`FontContext`].
///
/// Handles stay valid for the life of the context that issued them,
/// including after more fonts are loaded. Using a handle with another
/// context gives unspecified (but memory-safe, panic-free) results.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FontId(u32);

impl FontId {
    fn index(self) -> usize {
        self.0 as usize
    }
}

/// Descriptive information about a face, for debugging and devtools.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontFaceInfo {
    /// The face's family name (the English typographic family when the font
    /// has one).
    pub family: String,
    /// The face's PostScript name.
    pub postscript_name: String,
    /// The face's weight.
    pub weight: FontWeight,
    /// The face's style.
    pub style: FontStyle,
    /// The face's stretch.
    pub stretch: FontStretch,
    /// Whether the font says it is monospaced.
    pub monospaced: bool,
}

/// Errors from loading fonts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FontLoadError {
    /// The file could not be read.
    Io(String),
    /// The data contained no usable font face.
    NoFaces,
}

impl fmt::Display for FontLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "could not read font: {e}"),
            Self::NoFaces => f.write_str("no usable font face in data"),
        }
    }
}

impl std::error::Error for FontLoadError {}

#[derive(Clone)]
enum FaceSource {
    File(PathBuf),
    Memory(FontBytes),
}

pub(crate) struct Face {
    family: String,
    /// All family names (all languages), ASCII-lowercased for matching.
    names_lower: Vec<String>,
    postscript_name: String,
    source: FaceSource,
    index: u32,
    attrs: FaceAttrs,
    monospaced: bool,
}

struct Family {
    faces: Vec<FontId>,
}

/// Sorted, closed codepoint ranges covered by a face's cmap.
struct Coverage(Vec<(u32, u32)>);

impl Coverage {
    fn contains(&self, c: char) -> bool {
        let c = u32::from(c);
        match self.0.binary_search_by(|(start, _)| start.cmp(&c)) {
            Ok(_) => true,
            Err(0) => false,
            Err(i) => self.0.get(i - 1).is_some_and(|(_, end)| c <= *end),
        }
    }
}

type FallbackKey = (char, u16, FontStyle, u16);

/// The font system: every font the engine can use, CSS font matching,
/// per-character fallback, shaping, metrics and glyph rasterization.
///
/// It is cheap to share: all lookups take `&self` and use internal caches
/// guarded by mutexes, so one context can serve layout and paint (and
/// several threads). Loading fonts takes `&mut self`.
///
/// Fonts come only from the system (or directories and data the embedder
/// passes in); no fonts are bundled.
pub struct FontContext {
    faces: Vec<Face>,
    families: Vec<Family>,
    family_index: HashMap<String, usize>,
    generics: [Option<usize>; 6],
    fontconfig_aliases: [Option<String>; 6],
    default_family: GenericFamily,
    file_data: Mutex<HashMap<PathBuf, Option<FontBytes>>>,
    coverage: Mutex<HashMap<FontId, Option<Arc<Coverage>>>>,
    fallback: Mutex<HashMap<FallbackKey, Option<FontId>>>,
    pub(crate) glyphs: Mutex<GlyphCache>,
    /// Per-face shaping tables (built once per face).
    pub(crate) shapers: Mutex<HashMap<FontId, Arc<harfrust::ShaperData>>>,
}

impl fmt::Debug for FontContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FontContext")
            .field("faces", &self.faces.len())
            .field("families", &self.families.len())
            .field("default_family", &self.default_family)
            .finish_non_exhaustive()
    }
}

impl Default for FontContext {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn generic_slot(g: GenericFamily) -> usize {
    match g {
        GenericFamily::Serif => 0,
        GenericFamily::SansSerif => 1,
        GenericFamily::Monospace => 2,
        GenericFamily::Cursive => 3,
        GenericFamily::Fantasy => 4,
        GenericFamily::SystemUi => 5,
    }
}

/// Installed families tried for each generic family, in order, before
/// fontconfig's own preference. Metric-compatible families come first so
/// that pages measure the same way they do in other browsers.
fn generic_preferences(g: GenericFamily) -> &'static [&'static str] {
    match g {
        GenericFamily::Serif => &[
            "Times New Roman",
            "Liberation Serif",
            "Tinos",
            "DejaVu Serif",
            "Noto Serif",
            "FreeSerif",
        ],
        GenericFamily::SansSerif => &[
            "Arial",
            "Helvetica",
            "Liberation Sans",
            "Arimo",
            "DejaVu Sans",
            "Noto Sans",
            "FreeSans",
        ],
        GenericFamily::Monospace => &[
            "Courier New",
            "Liberation Mono",
            "Cousine",
            "DejaVu Sans Mono",
            "Noto Sans Mono",
            "FreeMono",
        ],
        GenericFamily::Cursive => &[
            "Comic Sans MS",
            "Comic Neue",
            "URW Chancery L",
            "Z003",
            "TeX Gyre Chorus",
        ],
        GenericFamily::Fantasy => &["Impact", "Papyrus", "Luminari"],
        GenericFamily::SystemUi => &[
            "Cantarell",
            "Ubuntu",
            "Segoe UI",
            "Noto Sans",
            "DejaVu Sans",
        ],
    }
}

/// Families with broad coverage, tried by system fallback after the
/// script-specific ones.
const BROAD_FALLBACK: &[&str] = &[
    "DejaVu Sans",
    "Noto Sans",
    "Noto Sans Symbols",
    "Noto Sans Symbols2",
    "FreeSans",
    "FreeSerif",
    "Symbola",
    "Noto Emoji",
    "Noto Color Emoji",
    "OpenSymbol",
    "Unifont",
    "Unifont Upper",
];

/// Script-specific families for system fallback (CSS Fonts 4 §5.2 step 7
/// leaves the choice to the UA).
fn script_preferences(script: Script) -> &'static [&'static str] {
    match script {
        Script::Han | Script::Hiragana | Script::Katakana | Script::Bopomofo => &[
            "Noto Sans CJK SC",
            "Noto Sans CJK JP",
            "Noto Sans CJK TC",
            "Source Han Sans",
            "WenQuanYi Zen Hei",
            "WenQuanYi Micro Hei",
            "IPAGothic",
            "IPAPGothic",
            "Droid Sans Fallback",
        ],
        Script::Hangul => &[
            "Noto Sans CJK KR",
            "NanumGothic",
            "UnDotum",
            "WenQuanYi Zen Hei",
        ],
        Script::Arabic => &["Noto Naskh Arabic", "Noto Sans Arabic", "DejaVu Sans"],
        Script::Hebrew => &["Noto Sans Hebrew", "DejaVu Sans"],
        Script::Thai => &["Noto Sans Thai", "Loma", "Garuda", "Norasi"],
        Script::Devanagari => &["Noto Sans Devanagari", "Lohit Devanagari"],
        _ => &[],
    }
}

/// Emoji presentation ranges that should prefer emoji fonts.
fn is_emoji_like(c: char) -> bool {
    matches!(u32::from(c), 0x1F300..=0x1FAFF | 0x2600..=0x27BF | 0x1F000..=0x1F2FF)
}

const EMOJI_FALLBACK: &[&str] = &["Noto Emoji", "Symbola", "Noto Color Emoji"];

impl FontContext {
    /// Creates a context with no fonts. Text shaped with it has no glyphs.
    pub fn new() -> Self {
        Self {
            faces: Vec::new(),
            families: Vec::new(),
            family_index: HashMap::new(),
            generics: [None; 6],
            fontconfig_aliases: Default::default(),
            default_family: GenericFamily::Serif,
            file_data: Mutex::new(HashMap::new()),
            coverage: Mutex::new(HashMap::new()),
            fallback: Mutex::new(HashMap::new()),
            glyphs: Mutex::new(GlyphCache::new()),
            shapers: Mutex::new(HashMap::new()),
        }
    }

    /// Creates a context with every installed system font.
    ///
    /// On Linux this reads the fontconfig configuration for font
    /// directories and generic-family aliases, then scans those directories
    /// (no fontconfig library is linked). Faces are ordered by family name
    /// and attributes, not by directory order, so matching is reproducible.
    pub fn with_system_fonts() -> Self {
        let mut ctx = Self::new();
        ctx.load_system_fonts();
        ctx
    }

    /// Adds every installed system font. See [`FontContext::with_system_fonts`].
    pub fn load_system_fonts(&mut self) {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let aliases = [
            (GenericFamily::Serif, fontdb::Family::Serif),
            (GenericFamily::SansSerif, fontdb::Family::SansSerif),
            (GenericFamily::Monospace, fontdb::Family::Monospace),
            (GenericFamily::Cursive, fontdb::Family::Cursive),
            (GenericFamily::Fantasy, fontdb::Family::Fantasy),
        ];
        for (generic, family) in aliases {
            if let Some(slot) = self.fontconfig_aliases.get_mut(generic_slot(generic)) {
                *slot = Some(db.family_name(&family).to_owned());
            }
        }
        self.absorb(&db);
    }

    /// Adds every font file found (recursively) in `dir`.
    pub fn load_fonts_dir(&mut self, dir: &Path) {
        let mut db = fontdb::Database::new();
        db.load_fonts_dir(dir);
        self.absorb(&db);
    }

    /// Adds the faces of one font file. Returns how many faces were added.
    pub fn load_font_file(&mut self, path: &Path) -> Result<usize, FontLoadError> {
        let mut db = fontdb::Database::new();
        db.load_font_file(path)
            .map_err(|e| FontLoadError::Io(e.to_string()))?;
        let added = self.absorb(&db);
        if added == 0 {
            return Err(FontLoadError::NoFaces);
        }
        Ok(added)
    }

    /// Adds the faces of a font (or font collection) held in memory, for
    /// example a downloaded web font. Returns how many faces were added.
    pub fn load_font_data(&mut self, data: Vec<u8>) -> Result<usize, FontLoadError> {
        let mut db = fontdb::Database::new();
        let bytes: FontBytes = Arc::new(data);
        db.load_font_source(fontdb::Source::Binary(bytes));
        let added = self.absorb(&db);
        if added == 0 {
            return Err(FontLoadError::NoFaces);
        }
        Ok(added)
    }

    /// Sets the family used when nothing in a `font-family` list matches
    /// (the "user agent default font"). The default is `serif`, as in other
    /// browsers.
    pub fn set_default_family(&mut self, family: GenericFamily) {
        self.default_family = family;
        lock(&self.fallback).clear();
    }

    /// The number of faces in the context.
    pub fn face_count(&self) -> usize {
        self.faces.len()
    }

    /// Whether the context has no fonts at all.
    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    /// Information about a face.
    pub fn face_info(&self, font: FontId) -> Option<FontFaceInfo> {
        let face = self.faces.get(font.index())?;
        Some(FontFaceInfo {
            family: face.family.clone(),
            postscript_name: face.postscript_name.clone(),
            weight: face.attrs.weight,
            style: face.attrs.style,
            stretch: face.attrs.stretch,
            monospaced: face.monospaced,
        })
    }

    /// The installed family name a generic family maps to, if any.
    pub fn generic_family_name(&self, generic: GenericFamily) -> Option<&str> {
        let family = (*self.generics.get(generic_slot(generic))?)?;
        let first = *self.families.get(family)?.faces.first()?;
        Some(self.faces.get(first.index())?.family.as_str())
    }

    /// Whether the face has a glyph for `c`.
    pub fn has_glyph(&self, font: FontId, c: char) -> bool {
        self.coverage_of(font).is_some_and(|cov| cov.contains(c))
    }

    /// Selects the face for each family of the query (CSS Fonts 4 §5.2
    /// steps 3–4), in family-list order, followed by the default family.
    /// Families that are not installed are skipped.
    pub fn match_families(&self, query: &FontQuery) -> Vec<FontId> {
        let want = attrs_of(query);
        let mut out: Vec<FontId> = Vec::new();
        let default = FontFamily::Generic(self.default_family);
        for family in query.families.iter().chain(std::iter::once(&default)) {
            let index = match family {
                FontFamily::Named(name) => {
                    self.family_index.get(&name.to_ascii_lowercase()).copied()
                }
                FontFamily::Generic(g) => self.generics.get(generic_slot(*g)).copied().flatten(),
            };
            if let Some(face) = index.and_then(|i| self.best_in_family(i, want))
                && !out.contains(&face)
            {
                out.push(face);
            }
        }
        out
    }

    /// The *first available font* for a query (CSS Fonts 4 §5.2): the
    /// first matched face that has a glyph for U+0020 SPACE. Its metrics
    /// give `line-height: normal`, the strut and `ex`/`ch` units.
    ///
    /// Returns `None` only when the context has no fonts.
    pub fn first_available_font(&self, query: &FontQuery) -> Option<FontId> {
        let list = self.match_families(query);
        list.iter()
            .copied()
            .find(|f| self.has_glyph(*f, ' '))
            .or_else(|| list.first().copied())
            .or_else(|| self.any_face())
    }

    pub(crate) fn face_attrs(&self, font: FontId) -> Option<FaceAttrs> {
        self.faces.get(font.index()).map(|f| f.attrs)
    }

    fn any_face(&self) -> Option<FontId> {
        self.families.iter().find_map(|f| f.faces.first().copied())
    }

    fn best_in_family(&self, family: usize, want: FaceAttrs) -> Option<FontId> {
        let fam = self.families.get(family)?;
        let attrs: Vec<FaceAttrs> = fam
            .faces
            .iter()
            .filter_map(|f| self.faces.get(f.index()).map(|face| face.attrs))
            .collect();
        let i = best_match(&attrs, want)?;
        fam.faces.get(i).copied()
    }

    fn family_by_name(&self, name: &str) -> Option<usize> {
        self.family_index.get(&name.to_ascii_lowercase()).copied()
    }

    /// Chooses the face for one character (CSS Fonts 4 §5.2 steps 5–7):
    /// the first face in `list` that supports it, else a system fallback
    /// face, else the first face in `list` (which draws `.notdef`).
    pub(crate) fn font_for_char(
        &self,
        list: &[FontId],
        c: char,
        want: FaceAttrs,
        script: Script,
    ) -> Option<FontId> {
        if let Some(f) = list.iter().copied().find(|f| self.has_glyph(*f, c)) {
            return Some(f);
        }
        self.system_fallback(c, want, script)
            .or_else(|| list.first().copied())
            .or_else(|| self.any_face())
    }

    fn system_fallback(&self, c: char, want: FaceAttrs, script: Script) -> Option<FontId> {
        let key = (c, want.weight.get(), want.style, want.stretch.tenths());
        if let Some(hit) = lock(&self.fallback).get(&key) {
            return *hit;
        }
        let found = self.search_fallback(c, want, script);
        lock(&self.fallback).insert(key, found);
        found
    }

    fn search_fallback(&self, c: char, want: FaceAttrs, script: Script) -> Option<FontId> {
        let mut tried: Vec<usize> = Vec::new();
        let emoji: &[&str] = if is_emoji_like(c) {
            EMOJI_FALLBACK
        } else {
            &[]
        };
        // "Noto Sans <Script>" is the usual name for per-script fonts.
        let noto = format!("Noto Sans {}", script.full_name().replace('_', " "));
        let named = emoji
            .iter()
            .copied()
            .chain(script_preferences(script).iter().copied())
            .chain(std::iter::once(noto.as_str()))
            .chain(BROAD_FALLBACK.iter().copied())
            .filter_map(|n| self.family_by_name(n));
        for family in named.chain(0..self.families.len()) {
            if tried.contains(&family) {
                continue;
            }
            tried.push(family);
            if let Some(face) = self.best_in_family(family, want)
                && self.has_glyph(face, c)
            {
                return Some(face);
            }
            // Other faces of the family may cover more (e.g. a symbols
            // face in a large family); try them in order.
            if let Some(fam) = self.families.get(family)
                && let Some(face) = fam.faces.iter().copied().find(|f| self.has_glyph(*f, c))
            {
                return Some(face);
            }
        }
        None
    }

    /// Returns the bytes and collection index of a face, loading the file
    /// on first use.
    pub(crate) fn face_data(&self, font: FontId) -> Option<(FontBytes, u32)> {
        let face = self.faces.get(font.index())?;
        match &face.source {
            FaceSource::Memory(bytes) => Some((bytes.clone(), face.index)),
            FaceSource::File(path) => {
                if let Some(cached) = lock(&self.file_data).get(path) {
                    return cached.clone().map(|b| (b, face.index));
                }
                let loaded: Option<FontBytes> =
                    std::fs::read(path).ok().map(|v| Arc::new(v) as FontBytes);
                lock(&self.file_data).insert(path.clone(), loaded.clone());
                loaded.map(|b| (b, face.index))
            }
        }
    }

    fn coverage_of(&self, font: FontId) -> Option<Arc<Coverage>> {
        if let Some(c) = lock(&self.coverage).get(&font) {
            return c.clone();
        }
        let computed = self.compute_coverage(font).map(Arc::new);
        lock(&self.coverage).insert(font, computed.clone());
        computed
    }

    fn compute_coverage(&self, font: FontId) -> Option<Coverage> {
        let face = self.faces.get(font.index())?;
        // Avoid keeping whole files in memory just to read their cmap:
        // use cached bytes when present, else read and drop.
        let bytes: FontBytes = match &face.source {
            FaceSource::Memory(b) => b.clone(),
            FaceSource::File(path) => {
                let cached = lock(&self.file_data).get(path).cloned().flatten();
                match cached {
                    Some(b) => b,
                    None => Arc::new(std::fs::read(path).ok()?),
                }
            }
        };
        let data: &[u8] = (*bytes).as_ref();
        let font_ref = swash::FontRef::from_index(data, face.index as usize)?;
        let mut points: Vec<u32> = Vec::new();
        font_ref.charmap().enumerate(|cp, glyph| {
            if glyph != 0 {
                points.push(cp);
            }
        });
        points.sort_unstable();
        points.dedup();
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        for cp in points {
            match ranges.last_mut() {
                Some((_, end)) if end.checked_add(1) == Some(cp) => *end = cp,
                _ => ranges.push((cp, cp)),
            }
        }
        Some(Coverage(ranges))
    }

    /// Takes the faces of a freshly loaded fontdb database, in a
    /// deterministic order, and rebuilds the family tables. Returns how
    /// many faces were added.
    fn absorb(&mut self, db: &fontdb::Database) -> usize {
        let mut new_faces: Vec<Face> = db
            .faces()
            .filter_map(|info| {
                let family = info.families.first()?.0.clone();
                let source = match &info.source {
                    fontdb::Source::File(p) => FaceSource::File(p.clone()),
                    fontdb::Source::Binary(b) => FaceSource::Memory(b.clone()),
                };
                Some(Face {
                    names_lower: info
                        .families
                        .iter()
                        .map(|(n, _)| n.to_ascii_lowercase())
                        .collect(),
                    family,
                    postscript_name: info.post_script_name.clone(),
                    source,
                    index: info.index,
                    attrs: FaceAttrs {
                        weight: FontWeight::new(info.weight.0),
                        style: match info.style {
                            fontdb::Style::Normal => FontStyle::Normal,
                            fontdb::Style::Italic => FontStyle::Italic,
                            fontdb::Style::Oblique => FontStyle::Oblique,
                        },
                        stretch: FontStretch::from_tenths(stretch_tenths(info.stretch)),
                    },
                    monospaced: info.monospaced,
                })
            })
            .collect();
        new_faces.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
        let added = new_faces.len();
        self.faces.extend(new_faces);
        self.rebuild();
        added
    }

    fn rebuild(&mut self) {
        let mut order: Vec<usize> = (0..self.faces.len()).collect();
        order.sort_by(|a, b| {
            let fa = self.faces.get(*a).map(sort_key);
            let fb = self.faces.get(*b).map(sort_key);
            fa.cmp(&fb).then(a.cmp(b))
        });
        self.families.clear();
        self.family_index.clear();
        let mut primary: HashMap<String, usize> = HashMap::new();
        for i in order {
            let Some(face) = self.faces.get(i) else {
                continue;
            };
            let Ok(raw) = u32::try_from(i) else { continue };
            let id = FontId(raw);
            let key = face.family.to_ascii_lowercase();
            let fam = match primary.get(&key) {
                Some(f) => *f,
                None => {
                    self.families.push(Family { faces: Vec::new() });
                    let f = self.families.len() - 1;
                    primary.insert(key, f);
                    f
                }
            };
            if let Some(family) = self.families.get_mut(fam) {
                family.faces.push(id);
            }
            for name in &face.names_lower {
                self.family_index.entry(name.clone()).or_insert(fam);
            }
        }
        // Primary names win over localized aliases.
        for (name, fam) in primary {
            self.family_index.insert(name, fam);
        }
        let generics = [
            GenericFamily::Serif,
            GenericFamily::SansSerif,
            GenericFamily::Monospace,
            GenericFamily::Cursive,
            GenericFamily::Fantasy,
            GenericFamily::SystemUi,
        ];
        for g in generics {
            let resolved = self.resolve_generic(g, 0);
            if let Some(slot) = self.generics.get_mut(generic_slot(g)) {
                *slot = resolved;
            }
        }
        lock(&self.fallback).clear();
        lock(&self.coverage).clear();
        lock(&self.glyphs).clear();
    }

    fn resolve_generic(&self, g: GenericFamily, depth: u8) -> Option<usize> {
        let preferred = generic_preferences(g)
            .iter()
            .find_map(|n| self.family_by_name(n));
        if preferred.is_some() {
            return preferred;
        }
        let alias = self
            .fontconfig_aliases
            .get(generic_slot(g))
            .and_then(|a| a.as_deref())
            .and_then(|n| self.family_by_name(n));
        if alias.is_some() {
            return alias;
        }
        if depth > 2 {
            return (!self.families.is_empty()).then_some(0);
        }
        match g {
            GenericFamily::Monospace => self
                .families
                .iter()
                .position(|f| {
                    f.faces
                        .iter()
                        .any(|id| self.faces.get(id.index()).is_some_and(|x| x.monospaced))
                })
                .or_else(|| self.resolve_generic(GenericFamily::SansSerif, depth + 1)),
            GenericFamily::Cursive => self.resolve_generic(GenericFamily::Serif, depth + 1),
            GenericFamily::Serif => self.resolve_generic(GenericFamily::SansSerif, depth + 1),
            GenericFamily::Fantasy | GenericFamily::SystemUi | GenericFamily::SansSerif => {
                if g == GenericFamily::SansSerif {
                    (!self.families.is_empty()).then_some(0)
                } else {
                    self.resolve_generic(GenericFamily::SansSerif, depth + 1)
                }
            }
        }
    }
}

pub(crate) fn attrs_of(query: &FontQuery) -> FaceAttrs {
    FaceAttrs {
        weight: query.weight,
        style: query.style,
        stretch: query.stretch,
    }
}

fn stretch_tenths(s: fontdb::Stretch) -> u16 {
    match s {
        fontdb::Stretch::UltraCondensed => 500,
        fontdb::Stretch::ExtraCondensed => 625,
        fontdb::Stretch::Condensed => 750,
        fontdb::Stretch::SemiCondensed => 875,
        fontdb::Stretch::Normal => 1000,
        fontdb::Stretch::SemiExpanded => 1125,
        fontdb::Stretch::Expanded => 1250,
        fontdb::Stretch::ExtraExpanded => 1500,
        fontdb::Stretch::UltraExpanded => 2000,
    }
}

fn style_rank(s: FontStyle) -> u8 {
    match s {
        FontStyle::Normal => 0,
        FontStyle::Italic => 1,
        FontStyle::Oblique => 2,
    }
}

type SortKey<'a> = (String, u16, u16, u8, &'a str, Option<&'a Path>, u32);

fn sort_key(f: &Face) -> SortKey<'_> {
    let path = match &f.source {
        FaceSource::File(p) => Some(p.as_path()),
        FaceSource::Memory(_) => None,
    };
    (
        f.family.to_ascii_lowercase(),
        f.attrs.stretch.tenths(),
        f.attrs.weight.get(),
        style_rank(f.attrs.style),
        f.postscript_name.as_str(),
        path,
        f.index,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_lookup() {
        let c = Coverage(vec![(0x20, 0x7e), (0x100, 0x100), (0x4e00, 0x9fff)]);
        assert!(c.contains(' '));
        assert!(c.contains('~'));
        assert!(!c.contains('\u{7f}'));
        assert!(c.contains('\u{100}'));
        assert!(!c.contains('\u{101}'));
        assert!(c.contains('中'));
        assert!(!c.contains('\u{1f}'));
        assert!(!Coverage(Vec::new()).contains('a'));
    }

    #[test]
    fn empty_context() {
        let ctx = FontContext::new();
        assert!(ctx.is_empty());
        assert_eq!(ctx.first_available_font(&FontQuery::default()), None);
        assert!(ctx.match_families(&FontQuery::default()).is_empty());
        assert_eq!(ctx.generic_family_name(GenericFamily::Serif), None);
        assert!(!ctx.has_glyph(FontId(0), 'a'));
        assert!(ctx.face_info(FontId(3)).is_none());
    }

    #[test]
    fn bad_font_data_is_rejected() {
        let mut ctx = FontContext::new();
        assert_eq!(
            ctx.load_font_data(b"not a font".to_vec()),
            Err(FontLoadError::NoFaces)
        );
        assert!(
            ctx.load_font_file(Path::new("/nonexistent/font.ttf"))
                .is_err()
        );
    }

    #[test]
    fn context_is_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<FontContext>();
    }
}

#[cfg(test)]
pub(crate) mod tests_support {
    use super::FontId;

    pub(crate) fn font_id(i: u32) -> FontId {
        FontId(i)
    }
}
