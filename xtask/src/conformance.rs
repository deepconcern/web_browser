//! Runs the conformance suites, writes `docs/scorecard.md`, and compares pass
//! counts against `conformance/baseline.json`.
//!
//! - `html5lib-tokenizer`: one case per (test, initial state) in
//!   `tokenizer/*.test`. A case passes when the token stream matches and the
//!   parse error codes match as a multiset (positions are not compared).
//! - `wpt-html-tree-construction`: one case per `#data` block in the WPT
//!   `html/syntax/parsing/resources/*.dat` files. A case passes when the
//!   tree matches. Tests without `#script-on`/`#script-off` must pass with
//!   the scripting flag both off and on. Error counts are not compared (the
//!   spec does not define tree-construction error codes), matching what the
//!   WPT harness checks.
//!
//! `xmlViolationTests` compare the token stream after the infoset coercion
//! of HTML §13.2.9 and do not compare errors.
//!
//! Set `XTASK_CONFORMANCE_FAILURES=1` to print the failing case ids, and
//! `XTASK_SHOW=1` to print the expected and actual trees of failing
//! tree-construction cases.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use wb_dom::{Attribute, Namespace, QuirksMode};
use wb_html::tokenizer::{State, Token, Tokenizer};
use wb_html::{FragmentContext, ParseOptions, dump_tree, parse_document, parse_fragment};

use crate::corpora::corpora_dir;
use crate::{Result, read_json};

#[derive(Debug)]
struct SuiteResult {
    id: &'static str,
    title: &'static str,
    passed: u64,
    total: u64,
}

pub fn run(root: &Path, args: &[String]) -> Result<()> {
    let html5lib = corpora_dir(root).join("html5lib-tests");
    let wpt = corpora_dir(root).join("wpt");
    if !html5lib.is_dir() || !wpt.is_dir() {
        return Err("corpora not found; run `cargo xtask fetch-corpora` first".to_string());
    }

    let results = vec![
        run_suite(
            "html5lib-tokenizer",
            "HTML tokenizer (html5lib-tests)",
            tokenizer_cases(&html5lib.join("tokenizer"))?,
        ),
        run_suite(
            "wpt-html-tree-construction",
            "HTML tree construction (WPT html/syntax/parsing)",
            // The html5lib tree-construction tests now live in WPT.
            tree_construction_cases(&wpt.join("html/syntax/parsing/resources"))?,
        ),
    ];

    for r in &results {
        println!("{}: {}/{} passed", r.id, r.passed, r.total);
    }

    let scorecard = root.join("docs").join("scorecard.md");
    std::fs::write(&scorecard, render_scorecard(&results))
        .map_err(|e| format!("cannot write {}: {e}", scorecard.display()))?;

    let baseline_path = root.join("conformance").join("baseline.json");
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] => Ok(()),
        ["--check"] => check_baseline(&results, &read_baseline(&baseline_path)?),
        ["--check", "--base", base] => {
            check_baseline(&results, &read_baseline(&baseline_path)?)?;
            check_against_base(&results, &read_baseline(Path::new(base))?)
        }
        ["--update-baseline"] => write_baseline(&baseline_path, &results),
        _ => Err(format!("unknown conformance options: {}", args.join(" "))),
    }
}

/// One test case: an id and whether it passed.
struct Case {
    id: String,
    passed: bool,
}

fn run_suite(id: &'static str, title: &'static str, cases: Vec<Case>) -> SuiteResult {
    let show_failures = std::env::var_os("XTASK_CONFORMANCE_FAILURES").is_some();
    let total = cases.len() as u64;
    let mut passed = 0;
    for case in &cases {
        if case.passed {
            passed += 1;
        } else if show_failures {
            eprintln!("FAIL {id} {}", case.id);
        }
    }
    SuiteResult {
        id,
        title,
        passed,
        total,
    }
}

// ----- html5lib tokenizer tests ---------------------------------------------

/// One case per (test, initial state) pair in `tokenizer/*.test`.
fn tokenizer_cases(dir: &Path) -> Result<Vec<Case>> {
    let mut cases = Vec::new();
    for file in files_with_extension(dir, "test")? {
        let json = read_json(&file)?;
        let file_name = display_name(&file);
        for key in ["tests", "xmlViolationTests"] {
            let tests = json
                .get(key)
                .and_then(Value::as_array)
                .map_or(&[][..], Vec::as_slice);
            for (index, test) in tests.iter().enumerate() {
                let states: Vec<&str> = test
                    .get("initialStates")
                    .and_then(Value::as_array)
                    .map(|s| s.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_else(|| vec!["Data state"]);
                for (state_index, state) in states.iter().enumerate() {
                    cases.push(Case {
                        id: format!("{file_name}:{key}:{index}:{state_index}"),
                        passed: run_tokenizer_test(test, state, key == "xmlViolationTests"),
                    });
                }
            }
        }
    }
    Ok(cases)
}

/// Undoes the extra `\uHHHH` escaping of `doubleEscaped` tests. Returns
/// `None` for lone surrogates, which a Rust string cannot hold.
fn unescape(s: &str) -> Option<String> {
    let mut units: Vec<u16> = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&'u') {
            chars.next();
            let hex: String = chars.by_ref().take(4).collect();
            units.push(u16::from_str_radix(&hex, 16).ok()?);
        } else {
            let mut buf = [0; 2];
            units.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    String::from_utf16(&units).ok()
}

fn unescape_value(value: &Value) -> Option<Value> {
    Some(match value {
        Value::String(s) => Value::String(unescape(s)?),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(unescape_value)
                .collect::<Option<Vec<_>>>()?,
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| Some((unescape(k)?, unescape_value(v)?)))
                .collect::<Option<Map<_, _>>>()?,
        ),
        other => other.clone(),
    })
}

fn token_json(token: &Token) -> Option<Value> {
    Some(match token {
        Token::Doctype(d) => Value::Array(vec![
            "DOCTYPE".into(),
            d.name.clone().map_or(Value::Null, Value::from),
            d.public_id.clone().map_or(Value::Null, Value::from),
            d.system_id.clone().map_or(Value::Null, Value::from),
            Value::Bool(!d.force_quirks),
        ]),
        Token::StartTag(tag) => {
            let attributes: Map<String, Value> = tag
                .attributes
                .iter()
                .map(|a| (a.name.clone(), Value::from(a.value.clone())))
                .collect();
            let mut out = vec![
                "StartTag".into(),
                tag.name.clone().into(),
                Value::Object(attributes),
            ];
            if tag.self_closing {
                out.push(Value::Bool(true));
            }
            Value::Array(out)
        }
        Token::EndTag(tag) => Value::Array(vec!["EndTag".into(), tag.name.clone().into()]),
        Token::Comment(data) => Value::Array(vec!["Comment".into(), data.clone().into()]),
        Token::ProcessingInstruction { target, data } => Value::Array(vec![
            "ProcessingInstruction".into(),
            target.clone().into(),
            data.clone().into(),
        ]),
        Token::Character(_) | Token::Eof => return None,
    })
}

/// "Coercing an HTML DOM into an infoset" (HTML §13.2.9), applied to the
/// token stream for `xmlViolationTests`: form feeds become spaces, other
/// non-XML characters become U+FFFD, and comments may not contain `--` or
/// end with `-`.
fn coerce_to_infoset(token: &Value) -> Value {
    fn chars(s: &str) -> String {
        s.chars()
            .map(|c| match c {
                '\u{c}' => ' ',
                '\t' | '\n' | '\r' => c,
                c if (c as u32) < 0x20
                    || matches!(c as u32, 0xFFFE | 0xFFFF)
                    || (0xFDD0..=0xFDEF).contains(&(c as u32)) =>
                {
                    '\u{FFFD}'
                }
                c => c,
            })
            .collect()
    }
    let Some(items) = token.as_array() else {
        return token.clone();
    };
    let kind = items.first().and_then(Value::as_str).unwrap_or_default();
    let items = items
        .iter()
        .enumerate()
        .map(|(i, item)| match item {
            Value::String(s) if i > 0 => {
                let mut s = chars(s);
                if kind == "Comment" {
                    while s.contains("--") {
                        s = s.replace("--", "- -");
                    }
                    if s.ends_with('-') {
                        s.push(' ');
                    }
                }
                Value::String(s)
            }
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().map_or(v.clone(), |v| chars(v).into())))
                    .collect(),
            ),
            other => other.clone(),
        })
        .collect();
    Value::Array(items)
}

fn run_tokenizer_test(test: &Value, state: &str, xml_violation: bool) -> bool {
    let double_escaped = test.get("doubleEscaped").and_then(Value::as_bool) == Some(true);
    let Some(input) = test.get("input").and_then(Value::as_str) else {
        return false;
    };
    let input = if double_escaped {
        match unescape(input) {
            Some(input) => input,
            None => return false,
        }
    } else {
        input.to_string()
    };
    let expected = match test.get("output") {
        Some(output) if double_escaped => match unescape_value(output) {
            Some(output) => output,
            None => return false,
        },
        Some(output) => output.clone(),
        None => return false,
    };

    let mut tokenizer = Tokenizer::new(&input);
    tokenizer.set_state(match state {
        "Data state" => State::Data,
        "PLAINTEXT state" => State::Plaintext,
        "RCDATA state" => State::Rcdata,
        "RAWTEXT state" => State::Rawtext,
        "Script data state" => State::ScriptData,
        "CDATA section state" => State::CdataSection,
        _ => return false,
    });
    tokenizer.set_last_start_tag(test.get("lastStartTag").and_then(Value::as_str));

    let mut actual = Vec::new();
    let mut text = String::new();
    loop {
        let token = tokenizer.next_token();
        match token {
            Token::Character(c) => text.push(c),
            Token::Eof => break,
            token => {
                if !text.is_empty() {
                    actual.push(Value::Array(vec![
                        "Character".into(),
                        std::mem::take(&mut text).into(),
                    ]));
                }
                if let Some(json) = token_json(&token) {
                    actual.push(json);
                }
            }
        }
    }
    if !text.is_empty() {
        actual.push(Value::Array(vec!["Character".into(), text.into()]));
    }
    if xml_violation {
        // These tests only describe the coerced token stream.
        let coerced: Vec<Value> = actual.iter().map(coerce_to_infoset).collect();
        return Value::Array(coerced) == expected;
    }
    if Value::Array(actual) != expected {
        return false;
    }

    let mut expected_errors: Vec<String> = test
        .get("errors")
        .and_then(Value::as_array)
        .map(|errors| {
            errors
                .iter()
                .filter_map(|e| e.get("code").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let mut actual_errors: Vec<String> = tokenizer
        .take_errors()
        .iter()
        .map(|e| e.code.to_string())
        .collect();
    expected_errors.sort();
    actual_errors.sort();
    expected_errors == actual_errors
}

// ----- Tree construction tests ----------------------------------------------

/// One parsed `.dat` test.
#[derive(Default)]
struct TreeTest {
    data: String,
    fragment: Option<String>,
    scripting: Option<bool>,
    document: String,
}

/// Splits a `.dat` file into tests: each starts at a `#description` or
/// `#data` line that is the first line of the file or follows an empty line.
fn parse_dat(text: &str) -> Vec<TreeTest> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut starts = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let header = *line == "#data" || *line == "#description";
        if header && (i == 0 || lines.get(i - 1).is_some_and(|l| l.is_empty())) {
            starts.push(i);
        }
    }
    let mut tests = Vec::new();
    for (n, &start) in starts.iter().enumerate() {
        let end = starts.get(n + 1).copied().unwrap_or(lines.len());
        let block = lines.get(start..end).unwrap_or_default();
        let mut test = TreeTest::default();
        let mut section = "";
        let mut data_lines = Vec::new();
        let mut document_lines = Vec::new();
        for line in block {
            match *line {
                "#description" | "#data" | "#errors" | "#new-errors" | "#document-fragment"
                | "#document"
                    if section != "#document" && section != "#data" || *line == "#errors" =>
                {
                    section = line;
                    continue;
                }
                "#script-on" if section != "#document" && section != "#data" => {
                    test.scripting = Some(true);
                    continue;
                }
                "#script-off" if section != "#document" && section != "#data" => {
                    test.scripting = Some(false);
                    continue;
                }
                _ => {}
            }
            match section {
                "#data" => data_lines.push(*line),
                "#document-fragment" => test.fragment = Some((*line).to_string()),
                "#document" => document_lines.push(*line),
                _ => {}
            }
        }
        test.data = data_lines.join("\n");
        test.document = document_lines.join("\n").trim_end_matches('\n').to_string();
        tests.push(test);
    }
    tests
}

fn fragment_context(spec: &str) -> FragmentContext {
    let (namespace, name) = if let Some(name) = spec.strip_prefix("svg ") {
        (Namespace::Svg, name)
    } else if let Some(name) = spec.strip_prefix("math ") {
        (Namespace::MathMl, name)
    } else {
        (Namespace::Html, spec)
    };
    FragmentContext {
        namespace,
        local_name: name.to_string(),
        attributes: Vec::<Attribute>::new(),
        quirks_mode: QuirksMode::NoQuirks,
    }
}

fn run_tree_test(test: &TreeTest, scripting: bool) -> bool {
    let options = ParseOptions { scripting };
    let actual = match &test.fragment {
        Some(context) => {
            let parsed = parse_fragment(&test.data, &fragment_context(context), &options);
            dump_tree(&parsed.document, parsed.fragment)
        }
        None => {
            let parsed = parse_document(&test.data, &options);
            dump_tree(&parsed.document, parsed.document.root())
        }
    };
    let ok = actual.trim_end_matches('\n') == test.document;
    if !ok && std::env::var_os("XTASK_SHOW").is_some() {
        eprintln!(
            "DATA {:?} context={:?} scripting={scripting}\nEXPECTED\n{}\nACTUAL\n{}",
            test.data, test.fragment, test.document, actual
        );
    }
    ok
}

/// One case per `#data` block in the `*.dat` files.
fn tree_construction_cases(dir: &Path) -> Result<Vec<Case>> {
    let mut cases = Vec::new();
    for file in files_with_extension(dir, "dat")? {
        let text = std::fs::read_to_string(&file)
            .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        let file_name = display_name(&file);
        let declared = text.lines().filter(|line| *line == "#data").count();
        let tests = parse_dat(&text);
        if tests.len() != declared {
            return Err(format!(
                "{file_name}: found {} tests but {declared} #data lines",
                tests.len()
            ));
        }
        for (index, test) in tests.iter().enumerate() {
            let passed = match test.scripting {
                Some(scripting) => run_tree_test(test, scripting),
                None => run_tree_test(test, false) && run_tree_test(test, true),
            };
            cases.push(Case {
                id: format!("{file_name}:{index}"),
                passed,
            });
        }
    }
    Ok(cases)
}

fn files_with_extension(dir: &Path, extension: &str) -> Result<Vec<PathBuf>> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == extension))
        .collect();
    files.sort();
    Ok(files)
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn render_scorecard(results: &[SuiteResult]) -> String {
    let mut out = String::new();
    out.push_str("# Conformance scorecard\n\n");
    out.push_str(
        "Generated by `cargo xtask conformance`. Do not edit by hand: CI fails if this file \
         is out of date. Corpus commits are pinned in `conformance/corpora.json`.\n\n",
    );
    out.push_str("| Suite | Passed | Total | Pass rate |\n|---|---:|---:|---:|\n");
    for r in results {
        let rate = if r.total == 0 {
            0.0
        } else {
            r.passed as f64 * 100.0 / r.total as f64
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {rate:.1}% |",
            r.title, r.passed, r.total
        );
    }
    out.push_str(
        "\nThe rest of web-platform-tests (reftests and testharness.js tests) is not run yet: \
         it needs the M1 rendering pipeline and, later, a JavaScript engine.\n",
    );
    out
}

fn read_baseline(path: &Path) -> Result<BTreeMap<String, u64>> {
    let json = read_json(path)?;
    let object = json.as_object().ok_or("baseline.json must be an object")?;
    object
        .iter()
        .map(|(k, v)| {
            v.as_u64()
                .map(|n| (k.clone(), n))
                .ok_or_else(|| format!("baseline value for {k} must be a number"))
        })
        .collect()
}

/// The committed baseline must match the results exactly: lower means a
/// regression, higher means the baseline wasn't updated (and later losses of
/// those passes would go unnoticed). Every baseline suite must still exist.
fn check_baseline(results: &[SuiteResult], baseline: &BTreeMap<String, u64>) -> Result<()> {
    let mut problems = missing_suites(results, baseline);
    for r in results {
        let expected = baseline.get(r.id).copied().unwrap_or(0);
        if r.passed < expected {
            problems.push(format!(
                "{}: {} passed, baseline is {expected}",
                r.id, r.passed
            ));
        } else if r.passed > expected {
            problems.push(format!(
                "{}: improved from {expected} to {}; run `cargo xtask conformance --update-baseline` \
                 and commit the result",
                r.id, r.passed
            ));
        }
    }
    report(problems, "conformance: matches baseline")
}

/// Guards against a PR lowering the committed baseline itself: results may not
/// drop below the target branch's baseline.
fn check_against_base(results: &[SuiteResult], base: &BTreeMap<String, u64>) -> Result<()> {
    let mut problems = missing_suites(results, base);
    for r in results {
        let expected = base.get(r.id).copied().unwrap_or(0);
        if r.passed < expected {
            problems.push(format!(
                "{}: {} passed, target branch baseline is {expected}",
                r.id, r.passed
            ));
        }
    }
    report(
        problems,
        "conformance: no regressions against the target branch",
    )
}

fn missing_suites(results: &[SuiteResult], baseline: &BTreeMap<String, u64>) -> Vec<String> {
    baseline
        .keys()
        .filter(|id| !results.iter().any(|r| r.id == id.as_str()))
        .map(|id| format!("{id}: suite is in the baseline but produced no results"))
        .collect()
}

fn report(problems: Vec<String>, ok_message: &str) -> Result<()> {
    if problems.is_empty() {
        println!("{ok_message}");
        Ok(())
    } else {
        Err(format!(
            "conformance check failed:\n  {}",
            problems.join("\n  ")
        ))
    }
}

fn write_baseline(path: &Path, results: &[SuiteResult]) -> Result<()> {
    let map: serde_json::Map<String, Value> = results
        .iter()
        .map(|r| (r.id.to_string(), Value::from(r.passed)))
        .collect();
    let text = serde_json::to_string_pretty(&Value::Object(map))
        .map_err(|e| format!("cannot serialize baseline: {e}"))?;
    std::fs::write(path, text + "\n").map_err(|e| format!("cannot write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(id: &'static str, passed: u64) -> SuiteResult {
        SuiteResult {
            id,
            title: id,
            passed,
            total: 10,
        }
    }

    #[test]
    fn regression_fails_the_check() {
        let baseline = BTreeMap::from([("a".to_string(), 5)]);
        assert!(check_baseline(&[result("a", 4)], &baseline).is_err());
    }

    #[test]
    fn matching_baseline_passes_the_check() {
        let baseline = BTreeMap::from([("a".to_string(), 5)]);
        assert!(check_baseline(&[result("a", 5)], &baseline).is_ok());
    }

    #[test]
    fn stale_baseline_fails_the_check() {
        let baseline = BTreeMap::from([("a".to_string(), 5)]);
        assert!(check_baseline(&[result("a", 6)], &baseline).is_err());
    }

    #[test]
    fn missing_suite_fails_the_check() {
        let baseline = BTreeMap::from([("a".to_string(), 5), ("b".to_string(), 5)]);
        assert!(check_baseline(&[result("a", 5)], &baseline).is_err());
        assert!(check_against_base(&[result("a", 5)], &baseline).is_err());
    }

    #[test]
    fn lowering_below_target_branch_fails() {
        let base = BTreeMap::from([("a".to_string(), 5)]);
        assert!(check_against_base(&[result("a", 4)], &base).is_err());
        assert!(check_against_base(&[result("a", 5)], &base).is_ok());
        assert!(check_against_base(&[result("a", 6)], &base).is_ok());
    }
}
