//! Semantic tokenization for syntax highlighting.
//!
//! Walks the AST and emits token spans with semantic types, suitable for
//! driving CodeMirror decorations via WASM.

use crate::error::JsonataError;
use crate::parser::ast::{AstArena, Expr, GroupExpr, NodeId, UnaryOp};
use crate::parser::{Parser, process_ast};

/// Semantic token type for highlighting.
#[derive(Debug, Clone, Copy)]
pub enum HlType {
    Variable,  // $name
    Function,  // procedure name in function call position
    String,    // "..."
    Number,    // 123, 3.14
    Bool,      // true, false
    Null,      // null
    Operator,  // +, -, *, and, or, etc.
    Keyword,   // function, in
    Bracket,   // ( ) [ ] { }
    Name,      // field/path name
    ObjectKey, // key in object constructor
    Regex,     // /pattern/flags
    Comment,   // /* ... */
}

impl HlType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Variable => "variable",
            Self::Function => "function",
            Self::String => "string",
            Self::Number => "number",
            Self::Bool => "bool",
            Self::Null => "null",
            Self::Operator => "operator",
            Self::Keyword => "keyword",
            Self::Bracket => "bracket",
            Self::Name => "name",
            Self::ObjectKey => "object-key",
            Self::Regex => "regex",
            Self::Comment => "comment",
        }
    }
}

#[derive(Debug, Clone)]
pub struct HlSpan {
    pub start: usize,
    pub end: usize,
    pub typ: HlType,
}

/// Tokenize an expression for syntax highlighting.
/// Returns a JSON string: `[[start, end, "type"], ...]`, where `start`
/// and `end` are UTF-8 byte offsets into `expr` (not char indices).
///
/// # Errors
/// Returns `JsonataError` if the expression fails to parse.
pub fn highlight(expr: &str) -> Result<String, JsonataError> {
    // Parse and walk AST for semantic tokens first.
    let (mut arena, root) = Parser::parse(expr)?;
    let root = process_ast(&mut arena, root)?;

    let mut spans = Vec::new();
    if !root.is_empty() {
        let mut walker = HlWalker::new(&arena, expr);
        walker.walk(root);
        spans = walker.spans;
    }

    // Comments are stripped by the lexer — recover them lexically, treating
    // the walker's token spans as opaque: a "/*" inside a regex literal or
    // backtick-quoted name is not a comment.
    let mut comments = Vec::new();
    extract_comment_spans(expr, &spans, &mut comments);
    spans.extend(comments);

    // Sort by start position
    spans.sort_by_key(|s| s.start);

    // Serialize as JSON array
    let mut out = String::from("[");
    for (i, span) in spans.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('[');
        out.push_str(&span.start.to_string());
        out.push(',');
        out.push_str(&span.end.to_string());
        out.push_str(",\"");
        out.push_str(span.typ.as_str());
        out.push_str("\"]");
    }
    out.push(']');
    Ok(out)
}

fn extract_comment_spans(src: &str, token_spans: &[HlSpan], spans: &mut Vec<HlSpan>) {
    let bytes = src.as_bytes();
    let mut regions: Vec<(usize, usize)> = token_spans.iter().map(|t| (t.start, t.end)).collect();
    regions.sort_unstable();
    let mut r = 0;
    let mut i = 0;
    while i + 1 < bytes.len() {
        // Jump over any real token containing this position.
        while r < regions.len() && regions[r].1 <= i {
            r += 1;
        }
        if r < regions.len() && regions[r].0 <= i {
            i = regions[r].1;
            continue;
        }
        if bytes[i] == b'/' && bytes[i + 1] == b'*' {
            let start = i;
            i += 2;
            while i + 1 < bytes.len() {
                if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    i += 2;
                    break;
                }
                i += 1;
            }
            spans.push(HlSpan {
                start,
                end: i,
                typ: HlType::Comment,
            });
        } else if bytes[i] == b'"' || bytes[i] == b'\'' {
            let quote = bytes[i];
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if bytes[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
        } else {
            i += 1;
        }
    }
}

struct HlWalker<'a> {
    arena: &'a AstArena,
    src: &'a str,
    spans: Vec<HlSpan>,
}

impl<'a> HlWalker<'a> {
    fn new(arena: &'a AstArena, src: &'a str) -> Self {
        Self {
            arena,
            src,
            spans: Vec::new(),
        }
    }

    fn push(&mut self, start: usize, end: usize, typ: HlType) {
        if start < end && end <= self.src.len() {
            self.spans.push(HlSpan { start, end, typ });
        }
    }

    /// Find the end of an identifier/name starting at `pos`.
    fn name_end(&self, pos: usize) -> usize {
        let bytes = self.src.as_bytes();
        if pos >= bytes.len() {
            return pos;
        }
        // Backtick-quoted name
        if bytes[pos] == b'`' {
            let mut i = pos + 1;
            while i < bytes.len() && bytes[i] != b'`' {
                i += 1;
            }
            return if i < bytes.len() { i + 1 } else { i };
        }
        let mut i = pos;
        while i < bytes.len()
            && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] >= 0x80)
        {
            i += 1;
        }
        i
    }

    /// Find the end of a variable starting at `pos` (which points at `$`).
    fn variable_end(&self, pos: usize) -> usize {
        let bytes = self.src.as_bytes();
        if pos >= bytes.len() {
            return pos;
        }
        let mut i = pos + 1; // skip $
        if i < bytes.len() && bytes[i] == b'$' {
            return i + 1;
        } // $$
        while i < bytes.len()
            && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] >= 0x80)
        {
            i += 1;
        }
        if i == pos + 1 {
            return i;
        } // bare $
        i
    }

    /// Find the end of a string literal starting at `pos`.
    fn string_end(&self, pos: usize) -> usize {
        let bytes = self.src.as_bytes();
        if pos >= bytes.len() {
            return pos;
        }
        let quote = bytes[pos];
        let mut i = pos + 1;
        while i < bytes.len() {
            if bytes[i] == b'\\' {
                i += 2;
                continue;
            }
            if bytes[i] == quote {
                return i + 1;
            }
            i += 1;
        }
        i
    }

    #[expect(clippy::too_many_lines)]
    fn walk(&mut self, id: NodeId) {
        if id.is_empty() {
            return;
        }
        // `arena` is a shared `&'a` borrow independent of `&mut self`, so
        // nodes are matched in place — no per-node deep clone.
        match self.arena.get(id) {
            Expr::Name { pos, .. } => {
                let pos = *pos;
                let end = self.name_end(pos);
                self.push(pos, end, HlType::Name);
            }
            Expr::StringLit { pos, .. } => {
                let pos = *pos;
                let end = self.string_end(pos);
                self.push(pos, end, HlType::String);
            }
            Expr::NumberLit { pos, raw, .. } => {
                let pos = *pos;
                // Folded negative literals keep the digit token's pos while
                // raw carries the '-'; the sign may sit earlier in the
                // source, separated by whitespace. Cover it when found.
                let (start, end) = if raw.starts_with('-') && !self.src[pos..].starts_with('-') {
                    let bytes = self.src.as_bytes();
                    let mut j = pos;
                    while j > 0 && bytes[j - 1].is_ascii_whitespace() {
                        j -= 1;
                    }
                    let start = if j > 0 && bytes[j - 1] == b'-' {
                        j - 1
                    } else {
                        pos
                    };
                    (start, pos + raw.len() - 1)
                } else {
                    (pos, pos + raw.len())
                };
                self.push(start, end, HlType::Number);
            }
            Expr::ValueLit { value, pos, .. } => {
                let pos = *pos;
                let typ = match value.as_str() {
                    "null" => HlType::Null,
                    _ => HlType::Bool,
                };
                self.push(pos, pos + value.len(), typ);
            }
            Expr::Variable { pos, group, .. } => {
                let pos = *pos;
                let end = self.variable_end(pos);
                self.push(pos, end, HlType::Variable);
                self.walk_group(group.as_ref());
            }
            Expr::Wildcard { pos, .. } => {
                let pos = *pos;
                self.push(pos, pos + 1, HlType::Operator);
            }
            Expr::Descendant { pos, .. } => {
                let pos = *pos;
                self.push(pos, pos + 2, HlType::Operator);
            }
            Expr::Parent { pos, .. } => {
                let pos = *pos;
                self.push(pos, pos + 1, HlType::Operator);
            }
            Expr::Regex {
                pos,
                pattern,
                flags,
                ..
            } => {
                let pos = *pos;
                // /pattern/flags — the lexer appends a synthetic 'g' flag
                // that is never in the source; exclude it from the span.
                let src_flags = flags.len().saturating_sub(1);
                let end = pos + 1 + pattern.len() + 1 + src_flags;
                self.push(pos, end, HlType::Regex);
            }
            Expr::Placeholder { pos } => {
                let pos = *pos;
                self.push(pos, pos + 1, HlType::Operator);
            }
            Expr::Path { steps, group, .. } => {
                for &step in steps {
                    self.walk(step);
                }
                self.walk_group(group.as_ref());
            }
            Expr::Binary {
                op,
                lhs,
                rhs,
                group,
                pos,
                ..
            } => {
                let (op, lhs, rhs, pos) = (*op, *lhs, *rhs, *pos);
                self.walk(lhs);
                // Emit operator span
                let op_str = op.as_str();
                self.push(pos, pos + op_str.len(), HlType::Operator);
                self.walk(rhs);
                self.walk_group(group.as_ref());
            }
            Expr::Unary {
                op,
                operand,
                expressions,
                lhs,
                group,
                pos,
                ..
            } => {
                let (op, operand, pos) = (*op, *operand, *pos);
                match op {
                    UnaryOp::Negate => {
                        self.push(pos, pos + 1, HlType::Operator);
                        self.walk(operand);
                    }
                    UnaryOp::ArrayCons => {
                        self.push(pos, pos + 1, HlType::Bracket); // [
                        for &el in expressions {
                            self.walk(el);
                        }
                    }
                    UnaryOp::ObjCons => {
                        self.push(pos, pos + 1, HlType::Bracket); // {
                        // lhs is flat [k0, v0, k1, v1, ...]
                        for (i, &node) in lhs.iter().enumerate() {
                            if i % 2 == 0 {
                                self.walk_as_object_key(node);
                            } else {
                                self.walk(node);
                            }
                        }
                        self.walk_group(group.as_ref());
                    }
                }
            }
            Expr::Block {
                expressions, pos, ..
            } => {
                let pos = *pos;
                self.push(pos, pos + 1, HlType::Bracket); // (
                for &e in expressions {
                    self.walk(e);
                }
            }
            Expr::Condition {
                condition,
                then,
                else_,
                ..
            } => {
                let (condition, then, else_) = (*condition, *then, *else_);
                self.walk(condition);
                self.walk(then);
                if let Some(e) = else_ {
                    self.walk(e);
                }
            }
            Expr::Bind { lhs, rhs, .. } => {
                let (lhs, rhs) = (*lhs, *rhs);
                self.walk(lhs);
                self.walk(rhs);
            }
            Expr::Function {
                procedure,
                arguments,
                group,
                ..
            } => {
                // The procedure name gets "function" highlighting
                self.walk_as_function(*procedure);
                for &arg in arguments {
                    self.walk(arg);
                }
                self.walk_group(group.as_ref());
            }
            Expr::Partial {
                procedure,
                arguments,
                ..
            } => {
                self.walk_as_function(*procedure);
                for &arg in arguments {
                    self.walk(arg);
                }
            }
            Expr::Lambda {
                params, body, pos, ..
            } => {
                let (body, pos) = (*body, *pos);
                // "function" keyword or its 2-byte λ shorthand.
                let kw_len = if self.src[pos..].starts_with('λ') {
                    'λ'.len_utf8()
                } else {
                    "function".len()
                };
                self.push(pos, pos + kw_len, HlType::Keyword);
                for &p in params {
                    self.walk(p);
                }
                self.walk(body);
            }
            Expr::Transform {
                pattern,
                update,
                delete,
                ..
            } => {
                let (pattern, update, delete) = (*pattern, *update, *delete);
                self.walk(pattern);
                self.walk(update);
                if let Some(d) = delete {
                    self.walk(d);
                }
            }
            Expr::Sort { expr, terms, .. } => {
                self.walk(*expr);
                for term in terms {
                    self.walk(term.expression);
                }
            }
            Expr::Grouped { expr, group, .. } => {
                self.walk(*expr);
                self.walk_group(Some(group));
            }
        }
    }

    /// Walk a node but emit it as an object key.
    fn walk_as_object_key(&mut self, id: NodeId) {
        if id.is_empty() {
            return;
        }
        match self.arena.get(id) {
            Expr::StringLit { pos, .. } => {
                let pos = *pos;
                let end = self.string_end(pos);
                self.push(pos, end, HlType::ObjectKey);
            }
            Expr::Name { pos, .. } => {
                let pos = *pos;
                let end = self.name_end(pos);
                self.push(pos, end, HlType::ObjectKey);
            }
            // Dynamic keys (expressions) — walk normally
            _ => self.walk(id),
        }
    }

    /// Walk a node but emit it as a function name instead of plain name/variable.
    fn walk_as_function(&mut self, id: NodeId) {
        if id.is_empty() {
            return;
        }
        match self.arena.get(id) {
            Expr::Variable { pos, .. } => {
                let pos = *pos;
                let end = self.variable_end(pos);
                self.push(pos, end, HlType::Function);
            }
            Expr::Name { pos, .. } => {
                let pos = *pos;
                let end = self.name_end(pos);
                self.push(pos, end, HlType::Function);
            }
            _ => self.walk(id),
        }
    }

    fn walk_group(&mut self, group: Option<&GroupExpr>) {
        if let Some(g) = group {
            for pair in &g.pairs {
                self.walk(pair[0]);
                self.walk(pair[1]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse the JSON emitted by highlight() back into (start, end, type).
    fn spans(expr: &str) -> Vec<(usize, usize, String)> {
        let json = highlight(expr).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        parsed
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                let a = s.as_array().unwrap();
                (
                    a[0].as_u64().unwrap() as usize,
                    a[1].as_u64().unwrap() as usize,
                    a[2].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    /// Regex spans stop at the source flags — the lexer's synthetic
    /// trailing 'g' is not in the source (gnata-0mb.2).
    #[test]
    fn regex_span_excludes_synthetic_flag() {
        assert!(spans("$match(a, /ab+c/)").contains(&(10, 16, "regex".into())));
        assert!(spans("$match(a, /ab+c/i)").contains(&(10, 17, "regex".into())));
    }

    /// Folded negative number literals cover the sign; previously the
    /// span overran the source and was dropped entirely (gnata-0mb.2).
    #[test]
    fn negative_number_spans_cover_the_sign() {
        assert_eq!(spans("-42"), vec![(0, 3, "number".into())]);
        assert!(spans("x + -42").contains(&(4, 7, "number".into())));
        assert_eq!(spans("- 42"), vec![(0, 4, "number".into())]);
    }

    /// The λ lambda shorthand is 2 bytes, not 8 (gnata-0mb.2).
    #[test]
    fn lambda_shorthand_keyword_span() {
        assert!(spans("λ($x){$x}").contains(&(0, 2, "keyword".into())));
        assert!(spans("function($x){$x}").contains(&(0, 8, "keyword".into())));
    }

    /// "/*" inside backtick names or regex literals (character classes
    /// admit an unescaped '/') is not a comment; real comments still
    /// highlight (gnata-0mb.2).
    #[test]
    fn comment_scan_skips_token_interiors() {
        let s = spans("`weird/*name` + 1");
        assert!(s.contains(&(0, 13, "name".into())));
        assert!(!s.iter().any(|(_, _, t)| t == "comment"));

        let s = spans("$match(a, /[/*]/)");
        assert!(s.contains(&(10, 16, "regex".into())));
        assert!(!s.iter().any(|(_, _, t)| t == "comment"));

        assert!(spans("/* leading */ a + 1").contains(&(0, 13, "comment".into())));
        assert!(spans("a /* mid */ + 1").contains(&(2, 11, "comment".into())));
    }

    #[test]
    fn spans_are_in_bounds_and_cover_the_token_kinds() {
        let src = r#"$sum(items.price) + 42 - "x""#;
        let sp = spans(src);
        assert!(!sp.is_empty());
        for (start, end, typ) in &sp {
            assert!(
                start < end && *end <= src.len(),
                "span out of bounds: {start}..{end} ({typ})"
            );
        }
        let types: Vec<&str> = sp.iter().map(|(_, _, t)| t.as_str()).collect();
        for expected in ["function", "name", "number", "string", "operator"] {
            assert!(types.contains(&expected), "missing {expected} in {types:?}");
        }
    }

    #[test]
    fn function_call_position_highlights_as_function() {
        let sp = spans("$sum(a)");
        let f = sp
            .iter()
            .find(|(_, _, t)| t == "function")
            .expect("function span");
        assert_eq!(&"$sum(a)"[f.0..f.1], "$sum");
    }

    #[test]
    fn parse_errors_propagate() {
        assert!(highlight("(").is_err());
    }
}
