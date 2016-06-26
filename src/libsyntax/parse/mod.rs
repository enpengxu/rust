// Copyright 2012-2014 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! The main parser interface

use ast;
use codemap::CodeMap;
use syntax_pos::{self, Span, FileMap};
use errors::{Handler, ColorConfig, DiagnosticBuilder};
use parse::parser::Parser;
use parse::token::InternedString;
use ptr::P;
use str::char_at;
use tokenstream;

use std::cell::RefCell;
use std::iter;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::str;

pub type PResult<'a, T> = Result<T, DiagnosticBuilder<'a>>;

#[macro_use]
pub mod parser;

pub mod lexer;
pub mod token;
pub mod attr;

pub mod common;
pub mod classify;
pub mod obsolete;

/// Info about a parsing session.
pub struct ParseSess {
    pub span_diagnostic: Handler, // better be the same as the one in the reader!
    /// Used to determine and report recursive mod inclusions
    included_mod_stack: RefCell<Vec<PathBuf>>,
    code_map: Rc<CodeMap>,
}

impl ParseSess {
    pub fn new() -> ParseSess {
        let cm = Rc::new(CodeMap::new());
        let handler = Handler::with_tty_emitter(ColorConfig::Auto, None, true, false, cm.clone());
        ParseSess::with_span_handler(handler, cm)
    }

    pub fn with_span_handler(handler: Handler, code_map: Rc<CodeMap>) -> ParseSess {
        ParseSess {
            span_diagnostic: handler,
            included_mod_stack: RefCell::new(vec![]),
            code_map: code_map
        }
    }

    pub fn codemap(&self) -> &CodeMap {
        &self.code_map
    }
}

// a bunch of utility functions of the form parse_<thing>_from_<source>
// where <thing> includes crate, expr, item, stmt, tts, and one that
// uses a HOF to parse anything, and <source> includes file and
// source_str.

pub fn parse_crate_from_file<'a>(input: &Path,
                                 cfg: ast::CrateConfig,
                                 sess: &'a ParseSess)
                                 -> PResult<'a, ast::Crate> {
    let mut parser = new_parser_from_file(sess, cfg, input);
    parser.parse_crate_mod()
}

pub fn parse_crate_attrs_from_file<'a>(input: &Path,
                                       cfg: ast::CrateConfig,
                                       sess: &'a ParseSess)
                                       -> PResult<'a, Vec<ast::Attribute>> {
    let mut parser = new_parser_from_file(sess, cfg, input);
    parser.parse_inner_attributes()
}

pub fn parse_crate_from_source_str<'a>(name: String,
                                       source: String,
                                       cfg: ast::CrateConfig,
                                       sess: &'a ParseSess)
                                       -> PResult<'a, ast::Crate> {
    let mut p = new_parser_from_source_str(sess,
                                           cfg,
                                           name,
                                           source);
    p.parse_crate_mod()
}

pub fn parse_crate_attrs_from_source_str<'a>(name: String,
                                             source: String,
                                             cfg: ast::CrateConfig,
                                             sess: &'a ParseSess)
                                             -> PResult<'a, Vec<ast::Attribute>> {
    let mut p = new_parser_from_source_str(sess,
                                           cfg,
                                           name,
                                           source);
    p.parse_inner_attributes()
}

pub fn parse_expr_from_source_str<'a>(name: String,
                                      source: String,
                                      cfg: ast::CrateConfig,
                                      sess: &'a ParseSess)
                                      -> PResult<'a, P<ast::Expr>> {
    let mut p = new_parser_from_source_str(sess, cfg, name, source);
    p.parse_expr()
}

/// Parses an item.
///
/// Returns `Ok(Some(item))` when successful, `Ok(None)` when no item was found, and`Err`
/// when a syntax error occurred.
pub fn parse_item_from_source_str<'a>(name: String,
                                      source: String,
                                      cfg: ast::CrateConfig,
                                      sess: &'a ParseSess)
                                      -> PResult<'a, Option<P<ast::Item>>> {
    let mut p = new_parser_from_source_str(sess, cfg, name, source);
    p.parse_item()
}

pub fn parse_meta_from_source_str<'a>(name: String,
                                      source: String,
                                      cfg: ast::CrateConfig,
                                      sess: &'a ParseSess)
                                      -> PResult<'a, P<ast::MetaItem>> {
    let mut p = new_parser_from_source_str(sess, cfg, name, source);
    p.parse_meta_item()
}

pub fn parse_stmt_from_source_str<'a>(name: String,
                                      source: String,
                                      cfg: ast::CrateConfig,
                                      sess: &'a ParseSess)
                                      -> PResult<'a, Option<ast::Stmt>> {
    let mut p = new_parser_from_source_str(
        sess,
        cfg,
        name,
        source
    );
    p.parse_stmt()
}

// Warning: This parses with quote_depth > 0, which is not the default.
pub fn parse_tts_from_source_str<'a>(name: String,
                                     source: String,
                                     cfg: ast::CrateConfig,
                                     sess: &'a ParseSess)
                                     -> PResult<'a, Vec<tokenstream::TokenTree>> {
    let mut p = new_parser_from_source_str(
        sess,
        cfg,
        name,
        source
    );
    p.quote_depth += 1;
    // right now this is re-creating the token trees from ... token trees.
    p.parse_all_token_trees()
}

// Create a new parser from a source string
pub fn new_parser_from_source_str<'a>(sess: &'a ParseSess,
                                      cfg: ast::CrateConfig,
                                      name: String,
                                      source: String)
                                      -> Parser<'a> {
    filemap_to_parser(sess, sess.codemap().new_filemap(name, None, source), cfg)
}

/// Create a new parser, handling errors as appropriate
/// if the file doesn't exist
pub fn new_parser_from_file<'a>(sess: &'a ParseSess,
                                cfg: ast::CrateConfig,
                                path: &Path) -> Parser<'a> {
    filemap_to_parser(sess, file_to_filemap(sess, path, None), cfg)
}

/// Given a session, a crate config, a path, and a span, add
/// the file at the given path to the codemap, and return a parser.
/// On an error, use the given span as the source of the problem.
pub fn new_sub_parser_from_file<'a>(sess: &'a ParseSess,
                                    cfg: ast::CrateConfig,
                                    path: &Path,
                                    owns_directory: bool,
                                    module_name: Option<String>,
                                    sp: Span) -> Parser<'a> {
    let mut p = filemap_to_parser(sess, file_to_filemap(sess, path, Some(sp)), cfg);
    p.owns_directory = owns_directory;
    p.root_module_name = module_name;
    p
}

/// Given a filemap and config, return a parser
pub fn filemap_to_parser<'a>(sess: &'a ParseSess,
                             filemap: Rc<FileMap>,
                             cfg: ast::CrateConfig) -> Parser<'a> {
    let end_pos = filemap.end_pos;
    let mut parser = tts_to_parser(sess, filemap_to_tts(sess, filemap), cfg);

    if parser.token == token::Eof && parser.span == syntax_pos::DUMMY_SP {
        parser.span = syntax_pos::mk_sp(end_pos, end_pos);
    }

    parser
}

// must preserve old name for now, because quote! from the *existing*
// compiler expands into it
pub fn new_parser_from_tts<'a>(sess: &'a ParseSess,
                               cfg: ast::CrateConfig,
                               tts: Vec<tokenstream::TokenTree>) -> Parser<'a> {
    tts_to_parser(sess, tts, cfg)
}


// base abstractions

/// Given a session and a path and an optional span (for error reporting),
/// add the path to the session's codemap and return the new filemap.
fn file_to_filemap(sess: &ParseSess, path: &Path, spanopt: Option<Span>)
                   -> Rc<FileMap> {
    match sess.codemap().load_file(path) {
        Ok(filemap) => filemap,
        Err(e) => {
            let msg = format!("couldn't read {:?}: {}", path.display(), e);
            match spanopt {
                Some(sp) => panic!(sess.span_diagnostic.span_fatal(sp, &msg)),
                None => panic!(sess.span_diagnostic.fatal(&msg))
            }
        }
    }
}

/// Given a filemap, produce a sequence of token-trees
pub fn filemap_to_tts(sess: &ParseSess, filemap: Rc<FileMap>)
    -> Vec<tokenstream::TokenTree> {
    // it appears to me that the cfg doesn't matter here... indeed,
    // parsing tt's probably shouldn't require a parser at all.
    let cfg = Vec::new();
    let srdr = lexer::StringReader::new(&sess.span_diagnostic, filemap);
    let mut p1 = Parser::new(sess, cfg, Box::new(srdr));
    panictry!(p1.parse_all_token_trees())
}

/// Given tts and cfg, produce a parser
pub fn tts_to_parser<'a>(sess: &'a ParseSess,
                         tts: Vec<tokenstream::TokenTree>,
                         cfg: ast::CrateConfig) -> Parser<'a> {
    let trdr = lexer::new_tt_reader(&sess.span_diagnostic, None, None, tts);
    let mut p = Parser::new(sess, cfg, Box::new(trdr));
    p.check_unknown_macro_variable();
    p
}

/// Parse a string representing a character literal into its final form.
/// Rather than just accepting/rejecting a given literal, unescapes it as
/// well. Can take any slice prefixed by a character escape. Returns the
/// character and the number of characters consumed.
pub fn char_lit(lit: &str) -> (char, isize) {
    use std::char;

    let mut chars = lit.chars();
    let c = match (chars.next(), chars.next()) {
        (Some(c), None) if c != '\\' => return (c, 1),
        (Some('\\'), Some(c)) => match c {
            '"' => Some('"'),
            'n' => Some('\n'),
            'r' => Some('\r'),
            't' => Some('\t'),
            '\\' => Some('\\'),
            '\'' => Some('\''),
            '0' => Some('\0'),
            _ => { None }
        },
        _ => panic!("lexer accepted invalid char escape `{}`", lit)
    };

    match c {
        Some(x) => return (x, 2),
        None => { }
    }

    let msg = format!("lexer should have rejected a bad character escape {}", lit);
    let msg2 = &msg[..];

    fn esc(len: usize, lit: &str) -> Option<(char, isize)> {
        u32::from_str_radix(&lit[2..len], 16).ok()
        .and_then(char::from_u32)
        .map(|x| (x, len as isize))
    }

    let unicode_escape = || -> Option<(char, isize)> {
        if lit.as_bytes()[2] == b'{' {
            let idx = lit.find('}').expect(msg2);
            let subslice = &lit[3..idx];
            u32::from_str_radix(subslice, 16).ok()
                .and_then(char::from_u32)
                .map(|x| (x, subslice.chars().count() as isize + 4))
        } else {
            esc(6, lit)
        }
    };

    // Unicode escapes
    return match lit.as_bytes()[1] as char {
        'x' | 'X' => esc(4, lit),
        'u' => unicode_escape(),
        'U' => esc(10, lit),
        _ => None,
    }.expect(msg2);
}

/// Parse a string representing a string literal into its final form. Does
/// unescaping.
pub fn str_lit(lit: &str) -> String {
    debug!("parse_str_lit: given {}", lit.escape_default());
    let mut res = String::with_capacity(lit.len());

    // FIXME #8372: This could be a for-loop if it didn't borrow the iterator
    let error = |i| format!("lexer should have rejected {} at {}", lit, i);

    /// Eat everything up to a non-whitespace
    fn eat<'a>(it: &mut iter::Peekable<str::CharIndices<'a>>) {
        loop {
            match it.peek().map(|x| x.1) {
                Some(' ') | Some('\n') | Some('\r') | Some('\t') => {
                    it.next();
                },
                _ => { break; }
            }
        }
    }

    let mut chars = lit.char_indices().peekable();
    loop {
        match chars.next() {
            Some((i, c)) => {
                match c {
                    '\\' => {
                        let ch = chars.peek().unwrap_or_else(|| {
                            panic!("{}", error(i))
                        }).1;

                        if ch == '\n' {
                            eat(&mut chars);
                        } else if ch == '\r' {
                            chars.next();
                            let ch = chars.peek().unwrap_or_else(|| {
                                panic!("{}", error(i))
                            }).1;

                            if ch != '\n' {
                                panic!("lexer accepted bare CR");
                            }
                            eat(&mut chars);
                        } else {
                            // otherwise, a normal escape
                            let (c, n) = char_lit(&lit[i..]);
                            for _ in 0..n - 1 { // we don't need to move past the first \
                                chars.next();
                            }
                            res.push(c);
                        }
                    },
                    '\r' => {
                        let ch = chars.peek().unwrap_or_else(|| {
                            panic!("{}", error(i))
                        }).1;

                        if ch != '\n' {
                            panic!("lexer accepted bare CR");
                        }
                        chars.next();
                        res.push('\n');
                    }
                    c => res.push(c),
                }
            },
            None => break
        }
    }

    res.shrink_to_fit(); // probably not going to do anything, unless there was an escape.
    debug!("parse_str_lit: returning {}", res);
    res
}

/// Parse a string representing a raw string literal into its final form. The
/// only operation this does is convert embedded CRLF into a single LF.
pub fn raw_str_lit(lit: &str) -> String {
    debug!("raw_str_lit: given {}", lit.escape_default());
    let mut res = String::with_capacity(lit.len());

    // FIXME #8372: This could be a for-loop if it didn't borrow the iterator
    let mut chars = lit.chars().peekable();
    loop {
        match chars.next() {
            Some(c) => {
                if c == '\r' {
                    if *chars.peek().unwrap() != '\n' {
                        panic!("lexer accepted bare CR");
                    }
                    chars.next();
                    res.push('\n');
                } else {
                    res.push(c);
                }
            },
            None => break
        }
    }

    res.shrink_to_fit();
    res
}

// check if `s` looks like i32 or u1234 etc.
fn looks_like_width_suffix(first_chars: &[char], s: &str) -> bool {
    s.len() > 1 &&
        first_chars.contains(&char_at(s, 0)) &&
        s[1..].chars().all(|c| '0' <= c && c <= '9')
}

fn filtered_float_lit(data: token::InternedString, suffix: Option<&str>,
                      sd: &Handler, sp: Span) -> ast::LitKind {
    debug!("filtered_float_lit: {}, {:?}", data, suffix);
    match suffix.as_ref().map(|s| &**s) {
        Some("f32") => ast::LitKind::Float(data, ast::FloatTy::F32),
        Some("f64") => ast::LitKind::Float(data, ast::FloatTy::F64),
        Some(suf) => {
            if suf.len() >= 2 && looks_like_width_suffix(&['f'], suf) {
                // if it looks like a width, lets try to be helpful.
                sd.struct_span_err(sp, &format!("invalid width `{}` for float literal", &suf[1..]))
                 .help("valid widths are 32 and 64")
                 .emit();
            } else {
                sd.struct_span_err(sp, &format!("invalid suffix `{}` for float literal", suf))
                  .help("valid suffixes are `f32` and `f64`")
                  .emit();
            }

            ast::LitKind::FloatUnsuffixed(data)
        }
        None => ast::LitKind::FloatUnsuffixed(data)
    }
}
pub fn float_lit(s: &str, suffix: Option<InternedString>,
                 sd: &Handler, sp: Span) -> ast::LitKind {
    debug!("float_lit: {:?}, {:?}", s, suffix);
    // FIXME #2252: bounds checking float literals is deferred until trans
    let s = s.chars().filter(|&c| c != '_').collect::<String>();
    let data = token::intern_and_get_ident(&s);
    filtered_float_lit(data, suffix.as_ref().map(|s| &**s), sd, sp)
}

/// Parse a string representing a byte literal into its final form. Similar to `char_lit`
pub fn byte_lit(lit: &str) -> (u8, usize) {
    let err = |i| format!("lexer accepted invalid byte literal {} step {}", lit, i);

    if lit.len() == 1 {
        (lit.as_bytes()[0], 1)
    } else {
        assert!(lit.as_bytes()[0] == b'\\', err(0));
        let b = match lit.as_bytes()[1] {
            b'"' => b'"',
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'\\' => b'\\',
            b'\'' => b'\'',
            b'0' => b'\0',
            _ => {
                match u64::from_str_radix(&lit[2..4], 16).ok() {
                    Some(c) =>
                        if c > 0xFF {
                            panic!(err(2))
                        } else {
                            return (c as u8, 4)
                        },
                    None => panic!(err(3))
                }
            }
        };
        return (b, 2);
    }
}

pub fn byte_str_lit(lit: &str) -> Rc<Vec<u8>> {
    let mut res = Vec::with_capacity(lit.len());

    // FIXME #8372: This could be a for-loop if it didn't borrow the iterator
    let error = |i| format!("lexer should have rejected {} at {}", lit, i);

    /// Eat everything up to a non-whitespace
    fn eat<'a, I: Iterator<Item=(usize, u8)>>(it: &mut iter::Peekable<I>) {
        loop {
            match it.peek().map(|x| x.1) {
                Some(b' ') | Some(b'\n') | Some(b'\r') | Some(b'\t') => {
                    it.next();
                },
                _ => { break; }
            }
        }
    }

    // byte string literals *must* be ASCII, but the escapes don't have to be
    let mut chars = lit.bytes().enumerate().peekable();
    loop {
        match chars.next() {
            Some((i, b'\\')) => {
                let em = error(i);
                match chars.peek().expect(&em).1 {
                    b'\n' => eat(&mut chars),
                    b'\r' => {
                        chars.next();
                        if chars.peek().expect(&em).1 != b'\n' {
                            panic!("lexer accepted bare CR");
                        }
                        eat(&mut chars);
                    }
                    _ => {
                        // otherwise, a normal escape
                        let (c, n) = byte_lit(&lit[i..]);
                        // we don't need to move past the first \
                        for _ in 0..n - 1 {
                            chars.next();
                        }
                        res.push(c);
                    }
                }
            },
            Some((i, b'\r')) => {
                let em = error(i);
                if chars.peek().expect(&em).1 != b'\n' {
                    panic!("lexer accepted bare CR");
                }
                chars.next();
                res.push(b'\n');
            }
            Some((_, c)) => res.push(c),
            None => break,
        }
    }

    Rc::new(res)
}

pub fn integer_lit(s: &str,
                   suffix: Option<InternedString>,
                   sd: &Handler,
                   sp: Span)
                   -> ast::LitKind {
    // s can only be ascii, byte indexing is fine

    let s2 = s.chars().filter(|&c| c != '_').collect::<String>();
    let mut s = &s2[..];

    debug!("integer_lit: {}, {:?}", s, suffix);

    let mut base = 10;
    let orig = s;
    let mut ty = ast::LitIntType::Unsuffixed;

    if char_at(s, 0) == '0' && s.len() > 1 {
        match char_at(s, 1) {
            'x' => base = 16,
            'o' => base = 8,
            'b' => base = 2,
            _ => { }
        }
    }

    // 1f64 and 2f32 etc. are valid float literals.
    if let Some(ref suf) = suffix {
        if looks_like_width_suffix(&['f'], suf) {
            match base {
                16 => sd.span_err(sp, "hexadecimal float literal is not supported"),
                8 => sd.span_err(sp, "octal float literal is not supported"),
                2 => sd.span_err(sp, "binary float literal is not supported"),
                _ => ()
            }
            let ident = token::intern_and_get_ident(&s);
            return filtered_float_lit(ident, Some(&suf), sd, sp)
        }
    }

    if base != 10 {
        s = &s[2..];
    }

    if let Some(ref suf) = suffix {
        if suf.is_empty() { sd.span_bug(sp, "found empty literal suffix in Some")}
        ty = match &**suf {
            "isize" => ast::LitIntType::Signed(ast::IntTy::Is),
            "i8"  => ast::LitIntType::Signed(ast::IntTy::I8),
            "i16" => ast::LitIntType::Signed(ast::IntTy::I16),
            "i32" => ast::LitIntType::Signed(ast::IntTy::I32),
            "i64" => ast::LitIntType::Signed(ast::IntTy::I64),
            "usize" => ast::LitIntType::Unsigned(ast::UintTy::Us),
            "u8"  => ast::LitIntType::Unsigned(ast::UintTy::U8),
            "u16" => ast::LitIntType::Unsigned(ast::UintTy::U16),
            "u32" => ast::LitIntType::Unsigned(ast::UintTy::U32),
            "u64" => ast::LitIntType::Unsigned(ast::UintTy::U64),
            _ => {
                // i<digits> and u<digits> look like widths, so lets
                // give an error message along those lines
                if looks_like_width_suffix(&['i', 'u'], suf) {
                    sd.struct_span_err(sp, &format!("invalid width `{}` for integer literal",
                                             &suf[1..]))
                      .help("valid widths are 8, 16, 32 and 64")
                      .emit();
                } else {
                    sd.struct_span_err(sp, &format!("invalid suffix `{}` for numeric literal", suf))
                      .help("the suffix must be one of the integral types \
                             (`u32`, `isize`, etc)")
                      .emit();
                }

                ty
            }
        }
    }

    debug!("integer_lit: the type is {:?}, base {:?}, the new string is {:?}, the original \
           string was {:?}, the original suffix was {:?}", ty, base, s, orig, suffix);

    match u64::from_str_radix(s, base) {
        Ok(r) => ast::LitKind::Int(r, ty),
        Err(_) => {
            // small bases are lexed as if they were base 10, e.g, the string
            // might be `0b10201`. This will cause the conversion above to fail,
            // but these cases have errors in the lexer: we don't want to emit
            // two errors, and we especially don't want to emit this error since
            // it isn't necessarily true.
            let already_errored = base < 10 &&
                s.chars().any(|c| c.to_digit(10).map_or(false, |d| d >= base));

            if !already_errored {
                sd.span_err(sp, "int literal is too large");
            }
            ast::LitKind::Int(0, ty)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syntax_pos::{Span, BytePos, Pos, NO_EXPANSION};
    use codemap::Spanned;
    use ast::{self, PatKind};
    use abi::Abi;
    use attr::{first_attr_value_str_by_name, AttrMetaMethods};
    use parse;
    use parse::parser::Parser;
    use parse::token::{str_to_ident};
    use print::pprust::item_to_string;
    use ptr::P;
    use tokenstream::{self, TokenTree};
    use util::parser_testing::{string_to_tts, string_to_parser};
    use util::parser_testing::{string_to_expr, string_to_item, string_to_stmt};
    use util::ThinVec;

    // produce a syntax_pos::span
    fn sp(a: u32, b: u32) -> Span {
        Span {lo: BytePos(a), hi: BytePos(b), expn_id: NO_EXPANSION}
    }

    #[test] fn path_exprs_1() {
        assert!(string_to_expr("a".to_string()) ==
                   P(ast::Expr{
                    id: ast::DUMMY_NODE_ID,
                    node: ast::ExprKind::Path(None, ast::Path {
                        span: sp(0, 1),
                        global: false,
                        segments: vec!(
                            ast::PathSegment {
                                identifier: str_to_ident("a"),
                                parameters: ast::PathParameters::none(),
                            }
                        ),
                    }),
                    span: sp(0, 1),
                    attrs: ThinVec::new(),
                   }))
    }

    #[test] fn path_exprs_2 () {
        assert!(string_to_expr("::a::b".to_string()) ==
                   P(ast::Expr {
                    id: ast::DUMMY_NODE_ID,
                    node: ast::ExprKind::Path(None, ast::Path {
                            span: sp(0, 6),
                            global: true,
                            segments: vec!(
                                ast::PathSegment {
                                    identifier: str_to_ident("a"),
                                    parameters: ast::PathParameters::none(),
                                },
                                ast::PathSegment {
                                    identifier: str_to_ident("b"),
                                    parameters: ast::PathParameters::none(),
                                }
                            )
                        }),
                    span: sp(0, 6),
                    attrs: ThinVec::new(),
                   }))
    }

    #[should_panic]
    #[test] fn bad_path_expr_1() {
        string_to_expr("::abc::def::return".to_string());
    }

    // check the token-tree-ization of macros
    #[test]
    fn string_to_tts_macro () {
        let tts = string_to_tts("macro_rules! zip (($a)=>($a))".to_string());
        let tts: &[tokenstream::TokenTree] = &tts[..];

        match (tts.len(), tts.get(0), tts.get(1), tts.get(2), tts.get(3)) {
            (
                4,
                Some(&TokenTree::Token(_, token::Ident(name_macro_rules))),
                Some(&TokenTree::Token(_, token::Not)),
                Some(&TokenTree::Token(_, token::Ident(name_zip))),
                Some(&TokenTree::Delimited(_, ref macro_delimed)),
            )
            if name_macro_rules.name.as_str() == "macro_rules"
            && name_zip.name.as_str() == "zip" => {
                let tts = &macro_delimed.tts[..];
                match (tts.len(), tts.get(0), tts.get(1), tts.get(2)) {
                    (
                        3,
                        Some(&TokenTree::Delimited(_, ref first_delimed)),
                        Some(&TokenTree::Token(_, token::FatArrow)),
                        Some(&TokenTree::Delimited(_, ref second_delimed)),
                    )
                    if macro_delimed.delim == token::Paren => {
                        let tts = &first_delimed.tts[..];
                        match (tts.len(), tts.get(0), tts.get(1)) {
                            (
                                2,
                                Some(&TokenTree::Token(_, token::Dollar)),
                                Some(&TokenTree::Token(_, token::Ident(ident))),
                            )
                            if first_delimed.delim == token::Paren
                            && ident.name.as_str() == "a" => {},
                            _ => panic!("value 3: {:?}", *first_delimed),
                        }
                        let tts = &second_delimed.tts[..];
                        match (tts.len(), tts.get(0), tts.get(1)) {
                            (
                                2,
                                Some(&TokenTree::Token(_, token::Dollar)),
                                Some(&TokenTree::Token(_, token::Ident(ident))),
                            )
                            if second_delimed.delim == token::Paren
                            && ident.name.as_str() == "a" => {},
                            _ => panic!("value 4: {:?}", *second_delimed),
                        }
                    },
                    _ => panic!("value 2: {:?}", *macro_delimed),
                }
            },
            _ => panic!("value: {:?}",tts),
        }
    }

    #[test]
    fn string_to_tts_1() {
        let tts = string_to_tts("fn a (b : i32) { b; }".to_string());

        let expected = vec![
            TokenTree::Token(sp(0, 2), token::Ident(str_to_ident("fn"))),
            TokenTree::Token(sp(3, 4), token::Ident(str_to_ident("a"))),
            TokenTree::Delimited(
                sp(5, 14),
                tokenstream::Delimited {
                    delim: token::DelimToken::Paren,
                    open_span: sp(5, 6),
                    tts: vec![
                        TokenTree::Token(sp(6, 7), token::Ident(str_to_ident("b"))),
                        TokenTree::Token(sp(8, 9), token::Colon),
                        TokenTree::Token(sp(10, 13), token::Ident(str_to_ident("i32"))),
                    ],
                    close_span: sp(13, 14),
                }),
            TokenTree::Delimited(
                sp(15, 21),
                tokenstream::Delimited {
                    delim: token::DelimToken::Brace,
                    open_span: sp(15, 16),
                    tts: vec![
                        TokenTree::Token(sp(17, 18), token::Ident(str_to_ident("b"))),
                        TokenTree::Token(sp(18, 19), token::Semi),
                    ],
                    close_span: sp(20, 21),
                })
        ];

        assert_eq!(tts, expected);
    }

    #[test] fn ret_expr() {
        assert!(string_to_expr("return d".to_string()) ==
                   P(ast::Expr{
                    id: ast::DUMMY_NODE_ID,
                    node:ast::ExprKind::Ret(Some(P(ast::Expr{
                        id: ast::DUMMY_NODE_ID,
                        node:ast::ExprKind::Path(None, ast::Path{
                            span: sp(7, 8),
                            global: false,
                            segments: vec!(
                                ast::PathSegment {
                                    identifier: str_to_ident("d"),
                                    parameters: ast::PathParameters::none(),
                                }
                            ),
                        }),
                        span:sp(7,8),
                        attrs: ThinVec::new(),
                    }))),
                    span:sp(0,8),
                    attrs: ThinVec::new(),
                   }))
    }

    #[test] fn parse_stmt_1 () {
        assert!(string_to_stmt("b;".to_string()) ==
                   Some(ast::Stmt {
                       node: ast::StmtKind::Expr(P(ast::Expr {
                           id: ast::DUMMY_NODE_ID,
                           node: ast::ExprKind::Path(None, ast::Path {
                               span:sp(0,1),
                               global:false,
                               segments: vec!(
                                ast::PathSegment {
                                    identifier: str_to_ident("b"),
                                    parameters: ast::PathParameters::none(),
                                }
                               ),
                            }),
                           span: sp(0,1),
                           attrs: ThinVec::new()})),
                       id: ast::DUMMY_NODE_ID,
                       span: sp(0,1)}))

    }

    fn parser_done(p: Parser){
        assert_eq!(p.token.clone(), token::Eof);
    }

    #[test] fn parse_ident_pat () {
        let sess = ParseSess::new();
        let mut parser = string_to_parser(&sess, "b".to_string());
        assert!(panictry!(parser.parse_pat())
                == P(ast::Pat{
                id: ast::DUMMY_NODE_ID,
                node: PatKind::Ident(ast::BindingMode::ByValue(ast::Mutability::Immutable),
                                    Spanned{ span:sp(0, 1),
                                             node: str_to_ident("b")
                    },
                                    None),
                span: sp(0,1)}));
        parser_done(parser);
    }

    // check the contents of the tt manually:
    #[test] fn parse_fundecl () {
        // this test depends on the intern order of "fn" and "i32"
        assert_eq!(string_to_item("fn a (b : i32) { b; }".to_string()),
                  Some(
                      P(ast::Item{ident:str_to_ident("a"),
                            attrs:Vec::new(),
                            id: ast::DUMMY_NODE_ID,
                            node: ast::ItemKind::Fn(P(ast::FnDecl {
                                inputs: vec!(ast::Arg{
                                    ty: P(ast::Ty{id: ast::DUMMY_NODE_ID,
                                                  node: ast::TyKind::Path(None, ast::Path{
                                        span:sp(10,13),
                                        global:false,
                                        segments: vec!(
                                            ast::PathSegment {
                                                identifier:
                                                    str_to_ident("i32"),
                                                parameters: ast::PathParameters::none(),
                                            }
                                        ),
                                        }),
                                        span:sp(10,13)
                                    }),
                                    pat: P(ast::Pat {
                                        id: ast::DUMMY_NODE_ID,
                                        node: PatKind::Ident(
                                            ast::BindingMode::ByValue(ast::Mutability::Immutable),
                                                Spanned{
                                                    span: sp(6,7),
                                                    node: str_to_ident("b")},
                                                None
                                                    ),
                                            span: sp(6,7)
                                    }),
                                        id: ast::DUMMY_NODE_ID
                                    }),
                                output: ast::FunctionRetTy::Default(sp(15, 15)),
                                variadic: false
                            }),
                                    ast::Unsafety::Normal,
                                    ast::Constness::NotConst,
                                    Abi::Rust,
                                    ast::Generics{ // no idea on either of these:
                                        lifetimes: Vec::new(),
                                        ty_params: P::new(),
                                        where_clause: ast::WhereClause {
                                            id: ast::DUMMY_NODE_ID,
                                            predicates: Vec::new(),
                                        }
                                    },
                                    P(ast::Block {
                                        stmts: vec!(ast::Stmt {
                                            node: ast::StmtKind::Semi(P(ast::Expr{
                                                id: ast::DUMMY_NODE_ID,
                                                node: ast::ExprKind::Path(None,
                                                      ast::Path{
                                                        span:sp(17,18),
                                                        global:false,
                                                        segments: vec!(
                                                            ast::PathSegment {
                                                                identifier:
                                                                str_to_ident(
                                                                    "b"),
                                                                parameters:
                                                                ast::PathParameters::none(),
                                                            }
                                                        ),
                                                      }),
                                                span: sp(17,18),
                                                attrs: ThinVec::new()})),
                                            id: ast::DUMMY_NODE_ID,
                                            span: sp(17,19)}),
                                        id: ast::DUMMY_NODE_ID,
                                        rules: ast::BlockCheckMode::Default, // no idea
                                        span: sp(15,21),
                                    })),
                            vis: ast::Visibility::Inherited,
                            span: sp(0,21)})));
    }

    #[test] fn parse_use() {
        let use_s = "use foo::bar::baz;";
        let vitem = string_to_item(use_s.to_string()).unwrap();
        let vitem_s = item_to_string(&vitem);
        assert_eq!(&vitem_s[..], use_s);

        let use_s = "use foo::bar as baz;";
        let vitem = string_to_item(use_s.to_string()).unwrap();
        let vitem_s = item_to_string(&vitem);
        assert_eq!(&vitem_s[..], use_s);
    }

    #[test] fn parse_extern_crate() {
        let ex_s = "extern crate foo;";
        let vitem = string_to_item(ex_s.to_string()).unwrap();
        let vitem_s = item_to_string(&vitem);
        assert_eq!(&vitem_s[..], ex_s);

        let ex_s = "extern crate foo as bar;";
        let vitem = string_to_item(ex_s.to_string()).unwrap();
        let vitem_s = item_to_string(&vitem);
        assert_eq!(&vitem_s[..], ex_s);
    }

    fn get_spans_of_pat_idents(src: &str) -> Vec<Span> {
        let item = string_to_item(src.to_string()).unwrap();

        struct PatIdentVisitor {
            spans: Vec<Span>
        }
        impl ::visit::Visitor for PatIdentVisitor {
            fn visit_pat(&mut self, p: &ast::Pat) {
                match p.node {
                    PatKind::Ident(_ , ref spannedident, _) => {
                        self.spans.push(spannedident.span.clone());
                    }
                    _ => {
                        ::visit::walk_pat(self, p);
                    }
                }
            }
        }
        let mut v = PatIdentVisitor { spans: Vec::new() };
        ::visit::walk_item(&mut v, &item);
        return v.spans;
    }

    #[test] fn span_of_self_arg_pat_idents_are_correct() {

        let srcs = ["impl z { fn a (&self, &myarg: i32) {} }",
                    "impl z { fn a (&mut self, &myarg: i32) {} }",
                    "impl z { fn a (&'a self, &myarg: i32) {} }",
                    "impl z { fn a (self, &myarg: i32) {} }",
                    "impl z { fn a (self: Foo, &myarg: i32) {} }",
                    ];

        for &src in &srcs {
            let spans = get_spans_of_pat_idents(src);
            let Span{ lo, hi, .. } = spans[0];
            assert!("self" == &src[lo.to_usize()..hi.to_usize()],
                    "\"{}\" != \"self\". src=\"{}\"",
                    &src[lo.to_usize()..hi.to_usize()], src)
        }
    }

    #[test] fn parse_exprs () {
        // just make sure that they parse....
        string_to_expr("3 + 4".to_string());
        string_to_expr("a::z.froob(b,&(987+3))".to_string());
    }

    #[test] fn attrs_fix_bug () {
        string_to_item("pub fn mk_file_writer(path: &Path, flags: &[FileFlag])
                   -> Result<Box<Writer>, String> {
    #[cfg(windows)]
    fn wb() -> c_int {
      (O_WRONLY | libc::consts::os::extra::O_BINARY) as c_int
    }

    #[cfg(unix)]
    fn wb() -> c_int { O_WRONLY as c_int }

    let mut fflags: c_int = wb();
}".to_string());
    }

    #[test] fn crlf_doc_comments() {
        let sess = ParseSess::new();

        let name = "<source>".to_string();
        let source = "/// doc comment\r\nfn foo() {}".to_string();
        let item = parse_item_from_source_str(name.clone(), source, Vec::new(), &sess)
            .unwrap().unwrap();
        let doc = first_attr_value_str_by_name(&item.attrs, "doc").unwrap();
        assert_eq!(&doc[..], "/// doc comment");

        let source = "/// doc comment\r\n/// line 2\r\nfn foo() {}".to_string();
        let item = parse_item_from_source_str(name.clone(), source, Vec::new(), &sess)
            .unwrap().unwrap();
        let docs = item.attrs.iter().filter(|a| &*a.name() == "doc")
                    .map(|a| a.value_str().unwrap().to_string()).collect::<Vec<_>>();
        let b: &[_] = &["/// doc comment".to_string(), "/// line 2".to_string()];
        assert_eq!(&docs[..], b);

        let source = "/** doc comment\r\n *  with CRLF */\r\nfn foo() {}".to_string();
        let item = parse_item_from_source_str(name, source, Vec::new(), &sess).unwrap().unwrap();
        let doc = first_attr_value_str_by_name(&item.attrs, "doc").unwrap();
        assert_eq!(&doc[..], "/** doc comment\n *  with CRLF */");
    }

    #[test]
    fn ttdelim_span() {
        let sess = ParseSess::new();
        let expr = parse::parse_expr_from_source_str("foo".to_string(),
            "foo!( fn main() { body } )".to_string(), vec![], &sess).unwrap();

        let tts = match expr.node {
            ast::ExprKind::Mac(ref mac) => mac.node.tts.clone(),
            _ => panic!("not a macro"),
        };

        let span = tts.iter().rev().next().unwrap().get_span();

        match sess.codemap().span_to_snippet(span) {
            Ok(s) => assert_eq!(&s[..], "{ body }"),
            Err(_) => panic!("could not get snippet"),
        }
    }
}
