//! Fuzzes the tokenizer from every externally settable state.
#![no_main]

use libfuzzer_sys::fuzz_target;
use wb_html::tokenizer::{State, Token, Tokenizer};

fuzz_target!(|data: &[u8]| {
    let Some((&selector, rest)) = data.split_first() else {
        return;
    };
    let input = String::from_utf8_lossy(rest);
    let state = match selector % 6 {
        0 => State::Data,
        1 => State::Rcdata,
        2 => State::Rawtext,
        3 => State::ScriptData,
        4 => State::Plaintext,
        _ => State::CdataSection,
    };
    let mut tokenizer = Tokenizer::new(&input);
    tokenizer.set_state(state);
    tokenizer.set_last_start_tag(Some("script"));
    tokenizer.set_cdata_allowed(selector & 0x80 != 0);
    // Every character produces at most a handful of tokens, so this bound
    // catches a tokenizer that never reaches end-of-file.
    let limit = input.len() * 8 + 16;
    for _ in 0..limit {
        if tokenizer.next_token() == Token::Eof {
            return;
        }
    }
    panic!("tokenizer did not reach end-of-file");
});
