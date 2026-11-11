//! Form controls and form submission (HTML §4.10).
//!
//! M1 keeps control state in the DOM, where layout reads it: a text
//! field's current value is its `value` attribute, checkedness is the
//! `checked` attribute, option selectedness is `selected`, and a
//! textarea's value is its text content. [`FormState`] remembers the
//! original values so `reset` can restore them.
//!
//! Submission follows HTML §4.10.21.3 "form submission algorithm":
//! [`form_data_set`] builds the entry list (§4.10.21.4) and
//! [`submission`] encodes it as `application/x-www-form-urlencoded`
//! (URL §5.2), `multipart/form-data` (§4.10.21.8) or `text/plain`
//! (§4.10.21.9) and turns it into a navigation.

use std::collections::HashMap;
use std::fmt::Write as _;

use wb_dom::{Document, NodeId};
use wb_ipc::{NavigationMethod, NavigationRequest};
use wb_url::Url;

/// The most entries a form submission may carry; more are dropped.
const MAX_ENTRIES: usize = 10_000;

/// URL §5.2 `application/x-www-form-urlencoded` byte serializer of a
/// string (encoded as UTF-8): ASCII alphanumerics and `*-._` stay, space
/// becomes `+`, every other byte is percent-encoded.
#[must_use]
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                out.push(char::from(b));
            }
            b' ' => out.push('+'),
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// One entry of a form's entry list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The control's name.
    pub name: String,
    /// The value.
    pub value: EntryValue,
}

/// An entry's value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryValue {
    /// A string value.
    Text(String),
    /// A file (M1 has no file picker: always an empty, unnamed file).
    File {
        /// The file name.
        filename: String,
    },
}

fn is_html(doc: &Document, n: NodeId, name: &str) -> bool {
    doc.is_html_element(n, name)
}

fn attr<'d>(doc: &'d Document, n: NodeId, name: &str) -> Option<&'d str> {
    doc.attribute(n, name)
}

/// The lowercased `type` of an `<input>`, `"text"` for missing or unknown
/// types (HTML §4.10.5 "the type attribute ... invalid value default").
#[must_use]
pub fn input_type(doc: &Document, n: NodeId) -> String {
    const KNOWN: [&str; 22] = [
        "hidden",
        "text",
        "search",
        "tel",
        "url",
        "email",
        "password",
        "date",
        "month",
        "week",
        "time",
        "datetime-local",
        "number",
        "range",
        "color",
        "checkbox",
        "radio",
        "file",
        "submit",
        "image",
        "reset",
        "button",
    ];
    let t = attr(doc, n, "type").unwrap_or("text").to_ascii_lowercase();
    if KNOWN.contains(&t.as_str()) {
        t
    } else {
        "text".to_string()
    }
}

/// Whether `n` is a submittable element (HTML §4.10.2: `button`, `input`,
/// `select`, `textarea`; `object` is never submitted in M1).
fn is_submittable(doc: &Document, n: NodeId) -> bool {
    ["button", "input", "select", "textarea"]
        .iter()
        .any(|t| is_html(doc, n, t))
}

/// The form owner of a form-associated element (HTML §4.10.17.3): the
/// form named by its `form` attribute, else its nearest `<form>`
/// ancestor.
#[must_use]
pub fn form_owner(doc: &Document, n: NodeId) -> Option<NodeId> {
    if let Some(id) = attr(doc, n, "form") {
        return doc.element_by_id(id).filter(|&f| is_html(doc, f, "form"));
    }
    doc.ancestors(n).find(|&a| is_html(doc, a, "form"))
}

/// Whether the control is disabled (HTML §4.10.18.5): its own `disabled`
/// attribute, or a disabled `<fieldset>` ancestor unless the control is
/// inside that fieldset's first `<legend>`.
#[must_use]
pub fn is_disabled(doc: &Document, n: NodeId) -> bool {
    if attr(doc, n, "disabled").is_some() {
        return true;
    }
    let mut child = n;
    for a in doc.ancestors(n) {
        if is_html(doc, a, "fieldset") && attr(doc, a, "disabled").is_some() {
            let first_legend = doc.element_children(a).find(|&c| is_html(doc, c, "legend"));
            if first_legend != Some(child) {
                return true;
            }
        }
        child = a;
    }
    false
}

/// Whether the button-like control `n` is a submit button (HTML §4.10.6,
/// §4.10.5.1.18/19).
#[must_use]
pub fn is_submit_button(doc: &Document, n: NodeId) -> bool {
    if is_html(doc, n, "button") {
        let t = attr(doc, n, "type").map(str::to_ascii_lowercase);
        return !matches!(t.as_deref(), Some("reset" | "button"));
    }
    is_html(doc, n, "input") && matches!(input_type(doc, n).as_str(), "submit" | "image")
}

/// An `<option>`'s value: its `value` attribute, else its text with ASCII
/// whitespace stripped and collapsed (HTML §4.10.10).
#[must_use]
pub fn option_value(doc: &Document, option: NodeId) -> String {
    if let Some(v) = attr(doc, option, "value") {
        return v.to_string();
    }
    option_label(doc, option)
}

/// An `<option>`'s label: its `label` attribute or its collapsed text.
#[must_use]
pub fn option_label(doc: &Document, option: NodeId) -> String {
    if let Some(l) = attr(doc, option, "label").filter(|l| !l.is_empty()) {
        return l.to_string();
    }
    doc.text_content(option)
        .unwrap_or_default()
        .split(|c: char| c.is_ascii_whitespace())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The options of a `<select>` in tree order (HTML §4.10.7 "list of
/// options": children and `<optgroup>` children).
#[must_use]
pub fn select_options(doc: &Document, select: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    for c in doc.element_children(select) {
        if is_html(doc, c, "option") {
            out.push(c);
        } else if is_html(doc, c, "optgroup") {
            out.extend(
                doc.element_children(c)
                    .filter(|&o| is_html(doc, o, "option")),
            );
        }
    }
    out
}

/// Whether an option is disabled (its own attribute or its optgroup's).
fn option_disabled(doc: &Document, option: NodeId) -> bool {
    attr(doc, option, "disabled").is_some()
        || doc
            .parent(option)
            .is_some_and(|p| is_html(doc, p, "optgroup") && attr(doc, p, "disabled").is_some())
}

/// The selected options of a `<select>`, applying the selectedness
/// setting algorithm (HTML §4.10.7): a single-select drop-down with none
/// selected selects its first enabled option; with several selected, the
/// last wins.
#[must_use]
pub fn selected_options(doc: &Document, select: NodeId) -> Vec<NodeId> {
    let options = select_options(doc, select);
    let selected: Vec<NodeId> = options
        .iter()
        .copied()
        .filter(|&o| attr(doc, o, "selected").is_some())
        .collect();
    if attr(doc, select, "multiple").is_some() {
        return selected;
    }
    if let Some(&last) = selected.last() {
        return vec![last];
    }
    let size = attr(doc, select, "size")
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(1);
    if size > 1 {
        return Vec::new();
    }
    options
        .into_iter()
        .find(|&o| !option_disabled(doc, o))
        .into_iter()
        .collect()
}

/// A text control's value: the `value` attribute (with line breaks
/// stripped, the value sanitization of text-like types), or a textarea's
/// text content.
#[must_use]
pub fn text_value(doc: &Document, n: NodeId) -> String {
    if is_html(doc, n, "textarea") {
        return doc.text_content(n).unwrap_or_default();
    }
    attr(doc, n, "value")
        .unwrap_or("")
        .chars()
        .filter(|&c| c != '\n' && c != '\r')
        .collect()
}

/// Whether `n` is a control the user types into.
#[must_use]
pub fn is_text_control(doc: &Document, n: NodeId) -> bool {
    if is_html(doc, n, "textarea") {
        return true;
    }
    is_html(doc, n, "input")
        && matches!(
            input_type(doc, n).as_str(),
            "text"
                | "search"
                | "tel"
                | "url"
                | "email"
                | "password"
                | "number"
                | "date"
                | "month"
                | "week"
                | "time"
                | "datetime-local"
        )
}

/// HTML §4.10.21.4 "constructing the entry list" for `form`, submitted by
/// `submitter` (a submit button, or `None` for implicit submission).
#[must_use]
pub fn form_data_set(doc: &Document, form: NodeId, submitter: Option<NodeId>) -> Vec<Entry> {
    let mut out = Vec::new();
    let text = |name: &str, value: String| Entry {
        name: name.to_string(),
        value: EntryValue::Text(value),
    };
    for n in doc.descendants(doc.root()) {
        if out.len() >= MAX_ENTRIES {
            break;
        }
        if !is_submittable(doc, n) || form_owner(doc, n) != Some(form) {
            continue;
        }
        // Step 5.1: skip controls in a datalist, disabled controls, buttons
        // other than the submitter, unchecked checkboxes and radios, and
        // image buttons other than the submitter.
        if doc.ancestors(n).any(|a| is_html(doc, a, "datalist")) || is_disabled(doc, n) {
            continue;
        }
        let ty = if is_html(doc, n, "input") {
            input_type(doc, n)
        } else {
            String::new()
        };
        let is_button = is_html(doc, n, "button")
            || matches!(ty.as_str(), "submit" | "reset" | "button" | "image");
        if is_button && Some(n) != submitter {
            continue;
        }
        if matches!(ty.as_str(), "checkbox" | "radio") && attr(doc, n, "checked").is_none() {
            continue;
        }
        let name = attr(doc, n, "name").unwrap_or("");
        // Step 5.2: the image button submitter adds its click coordinates.
        if ty == "image" {
            let prefix = if name.is_empty() {
                String::new()
            } else {
                format!("{name}.")
            };
            // M1 follow-up: the actual click coordinates.
            out.push(text(&format!("{prefix}x"), "0".into()));
            out.push(text(&format!("{prefix}y"), "0".into()));
            continue;
        }
        if name.is_empty() {
            continue;
        }
        if is_html(doc, n, "select") {
            for o in selected_options(doc, n) {
                if !option_disabled(doc, o) {
                    out.push(text(name, option_value(doc, o)));
                }
            }
        } else if matches!(ty.as_str(), "checkbox" | "radio") {
            out.push(text(
                name,
                attr(doc, n, "value").unwrap_or("on").to_string(),
            ));
        } else if ty == "file" {
            out.push(Entry {
                name: name.to_string(),
                value: EntryValue::File {
                    filename: String::new(),
                },
            });
        } else if ty == "hidden" && name.eq_ignore_ascii_case("_charset_") {
            out.push(text(name, "UTF-8".into()));
        } else if is_html(doc, n, "textarea") || is_text_control(doc, n) || ty == "hidden" {
            out.push(text(name, text_value(doc, n)));
        } else {
            // Buttons (the submitter) and range/color controls.
            out.push(text(name, attr(doc, n, "value").unwrap_or("").to_string()));
        }
        // Step 5.12: `dirname` adds the text direction.
        if (is_html(doc, n, "textarea") || matches!(ty.as_str(), "text" | "search"))
            && let Some(d) = attr(doc, n, "dirname").filter(|d| !d.is_empty())
        {
            out.push(text(d, "ltr".into()));
        }
    }
    out
}

/// Normalizes line breaks to CRLF ("convert to a list of name-value
/// pairs", HTML §4.10.21.4).
fn crlf(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("\r\n");
            }
            '\n' => out.push_str("\r\n"),
            c => out.push(c),
        }
    }
    out
}

fn pairs(entries: &[Entry]) -> Vec<(String, String)> {
    entries
        .iter()
        .map(|e| {
            let v = match &e.value {
                EntryValue::Text(t) => crlf(t),
                EntryValue::File { filename } => filename.clone(),
            };
            (crlf(&e.name), v)
        })
        .collect()
}

/// The `application/x-www-form-urlencoded` serialization (URL §5.2).
#[must_use]
pub fn encode_urlencoded(entries: &[Entry]) -> String {
    pairs(entries)
        .iter()
        .map(|(n, v)| format!("{}={}", urlencode(n), urlencode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// The `text/plain` encoding (HTML §4.10.21.9).
#[must_use]
pub fn encode_text_plain(entries: &[Entry]) -> String {
    pairs(entries)
        .iter()
        .map(|(n, v)| format!("{n}={v}\r\n"))
        .collect()
}

/// The `multipart/form-data` encoding (HTML §4.10.21.8, RFC 7578) with
/// `boundary`. Returns `None` if the boundary occurs in the content (pick
/// another one).
#[must_use]
pub fn encode_multipart(entries: &[Entry], boundary: &str) -> Option<Vec<u8>> {
    // Names and filenames escape LF, CR and `"` as %0A, %0D and %22.
    let escape = |s: &str| {
        s.replace('\n', "%0A")
            .replace('\r', "%0D")
            .replace('"', "%22")
    };
    let mut out = String::new();
    for e in entries {
        let name = escape(&crlf(&e.name));
        let _ = write!(
            out,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\""
        );
        match &e.value {
            EntryValue::Text(t) => {
                let _ = write!(out, "\r\n\r\n{}\r\n", crlf(t));
            }
            EntryValue::File { filename } => {
                let _ = write!(
                    out,
                    "; filename=\"{}\"\r\nContent-Type: application/octet-stream\r\n\r\n\r\n",
                    escape(filename)
                );
            }
        }
    }
    let _ = write!(out, "--{boundary}--\r\n");
    let delimiter = format!("--{boundary}");
    if out.matches(&delimiter).count() != entries.len() + 1 {
        return None;
    }
    Some(out.into_bytes())
}

/// HTML §4.10.21.3 "form submission algorithm" (steps for the action,
/// method and encoding type): the navigation submitting `form` with
/// `submitter` makes, or `None` when nothing is submitted (method
/// `dialog`, an unparsable action, or an action scheme M1 doesn't submit
/// to, such as `javascript:` or `mailto:`).
#[must_use]
pub fn submission(
    doc: &Document,
    form: NodeId,
    submitter: Option<NodeId>,
    doc_url: &Url,
    base_url: &Url,
) -> Option<NavigationRequest> {
    let from_submitter = |a: &str| {
        submitter
            .filter(|&s| is_submit_button(doc, s))
            .and_then(|s| attr(doc, s, a))
    };
    let action = from_submitter("formaction").or_else(|| attr(doc, form, "action"));
    let action = match action.map(|a| a.trim_matches(|c: char| c.is_ascii_whitespace())) {
        None | Some("") => doc_url.clone(),
        Some(a) => base_url.join(a).ok()?,
    };
    let method = from_submitter("formmethod")
        .or_else(|| attr(doc, form, "method"))
        .map(str::to_ascii_lowercase);
    let method = match method.as_deref() {
        Some("post") => "post",
        Some("dialog") => return None,
        _ => "get",
    };
    let enctype = from_submitter("formenctype")
        .or_else(|| attr(doc, form, "enctype"))
        .map(str::to_ascii_lowercase);
    let entries = form_data_set(doc, form, submitter);
    let nav_method = match (action.scheme(), method) {
        ("http" | "https" | "data" | "file", "get") => {
            // "Mutate action URL": the query becomes the serialization.
            let mut s = action.without_fragment().as_str().to_string();
            if let Some(q) = s.find('?') {
                s.truncate(q);
            }
            s.push('?');
            s.push_str(&encode_urlencoded(&entries));
            if let Some(f) = action.fragment() {
                s.push('#');
                s.push_str(f);
            }
            return Some(NavigationRequest {
                initiator: Some(doc_url.to_string()),
                ..NavigationRequest::user(Url::parse(&s).ok()?)
            });
        }
        ("http" | "https", _) => match enctype.as_deref() {
            Some("multipart/form-data") => {
                let mut seed = entries.len() as u64 ^ 0x9e37_79b9_7f4a_7c15;
                loop {
                    let boundary = format!("----wbFormBoundary{seed:016x}");
                    if let Some(body) = encode_multipart(&entries, &boundary) {
                        break NavigationMethod::Post {
                            content_type: format!("multipart/form-data; boundary={boundary}"),
                            body,
                        };
                    }
                    seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                }
            }
            Some("text/plain") => NavigationMethod::Post {
                content_type: "text/plain".into(),
                body: encode_text_plain(&entries).into_bytes(),
            },
            _ => NavigationMethod::Post {
                content_type: "application/x-www-form-urlencoded".into(),
                body: encode_urlencoded(&entries).into_bytes(),
            },
        },
        // M1 follow-up: `mailto:` and POST to `data:`.
        _ => return None,
    };
    Some(NavigationRequest {
        method: nav_method,
        initiator: Some(doc_url.to_string()),
        ..NavigationRequest::user(action)
    })
}

/// What activating (clicking) an element does.
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)] // returned once per click
pub enum Activation {
    /// Nothing.
    None,
    /// The control's state changed (checkbox, radio, reset); repaint.
    Changed,
    /// Focus the text control.
    Focus(NodeId),
    /// Open the drop-down of this `<select>`.
    OpenSelect(NodeId),
    /// Submit the form.
    Submit(NavigationRequest),
}

fn set_bool_attr(doc: &mut Document, n: NodeId, name: &str, on: bool) {
    if let Some(el) = doc.element_mut(n) {
        if on {
            el.set_attribute(name, "");
        } else {
            el.attributes
                .retain(|a| !(a.namespace.is_none() && a.local_name == name));
        }
    }
}

/// Per-page form state: the original (default) values for `reset`.
#[derive(Clone, Debug, Default)]
pub struct FormState {
    defaults: HashMap<NodeId, Initial>,
}

#[derive(Clone, Debug)]
enum Initial {
    Value(Option<String>),
    Checked(bool),
    Text(String),
}

impl FormState {
    /// Remembers `n`'s current state as its default, the first time it is
    /// about to change.
    fn remember(&mut self, doc: &Document, n: NodeId) {
        if self.defaults.contains_key(&n) {
            return;
        }
        let d = if is_html(doc, n, "textarea") {
            Initial::Text(text_value(doc, n))
        } else if is_html(doc, n, "option")
            || matches!(input_type(doc, n).as_str(), "checkbox" | "radio")
        {
            let name = if is_html(doc, n, "option") {
                "selected"
            } else {
                "checked"
            };
            Initial::Checked(attr(doc, n, name).is_some())
        } else {
            Initial::Value(attr(doc, n, "value").map(str::to_string))
        };
        self.defaults.insert(n, d);
    }

    /// Sets a text control's value (typing, deleting).
    pub fn set_text_value(&mut self, doc: &mut Document, n: NodeId, value: &str) {
        self.remember(doc, n);
        if is_html(doc, n, "textarea") {
            let children: Vec<NodeId> = doc.children(n).collect();
            for c in children {
                doc.detach(c);
            }
            let t = doc.create_text(value);
            let _ = doc.append_child(n, t);
        } else if let Some(el) = doc.element_mut(n) {
            el.set_attribute("value", value);
        }
    }

    /// Selects option `index` of a `<select>` (deselecting the others
    /// unless it is `multiple`, where the option is toggled).
    pub fn select_option(&mut self, doc: &mut Document, select: NodeId, index: usize) {
        let options = select_options(doc, select);
        let Some(&chosen) = options.get(index) else {
            return;
        };
        if option_disabled(doc, chosen) {
            return;
        }
        let multiple = attr(doc, select, "multiple").is_some();
        for &o in &options {
            self.remember(doc, o);
        }
        if multiple {
            let on = attr(doc, chosen, "selected").is_none();
            set_bool_attr(doc, chosen, "selected", on);
        } else {
            for &o in &options {
                set_bool_attr(doc, o, "selected", o == chosen);
            }
        }
    }

    /// Restores every control of `form` to its default (HTML §4.10.21.6).
    pub fn reset(&mut self, doc: &mut Document, form: NodeId) {
        let owned: Vec<NodeId> = self
            .defaults
            .keys()
            .copied()
            .filter(|&n| {
                let control = if is_html(doc, n, "option") {
                    doc.ancestors(n).find(|&a| is_html(doc, a, "select"))
                } else {
                    Some(n)
                };
                control.and_then(|c| form_owner(doc, c)) == Some(form)
            })
            .collect();
        for n in owned {
            let Some(d) = self.defaults.remove(&n) else {
                continue;
            };
            match d {
                Initial::Text(t) => {
                    self.set_text_value(doc, n, &t);
                    self.defaults.remove(&n);
                }
                Initial::Checked(on) => {
                    let name = if is_html(doc, n, "option") {
                        "selected"
                    } else {
                        "checked"
                    };
                    set_bool_attr(doc, n, name, on);
                }
                Initial::Value(v) => {
                    if let Some(el) = doc.element_mut(n) {
                        match v {
                            Some(v) => el.set_attribute("value", v),
                            None => el
                                .attributes
                                .retain(|a| !(a.namespace.is_none() && a.local_name == "value")),
                        }
                    }
                }
            }
        }
    }

    /// The activation behavior of `target` (HTML §4.10.5.1, §4.10.6,
    /// §4.10.4 for labels): toggles checkboxes, checks radios (unchecking
    /// the rest of the group), focuses text controls, opens selects,
    /// resets or submits forms.
    pub fn activate(
        &mut self,
        doc: &mut Document,
        target: NodeId,
        doc_url: &Url,
        base_url: &Url,
    ) -> Activation {
        // A click inside a <label> activates its labeled control.
        let target = control_for(doc, target).unwrap_or(target);
        if is_disabled(doc, target) {
            return Activation::None;
        }
        if is_text_control(doc, target) {
            return Activation::Focus(target);
        }
        if is_html(doc, target, "select") {
            return Activation::OpenSelect(target);
        }
        let ty = if is_html(doc, target, "input") {
            input_type(doc, target)
        } else if is_html(doc, target, "button") {
            attr(doc, target, "type")
                .map(str::to_ascii_lowercase)
                .filter(|t| matches!(t.as_str(), "reset" | "button"))
                .unwrap_or_else(|| "submit".into())
        } else {
            return Activation::None;
        };
        match ty.as_str() {
            "checkbox" => {
                self.remember(doc, target);
                let on = attr(doc, target, "checked").is_none();
                set_bool_attr(doc, target, "checked", on);
                Activation::Changed
            }
            "radio" => {
                for r in radio_group(doc, target) {
                    self.remember(doc, r);
                    set_bool_attr(doc, r, "checked", r == target);
                }
                Activation::Changed
            }
            "submit" | "image" => match form_owner(doc, target)
                .and_then(|f| submission(doc, f, Some(target), doc_url, base_url))
            {
                Some(req) => Activation::Submit(req),
                None => Activation::None,
            },
            "reset" => match form_owner(doc, target) {
                Some(f) => {
                    self.reset(doc, f);
                    Activation::Changed
                }
                None => Activation::None,
            },
            _ => Activation::None,
        }
    }
}

/// The labeled control of a `<label>` that is `n` or an ancestor of `n`
/// (HTML §4.10.4), or `n` itself when it is a control.
fn control_for(doc: &Document, n: NodeId) -> Option<NodeId> {
    if is_submittable(doc, n) {
        return Some(n);
    }
    let label = std::iter::once(n)
        .chain(doc.ancestors(n))
        .find(|&a| is_html(doc, a, "label"))?;
    if let Some(id) = attr(doc, label, "for") {
        return doc.element_by_id(id).filter(|&c| is_submittable(doc, c));
    }
    doc.descendants(label).find(|&c| is_submittable(doc, c))
}

/// The radio button group of `radio` (HTML §4.10.5.1.16): radios with the
/// same non-empty name and the same form owner, in the same tree.
fn radio_group(doc: &Document, radio: NodeId) -> Vec<NodeId> {
    let name = attr(doc, radio, "name").unwrap_or("");
    if name.is_empty() {
        return vec![radio];
    }
    let owner = form_owner(doc, radio);
    doc.descendants(doc.root())
        .filter(|&n| {
            is_html(doc, n, "input")
                && input_type(doc, n) == "radio"
                && attr(doc, n, "name") == Some(name)
                && form_owner(doc, n) == owner
        })
        .collect()
}

/// HTML §4.10.21.2 implicit submission: pressing Enter in a text field of
/// `form` activates its default button (the first submit button in tree
/// order), or, without one, submits the form when it has at most one
/// field that blocks implicit submission.
#[must_use]
pub fn implicit_submission(
    doc: &Document,
    field: NodeId,
    doc_url: &Url,
    base_url: &Url,
) -> Option<NavigationRequest> {
    let form = form_owner(doc, field)?;
    let owned: Vec<NodeId> = doc
        .descendants(doc.root())
        .filter(|&n| is_submittable(doc, n) && form_owner(doc, n) == Some(form))
        .collect();
    if let Some(&default) = owned.iter().find(|&&n| is_submit_button(doc, n)) {
        if is_disabled(doc, default) {
            return None;
        }
        return submission(doc, form, Some(default), doc_url, base_url);
    }
    let blocking = owned
        .iter()
        .filter(|&&n| {
            is_html(doc, n, "input")
                && matches!(
                    input_type(doc, n).as_str(),
                    "text"
                        | "search"
                        | "url"
                        | "tel"
                        | "email"
                        | "password"
                        | "date"
                        | "month"
                        | "week"
                        | "time"
                        | "datetime-local"
                        | "number"
                )
        })
        .count();
    if blocking > 1 {
        return None;
    }
    submission(doc, form, None, doc_url, base_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(html: &str) -> Document {
        wb_html::parse_document(html, &wb_html::ParseOptions::default()).document
    }

    fn id(d: &Document, s: &str) -> NodeId {
        d.element_by_id(s).unwrap()
    }

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    const FORM: &str = r#"<form id=f action="/search?old=1#frag">
      <input name=q value="a b&c=d">
      <input name=empty>
      <input type=password name=pw value="p">
      <input type=hidden name=_charset_>
      <input type=checkbox name=cb>
      <input type=checkbox name=cb2 checked>
      <input type=checkbox name=cb3 value=yes checked>
      <input type=radio name=r value=1 id=r1 checked>
      <input type=radio name=r value=2 id=r2>
      <select name=s><option>One <b>1</b></option><option value=two selected>Two</select>
      <select name=m multiple><option selected>x<option>y<option selected disabled>z</select>
      <select name=none size=3><option>a</select>
      <textarea name=t>line1
line2</textarea>
      <input name=dis disabled value=no>
      <fieldset disabled><input name=fs value=no><legend><input name=lg value=yes></legend></fieldset>
      <input name=dir dirname=dir.dir value=x>
      <datalist><input name=dl value=no></datalist>
      <input type=submit name=go value=Go id=go>
      <input type=submit name=other value=Other>
      <button name=b value=bv id=b>B</button>
      <input type=image name=img id=img>
      <input type=file name=file>
      <input name=unnamed_value value="">
      <input value=nameless>
    </form>
    <input form=f name=outside value=o>
    <form id=g><input name=elsewhere></form>"#;

    #[test]
    fn entry_list() {
        let d = doc(FORM);
        let f = id(&d, "f");
        let entries = form_data_set(&d, f, Some(id(&d, "go")));
        let got: Vec<(String, String)> = entries
            .iter()
            .map(|e| {
                let v = match &e.value {
                    EntryValue::Text(t) => t.clone(),
                    EntryValue::File { filename } => format!("file:{filename}"),
                };
                (e.name.clone(), v)
            })
            .collect();
        let want: Vec<(&str, &str)> = vec![
            ("q", "a b&c=d"),
            ("empty", ""),
            ("pw", "p"),
            ("_charset_", "UTF-8"),
            ("cb2", "on"),
            ("cb3", "yes"),
            ("r", "1"),
            ("s", "two"),
            ("m", "x"),
            ("t", "line1\nline2"),
            ("lg", "yes"),
            ("dir", "x"),
            ("dir.dir", "ltr"),
            ("go", "Go"),
            ("file", "file:"),
            ("unnamed_value", ""),
            ("outside", "o"),
        ];
        let want: Vec<(String, String)> = want
            .into_iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        assert_eq!(got, want);
        // An image submitter adds name.x / name.y instead.
        let entries = form_data_set(&d, f, Some(id(&d, "img")));
        assert!(entries.iter().any(|e| e.name == "img.x"));
        assert!(!entries.iter().any(|e| e.name == "go"));
    }

    #[test]
    fn urlencoded_get_submission() {
        let d = doc(FORM);
        let req = submission(
            &d,
            id(&d, "f"),
            Some(id(&d, "b")),
            &url("https://a.test/dir/page"),
            &url("https://a.test/dir/page"),
        )
        .unwrap();
        assert_eq!(req.method, NavigationMethod::Get);
        let s = req.url.as_str();
        assert!(
            s.starts_with("https://a.test/search?q=a+b%26c%3Dd&empty=&pw=p&_charset_=UTF-8&"),
            "{s}"
        );
        assert!(s.contains("&t=line1%0D%0Aline2&"), "{s}");
        assert!(s.contains("&b=bv&"), "{s}");
        assert!(s.ends_with("&outside=o#frag"), "{s}");
        assert!(!s.contains("old=1"));
        assert_eq!(req.initiator.as_deref(), Some("https://a.test/dir/page"));
    }

    #[test]
    fn post_encodings() {
        let d = doc(
            "<form id=f method=POST action=x><input name='a b' value='1+1'>\
             <textarea name=t>x\ry</textarea><button id=s formenctype=text/plain>s</button>\
             <button id=m formenctype=multipart/form-data formaction=/up>m</button>\
             <input type=file name='f\"n'></form>",
        );
        let u = url("https://a.test/p/q");
        let f = id(&d, "f");
        let req = submission(&d, f, None, &u, &u).unwrap();
        assert_eq!(req.url.as_str(), "https://a.test/p/x");
        assert_eq!(
            req.method,
            NavigationMethod::Post {
                content_type: "application/x-www-form-urlencoded".into(),
                body: b"a+b=1%2B1&t=x%0D%0Ay&f%22n=".to_vec(),
            }
        );
        let req = submission(&d, f, Some(id(&d, "s")), &u, &u).unwrap();
        assert_eq!(
            req.method,
            NavigationMethod::Post {
                content_type: "text/plain".into(),
                body: b"a b=1+1\r\nt=x\r\ny\r\nf\"n=\r\n".to_vec(),
            }
        );
        let req = submission(&d, f, Some(id(&d, "m")), &u, &u).unwrap();
        assert_eq!(req.url.as_str(), "https://a.test/up");
        let NavigationMethod::Post { content_type, body } = req.method else {
            panic!()
        };
        let boundary = content_type
            .strip_prefix("multipart/form-data; boundary=")
            .unwrap();
        let body = String::from_utf8(body).unwrap();
        assert_eq!(
            body,
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"a b\"\r\n\r\n1+1\r\n\
                 --{boundary}\r\nContent-Disposition: form-data; name=\"t\"\r\n\r\nx\r\ny\r\n\
                 --{boundary}\r\nContent-Disposition: form-data; name=\"f%22n\"; filename=\"\"\r\n\
                 Content-Type: application/octet-stream\r\n\r\n\r\n--{boundary}--\r\n"
            )
        );
    }

    #[test]
    fn multipart_rejects_colliding_boundary() {
        let e = vec![Entry {
            name: "a".into(),
            value: EntryValue::Text("--XYZ".into()),
        }];
        assert!(encode_multipart(&e, "XYZ").is_none());
        assert!(encode_multipart(&e, "ABC").is_some());
    }

    #[test]
    fn non_submitting_actions() {
        let u = url("https://a.test/");
        for form in [
            "<form id=f method=dialog></form>",
            "<form id=f action='javascript:alert(1)'></form>",
            "<form id=f action='mailto:x@y'></form>",
            "<form id=f action='https://[bad'></form>",
        ] {
            let d = doc(form);
            assert_eq!(submission(&d, id(&d, "f"), None, &u, &u), None, "{form}");
        }
    }

    #[test]
    fn activation() {
        let mut d = doc("<form id=f action=/s><input type=checkbox id=c name=c>\
             <label id=l><span id=sp>tick</span><input type=checkbox id=c2 name=c2></label>\
             <input type=radio name=r id=r1 checked><input type=radio name=r id=r2>\
             <input id=t name=t value=v><select id=sel name=sel><option>a<option>b</select>\
             <input type=reset id=reset><input type=submit id=sub disabled></form>");
        let u = url("https://a.test/");
        let mut st = FormState::default();
        assert_eq!(
            {
                let n = id(&d, "c");
                st.activate(&mut d, n, &u, &u)
            },
            Activation::Changed
        );
        assert!(d.attribute(id(&d, "c"), "checked").is_some());
        assert_eq!(
            {
                let n = id(&d, "sp");
                st.activate(&mut d, n, &u, &u)
            },
            Activation::Changed
        );
        assert!(d.attribute(id(&d, "c2"), "checked").is_some());
        {
            let n = id(&d, "r2");
            st.activate(&mut d, n, &u, &u)
        };
        assert!(d.attribute(id(&d, "r1"), "checked").is_none());
        assert!(d.attribute(id(&d, "r2"), "checked").is_some());
        let t = id(&d, "t");
        assert_eq!(st.activate(&mut d, t, &u, &u), Activation::Focus(t));
        st.set_text_value(&mut d, t, "typed");
        let sel = id(&d, "sel");
        assert_eq!(
            st.activate(&mut d, sel, &u, &u),
            Activation::OpenSelect(sel)
        );
        st.select_option(&mut d, sel, 1);
        assert_eq!(selected_options(&d, sel).len(), 1);
        assert_eq!(option_value(&d, selected_options(&d, sel)[0]), "b");
        assert_eq!(
            {
                let n = id(&d, "sub");
                st.activate(&mut d, n, &u, &u)
            },
            Activation::None
        );
        // Implicit submission: no enabled default button? The disabled
        // default button blocks it.
        assert_eq!(implicit_submission(&d, t, &u, &u), None);
        // Reset restores everything.
        assert_eq!(
            {
                let n = id(&d, "reset");
                st.activate(&mut d, n, &u, &u)
            },
            Activation::Changed
        );
        assert!(d.attribute(id(&d, "c"), "checked").is_none());
        assert!(d.attribute(id(&d, "r1"), "checked").is_some());
        assert!(d.attribute(id(&d, "r2"), "checked").is_none());
        assert_eq!(text_value(&d, t), "v");
        assert_eq!(option_value(&d, selected_options(&d, sel)[0]), "a");
    }

    #[test]
    fn implicit_submission_rules() {
        let u = url("https://a.test/");
        let d = doc("<form id=f action=/s><input id=a name=a value=1></form>");
        let req = implicit_submission(&d, id(&d, "a"), &u, &u).unwrap();
        assert_eq!(req.url.as_str(), "https://a.test/s?a=1");
        let d = doc("<form><input id=a name=a><input name=b></form>");
        assert_eq!(implicit_submission(&d, id(&d, "a"), &u, &u), None);
        let d = doc(
            "<form action=/s><input id=a name=a><input name=b><button name=go value=1>x</button></form>",
        );
        let req = implicit_submission(&d, id(&d, "a"), &u, &u).unwrap();
        assert_eq!(req.url.as_str(), "https://a.test/s?a=&b=&go=1");
    }

    #[test]
    fn urlencode_bytes() {
        assert_eq!(urlencode("aZ09*-._ ~!é"), "aZ09*-._+%7E%21%C3%A9");
    }
}
