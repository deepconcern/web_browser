//! The HTML tokenizer (HTML §13.2.5).
//!
//! A direct implementation of the tokenizer state machine. The tokenizer
//! works on the whole input at once (it is preprocessed up front: newlines
//! are normalized, HTML §13.2.3.5) and hands out one token at a time through
//! [`Tokenizer::next_token`], so that the tree builder can switch its state
//! between tokens, as the spec requires.

use std::collections::{HashSet, VecDeque};

use crate::ParseError;
use crate::entities::{ENTITIES, LONGEST_NAME};

/// The tokenizer states that the tree builder (or a test) can switch to from
/// outside (HTML §13.2.5.1–§13.2.5.5 and §13.2.5.69).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    /// The data state.
    Data,
    /// The RCDATA state (`title`, `textarea`).
    Rcdata,
    /// The RAWTEXT state (`style`, `xmp`, `iframe`, ...).
    Rawtext,
    /// The script data state.
    ScriptData,
    /// The PLAINTEXT state.
    Plaintext,
    /// The CDATA section state.
    CdataSection,
}

/// A DOCTYPE token.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Doctype {
    /// The name, or `None` when missing.
    pub name: Option<String>,
    /// The public identifier, or `None` when missing.
    pub public_id: Option<String>,
    /// The system identifier, or `None` when missing.
    pub system_id: Option<String>,
    /// The force-quirks flag.
    pub force_quirks: bool,
}

/// An attribute on a tag token.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Attribute {
    /// The attribute's name (lowercased by the tokenizer).
    pub name: String,
    /// The attribute's value.
    pub value: String,
}

/// A start or end tag token.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Tag {
    /// The tag name (lowercased by the tokenizer).
    pub name: String,
    /// The attributes, in source order, without duplicates.
    pub attributes: Vec<Attribute>,
    /// The self-closing flag.
    pub self_closing: bool,
}

impl Tag {
    /// The value of the attribute called `name`, if present.
    #[must_use]
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.value.as_str())
    }
}

/// A token produced by the tokenizer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Token {
    /// A DOCTYPE.
    Doctype(Doctype),
    /// A start tag.
    StartTag(Tag),
    /// An end tag.
    EndTag(Tag),
    /// A comment with its data.
    Comment(String),
    /// A processing instruction.
    ProcessingInstruction {
        /// The target.
        target: String,
        /// The data.
        data: String,
    },
    /// One character.
    Character(char),
    /// The end of the input.
    Eof,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum S {
    Data,
    Rcdata,
    Rawtext,
    ScriptData,
    Plaintext,
    TagOpen,
    EndTagOpen,
    TagName,
    RcdataLessThan,
    RcdataEndTagOpen,
    RcdataEndTagName,
    RawtextLessThan,
    RawtextEndTagOpen,
    RawtextEndTagName,
    ScriptDataLessThan,
    ScriptDataEndTagOpen,
    ScriptDataEndTagName,
    ScriptDataEscapeStart,
    ScriptDataEscapeStartDash,
    ScriptDataEscaped,
    ScriptDataEscapedDash,
    ScriptDataEscapedDashDash,
    ScriptDataEscapedLessThan,
    ScriptDataEscapedEndTagOpen,
    ScriptDataEscapedEndTagName,
    ScriptDataDoubleEscapeStart,
    ScriptDataDoubleEscaped,
    ScriptDataDoubleEscapedDash,
    ScriptDataDoubleEscapedDashDash,
    ScriptDataDoubleEscapedLessThan,
    ScriptDataDoubleEscapeEnd,
    BeforeAttributeName,
    AttributeName,
    AfterAttributeName,
    BeforeAttributeValue,
    AttributeValueDoubleQuoted,
    AttributeValueSingleQuoted,
    AttributeValueUnquoted,
    AfterAttributeValueQuoted,
    SelfClosingStartTag,
    BogusComment,
    MarkupDeclarationOpen,
    CommentStart,
    CommentStartDash,
    Comment,
    CommentLessThan,
    CommentLessThanBang,
    CommentLessThanBangDash,
    CommentLessThanBangDashDash,
    CommentEndDash,
    CommentEnd,
    CommentEndBang,
    Doctype,
    BeforeDoctypeName,
    DoctypeName,
    AfterDoctypeName,
    AfterDoctypePublicKeyword,
    BeforeDoctypePublicIdentifier,
    DoctypePublicIdentifierDoubleQuoted,
    DoctypePublicIdentifierSingleQuoted,
    AfterDoctypePublicIdentifier,
    BetweenDoctypePublicAndSystemIdentifiers,
    AfterDoctypeSystemKeyword,
    BeforeDoctypeSystemIdentifier,
    DoctypeSystemIdentifierDoubleQuoted,
    DoctypeSystemIdentifierSingleQuoted,
    AfterDoctypeSystemIdentifier,
    BogusDoctype,
    CdataSection,
    CdataSectionBracket,
    CdataSectionEnd,
    ProcessingInstructionOpen,
    ProcessingInstructionTarget,
    AfterProcessingInstructionTarget,
    ProcessingInstructionData,
    ProcessingInstructionQuestionable,
    CharacterReference,
    NamedCharacterReference,
    AmbiguousAmpersand,
    NumericCharacterReference,
    HexadecimalCharacterReferenceStart,
    HexadecimalCharacterReference,
    DecimalCharacterReference,
    NumericCharacterReferenceEnd,
}

/// The HTML tokenizer.
#[derive(Debug)]
pub struct Tokenizer {
    input: Vec<char>,
    pos: usize,
    checked: usize,
    line: u32,
    column: u32,
    current: Option<char>,
    reconsume: bool,
    state: S,
    return_state: S,
    tag: Tag,
    tag_is_end: bool,
    attribute: Option<Attribute>,
    /// Whether the current attribute repeats an earlier name on the tag and
    /// will be dropped.
    attribute_is_duplicate: bool,
    /// The attribute names on the current tag, for linear-time duplicate
    /// detection.
    attribute_names: HashSet<String>,
    comment: String,
    doctype: Doctype,
    pi_target: String,
    pi_data: String,
    temp: String,
    char_ref_code: u32,
    last_start_tag: Option<String>,
    queue: VecDeque<Token>,
    errors: Vec<ParseError>,
    cdata_allowed: bool,
    finished: bool,
}

impl Tokenizer {
    /// Creates a tokenizer for `input`, starting in the data state. The
    /// input is preprocessed first: CR LF and lone CR become LF (HTML
    /// §13.2.3.5).
    #[must_use]
    pub fn new(input: &str) -> Tokenizer {
        let mut chars = Vec::with_capacity(input.len());
        let mut iter = input.chars().peekable();
        while let Some(c) = iter.next() {
            if c == '\r' {
                if iter.peek() == Some(&'\n') {
                    iter.next();
                }
                chars.push('\n');
            } else {
                chars.push(c);
            }
        }
        Tokenizer {
            input: chars,
            pos: 0,
            checked: 0,
            line: 1,
            column: 0,
            current: None,
            reconsume: false,
            state: S::Data,
            return_state: S::Data,
            tag: Tag::default(),
            tag_is_end: false,
            attribute: None,
            attribute_is_duplicate: false,
            attribute_names: HashSet::new(),
            comment: String::new(),
            doctype: Doctype::default(),
            pi_target: String::new(),
            pi_data: String::new(),
            temp: String::new(),
            char_ref_code: 0,
            last_start_tag: None,
            queue: VecDeque::new(),
            errors: Vec::new(),
            cdata_allowed: false,
            finished: false,
        }
    }

    /// Switches the tokenizer to `state` (used by the tree builder after
    /// `script`, `title`, `plaintext` and similar start tags).
    pub fn set_state(&mut self, state: State) {
        self.state = match state {
            State::Data => S::Data,
            State::Rcdata => S::Rcdata,
            State::Rawtext => S::Rawtext,
            State::ScriptData => S::ScriptData,
            State::Plaintext => S::Plaintext,
            State::CdataSection => S::CdataSection,
        };
    }

    /// Sets "the tag name of the last start tag to have been emitted", which
    /// decides what an appropriate end tag token is.
    pub fn set_last_start_tag(&mut self, name: Option<&str>) {
        self.last_start_tag = name.map(str::to_string);
    }

    /// Tells the tokenizer whether there is an adjusted current node that is
    /// not in the HTML namespace, which makes `<![CDATA[` start a CDATA
    /// section (HTML §13.2.5.42).
    pub fn set_cdata_allowed(&mut self, allowed: bool) {
        self.cdata_allowed = allowed;
    }

    /// Takes the parse errors reported so far.
    pub fn take_errors(&mut self) -> Vec<ParseError> {
        std::mem::take(&mut self.errors)
    }

    /// The line and column (both 1-based) of the current input character.
    #[must_use]
    pub fn position(&self) -> (u32, u32) {
        (self.line, self.column)
    }

    /// Returns the next token. After the input is exhausted it returns
    /// [`Token::Eof`] forever.
    pub fn next_token(&mut self) -> Token {
        loop {
            if let Some(token) = self.queue.pop_front() {
                return token;
            }
            if self.finished {
                return Token::Eof;
            }
            self.step();
        }
    }

    // ----- Input ----------------------------------------------------------

    fn consume(&mut self) -> Option<char> {
        if self.reconsume {
            self.reconsume = false;
            return self.current;
        }
        let c = self.input.get(self.pos).copied();
        if let Some(ch) = c {
            self.pos += 1;
            if ch == '\n' {
                self.line += 1;
                self.column = 0;
            } else {
                self.column += 1;
            }
            if self.pos > self.checked {
                self.checked = self.pos;
                self.check_input_character(ch);
            }
        }
        self.current = c;
        c
    }

    /// Parse errors from preprocessing the input stream (HTML §13.2.3.5),
    /// reported when the character is first consumed.
    fn check_input_character(&mut self, c: char) {
        let cp = u32::from(c);
        if is_noncharacter(cp) {
            self.error("noncharacter-in-input-stream");
        } else if is_control(cp) && !is_ascii_whitespace(c) && c != '\0' {
            self.error("control-character-in-input-stream");
        }
    }

    fn reconsume_in(&mut self, state: S) {
        self.reconsume = true;
        self.state = state;
    }

    /// The input starting at the next input character.
    fn remaining(&self) -> &[char] {
        let start = if self.reconsume {
            self.pos.saturating_sub(1)
        } else {
            self.pos
        };
        self.input.get(start..).unwrap_or_default()
    }

    /// Whether the next characters match `word`, ASCII case-insensitively
    /// when `case_insensitive` is set.
    fn next_chars_match(&self, word: &str, case_insensitive: bool) -> bool {
        let rest = self.remaining();
        let mut n = 0;
        for (expected, actual) in word.chars().zip(rest.iter()) {
            let ok = if case_insensitive {
                expected.eq_ignore_ascii_case(actual)
            } else {
                expected == *actual
            };
            if !ok {
                return false;
            }
            n += 1;
        }
        n == word.chars().count()
    }

    fn consume_n(&mut self, n: usize) {
        for _ in 0..n {
            self.consume();
        }
    }

    // ----- Output ---------------------------------------------------------

    fn error(&mut self, code: &'static str) {
        self.errors.push(ParseError {
            code,
            line: self.line,
            column: self.column,
        });
    }

    fn emit_char(&mut self, c: char) {
        self.queue.push_back(Token::Character(c));
    }

    fn emit_str(&mut self, s: &str) {
        for c in s.chars() {
            self.emit_char(c);
        }
    }

    fn emit_eof(&mut self) {
        self.queue.push_back(Token::Eof);
        self.finished = true;
    }

    fn new_tag(&mut self, end: bool) {
        self.tag = Tag::default();
        self.tag_is_end = end;
        self.attribute = None;
        self.attribute_is_duplicate = false;
        self.attribute_names.clear();
    }

    fn start_attribute(&mut self, name: &str) {
        self.finish_attribute();
        self.attribute = Some(Attribute {
            name: name.to_string(),
            value: String::new(),
        });
        self.attribute_is_duplicate = false;
    }

    /// Runs when the tokenizer leaves the attribute name state (§13.2.5.33):
    /// a name already on the token is a duplicate-attribute parse error, and
    /// that attribute (with the value still to come) is discarded.
    fn leave_attribute_name(&mut self) {
        if let Some(attribute) = &self.attribute {
            if self.attribute_names.contains(&attribute.name) {
                self.attribute_is_duplicate = true;
                self.error("duplicate-attribute");
            } else {
                self.attribute_names.insert(attribute.name.clone());
            }
        }
    }

    fn finish_attribute(&mut self) {
        if let Some(attribute) = self.attribute.take()
            && !std::mem::take(&mut self.attribute_is_duplicate)
        {
            self.tag.attributes.push(attribute);
        }
    }

    fn attribute_name_push(&mut self, c: char) {
        if let Some(attribute) = &mut self.attribute {
            attribute.name.push(c);
        }
    }

    fn attribute_value_push(&mut self, c: char) {
        if let Some(attribute) = &mut self.attribute {
            attribute.value.push(c);
        }
    }

    fn emit_tag(&mut self) {
        self.finish_attribute();
        let tag = std::mem::take(&mut self.tag);
        if self.tag_is_end {
            if !tag.attributes.is_empty() {
                self.error("end-tag-with-attributes");
            }
            if tag.self_closing {
                self.error("end-tag-with-trailing-solidus");
            }
            self.queue.push_back(Token::EndTag(tag));
        } else {
            self.last_start_tag = Some(tag.name.clone());
            self.queue.push_back(Token::StartTag(tag));
        }
    }

    fn emit_comment(&mut self) {
        let data = std::mem::take(&mut self.comment);
        self.queue.push_back(Token::Comment(data));
    }

    fn emit_doctype(&mut self) {
        let doctype = std::mem::take(&mut self.doctype);
        self.queue.push_back(Token::Doctype(doctype));
    }

    fn emit_processing_instruction(&mut self) {
        let target = std::mem::take(&mut self.pi_target);
        let data = std::mem::take(&mut self.pi_data);
        self.queue
            .push_back(Token::ProcessingInstruction { target, data });
    }

    fn is_appropriate_end_tag(&self) -> bool {
        self.tag_is_end && self.last_start_tag.as_deref() == Some(self.tag.name.as_str())
    }

    fn doctype_eof(&mut self) {
        self.error("eof-in-doctype");
        self.doctype.force_quirks = true;
        self.emit_doctype();
        self.emit_eof();
    }

    fn doctype_name_push(&mut self, c: char) {
        self.doctype.name.get_or_insert_with(String::new).push(c);
    }

    fn doctype_public_push(&mut self, c: char) {
        self.doctype
            .public_id
            .get_or_insert_with(String::new)
            .push(c);
    }

    fn doctype_system_push(&mut self, c: char) {
        self.doctype
            .system_id
            .get_or_insert_with(String::new)
            .push(c);
    }

    /// "Convert the temporary buffer to a comment" (HTML §13.2.5).
    fn temp_to_comment(&mut self) {
        self.comment = format!("?{}", self.temp);
    }

    fn char_ref_in_attribute(&self) -> bool {
        matches!(
            self.return_state,
            S::AttributeValueDoubleQuoted
                | S::AttributeValueSingleQuoted
                | S::AttributeValueUnquoted
        )
    }

    /// "Flush code points consumed as a character reference".
    fn flush_char_ref(&mut self) {
        let temp = std::mem::take(&mut self.temp);
        if self.char_ref_in_attribute() {
            if let Some(attribute) = &mut self.attribute {
                attribute.value.push_str(&temp);
            }
        } else {
            self.emit_str(&temp);
        }
    }

    // ----- The state machine ---------------------------------------------

    #[allow(clippy::too_many_lines)]
    fn step(&mut self) {
        match self.state {
            // §13.2.5.1
            S::Data => match self.consume() {
                Some('&') => {
                    self.return_state = S::Data;
                    self.state = S::CharacterReference;
                }
                Some('<') => self.state = S::TagOpen,
                Some('\0') => {
                    self.error("unexpected-null-character");
                    self.emit_char('\0');
                }
                None => self.emit_eof(),
                Some(c) => self.emit_char(c),
            },
            // §13.2.5.2
            S::Rcdata => match self.consume() {
                Some('&') => {
                    self.return_state = S::Rcdata;
                    self.state = S::CharacterReference;
                }
                Some('<') => self.state = S::RcdataLessThan,
                Some('\0') => {
                    self.error("unexpected-null-character");
                    self.emit_char('\u{FFFD}');
                }
                None => self.emit_eof(),
                Some(c) => self.emit_char(c),
            },
            // §13.2.5.3
            S::Rawtext => match self.consume() {
                Some('<') => self.state = S::RawtextLessThan,
                Some('\0') => {
                    self.error("unexpected-null-character");
                    self.emit_char('\u{FFFD}');
                }
                None => self.emit_eof(),
                Some(c) => self.emit_char(c),
            },
            // §13.2.5.4
            S::ScriptData => match self.consume() {
                Some('<') => self.state = S::ScriptDataLessThan,
                Some('\0') => {
                    self.error("unexpected-null-character");
                    self.emit_char('\u{FFFD}');
                }
                None => self.emit_eof(),
                Some(c) => self.emit_char(c),
            },
            // §13.2.5.5
            S::Plaintext => match self.consume() {
                Some('\0') => {
                    self.error("unexpected-null-character");
                    self.emit_char('\u{FFFD}');
                }
                None => self.emit_eof(),
                Some(c) => self.emit_char(c),
            },
            // §13.2.5.6
            S::TagOpen => match self.consume() {
                Some('!') => self.state = S::MarkupDeclarationOpen,
                Some('/') => self.state = S::EndTagOpen,
                Some(c) if c.is_ascii_alphabetic() => {
                    self.new_tag(false);
                    self.reconsume_in(S::TagName);
                }
                Some('?') => {
                    self.temp.clear();
                    self.state = S::ProcessingInstructionOpen;
                }
                None => {
                    self.error("eof-before-tag-name");
                    self.emit_char('<');
                    self.emit_eof();
                }
                Some(_) => {
                    self.error("invalid-first-character-of-tag-name");
                    self.emit_char('<');
                    self.reconsume_in(S::Data);
                }
            },
            // §13.2.5.7
            S::EndTagOpen => match self.consume() {
                Some(c) if c.is_ascii_alphabetic() => {
                    self.new_tag(true);
                    self.reconsume_in(S::TagName);
                }
                Some('>') => {
                    self.error("missing-end-tag-name");
                    self.state = S::Data;
                }
                None => {
                    self.error("eof-before-tag-name");
                    self.emit_str("</");
                    self.emit_eof();
                }
                Some(_) => {
                    self.error("invalid-first-character-of-tag-name");
                    self.comment.clear();
                    self.reconsume_in(S::BogusComment);
                }
            },
            // §13.2.5.8
            S::TagName => match self.consume() {
                Some('\t' | '\n' | '\u{c}' | ' ') => self.state = S::BeforeAttributeName,
                Some('/') => self.state = S::SelfClosingStartTag,
                Some('>') => {
                    self.state = S::Data;
                    self.emit_tag();
                }
                Some('\0') => {
                    self.error("unexpected-null-character");
                    self.tag.name.push('\u{FFFD}');
                }
                None => {
                    self.error("eof-in-tag");
                    self.emit_eof();
                }
                Some(c) => self.tag.name.push(c.to_ascii_lowercase()),
            },
            // §13.2.5.9–11
            S::RcdataLessThan => self.text_less_than(S::Rcdata, S::RcdataEndTagOpen),
            S::RcdataEndTagOpen => self.text_end_tag_open(S::Rcdata, S::RcdataEndTagName),
            S::RcdataEndTagName => self.text_end_tag_name(S::Rcdata),
            // §13.2.5.12–14
            S::RawtextLessThan => self.text_less_than(S::Rawtext, S::RawtextEndTagOpen),
            S::RawtextEndTagOpen => self.text_end_tag_open(S::Rawtext, S::RawtextEndTagName),
            S::RawtextEndTagName => self.text_end_tag_name(S::Rawtext),
            // §13.2.5.15
            S::ScriptDataLessThan => match self.consume() {
                Some('/') => {
                    self.temp.clear();
                    self.state = S::ScriptDataEndTagOpen;
                }
                Some('!') => {
                    self.state = S::ScriptDataEscapeStart;
                    self.emit_str("<!");
                }
                _ => {
                    self.emit_char('<');
                    self.reconsume_in(S::ScriptData);
                }
            },
            // §13.2.5.16–17
            S::ScriptDataEndTagOpen => {
                self.text_end_tag_open(S::ScriptData, S::ScriptDataEndTagName);
            }
            S::ScriptDataEndTagName => self.text_end_tag_name(S::ScriptData),
            // §13.2.5.18
            S::ScriptDataEscapeStart => match self.consume() {
                Some('-') => {
                    self.state = S::ScriptDataEscapeStartDash;
                    self.emit_char('-');
                }
                _ => self.reconsume_in(S::ScriptData),
            },
            // §13.2.5.19
            S::ScriptDataEscapeStartDash => match self.consume() {
                Some('-') => {
                    self.state = S::ScriptDataEscapedDashDash;
                    self.emit_char('-');
                }
                _ => self.reconsume_in(S::ScriptData),
            },
            // §13.2.5.20
            S::ScriptDataEscaped => match self.consume() {
                Some('-') => {
                    self.state = S::ScriptDataEscapedDash;
                    self.emit_char('-');
                }
                Some('<') => self.state = S::ScriptDataEscapedLessThan,
                Some('\0') => {
                    self.error("unexpected-null-character");
                    self.emit_char('\u{FFFD}');
                }
                None => {
                    self.error("eof-in-script-html-comment-like-text");
                    self.emit_eof();
                }
                Some(c) => self.emit_char(c),
            },
            // §13.2.5.21
            S::ScriptDataEscapedDash => match self.consume() {
                Some('-') => {
                    self.state = S::ScriptDataEscapedDashDash;
                    self.emit_char('-');
                }
                Some('<') => self.state = S::ScriptDataEscapedLessThan,
                Some('\0') => {
                    self.error("unexpected-null-character");
                    self.state = S::ScriptDataEscaped;
                    self.emit_char('\u{FFFD}');
                }
                None => {
                    self.error("eof-in-script-html-comment-like-text");
                    self.emit_eof();
                }
                Some(c) => {
                    self.state = S::ScriptDataEscaped;
                    self.emit_char(c);
                }
            },
            // §13.2.5.22
            S::ScriptDataEscapedDashDash => match self.consume() {
                Some('-') => self.emit_char('-'),
                Some('<') => self.state = S::ScriptDataEscapedLessThan,
                Some('>') => {
                    self.state = S::ScriptData;
                    self.emit_char('>');
                }
                Some('\0') => {
                    self.error("unexpected-null-character");
                    self.state = S::ScriptDataEscaped;
                    self.emit_char('\u{FFFD}');
                }
                None => {
                    self.error("eof-in-script-html-comment-like-text");
                    self.emit_eof();
                }
                Some(c) => {
                    self.state = S::ScriptDataEscaped;
                    self.emit_char(c);
                }
            },
            // §13.2.5.23
            S::ScriptDataEscapedLessThan => match self.consume() {
                Some('/') => {
                    self.temp.clear();
                    self.state = S::ScriptDataEscapedEndTagOpen;
                }
                Some(c) if c.is_ascii_alphabetic() => {
                    self.temp.clear();
                    self.emit_char('<');
                    self.reconsume_in(S::ScriptDataDoubleEscapeStart);
                }
                _ => {
                    self.emit_char('<');
                    self.reconsume_in(S::ScriptDataEscaped);
                }
            },
            // §13.2.5.24–25
            S::ScriptDataEscapedEndTagOpen => {
                self.text_end_tag_open(S::ScriptDataEscaped, S::ScriptDataEscapedEndTagName);
            }
            S::ScriptDataEscapedEndTagName => self.text_end_tag_name(S::ScriptDataEscaped),
            // §13.2.5.26
            S::ScriptDataDoubleEscapeStart => self.double_escape_boundary(
                S::ScriptDataDoubleEscaped,
                S::ScriptDataEscaped,
                S::ScriptDataEscaped,
            ),
            // §13.2.5.27
            S::ScriptDataDoubleEscaped => match self.consume() {
                Some('-') => {
                    self.state = S::ScriptDataDoubleEscapedDash;
                    self.emit_char('-');
                }
                Some('<') => {
                    self.state = S::ScriptDataDoubleEscapedLessThan;
                    self.emit_char('<');
                }
                Some('\0') => {
                    self.error("unexpected-null-character");
                    self.emit_char('\u{FFFD}');
                }
                None => {
                    self.error("eof-in-script-html-comment-like-text");
                    self.emit_eof();
                }
                Some(c) => self.emit_char(c),
            },
            // §13.2.5.28
            S::ScriptDataDoubleEscapedDash => match self.consume() {
                Some('-') => {
                    self.state = S::ScriptDataDoubleEscapedDashDash;
                    self.emit_char('-');
                }
                Some('<') => {
                    self.state = S::ScriptDataDoubleEscapedLessThan;
                    self.emit_char('<');
                }
                Some('\0') => {
                    self.error("unexpected-null-character");
                    self.state = S::ScriptDataDoubleEscaped;
                    self.emit_char('\u{FFFD}');
                }
                None => {
                    self.error("eof-in-script-html-comment-like-text");
                    self.emit_eof();
                }
                Some(c) => {
                    self.state = S::ScriptDataDoubleEscaped;
                    self.emit_char(c);
                }
            },
            // §13.2.5.29
            S::ScriptDataDoubleEscapedDashDash => match self.consume() {
                Some('-') => self.emit_char('-'),
                Some('<') => {
                    self.state = S::ScriptDataDoubleEscapedLessThan;
                    self.emit_char('<');
                }
                Some('>') => {
                    self.state = S::ScriptData;
                    self.emit_char('>');
                }
                Some('\0') => {
                    self.error("unexpected-null-character");
                    self.state = S::ScriptDataDoubleEscaped;
                    self.emit_char('\u{FFFD}');
                }
                None => {
                    self.error("eof-in-script-html-comment-like-text");
                    self.emit_eof();
                }
                Some(c) => {
                    self.state = S::ScriptDataDoubleEscaped;
                    self.emit_char(c);
                }
            },
            // §13.2.5.30
            S::ScriptDataDoubleEscapedLessThan => match self.consume() {
                Some('/') => {
                    self.temp.clear();
                    self.state = S::ScriptDataDoubleEscapeEnd;
                    self.emit_char('/');
                }
                _ => self.reconsume_in(S::ScriptDataDoubleEscaped),
            },
            // §13.2.5.31
            S::ScriptDataDoubleEscapeEnd => self.double_escape_boundary(
                S::ScriptDataEscaped,
                S::ScriptDataDoubleEscaped,
                S::ScriptDataDoubleEscaped,
            ),
            // §13.2.5.32
            S::BeforeAttributeName => match self.consume() {
                Some('\t' | '\n' | '\u{c}' | ' ') => {}
                Some('/' | '>') | None => self.reconsume_in(S::AfterAttributeName),
                Some('=') => {
                    self.error("unexpected-equals-sign-before-attribute-name");
                    self.start_attribute("=");
                    self.state = S::AttributeName;
                }
                Some(_) => {
                    self.start_attribute("");
                    self.reconsume_in(S::AttributeName);
                }
            },
            // §13.2.5.33
            S::AttributeName => match self.consume() {
                Some('\t' | '\n' | '\u{c}' | ' ' | '/' | '>') | None => {
                    self.leave_attribute_name();
                    self.reconsume_in(S::AfterAttributeName);
                }
                Some('=') => {
                    self.leave_attribute_name();
                    self.state = S::BeforeAttributeValue;
                }
                Some('\0') => {
                    self.error("unexpected-null-character");
                    self.attribute_name_push('\u{FFFD}');
                }
                Some(c @ ('"' | '\'' | '<')) => {
                    self.error("unexpected-character-in-attribute-name");
                    self.attribute_name_push(c);
                }
                Some(c) => self.attribute_name_push(c.to_ascii_lowercase()),
            },
            // §13.2.5.34
            S::AfterAttributeName => match self.consume() {
                Some('\t' | '\n' | '\u{c}' | ' ') => {}
                Some('/') => self.state = S::SelfClosingStartTag,
                Some('=') => self.state = S::BeforeAttributeValue,
                Some('>') => {
                    self.state = S::Data;
                    self.emit_tag();
                }
                None => {
                    self.error("eof-in-tag");
                    self.emit_eof();
                }
                Some(_) => {
                    self.start_attribute("");
                    self.reconsume_in(S::AttributeName);
                }
            },
            // §13.2.5.35
            S::BeforeAttributeValue => match self.consume() {
                Some('\t' | '\n' | '\u{c}' | ' ') => {}
                Some('"') => self.state = S::AttributeValueDoubleQuoted,
                Some('\'') => self.state = S::AttributeValueSingleQuoted,
                Some('>') => {
                    self.error("missing-attribute-value");
                    self.state = S::Data;
                    self.emit_tag();
                }
                _ => self.reconsume_in(S::AttributeValueUnquoted),
            },
            // §13.2.5.36–37
            S::AttributeValueDoubleQuoted => {
                self.quoted_attribute_value('"', S::AttributeValueDoubleQuoted)
            }
            S::AttributeValueSingleQuoted => {
                self.quoted_attribute_value('\'', S::AttributeValueSingleQuoted)
            }
            // §13.2.5.38
            S::AttributeValueUnquoted => match self.consume() {
                Some('\t' | '\n' | '\u{c}' | ' ') => self.state = S::BeforeAttributeName,
                Some('&') => {
                    self.return_state = S::AttributeValueUnquoted;
                    self.state = S::CharacterReference;
                }
                Some('>') => {
                    self.state = S::Data;
                    self.emit_tag();
                }
                Some('\0') => {
                    self.error("unexpected-null-character");
                    self.attribute_value_push('\u{FFFD}');
                }
                Some(c @ ('"' | '\'' | '<' | '=' | '`')) => {
                    self.error("unexpected-character-in-unquoted-attribute-value");
                    self.attribute_value_push(c);
                }
                None => {
                    self.error("eof-in-tag");
                    self.emit_eof();
                }
                Some(c) => self.attribute_value_push(c),
            },
            // §13.2.5.39
            S::AfterAttributeValueQuoted => match self.consume() {
                Some('\t' | '\n' | '\u{c}' | ' ') => self.state = S::BeforeAttributeName,
                Some('/') => self.state = S::SelfClosingStartTag,
                Some('>') => {
                    self.state = S::Data;
                    self.emit_tag();
                }
                None => {
                    self.error("eof-in-tag");
                    self.emit_eof();
                }
                Some(_) => {
                    self.error("missing-whitespace-between-attributes");
                    self.reconsume_in(S::BeforeAttributeName);
                }
            },
            // §13.2.5.40
            S::SelfClosingStartTag => match self.consume() {
                Some('>') => {
                    self.tag.self_closing = true;
                    self.state = S::Data;
                    self.emit_tag();
                }
                None => {
                    self.error("eof-in-tag");
                    self.emit_eof();
                }
                Some(_) => {
                    self.error("unexpected-solidus-in-tag");
                    self.reconsume_in(S::BeforeAttributeName);
                }
            },
            // §13.2.5.41
            S::BogusComment => match self.consume() {
                Some('>') => {
                    self.state = S::Data;
                    self.emit_comment();
                }
                None => {
                    self.emit_comment();
                    self.emit_eof();
                }
                Some('\0') => {
                    self.error("unexpected-null-character");
                    self.comment.push('\u{FFFD}');
                }
                Some(c) => self.comment.push(c),
            },
            // §13.2.5.42
            S::MarkupDeclarationOpen => {
                if self.next_chars_match("--", false) {
                    self.consume_n(2);
                    self.comment.clear();
                    self.state = S::CommentStart;
                } else if self.next_chars_match("doctype", true) {
                    self.consume_n(7);
                    self.state = S::Doctype;
                } else if self.next_chars_match("[CDATA[", false) {
                    self.consume_n(7);
                    if self.cdata_allowed {
                        self.state = S::CdataSection;
                    } else {
                        self.error("cdata-in-html-content");
                        self.comment = "[CDATA[".to_string();
                        self.state = S::BogusComment;
                    }
                } else {
                    self.error("incorrectly-opened-comment");
                    self.comment.clear();
                    self.state = S::BogusComment;
                }
            }
            // §13.2.5.43
            S::CommentStart => match self.consume() {
                Some('-') => self.state = S::CommentStartDash,
                Some('>') => {
                    self.error("abrupt-closing-of-empty-comment");
                    self.state = S::Data;
                    self.emit_comment();
                }
                _ => self.reconsume_in(S::Comment),
            },
            // §13.2.5.44
            S::CommentStartDash => match self.consume() {
                Some('-') => self.state = S::CommentEnd,
                Some('>') => {
                    self.error("abrupt-closing-of-empty-comment");
                    self.state = S::Data;
                    self.emit_comment();
                }
                None => self.comment_eof(),
                Some(_) => {
                    self.comment.push('-');
                    self.reconsume_in(S::Comment);
                }
            },
            // §13.2.5.45
            S::Comment => match self.consume() {
                Some('<') => {
                    self.comment.push('<');
                    self.state = S::CommentLessThan;
                }
                Some('-') => self.state = S::CommentEndDash,
                Some('\0') => {
                    self.error("unexpected-null-character");
                    self.comment.push('\u{FFFD}');
                }
                None => self.comment_eof(),
                Some(c) => self.comment.push(c),
            },
            // §13.2.5.46
            S::CommentLessThan => match self.consume() {
                Some('!') => {
                    self.comment.push('!');
                    self.state = S::CommentLessThanBang;
                }
                Some('<') => self.comment.push('<'),
                _ => self.reconsume_in(S::Comment),
            },
            // §13.2.5.47
            S::CommentLessThanBang => match self.consume() {
                Some('-') => self.state = S::CommentLessThanBangDash,
                _ => self.reconsume_in(S::Comment),
            },
            // §13.2.5.48
            S::CommentLessThanBangDash => match self.consume() {
                Some('-') => self.state = S::CommentLessThanBangDashDash,
                _ => self.reconsume_in(S::CommentEndDash),
            },
            // §13.2.5.49
            S::CommentLessThanBangDashDash => match self.consume() {
                Some('>') | None => self.reconsume_in(S::CommentEnd),
                Some(_) => {
                    self.error("nested-comment");
                    self.reconsume_in(S::CommentEnd);
                }
            },
            // §13.2.5.50
            S::CommentEndDash => match self.consume() {
                Some('-') => self.state = S::CommentEnd,
                None => self.comment_eof(),
                Some(_) => {
                    self.comment.push('-');
                    self.reconsume_in(S::Comment);
                }
            },
            // §13.2.5.51
            S::CommentEnd => match self.consume() {
                Some('>') => {
                    self.state = S::Data;
                    self.emit_comment();
                }
                Some('!') => self.state = S::CommentEndBang,
                Some('-') => self.comment.push('-'),
                None => self.comment_eof(),
                Some(_) => {
                    self.comment.push_str("--");
                    self.reconsume_in(S::Comment);
                }
            },
            // §13.2.5.52
            S::CommentEndBang => match self.consume() {
                Some('-') => {
                    self.comment.push_str("--!");
                    self.state = S::CommentEndDash;
                }
                Some('>') => {
                    self.error("incorrectly-closed-comment");
                    self.state = S::Data;
                    self.emit_comment();
                }
                None => self.comment_eof(),
                Some(_) => {
                    self.comment.push_str("--!");
                    self.reconsume_in(S::Comment);
                }
            },
            // §13.2.5.53
            S::Doctype => match self.consume() {
                Some('\t' | '\n' | '\u{c}' | ' ') => self.state = S::BeforeDoctypeName,
                Some('>') => self.reconsume_in(S::BeforeDoctypeName),
                None => {
                    self.doctype = Doctype::default();
                    self.doctype_eof();
                }
                Some(_) => {
                    self.error("missing-whitespace-before-doctype-name");
                    self.reconsume_in(S::BeforeDoctypeName);
                }
            },
            // §13.2.5.54
            S::BeforeDoctypeName => match self.consume() {
                Some('\t' | '\n' | '\u{c}' | ' ') => {}
                Some('\0') => {
                    self.error("unexpected-null-character");
                    self.doctype = Doctype::default();
                    self.doctype_name_push('\u{FFFD}');
                    self.state = S::DoctypeName;
                }
                Some('>') => {
                    self.error("missing-doctype-name");
                    self.doctype = Doctype {
                        force_quirks: true,
                        ..Doctype::default()
                    };
                    self.state = S::Data;
                    self.emit_doctype();
                }
                None => {
                    self.doctype = Doctype::default();
                    self.doctype_eof();
                }
                Some(c) => {
                    self.doctype = Doctype::default();
                    self.doctype_name_push(c.to_ascii_lowercase());
                    self.state = S::DoctypeName;
                }
            },
            // §13.2.5.55
            S::DoctypeName => match self.consume() {
                Some('\t' | '\n' | '\u{c}' | ' ') => self.state = S::AfterDoctypeName,
                Some('>') => {
                    self.state = S::Data;
                    self.emit_doctype();
                }
                Some('\0') => {
                    self.error("unexpected-null-character");
                    self.doctype_name_push('\u{FFFD}');
                }
                None => self.doctype_eof(),
                Some(c) => self.doctype_name_push(c.to_ascii_lowercase()),
            },
            // §13.2.5.56
            S::AfterDoctypeName => match self.consume() {
                Some('\t' | '\n' | '\u{c}' | ' ') => {}
                Some('>') => {
                    self.state = S::Data;
                    self.emit_doctype();
                }
                None => self.doctype_eof(),
                Some(_) => {
                    self.reconsume = true;
                    if self.next_chars_match("public", true) {
                        self.consume_n(6);
                        self.state = S::AfterDoctypePublicKeyword;
                    } else if self.next_chars_match("system", true) {
                        self.consume_n(6);
                        self.state = S::AfterDoctypeSystemKeyword;
                    } else {
                        self.error("invalid-character-sequence-after-doctype-name");
                        self.doctype.force_quirks = true;
                        self.reconsume_in(S::BogusDoctype);
                    }
                }
            },
            // §13.2.5.57
            S::AfterDoctypePublicKeyword => match self.consume() {
                Some('\t' | '\n' | '\u{c}' | ' ') => self.state = S::BeforeDoctypePublicIdentifier,
                Some(q @ ('"' | '\'')) => {
                    self.error("missing-whitespace-after-doctype-public-keyword");
                    self.start_public_id(q);
                }
                Some('>') => self.missing_identifier("missing-doctype-public-identifier"),
                None => self.doctype_eof(),
                Some(_) => self.bogus_doctype("missing-quote-before-doctype-public-identifier"),
            },
            // §13.2.5.58
            S::BeforeDoctypePublicIdentifier => match self.consume() {
                Some('\t' | '\n' | '\u{c}' | ' ') => {}
                Some(q @ ('"' | '\'')) => self.start_public_id(q),
                Some('>') => self.missing_identifier("missing-doctype-public-identifier"),
                None => self.doctype_eof(),
                Some(_) => self.bogus_doctype("missing-quote-before-doctype-public-identifier"),
            },
            // §13.2.5.59–60
            S::DoctypePublicIdentifierDoubleQuoted => self.doctype_identifier('"', true),
            S::DoctypePublicIdentifierSingleQuoted => self.doctype_identifier('\'', true),
            // §13.2.5.61
            S::AfterDoctypePublicIdentifier => match self.consume() {
                Some('\t' | '\n' | '\u{c}' | ' ') => {
                    self.state = S::BetweenDoctypePublicAndSystemIdentifiers;
                }
                Some('>') => {
                    self.state = S::Data;
                    self.emit_doctype();
                }
                Some(q @ ('"' | '\'')) => {
                    self.error("missing-whitespace-between-doctype-public-and-system-identifiers");
                    self.start_system_id(q);
                }
                None => self.doctype_eof(),
                Some(_) => self.bogus_doctype("missing-quote-before-doctype-system-identifier"),
            },
            // §13.2.5.62
            S::BetweenDoctypePublicAndSystemIdentifiers => match self.consume() {
                Some('\t' | '\n' | '\u{c}' | ' ') => {}
                Some('>') => {
                    self.state = S::Data;
                    self.emit_doctype();
                }
                Some(q @ ('"' | '\'')) => self.start_system_id(q),
                None => self.doctype_eof(),
                Some(_) => self.bogus_doctype("missing-quote-before-doctype-system-identifier"),
            },
            // §13.2.5.63
            S::AfterDoctypeSystemKeyword => match self.consume() {
                Some('\t' | '\n' | '\u{c}' | ' ') => self.state = S::BeforeDoctypeSystemIdentifier,
                Some(q @ ('"' | '\'')) => {
                    self.error("missing-whitespace-after-doctype-system-keyword");
                    self.start_system_id(q);
                }
                Some('>') => self.missing_identifier("missing-doctype-system-identifier"),
                None => self.doctype_eof(),
                Some(_) => self.bogus_doctype("missing-quote-before-doctype-system-identifier"),
            },
            // §13.2.5.64
            S::BeforeDoctypeSystemIdentifier => match self.consume() {
                Some('\t' | '\n' | '\u{c}' | ' ') => {}
                Some(q @ ('"' | '\'')) => self.start_system_id(q),
                Some('>') => self.missing_identifier("missing-doctype-system-identifier"),
                None => self.doctype_eof(),
                Some(_) => self.bogus_doctype("missing-quote-before-doctype-system-identifier"),
            },
            // §13.2.5.65–66
            S::DoctypeSystemIdentifierDoubleQuoted => self.doctype_identifier('"', false),
            S::DoctypeSystemIdentifierSingleQuoted => self.doctype_identifier('\'', false),
            // §13.2.5.67
            S::AfterDoctypeSystemIdentifier => match self.consume() {
                Some('\t' | '\n' | '\u{c}' | ' ') => {}
                Some('>') => {
                    self.state = S::Data;
                    self.emit_doctype();
                }
                None => self.doctype_eof(),
                Some(_) => {
                    // This does not set the force-quirks flag.
                    self.error("unexpected-character-after-doctype-system-identifier");
                    self.reconsume_in(S::BogusDoctype);
                }
            },
            // §13.2.5.68
            S::BogusDoctype => match self.consume() {
                Some('>') => {
                    self.state = S::Data;
                    self.emit_doctype();
                }
                Some('\0') => self.error("unexpected-null-character"),
                None => {
                    self.emit_doctype();
                    self.emit_eof();
                }
                Some(_) => {}
            },
            // §13.2.5.69
            S::CdataSection => match self.consume() {
                Some(']') => self.state = S::CdataSectionBracket,
                None => {
                    self.error("eof-in-cdata");
                    self.emit_eof();
                }
                Some(c) => self.emit_char(c),
            },
            // §13.2.5.70
            S::CdataSectionBracket => match self.consume() {
                Some(']') => self.state = S::CdataSectionEnd,
                _ => {
                    self.emit_char(']');
                    self.reconsume_in(S::CdataSection);
                }
            },
            // §13.2.5.71
            S::CdataSectionEnd => match self.consume() {
                Some(']') => self.emit_char(']'),
                Some('>') => self.state = S::Data,
                _ => {
                    self.emit_str("]]");
                    self.reconsume_in(S::CdataSection);
                }
            },
            // Processing instruction open state.
            S::ProcessingInstructionOpen => match self.consume() {
                Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                    self.reconsume_in(S::ProcessingInstructionTarget);
                }
                None => {
                    self.error("eof-in-processing-instruction");
                    self.emit_eof();
                }
                Some(_) => {
                    self.error("invalid-first-character-of-processing-instruction-target");
                    self.temp_to_comment();
                    self.reconsume_in(S::BogusComment);
                }
            },
            // Processing instruction target state.
            S::ProcessingInstructionTarget => match self.consume() {
                Some('\t' | '\n' | '\u{c}' | ' ' | '?' | '>') => {
                    if self.temp.eq_ignore_ascii_case("xml")
                        || self.temp.eq_ignore_ascii_case("xml-stylesheet")
                    {
                        self.error("disallowed-processing-instruction-target");
                        self.temp_to_comment();
                        self.reconsume_in(S::BogusComment);
                    } else {
                        self.pi_target = std::mem::take(&mut self.temp);
                        self.pi_data.clear();
                        self.reconsume_in(S::AfterProcessingInstructionTarget);
                    }
                }
                Some(c) if c.is_ascii_alphanumeric() || c == '-' || c == '_' => self.temp.push(c),
                None => {
                    self.error("eof-in-processing-instruction");
                    self.emit_eof();
                }
                Some(_) => {
                    self.error("invalid-processing-instruction-target");
                    self.temp_to_comment();
                    self.reconsume_in(S::BogusComment);
                }
            },
            // After processing instruction target state.
            S::AfterProcessingInstructionTarget => match self.consume() {
                Some('\t' | '\n' | '\u{c}' | ' ') => {}
                _ => self.reconsume_in(S::ProcessingInstructionData),
            },
            // Processing instruction data state.
            S::ProcessingInstructionData => match self.consume() {
                Some('?') => self.state = S::ProcessingInstructionQuestionable,
                Some('>') => {
                    self.state = S::Data;
                    self.emit_processing_instruction();
                }
                None => {
                    self.error("eof-in-processing-instruction");
                    self.emit_eof();
                }
                Some(c) => self.pi_data.push(c),
            },
            // Processing instruction questionable state.
            S::ProcessingInstructionQuestionable => match self.consume() {
                Some('>') => {
                    self.state = S::Data;
                    self.emit_processing_instruction();
                }
                None => {
                    self.error("eof-in-processing-instruction");
                    self.emit_eof();
                }
                Some(_) => {
                    self.pi_data.push('?');
                    self.reconsume_in(S::ProcessingInstructionData);
                }
            },
            // §13.2.5.72
            S::CharacterReference => {
                self.temp.clear();
                self.temp.push('&');
                match self.consume() {
                    Some(c) if c.is_ascii_alphanumeric() => {
                        self.reconsume_in(S::NamedCharacterReference);
                    }
                    Some('#') => {
                        self.temp.push('#');
                        self.state = S::NumericCharacterReference;
                    }
                    _ => {
                        self.flush_char_ref();
                        self.reconsume_in(self.return_state);
                    }
                }
            }
            // §13.2.5.73
            S::NamedCharacterReference => self.named_character_reference(),
            // §13.2.5.74
            S::AmbiguousAmpersand => match self.consume() {
                Some(c) if c.is_ascii_alphanumeric() => {
                    if self.char_ref_in_attribute() {
                        self.attribute_value_push(c);
                    } else {
                        self.emit_char(c);
                    }
                }
                Some(';') => {
                    self.error("unknown-named-character-reference");
                    self.reconsume_in(self.return_state);
                }
                _ => self.reconsume_in(self.return_state),
            },
            // §13.2.5.75
            S::NumericCharacterReference => {
                self.char_ref_code = 0;
                match self.consume() {
                    Some(c @ ('x' | 'X')) => {
                        self.temp.push(c);
                        self.state = S::HexadecimalCharacterReferenceStart;
                    }
                    Some(c) if c.is_ascii_digit() => {
                        self.reconsume_in(S::DecimalCharacterReference);
                    }
                    _ => {
                        self.error("absence-of-digits-in-numeric-character-reference");
                        self.flush_char_ref();
                        self.reconsume_in(self.return_state);
                    }
                }
            }
            // §13.2.5.76
            S::HexadecimalCharacterReferenceStart => match self.consume() {
                Some(c) if c.is_ascii_hexdigit() => {
                    self.reconsume_in(S::HexadecimalCharacterReference);
                }
                _ => {
                    self.error("absence-of-digits-in-numeric-character-reference");
                    self.flush_char_ref();
                    self.reconsume_in(self.return_state);
                }
            },
            // §13.2.5.77
            S::HexadecimalCharacterReference => match self.consume() {
                Some(c) if c.is_ascii_hexdigit() => {
                    let digit = c.to_digit(16).unwrap_or(0);
                    self.char_ref_code =
                        self.char_ref_code.saturating_mul(16).saturating_add(digit);
                }
                Some(';') => self.state = S::NumericCharacterReferenceEnd,
                _ => {
                    self.error("missing-semicolon-after-character-reference");
                    self.reconsume_in(S::NumericCharacterReferenceEnd);
                }
            },
            // §13.2.5.78
            S::DecimalCharacterReference => match self.consume() {
                Some(c) if c.is_ascii_digit() => {
                    let digit = c.to_digit(10).unwrap_or(0);
                    self.char_ref_code =
                        self.char_ref_code.saturating_mul(10).saturating_add(digit);
                }
                Some(';') => self.state = S::NumericCharacterReferenceEnd,
                _ => {
                    self.error("missing-semicolon-after-character-reference");
                    self.reconsume_in(S::NumericCharacterReferenceEnd);
                }
            },
            // §13.2.5.79
            S::NumericCharacterReferenceEnd => self.numeric_character_reference_end(),
        }
    }

    fn comment_eof(&mut self) {
        self.error("eof-in-comment");
        self.emit_comment();
        self.emit_eof();
    }

    fn start_public_id(&mut self, quote: char) {
        self.doctype.public_id = Some(String::new());
        self.state = if quote == '"' {
            S::DoctypePublicIdentifierDoubleQuoted
        } else {
            S::DoctypePublicIdentifierSingleQuoted
        };
    }

    fn start_system_id(&mut self, quote: char) {
        self.doctype.system_id = Some(String::new());
        self.state = if quote == '"' {
            S::DoctypeSystemIdentifierDoubleQuoted
        } else {
            S::DoctypeSystemIdentifierSingleQuoted
        };
    }

    fn missing_identifier(&mut self, code: &'static str) {
        self.error(code);
        self.doctype.force_quirks = true;
        self.state = S::Data;
        self.emit_doctype();
    }

    fn bogus_doctype(&mut self, code: &'static str) {
        self.error(code);
        self.doctype.force_quirks = true;
        self.reconsume_in(S::BogusDoctype);
    }

    /// DOCTYPE public/system identifier (quoted) states, §13.2.5.59–60 and
    /// §13.2.5.65–66.
    fn doctype_identifier(&mut self, quote: char, public: bool) {
        match self.consume() {
            Some(c) if c == quote => {
                self.state = if public {
                    S::AfterDoctypePublicIdentifier
                } else {
                    S::AfterDoctypeSystemIdentifier
                };
            }
            Some('\0') => {
                self.error("unexpected-null-character");
                if public {
                    self.doctype_public_push('\u{FFFD}');
                } else {
                    self.doctype_system_push('\u{FFFD}');
                }
            }
            Some('>') => {
                self.error(if public {
                    "abrupt-doctype-public-identifier"
                } else {
                    "abrupt-doctype-system-identifier"
                });
                self.doctype.force_quirks = true;
                self.state = S::Data;
                self.emit_doctype();
            }
            None => self.doctype_eof(),
            Some(c) => {
                if public {
                    self.doctype_public_push(c);
                } else {
                    self.doctype_system_push(c);
                }
            }
        }
    }

    /// Attribute value (double-quoted and single-quoted) states.
    fn quoted_attribute_value(&mut self, quote: char, this_state: S) {
        match self.consume() {
            Some(c) if c == quote => self.state = S::AfterAttributeValueQuoted,
            Some('&') => {
                self.return_state = this_state;
                self.state = S::CharacterReference;
            }
            Some('\0') => {
                self.error("unexpected-null-character");
                self.attribute_value_push('\u{FFFD}');
            }
            None => {
                self.error("eof-in-tag");
                self.emit_eof();
            }
            Some(c) => self.attribute_value_push(c),
        }
    }

    /// RCDATA/RAWTEXT less-than sign states.
    fn text_less_than(&mut self, text_state: S, end_tag_open: S) {
        match self.consume() {
            Some('/') => {
                self.temp.clear();
                self.state = end_tag_open;
            }
            _ => {
                self.emit_char('<');
                self.reconsume_in(text_state);
            }
        }
    }

    /// RCDATA/RAWTEXT/script data (escaped) end tag open states.
    fn text_end_tag_open(&mut self, text_state: S, end_tag_name: S) {
        match self.consume() {
            Some(c) if c.is_ascii_alphabetic() => {
                self.new_tag(true);
                self.reconsume_in(end_tag_name);
            }
            _ => {
                self.emit_str("</");
                self.reconsume_in(text_state);
            }
        }
    }

    /// RCDATA/RAWTEXT/script data (escaped) end tag name states.
    fn text_end_tag_name(&mut self, text_state: S) {
        let c = self.consume();
        match c {
            Some('\t' | '\n' | '\u{c}' | ' ') if self.is_appropriate_end_tag() => {
                self.state = S::BeforeAttributeName;
            }
            Some('/') if self.is_appropriate_end_tag() => self.state = S::SelfClosingStartTag,
            Some('>') if self.is_appropriate_end_tag() => {
                self.state = S::Data;
                self.emit_tag();
            }
            Some(c) if c.is_ascii_alphabetic() => {
                self.tag.name.push(c.to_ascii_lowercase());
                self.temp.push(c);
            }
            _ => {
                self.emit_str("</");
                let temp = std::mem::take(&mut self.temp);
                self.emit_str(&temp);
                self.reconsume_in(text_state);
            }
        }
    }

    /// Script data double escape start/end states (§13.2.5.26, §13.2.5.31).
    fn double_escape_boundary(&mut self, if_script: S, otherwise: S, on_other: S) {
        match self.consume() {
            Some(c @ ('\t' | '\n' | '\u{c}' | ' ' | '/' | '>')) => {
                self.state = if self.temp == "script" {
                    if_script
                } else {
                    otherwise
                };
                self.emit_char(c);
            }
            Some(c) if c.is_ascii_alphabetic() => {
                self.temp.push(c.to_ascii_lowercase());
                self.emit_char(c);
            }
            _ => self.reconsume_in(on_other),
        }
    }

    /// §13.2.5.73 Named character reference state.
    fn named_character_reference(&mut self) {
        let rest = self.remaining();
        let mut candidate = String::new();
        for &c in rest.iter().take(LONGEST_NAME) {
            if c.is_ascii_alphanumeric() {
                candidate.push(c);
            } else {
                if c == ';' {
                    candidate.push(c);
                }
                break;
            }
        }
        let next_after = |len: usize| rest.get(len).copied();
        let mut found = None;
        // Names are ASCII, so byte lengths are character counts.
        for len in (1..=candidate.len()).rev() {
            let Some(prefix) = candidate.get(..len) else {
                continue;
            };
            if let Ok(index) = ENTITIES.binary_search_by(|(name, _)| (*name).cmp(prefix)) {
                if let Some((_, value)) = ENTITIES.get(index) {
                    found = Some((len, *value, next_after(len)));
                }
                break;
            }
        }
        match found {
            Some((len, value, next)) => {
                let name = candidate.get(..len).unwrap_or_default().to_string();
                self.consume_n(len);
                self.temp.push_str(&name);
                let ends_with_semicolon = name.ends_with(';');
                if self.char_ref_in_attribute()
                    && !ends_with_semicolon
                    && next.is_some_and(|c| c == '=' || c.is_ascii_alphanumeric())
                {
                    self.flush_char_ref();
                    self.state = self.return_state;
                } else {
                    if !ends_with_semicolon {
                        self.error("missing-semicolon-after-character-reference");
                    }
                    self.temp.clear();
                    self.temp.push_str(value);
                    self.flush_char_ref();
                    self.state = self.return_state;
                }
            }
            None => {
                self.flush_char_ref();
                self.state = S::AmbiguousAmpersand;
            }
        }
    }

    /// §13.2.5.80 Numeric character reference end state.
    fn numeric_character_reference_end(&mut self) {
        let mut code = self.char_ref_code;
        if code == 0 {
            self.error("null-character-reference");
            code = 0xFFFD;
        } else if code > 0x10FFFF {
            self.error("character-reference-outside-unicode-range");
            code = 0xFFFD;
        } else if (0xD800..=0xDFFF).contains(&code) {
            self.error("surrogate-character-reference");
            code = 0xFFFD;
        } else if is_noncharacter(code) {
            self.error("noncharacter-character-reference");
        } else if code == 0x0D || (is_control(code) && !matches!(code, 0x09 | 0x0A | 0x0C | 0x20)) {
            self.error("control-character-reference");
            if let Some(&(_, replacement)) = C1_REPLACEMENTS.iter().find(|(n, _)| *n == code) {
                code = replacement;
            }
        }
        self.temp.clear();
        self.temp.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
        self.flush_char_ref();
        self.state = self.return_state;
    }
}

/// The C1 control replacements in §13.2.5.80.
const C1_REPLACEMENTS: [(u32, u32); 27] = [
    (0x80, 0x20AC),
    (0x82, 0x201A),
    (0x83, 0x0192),
    (0x84, 0x201E),
    (0x85, 0x2026),
    (0x86, 0x2020),
    (0x87, 0x2021),
    (0x88, 0x02C6),
    (0x89, 0x2030),
    (0x8A, 0x0160),
    (0x8B, 0x2039),
    (0x8C, 0x0152),
    (0x8E, 0x017D),
    (0x91, 0x2018),
    (0x92, 0x2019),
    (0x93, 0x201C),
    (0x94, 0x201D),
    (0x95, 0x2022),
    (0x96, 0x2013),
    (0x97, 0x2014),
    (0x98, 0x02DC),
    (0x99, 0x2122),
    (0x9A, 0x0161),
    (0x9B, 0x203A),
    (0x9C, 0x0153),
    (0x9E, 0x017E),
    (0x9F, 0x0178),
];

/// Infra: a noncharacter is U+FDD0–U+FDEF or a code point ending in FFFE/FFFF.
fn is_noncharacter(cp: u32) -> bool {
    (0xFDD0..=0xFDEF).contains(&cp) || (cp & 0xFFFE) == 0xFFFE && cp <= 0x10FFFF
}

/// Infra: a control is a C0 control (U+0000–U+001F) or U+007F–U+009F.
fn is_control(cp: u32) -> bool {
    cp <= 0x1F || (0x7F..=0x9F).contains(&cp)
}

/// ASCII whitespace (Infra §4.6).
pub(crate) fn is_ascii_whitespace(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{c}' | '\r' | ' ')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(input: &str) -> Vec<Token> {
        let mut tokenizer = Tokenizer::new(input);
        let mut out = Vec::new();
        loop {
            let token = tokenizer.next_token();
            if token == Token::Eof {
                break;
            }
            out.push(token);
        }
        out
    }

    fn text(tokens: &[Token]) -> String {
        tokens
            .iter()
            .filter_map(|t| match t {
                Token::Character(c) => Some(*c),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn tokenizes_tags_and_attributes() {
        let out = tokens("<DIV Class=a id='b' data-x=\"c&amp;d\" hidden/>");
        let Some(Token::StartTag(tag)) = out.first() else {
            panic!("expected a start tag, got {out:?}");
        };
        assert_eq!(tag.name, "div");
        assert!(tag.self_closing);
        assert_eq!(tag.attribute("class"), Some("a"));
        assert_eq!(tag.attribute("id"), Some("b"));
        assert_eq!(tag.attribute("data-x"), Some("c&d"));
        assert_eq!(tag.attribute("hidden"), Some(""));
    }

    #[test]
    fn decodes_character_references() {
        assert_eq!(
            text(&tokens("&amp;&lt;&#65;&#x42;&notin;&notit;")),
            "&<AB∉¬it;"
        );
        assert_eq!(text(&tokens("&#0;&#x110000;&#128;")), "\u{FFFD}\u{FFFD}€");
        assert_eq!(text(&tokens("&bogus; &")), "&bogus; &");
    }

    #[test]
    fn attribute_references_follow_the_legacy_rule() {
        let out = tokens("<a href='?a=1&copy=2&copy;'>");
        let Some(Token::StartTag(tag)) = out.first() else {
            panic!("expected a start tag");
        };
        assert_eq!(tag.attribute("href"), Some("?a=1&copy=2©"));
    }

    #[test]
    fn handles_comments_doctypes_and_processing_instructions() {
        let out = tokens("<!DOCTYPE html><!-- hi --><?php echo?><?xml x?>");
        assert_eq!(
            out,
            vec![
                Token::Doctype(Doctype {
                    name: Some("html".into()),
                    ..Doctype::default()
                }),
                Token::Comment(" hi ".into()),
                Token::ProcessingInstruction {
                    target: "php".into(),
                    data: "echo".into()
                },
                Token::Comment("?xml x?".into()),
            ]
        );
    }

    #[test]
    fn respects_text_states_and_appropriate_end_tags() {
        let mut tokenizer = Tokenizer::new("a</b></script>c");
        tokenizer.set_state(State::ScriptData);
        tokenizer.set_last_start_tag(Some("script"));
        let mut out = Vec::new();
        loop {
            let token = tokenizer.next_token();
            if token == Token::Eof {
                break;
            }
            out.push(token);
        }
        assert_eq!(text(&out), "a</b>c");
        assert!(
            out.iter()
                .any(|t| matches!(t, Token::EndTag(tag) if tag.name == "script"))
        );
    }

    #[test]
    fn records_errors_without_panicking() {
        let mut tokenizer = Tokenizer::new("<a b b>\u{1}</a x>");
        while tokenizer.next_token() != Token::Eof {}
        let codes: Vec<&str> = tokenizer.take_errors().iter().map(|e| e.code).collect();
        assert_eq!(
            codes,
            vec![
                "duplicate-attribute",
                "control-character-in-input-stream",
                "end-tag-with-attributes"
            ]
        );
    }

    fn error_codes(input: &str) -> Vec<(&'static str, u32)> {
        let mut tokenizer = Tokenizer::new(input);
        while tokenizer.next_token() != Token::Eof {}
        tokenizer
            .take_errors()
            .iter()
            .map(|e| (e.code, e.column))
            .collect()
    }

    #[test]
    fn duplicate_attribute_is_reported_when_its_name_ends() {
        // At end of file the tag is never emitted, but the duplicate name
        // has still been seen.
        assert_eq!(
            error_codes("<a x=1 x"),
            vec![("duplicate-attribute", 8), ("eof-in-tag", 8)]
        );
        // Reported at the '=' that ends the name, before the value.
        assert_eq!(
            error_codes("<a x=1 x=22>"),
            vec![("duplicate-attribute", 9)]
        );
        let out = tokens("<a x=1 X=2 y x=3>");
        let Some(Token::StartTag(tag)) = out.first() else {
            panic!("expected a start tag");
        };
        assert_eq!(tag.attributes.len(), 2);
        assert_eq!(tag.attribute("x"), Some("1"));
    }

    #[test]
    fn many_attributes_stay_linear() {
        let names: String = (0..20_000).map(|i| format!("a{i} ")).collect();
        let out = tokens(&format!("<a {names}>"));
        let Some(Token::StartTag(tag)) = out.first() else {
            panic!("expected a start tag");
        };
        assert_eq!(tag.attributes.len(), 20_000);
    }

    #[test]
    fn normalizes_newlines() {
        assert_eq!(text(&tokens("a\r\nb\rc")), "a\nb\nc");
    }
}
