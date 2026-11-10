//! Unit tests for the parser API. The full html5lib/WPT suites run through
//! `cargo xtask conformance`; these cover each area with small cases.

use super::*;

fn dump(input: &str) -> String {
    let parsed = parse_document(input, &ParseOptions::default());
    dump_tree(&parsed.document, parsed.document.root())
}

fn body(input: &str) -> String {
    // The body's subtree, re-indented to start at depth 0.
    let full = dump(input);
    let mut out = String::new();
    let mut in_body = false;
    for line in full.lines() {
        if line == "|   <body>" {
            in_body = true;
            continue;
        }
        if in_body {
            if let Some(rest) = line.strip_prefix("|     ") {
                out.push_str("| ");
                out.push_str(rest);
                out.push('\n');
            } else {
                break;
            }
        }
    }
    out
}

fn fragment(input: &str, context: &str) -> String {
    let parsed = parse_fragment(
        input,
        &FragmentContext::html(context),
        &ParseOptions::default(),
    );
    dump_tree(&parsed.document, parsed.fragment)
}

#[test]
fn parses_a_simple_document() {
    assert_eq!(
        dump("<!DOCTYPE html><p>One<p>Two"),
        "| <!DOCTYPE html>\n| <html>\n|   <head>\n|   <body>\n|     <p>\n|       \"One\"\n|     <p>\n|       \"Two\"\n"
    );
}

#[test]
fn implies_html_head_and_body() {
    let parsed = parse_document("hello", &ParseOptions::default());
    let doc = &parsed.document;
    let html = doc.document_element().unwrap();
    assert!(doc.is_html_element(html, "html"));
    assert!(doc.html_child_of_document_element("head").is_some());
    let body = doc.html_child_of_document_element("body").unwrap();
    assert_eq!(doc.text_content(body).as_deref(), Some("hello"));
    assert_eq!(doc.quirks_mode(), QuirksMode::Quirks);
    assert!(!parsed.errors.is_empty());
}

#[test]
fn sets_document_modes_from_the_doctype() {
    let mode = |s: &str| {
        parse_document(s, &ParseOptions::default())
            .document
            .quirks_mode()
    };
    assert_eq!(mode("<!DOCTYPE html>"), QuirksMode::NoQuirks);
    assert_eq!(
        mode("<!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 4.01 Transitional//EN\">"),
        QuirksMode::Quirks
    );
    assert_eq!(
        mode(
            "<!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 4.01 Transitional//EN\" \
             \"http://www.w3.org/TR/html4/loose.dtd\">"
        ),
        QuirksMode::LimitedQuirks
    );
}

#[test]
fn runs_the_adoption_agency_algorithm() {
    assert_eq!(
        body("<b>1<p>2</b>3</p>"),
        "| <b>\n|   \"1\"\n| <p>\n|   <b>\n|     \"2\"\n|   \"3\"\n"
    );
}

#[test]
fn reconstructs_formatting_elements() {
    assert_eq!(
        body("<p><i>a<p>b"),
        "| <p>\n|   <i>\n|     \"a\"\n| <p>\n|   <i>\n|     \"b\"\n"
    );
}

#[test]
fn foster_parents_misplaced_table_content() {
    assert_eq!(
        body("<table>A<tr>B<td>C</table>"),
        "| \"AB\"\n| <table>\n|   <tbody>\n|     <tr>\n|       <td>\n|         \"C\"\n"
    );
}

#[test]
fn puts_template_contents_in_a_fragment() {
    let parsed = parse_document("<template><td>x</td></template>", &ParseOptions::default());
    let doc = &parsed.document;
    let template = doc.elements_by_tag_name(doc.root(), "template")[0];
    assert_eq!(doc.first_child(template), None);
    let contents = doc.template_contents(template).unwrap();
    assert_eq!(doc.text_content(contents).as_deref(), Some("x"));
}

#[test]
fn parses_foreign_content() {
    assert_eq!(
        body("<svg viewbox='0 0 1 1'><foreignobject><p>x</p></foreignobject></svg><math><mi>y"),
        "| <svg svg>\n|   viewBox=\"0 0 1 1\"\n|   <svg foreignObject>\n|     <p>\n|       \"x\"\n\
         | <math math>\n|   <math mi>\n|     \"y\"\n"
    );
    assert_eq!(body("<svg><![CDATA[a<b]]>"), "| <svg svg>\n|   \"a<b\"\n");
}

#[test]
fn parses_text_elements() {
    assert_eq!(
        body("<textarea>\n<b>&amp;</textarea><pre>\nx</pre>"),
        "| <textarea>\n|   \"<b>&\"\n| <pre>\n|   \"x\"\n"
    );
    let parsed = parse_document(
        "<title>a<b></title><script>if (a<b) x()</script>",
        &ParseOptions::default(),
    );
    let doc = &parsed.document;
    let script = doc.elements_by_tag_name(doc.root(), "script")[0];
    assert_eq!(doc.text_content(script).as_deref(), Some("if (a<b) x()"));
}

#[test]
fn noscript_depends_on_the_scripting_flag() {
    let scripting = parse_document(
        "<noscript><p>x</p></noscript>",
        &ParseOptions { scripting: true },
    );
    let doc = &scripting.document;
    let noscript = doc.elements_by_tag_name(doc.root(), "noscript")[0];
    assert_eq!(doc.text_content(noscript).as_deref(), Some("<p>x</p>"));
    let off = parse_document(
        "<body><noscript><p>x</p></noscript>",
        &ParseOptions::default(),
    );
    assert_eq!(
        off.document
            .elements_by_tag_name(off.document.root(), "p")
            .len(),
        1
    );
}

#[test]
fn parses_fragments_in_context() {
    assert_eq!(fragment("<td>x", "tr"), "| <td>\n|   \"x\"\n");
    assert_eq!(fragment("a<b>c", "div"), "| \"a\"\n| <b>\n|   \"c\"\n");
    assert_eq!(fragment("<p>x</p>", "title"), "| \"<p>x</p>\"\n");
}

#[test]
fn keeps_processing_instructions() {
    assert_eq!(body("<body><?php echo 1 ?>"), "| <?php echo 1 ?>\n");
}

#[test]
fn decodes_bytes_with_the_sniffed_encoding() {
    let parsed = parse_document_bytes(
        b"<meta charset=windows-1251><p>\xcf\xf0\xe8\xe2\xe5\xf2",
        None,
        &ParseOptions::default(),
    );
    assert_eq!(parsed.encoding, "windows-1251");
    let doc = &parsed.document;
    let p = doc.elements_by_tag_name(doc.root(), "p")[0];
    assert_eq!(doc.text_content(p).as_deref(), Some("Привет"));

    let parsed = parse_document_bytes("<p>é".as_bytes(), Some("utf-8"), &ParseOptions::default());
    assert_eq!(parsed.encoding, "UTF-8");
}

#[test]
fn a_late_meta_charset_triggers_a_reparse() {
    // The prescan only looks at the first 1024 bytes.
    let mut html = b"<!DOCTYPE html><head><!--".to_vec();
    html.extend(std::iter::repeat_n(b'x', 1100));
    html.extend_from_slice(b"--><meta charset=koi8-r></head><p>\xf0\xd2\xc9");
    let parsed = parse_document_bytes(&html, None, &ParseOptions::default());
    assert_eq!(parsed.encoding, "KOI8-R");
    let doc = &parsed.document;
    let p = doc.elements_by_tag_name(doc.root(), "p")[0];
    assert_eq!(doc.text_content(p).as_deref(), Some("При"));
}

#[test]
fn survives_deep_nesting_and_garbage() {
    let deep =
        "<div>".repeat(5000) + &"<b><i>".repeat(500) + "<table><tr><td>" + &"</div>".repeat(5000);
    let parsed = parse_document(&deep, &ParseOptions::default());
    let out = dump_tree(&parsed.document, parsed.document.root());
    assert!(out.lines().count() > 5000);
    for input in [
        "<",
        "</",
        "<!",
        "<!-",
        "<?",
        "&",
        "&#",
        "&#x",
        "<a b='",
        "<!DOCTYPE",
        "<svg><![CDATA[",
        "<table><caption><table>",
        "</p></br><frameset><select><option><hr></select>",
        "<math><annotation-xml encoding=text/html><div><svg><desc><td>",
        "\0\u{1}\u{FFFF}<\0>",
    ] {
        let _ = parse_document(input, &ParseOptions::default());
        let _ = parse_fragment(
            input,
            &FragmentContext::html("td"),
            &ParseOptions::default(),
        );
    }
}
