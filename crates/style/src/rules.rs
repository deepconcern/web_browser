//! Rule collection (stub; replaced below).
use crate::SheetInput;
use wb_css::MediaContext;

#[derive(Debug, Default)]
pub(crate) struct RuleSet {}

impl RuleSet {
    pub(crate) fn build(_sheets: &[SheetInput], _media: &MediaContext) -> Self {
        RuleSet {}
    }
    pub(crate) fn selector_count(&self) -> usize {
        0
    }
}
