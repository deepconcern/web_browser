//! A small backtracking cursor over component values, used by every grammar
//! in this crate (values, selectors, media queries).

use crate::syntax::ComponentValue;

/// Position in a list of component values. It is `Copy`, so saving and
/// restoring a position is a plain assignment.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Cursor<'a> {
    values: &'a [ComponentValue],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(values: &'a [ComponentValue]) -> Self {
        Cursor { values, pos: 0 }
    }

    /// Skips whitespace tokens.
    pub(crate) fn skip_ws(&mut self) {
        while matches!(self.values.get(self.pos), Some(ComponentValue::Whitespace)) {
            self.pos += 1;
        }
    }

    /// The next non-whitespace value, consumed.
    pub(crate) fn next(&mut self) -> Option<&'a ComponentValue> {
        self.skip_ws();
        self.next_including_ws()
    }

    /// The next value, whitespace included, consumed.
    pub(crate) fn next_including_ws(&mut self) -> Option<&'a ComponentValue> {
        let v = self.values.get(self.pos)?;
        self.pos += 1;
        Some(v)
    }

    /// The next non-whitespace value, not consumed.
    pub(crate) fn peek(&self) -> Option<&'a ComponentValue> {
        let mut c = *self;
        c.next()
    }

    /// The next value, whitespace included, not consumed.
    pub(crate) fn peek_including_ws(&self) -> Option<&'a ComponentValue> {
        self.values.get(self.pos)
    }

    /// Whether only whitespace remains.
    pub(crate) fn is_exhausted(&self) -> bool {
        self.peek().is_none()
    }

    /// Runs `f`; if it returns `None` the cursor is restored.
    pub(crate) fn attempt<T>(&mut self, f: impl FnOnce(&mut Self) -> Option<T>) -> Option<T> {
        let saved = *self;
        let r = f(self);
        if r.is_none() {
            *self = saved;
        }
        r
    }

    /// Consumes the next identifier if it matches `s` (ASCII
    /// case-insensitive).
    pub(crate) fn eat_ident(&mut self, s: &str) -> bool {
        self.attempt(|c| c.next().filter(|v| v.is_ident(s)).map(|_| ()))
            .is_some()
    }

    /// Consumes the next identifier and returns it.
    pub(crate) fn ident(&mut self) -> Option<&'a str> {
        self.attempt(|c| c.next().and_then(ComponentValue::as_ident))
    }

    /// Consumes a `,`.
    pub(crate) fn eat_comma(&mut self) -> bool {
        self.attempt(|c| match c.next() {
            Some(ComponentValue::Comma) => Some(()),
            _ => None,
        })
        .is_some()
    }

    /// Consumes a delimiter `d`.
    pub(crate) fn eat_delim(&mut self, d: char) -> bool {
        self.attempt(|c| match c.next() {
            Some(ComponentValue::Delim(x)) if *x == d => Some(()),
            _ => None,
        })
        .is_some()
    }

    /// The remaining values (including whitespace).
    pub(crate) fn rest(&self) -> &'a [ComponentValue] {
        self.values.get(self.pos..).unwrap_or(&[])
    }

    /// Runs `f` on the whole input and requires it to consume everything.
    pub(crate) fn parse_all<T>(
        values: &'a [ComponentValue],
        f: impl FnOnce(&mut Self) -> Option<T>,
    ) -> Option<T> {
        let mut c = Cursor::new(values);
        let r = f(&mut c)?;
        c.is_exhausted().then_some(r)
    }
}
