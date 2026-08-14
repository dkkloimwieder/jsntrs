//! JSONata expression formatter.
//!
//! Parses an expression using the existing parser, then walks the AST to emit
//! pretty-printed text with consistent indentation and line-breaking rules.

use crate::error::JsonataError;
use crate::parser::ast::{
    AstArena, BinaryOp, Expr, GroupExpr, NodeId, Signature, Stage, StageKind, UnaryOp,
};
use crate::parser::{Parser, process_ast};

const INDENT: &str = "  ";
const BREAK_THRESHOLD: usize = 3;
const LINE_WIDTH: usize = 60;

/// A block comment extracted from source: `/* text */` with its byte offset.
#[derive(Debug, Clone)]
struct Comment {
    text: String, // includes /* and */
    pos: usize,   // byte offset of the /*
}

/// Extract all block comments from source with their positions.
fn extract_comments(src: &str) -> Vec<Comment> {
    let bytes = src.as_bytes();
    let mut comments = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
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
            comments.push(Comment {
                text: src[start..i].to_string(),
                pos: start,
            });
        } else if bytes[i] == b'"' || bytes[i] == b'\'' {
            // Skip string literals to avoid false positives
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
        } else if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] != b'*' {
            // Could be regex or division — skip to avoid false comment detection
            i += 1;
        } else {
            i += 1;
        }
    }
    comments
}

/// Format a JSONata expression string.
///
/// # Errors
/// Returns `JsonataError` if the expression fails to parse.
pub fn format(expr: &str) -> Result<String, JsonataError> {
    let comments = extract_comments(expr);
    let (mut arena, root) = Parser::parse(expr)?;
    let root = process_ast(&mut arena, root)?;
    let mut f = Formatter::new(&arena, &comments);
    f.emit(root, 0);
    f.emit_trailing_comments();
    Ok(f.out.trim_end().to_string())
}

struct Formatter<'a> {
    arena: &'a AstArena,
    comments: &'a [Comment],
    comment_idx: usize, // next comment to emit
    out: String,
}

impl<'a> Formatter<'a> {
    fn new(arena: &'a AstArena, comments: &'a [Comment]) -> Self {
        Self {
            arena,
            comments,
            comment_idx: 0,
            out: String::new(),
        }
    }

    fn indent(&mut self, depth: usize) {
        for _ in 0..depth {
            self.out.push_str(INDENT);
        }
    }

    /// Emit any comments whose source position is before `pos`.
    /// Comments always go on their own line.
    fn emit_comments_before(&mut self, pos: usize, depth: usize) {
        while self.comment_idx < self.comments.len() && self.comments[self.comment_idx].pos < pos {
            let comment = &self.comments[self.comment_idx];
            // Ensure we're on a new line
            if !self.out.is_empty() && !self.out.ends_with('\n') {
                self.out.push('\n');
            }
            self.indent(depth);
            self.out.push_str(&comment.text);
            self.out.push('\n');
            self.indent(depth);
            self.comment_idx += 1;
        }
    }

    /// Emit any remaining comments that haven't been placed yet.
    fn emit_trailing_comments(&mut self) {
        while self.comment_idx < self.comments.len() {
            let comment = &self.comments[self.comment_idx];
            if !self.out.is_empty() && !self.out.ends_with('\n') {
                self.out.push('\n');
            }
            self.out.push_str(&comment.text);
            self.comment_idx += 1;
        }
    }

    #[expect(clippy::too_many_lines)]
    fn emit(&mut self, id: NodeId, depth: usize) {
        if id.is_empty() {
            return;
        }
        let expr = self.arena.get(id).clone();
        // Emit comments that appear before this node in the source
        self.emit_comments_before(expr.pos(), depth);
        match expr {
            Expr::Name {
                ref value,
                ref stages,
                ref group,
                keep_array,
                ref focus,
                ref index,
                ..
            } => {
                if keep_array {
                    self.out.push_str(&escape_name(value));
                    self.out.push_str("[]");
                } else {
                    self.out.push_str(&escape_name(value));
                }
                self.emit_stages(stages, depth);
                self.emit_bindings(focus.as_deref(), index.as_deref());
                self.emit_group(group.as_ref(), depth);
            }
            Expr::StringLit { ref value, .. } => {
                self.out.push('"');
                self.out.push_str(&escape_string(value));
                self.out.push('"');
            }
            Expr::NumberLit { ref raw, .. } => {
                self.out.push_str(raw);
            }
            Expr::ValueLit { ref value, .. } => {
                self.out.push_str(value);
            }
            Expr::Variable {
                ref name,
                ref group,
                keep_array,
                ref focus,
                ref index,
                ..
            } => {
                if name == "$" {
                    self.out.push_str("$$");
                } else if name.is_empty() {
                    self.out.push('$');
                } else {
                    self.out.push('$');
                    self.out.push_str(name);
                }
                if keep_array {
                    self.out.push_str("[]");
                }
                self.emit_bindings(focus.as_deref(), index.as_deref());
                self.emit_group(group.as_ref(), depth);
            }
            Expr::Wildcard { .. } => self.out.push('*'),
            Expr::Descendant { .. } => self.out.push_str("**"),
            Expr::Parent { ref slot, .. } => {
                self.out.push('%');
                if let Some(slot) = slot {
                    self.out.push_str(&slot.label);
                }
            }
            Expr::Regex {
                ref pattern,
                ref flags,
                ..
            } => {
                self.out.push('/');
                self.out.push_str(pattern);
                self.out.push('/');
                self.out.push_str(flags);
            }
            Expr::Placeholder { .. } => self.out.push('?'),

            Expr::Path {
                ref steps,
                ref group,
                ..
            } => {
                self.emit_path(steps, group.as_ref(), depth);
            }

            Expr::Binary {
                op,
                lhs,
                rhs,
                ref group,
                ref focus,
                ref index,
                ..
            } => {
                self.emit_binary(
                    op,
                    lhs,
                    rhs,
                    group.as_ref(),
                    focus.as_deref(),
                    index.as_deref(),
                    depth,
                );
            }

            Expr::Unary {
                op,
                operand,
                ref expressions,
                ref lhs,
                ref group,
                ..
            } => match op {
                UnaryOp::Negate => {
                    self.out.push('-');
                    self.emit(operand, depth);
                }
                UnaryOp::ArrayCons => {
                    self.emit_array(expressions, depth);
                }
                UnaryOp::ObjCons => {
                    self.emit_object(lhs, group.as_ref(), depth);
                }
            },

            Expr::Block {
                ref expressions,
                ref focus,
                ref index,
                ..
            } => {
                self.emit_block(expressions, depth);
                self.emit_bindings(focus.as_deref(), index.as_deref());
            }

            Expr::Condition {
                condition,
                then,
                else_,
                ..
            } => {
                self.emit_condition(condition, then, else_, depth);
            }

            Expr::Bind { lhs, rhs, .. } => {
                self.emit(lhs, depth);
                self.out.push_str(" := ");
                self.emit(rhs, depth);
            }

            Expr::Function {
                procedure,
                ref arguments,
                ref group,
                ..
            } => {
                self.emit(procedure, depth);
                self.emit_args(arguments, depth);
                self.emit_group(group.as_ref(), depth);
            }

            Expr::Partial {
                procedure,
                ref arguments,
                ..
            } => {
                self.emit(procedure, depth);
                self.emit_args(arguments, depth);
            }

            Expr::Lambda {
                ref params,
                body,
                ref signature,
                ..
            } => {
                self.emit_lambda(params, body, signature.as_ref(), depth);
            }

            Expr::Transform {
                pattern,
                update,
                delete,
                ..
            } => {
                self.out.push('|');
                self.emit(pattern, depth);
                self.out.push('|');
                self.emit(update, depth);
                if let Some(del) = delete {
                    self.out.push_str(", ");
                    self.emit(del, depth);
                }
                self.out.push('|');
            }

            Expr::Sort {
                expr,
                ref terms,
                ref focus,
                ref index,
                ..
            } => {
                self.emit(expr, depth);
                self.out.push_str("^(");
                for (i, term) in terms.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    if term.descending {
                        self.out.push('>');
                    } else {
                        self.out.push('<');
                    }
                    self.emit(term.expression, depth);
                }
                self.out.push(')');
                self.emit_bindings(focus.as_deref(), index.as_deref());
            }

            Expr::Grouped {
                expr, ref group, ..
            } => {
                self.emit(expr, depth);
                self.emit_group(Some(group), depth);
            }
        }
    }

    /// Render a subtree into a scratch string, advancing this formatter's
    /// comment cursor. The result is used for BOTH width measurement and
    /// emission — formatting each subtree exactly once. (Measuring with a
    /// throwaway render and then emitting again was exponential in
    /// nested-ternary depth: ~40 ternaries hung the public `format()`.)
    fn render(&mut self, id: NodeId, depth: usize) -> String {
        let mut f = Formatter::new(self.arena, self.comments);
        f.comment_idx = self.comment_idx;
        f.emit(id, depth);
        self.comment_idx = f.comment_idx;
        f.out
    }

    fn emit_path(&mut self, steps: &[NodeId], group: Option<&GroupExpr>, depth: usize) {
        if steps.len() > BREAK_THRESHOLD {
            self.emit(steps[0], depth);
            for &step in &steps[1..] {
                self.out.push('\n');
                self.indent(depth + 1);
                self.out.push('.');
                self.emit(step, depth + 1);
            }
        } else {
            for (i, &step) in steps.iter().enumerate() {
                if i > 0 {
                    self.out.push('.');
                }
                self.emit(step, depth);
            }
        }
        self.emit_group(group, depth);
    }

    #[expect(clippy::too_many_arguments)]
    fn emit_binary(
        &mut self,
        op: BinaryOp,
        lhs: NodeId,
        rhs: NodeId,
        group: Option<&GroupExpr>,
        focus: Option<&str>,
        index: Option<&str>,
        depth: usize,
    ) {
        match op {
            BinaryOp::Subscript => {
                self.emit(lhs, depth);
                self.out.push('[');
                self.emit(rhs, depth);
                self.out.push(']');
            }
            BinaryOp::Range => {
                self.emit(lhs, depth);
                self.out.push_str("..");
                self.emit(rhs, depth);
            }
            _ => {
                self.emit(lhs, depth);
                // Word (and, or, in) and symbol operators space identically.
                self.out.push(' ');
                self.out.push_str(op.as_str());
                self.out.push(' ');
                self.emit(rhs, depth);
            }
        }
        self.emit_bindings(focus, index);
        self.emit_group(group, depth);
    }

    fn emit_args(&mut self, args: &[NodeId], depth: usize) {
        if args.len() > BREAK_THRESHOLD {
            self.out.push_str("(\n");
            for (i, &arg) in args.iter().enumerate() {
                self.indent(depth + 1);
                self.emit(arg, depth + 1);
                if i < args.len() - 1 {
                    self.out.push(',');
                }
                self.out.push('\n');
            }
            self.indent(depth);
            self.out.push(')');
        } else {
            self.out.push('(');
            for (i, &arg) in args.iter().enumerate() {
                if i > 0 {
                    self.out.push_str(", ");
                }
                self.emit(arg, depth);
            }
            self.out.push(')');
        }
    }

    fn emit_array(&mut self, elements: &[NodeId], depth: usize) {
        if elements.len() > BREAK_THRESHOLD {
            self.out.push_str("[\n");
            for (i, &el) in elements.iter().enumerate() {
                self.indent(depth + 1);
                self.emit(el, depth + 1);
                if i < elements.len() - 1 {
                    self.out.push(',');
                }
                self.out.push('\n');
            }
            self.indent(depth);
            self.out.push(']');
        } else {
            self.out.push('[');
            for (i, &el) in elements.iter().enumerate() {
                if i > 0 {
                    self.out.push_str(", ");
                }
                self.emit(el, depth);
            }
            self.out.push(']');
        }
    }

    fn emit_object(&mut self, lhs: &[NodeId], group: Option<&GroupExpr>, depth: usize) {
        // lhs is flat [k0, v0, k1, v1, ...]
        let pair_count = lhs.len() / 2;
        if pair_count > BREAK_THRESHOLD {
            self.out.push_str("{\n");
            for i in 0..pair_count {
                self.indent(depth + 1);
                self.emit(lhs[i * 2], depth + 1);
                self.out.push_str(": ");
                self.emit(lhs[i * 2 + 1], depth + 1);
                if i < pair_count - 1 {
                    self.out.push(',');
                }
                self.out.push('\n');
            }
            self.indent(depth);
            self.out.push('}');
        } else {
            self.out.push('{');
            for i in 0..pair_count {
                if i > 0 {
                    self.out.push_str(", ");
                }
                self.emit(lhs[i * 2], depth);
                self.out.push_str(": ");
                self.emit(lhs[i * 2 + 1], depth);
            }
            self.out.push('}');
        }
        self.emit_group(group, depth);
    }

    fn emit_block(&mut self, expressions: &[NodeId], depth: usize) {
        if expressions.len() == 1 {
            self.out.push('(');
            self.emit(expressions[0], depth);
            self.out.push(')');
        } else {
            self.out.push_str("(\n");
            for (i, &expr) in expressions.iter().enumerate() {
                self.indent(depth + 1);
                self.emit(expr, depth + 1);
                if i < expressions.len() - 1 {
                    self.out.push(';');
                }
                self.out.push('\n');
            }
            self.indent(depth);
            self.out.push(')');
        }
    }

    fn emit_condition(
        &mut self,
        condition: NodeId,
        then: NodeId,
        else_: Option<NodeId>,
        depth: usize,
    ) {
        // Render each subtree once; the strings serve measurement and
        // emission. Branches are rendered at depth+1 (the multiline
        // indent); a newline-free render is depth-invariant, so the same
        // string is correct if the inline layout wins.
        let cond_str = self.render(condition, depth);
        let then_str = self.render(then, depth + 1);
        let else_str = else_.map(|e| self.render(e, depth + 1));

        let inline_len =
            cond_str.len() + 3 + then_str.len() + else_str.as_ref().map_or(0, |s| 3 + s.len());

        if inline_len <= LINE_WIDTH
            && !cond_str.contains('\n')
            && !then_str.contains('\n')
            && else_str.as_ref().is_none_or(|s| !s.contains('\n'))
        {
            self.out.push_str(&cond_str);
            self.out.push_str(" ? ");
            self.out.push_str(&then_str);
            if let Some(e) = &else_str {
                self.out.push_str(" : ");
                self.out.push_str(e);
            }
        } else {
            self.out.push_str(&cond_str);
            self.out.push('\n');
            self.indent(depth + 1);
            self.out.push_str("? ");
            self.out.push_str(&then_str);
            if let Some(e) = &else_str {
                self.out.push('\n');
                self.indent(depth + 1);
                self.out.push_str(": ");
                self.out.push_str(e);
            }
        }
    }

    fn emit_lambda(
        &mut self,
        params: &[NodeId],
        body: NodeId,
        signature: Option<&Signature>,
        depth: usize,
    ) {
        self.out.push_str("function(");
        for (i, &p) in params.iter().enumerate() {
            if i > 0 {
                self.out.push_str(", ");
            }
            self.emit(p, depth);
        }
        self.out.push(')');
        if let Some(sig) = signature {
            self.out.push('<');
            self.out.push_str(&sig.raw);
            self.out.push('>');
        }
        self.out.push_str(" {\n");
        self.indent(depth + 1);
        self.emit(body, depth + 1);
        self.out.push('\n');
        self.indent(depth);
        self.out.push('}');
    }

    /// Emit `@$focus` / `#$index` bindings after their owning step.
    /// The parser stores the variable names without the `$` sigil.
    fn emit_bindings(&mut self, focus: Option<&str>, index: Option<&str>) {
        if let Some(f) = focus {
            self.out.push_str("@$");
            self.out.push_str(f);
        }
        if let Some(i) = index {
            self.out.push_str("#$");
            self.out.push_str(i);
        }
    }

    fn emit_stages(&mut self, stages: &[Stage], depth: usize) {
        for stage in stages {
            match &stage.kind {
                StageKind::Filter { expression } => {
                    self.out.push('[');
                    self.emit(*expression, depth);
                    self.out.push(']');
                }
                StageKind::Index { var_name } => {
                    self.out.push('#');
                    self.out.push_str(var_name);
                }
            }
        }
    }

    fn emit_group(&mut self, group: Option<&GroupExpr>, depth: usize) {
        if let Some(g) = group {
            let pair_count = g.pairs.len();
            if pair_count > BREAK_THRESHOLD {
                self.out.push_str("{\n");
                for (i, pair) in g.pairs.iter().enumerate() {
                    self.indent(depth + 1);
                    self.emit(pair[0], depth + 1);
                    self.out.push_str(": ");
                    self.emit(pair[1], depth + 1);
                    if i < pair_count - 1 {
                        self.out.push(',');
                    }
                    self.out.push('\n');
                }
                self.indent(depth);
                self.out.push('}');
            } else {
                self.out.push('{');
                for (i, pair) in g.pairs.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    self.emit(pair[0], depth);
                    self.out.push_str(": ");
                    self.emit(pair[1], depth);
                }
                self.out.push('}');
            }
        }
    }
}

/// Escape a name that contains special characters or is a keyword.
fn escape_name(name: &str) -> String {
    if name.is_empty() || needs_backtick(name) {
        format!("`{name}`")
    } else {
        name.to_string()
    }
}

fn needs_backtick(name: &str) -> bool {
    // Keywords and names with special chars need backtick quoting
    let keywords = ["and", "or", "in", "true", "false", "null", "function"];
    if keywords.contains(&name) {
        return true;
    }
    let first = name.chars().next().unwrap_or(' ');
    if !first.is_alphabetic() && first != '_' {
        return true;
    }
    name.chars().any(|c| !c.is_alphanumeric() && c != '_')
}

fn escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(expr: &str) -> String {
        format(expr).unwrap_or_else(|e| panic!("format failed for `{expr}`: {e}"))
    }

    // ── Literals ──────────────────────────────────────────────

    #[test]
    fn literals() {
        assert_eq!(fmt("42"), "42");
        assert_eq!(fmt("\"hello\""), "\"hello\"");
        assert_eq!(fmt("true"), "true");
        assert_eq!(fmt("false"), "false");
        assert_eq!(fmt("null"), "null");
    }

    #[test]
    fn word_and_symbol_operators_spaced_uniformly() {
        assert_eq!(fmt("a and b"), "a and b");
        assert_eq!(fmt("a or b"), "a or b");
        assert_eq!(fmt("\"x\" in y"), "\"x\" in y");
        assert_eq!(fmt("a~>$sum()"), "a ~> $sum()");
    }

    #[test]
    fn number_literals_preserved() {
        assert_eq!(fmt("3.14159"), "3.14159");
        assert_eq!(fmt("0"), "0");
        assert_eq!(fmt("1e10"), "1e10");
        assert_eq!(fmt("-42"), "-42");
    }

    #[test]
    fn string_escapes_preserved() {
        assert_eq!(fmt(r#""hello \"world\"""#), r#""hello \"world\"""#);
        assert_eq!(fmt(r#""line\nbreak""#), r#""line\nbreak""#);
        assert_eq!(fmt(r#""tab\there""#), r#""tab\there""#);
    }

    #[test]
    fn regex_literals() {
        // Parser may add implicit flags (e.g., 'g'), so just check roundtrip
        let r = fmt("/abc/");
        assert!(r.starts_with('/') && r.len() > 2, "should be regex: {r}");
        assert_eq!(fmt("/test/i"), "/test/ig");
        assert_eq!(fmt("/^foo.*bar$/m"), "/^foo.*bar$/mg");
    }

    // ── Path expressions ─────────────────────────────────────

    #[test]
    fn simple_path() {
        assert_eq!(fmt("a.b.c"), "a.b.c");
    }

    #[test]
    fn path_two_steps() {
        assert_eq!(fmt("a.b"), "a.b");
    }

    #[test]
    fn path_at_threshold() {
        // Exactly 3 steps = threshold, should stay inline
        assert_eq!(fmt("a.b.c"), "a.b.c");
    }

    #[test]
    fn long_path_breaks() {
        let result = fmt("a.b.c.d");
        assert!(result.contains('\n'), "expected multiline, got: {result}");
        // Each continuation step should be indented with leading dot
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines[0], "a");
        for line in &lines[1..] {
            let trimmed = line.trim();
            assert!(
                trimmed.starts_with('.'),
                "continuation should start with dot: {line}"
            );
        }
    }

    #[test]
    fn very_long_path() {
        let result = fmt("a.b.c.d.e.f");
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines.len(), 6, "6 steps = 6 lines: {result}");
    }

    // ── Function calls ───────────────────────────────────────

    #[test]
    fn short_function_call() {
        assert_eq!(fmt("$sum(a, b, c)"), "$sum(a, b, c)");
    }

    #[test]
    fn function_no_args() {
        assert_eq!(fmt("$now()"), "$now()");
    }

    #[test]
    fn function_one_arg() {
        assert_eq!(fmt("$count(items)"), "$count(items)");
    }

    #[test]
    fn long_function_call_breaks() {
        let result = fmt("$foo(a, b, c, d)");
        assert!(result.contains('\n'), "expected multiline, got: {result}");
        // Each arg on its own line, indented
        let lines: Vec<&str> = result.lines().collect();
        assert!(
            lines[0].ends_with('('),
            "first line should end with '(': {}",
            lines[0]
        );
        assert_eq!(
            lines.last().unwrap().trim(),
            ")",
            "last line should be closing paren"
        );
    }

    #[test]
    fn nested_function_calls() {
        assert_eq!(fmt("$sum($map(x, f))"), "$sum($map(x, f))");
    }

    // ── Block expressions ────────────────────────────────────

    #[test]
    fn single_expression_block() {
        assert_eq!(fmt("($x + 1)"), "($x + 1)");
    }

    #[test]
    fn block_semicolons() {
        let result = fmt("($x := 1; $y := 2; $x + $y)");
        assert!(result.contains('\n'));
        assert!(result.contains("$x := 1;"));
        assert!(result.contains("$y := 2;"));
    }

    #[test]
    fn block_each_stmt_on_line() {
        let result = fmt("($a := 1; $b := 2; $a + $b)");
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines[0], "(");
        assert!(lines[1].trim().starts_with("$a"));
        assert!(lines[2].trim().starts_with("$b"));
        assert!(lines[3].trim().starts_with("$a"));
        assert_eq!(lines[4].trim(), ")");
    }

    // ── Conditionals ─────────────────────────────────────────

    #[test]
    fn simple_condition_inline() {
        assert_eq!(fmt("x ? 1 : 0"), "x ? 1 : 0");
    }

    #[test]
    fn condition_without_else() {
        assert_eq!(fmt("x ? 1"), "x ? 1");
    }

    #[test]
    fn long_condition_multiline() {
        // Build something that exceeds LINE_WIDTH (60)
        let expr = "this_is_a_long_variable_name ? this_is_another_long_value : yet_another_long_fallback_value";
        let result = fmt(expr);
        assert!(
            result.contains('\n'),
            "expected multiline for long condition, got: {result}"
        );
        assert!(
            result.contains("? "),
            "should have ? on its own indented line: {result}"
        );
        assert!(
            result.contains(": "),
            "should have : on its own indented line: {result}"
        );
    }

    #[test]
    fn nested_conditions() {
        let result = fmt("a ? b ? 1 : 2 : 3");
        assert!(
            result.contains("?"),
            "should contain ternary operator: {result}"
        );
        // Should be idempotent
        let second = fmt(&result);
        assert_eq!(result, second, "nested conditions not idempotent");
    }

    // ── Lambda / function definitions ────────────────────────

    #[test]
    fn lambda_multiline() {
        let result = fmt("function($x) { $x + 1 }");
        assert!(result.contains('\n'));
        assert!(result.contains("function($x)"));
        assert!(result.contains("$x + 1"));
    }

    #[test]
    fn lambda_multiple_params() {
        let result = fmt("function($x, $y, $z) { $x + $y + $z }");
        assert!(result.contains("function($x, $y, $z)"));
    }

    #[test]
    fn lambda_with_signature() {
        let result = fmt("function($x)<n:n> { $x + 1 }");
        assert!(
            result.contains("<n:n>"),
            "should preserve type signature: {result}"
        );
    }

    #[test]
    fn lambda_body_indented() {
        let result = fmt("function($x) { $x + 1 }");
        let lines: Vec<&str> = result.lines().collect();
        assert!(
            lines.len() >= 3,
            "lambda should be at least 3 lines: {result}"
        );
        // Body line should be indented
        assert!(
            lines[1].starts_with(INDENT),
            "body should be indented: {}",
            lines[1]
        );
    }

    // ── Object constructors ──────────────────────────────────

    #[test]
    fn short_object_inline() {
        let result = fmt("{\"a\": 1, \"b\": 2}");
        assert!(
            !result.contains('\n'),
            "short object should be inline: {result}"
        );
        assert!(result.contains("{"));
        assert!(result.contains("}"));
    }

    #[test]
    fn long_object_expanded() {
        let result = fmt("{\"a\": 1, \"b\": 2, \"c\": 3, \"d\": 4}");
        assert!(
            result.contains('\n'),
            "object with >3 pairs should expand: {result}"
        );
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines[0].trim(), "{");
        assert_eq!(lines.last().unwrap().trim(), "}");
    }

    #[test]
    fn object_three_pairs_inline() {
        let result = fmt("{\"a\": 1, \"b\": 2, \"c\": 3}");
        assert!(
            !result.contains('\n'),
            "3 pairs = threshold, should be inline: {result}"
        );
    }

    // ── Array constructors ───────────────────────────────────

    #[test]
    fn short_array_inline() {
        assert_eq!(fmt("[1, 2, 3]"), "[1, 2, 3]");
    }

    #[test]
    fn long_array_expanded() {
        let result = fmt("[1, 2, 3, 4]");
        assert!(
            result.contains('\n'),
            "array with >3 elements should expand: {result}"
        );
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines[0].trim(), "[");
        assert_eq!(lines.last().unwrap().trim(), "]");
    }

    #[test]
    fn empty_array() {
        assert_eq!(fmt("[]"), "[]");
    }

    #[test]
    fn single_element_array() {
        assert_eq!(fmt("[1]"), "[1]");
    }

    // ── Binary operators ─────────────────────────────────────

    #[test]
    fn arithmetic_operators() {
        assert_eq!(fmt("a + b"), "a + b");
        assert_eq!(fmt("a - b"), "a - b");
        assert_eq!(fmt("a * b"), "a * b");
        assert_eq!(fmt("a / b"), "a / b");
        assert_eq!(fmt("a % b"), "a % b");
    }

    #[test]
    fn comparison_operators() {
        assert_eq!(fmt("a = b"), "a = b");
        assert_eq!(fmt("a != b"), "a != b");
        assert_eq!(fmt("a < b"), "a < b");
        assert_eq!(fmt("a <= b"), "a <= b");
        assert_eq!(fmt("a > b"), "a > b");
        assert_eq!(fmt("a >= b"), "a >= b");
    }

    #[test]
    fn word_operators() {
        assert_eq!(fmt("a and b"), "a and b");
        assert_eq!(fmt("a or b"), "a or b");
        assert_eq!(fmt("a in b"), "a in b");
    }

    #[test]
    fn string_concat_operator() {
        assert_eq!(fmt("a & b"), "a & b");
    }

    #[test]
    fn chain_operator() {
        assert_eq!(fmt("a ~> b"), "a ~> b");
    }

    #[test]
    fn range_operator() {
        assert_eq!(fmt("[1..5]"), "[1..5]");
    }

    #[test]
    fn subscript_operator() {
        assert_eq!(fmt("a[0]"), "a[0]");
    }

    // ── Variables ────────────────────────────────────────────

    #[test]
    fn variable_binding() {
        assert_eq!(fmt("$x := 42"), "$x := 42");
    }

    #[test]
    fn root_variable() {
        assert_eq!(fmt("$$"), "$$");
    }

    #[test]
    fn context_variable() {
        assert_eq!(fmt("$"), "$");
    }

    // ── Special tokens ───────────────────────────────────────

    #[test]
    fn wildcard() {
        assert_eq!(fmt("a.*"), "a.*");
    }

    #[test]
    fn descendant() {
        assert_eq!(fmt("a.**"), "a.**");
    }

    // ── Transform expressions ────────────────────────────────

    #[test]
    fn transform_update() {
        let result = fmt("|a|b|");
        assert!(
            result.contains("|"),
            "transform should use pipe syntax: {result}"
        );
        assert!(result.contains("a"));
        assert!(result.contains("b"));
    }

    #[test]
    fn transform_update_delete() {
        let result = fmt("|a|b, c|");
        assert!(result.contains("a"));
        assert!(result.contains("b"));
        assert!(result.contains("c"));
    }

    // ── Sort expressions ─────────────────────────────────────

    #[test]
    fn sort_ascending() {
        let result = fmt("data^(<price)");
        assert!(result.contains("^("), "should have sort syntax: {result}");
        assert!(
            result.contains("<price"),
            "should have ascending marker: {result}"
        );
    }

    #[test]
    fn sort_descending() {
        let result = fmt("data^(>price)");
        assert!(
            result.contains(">price"),
            "should have descending marker: {result}"
        );
    }

    #[test]
    fn sort_multi_key() {
        let result = fmt("data^(<category, >price)");
        assert!(
            result.contains("<category"),
            "first key ascending: {result}"
        );
        assert!(result.contains(">price"), "second key descending: {result}");
    }

    // ── Unary negate ─────────────────────────────────────────

    #[test]
    fn unary_negate() {
        assert_eq!(fmt("-x"), "-x");
    }

    // ── Name escaping ────────────────────────────────────────

    #[test]
    fn backtick_keywords() {
        // "and", "or", "in", etc. used as field names need backtick quoting
        assert_eq!(fmt("`and`"), "`and`");
        assert_eq!(fmt("`or`"), "`or`");
        assert_eq!(fmt("`in`"), "`in`");
        assert_eq!(fmt("`true`"), "`true`");
        assert_eq!(fmt("`false`"), "`false`");
        assert_eq!(fmt("`null`"), "`null`");
    }

    #[test]
    fn backtick_special_chars() {
        assert_eq!(fmt("`hello world`"), "`hello world`");
        assert_eq!(fmt("`foo-bar`"), "`foo-bar`");
    }

    #[test]
    fn normal_name_no_backtick() {
        assert_eq!(fmt("foo"), "foo");
        assert_eq!(fmt("_private"), "_private");
        assert_eq!(fmt("camelCase"), "camelCase");
    }

    // ── Filter stages ────────────────────────────────────────

    #[test]
    fn filter_stage() {
        assert_eq!(fmt("items[price > 10]"), "items[price > 10]");
    }

    #[test]
    fn chained_filters() {
        let result = fmt("items[type = \"book\"][price < 20]");
        assert!(
            result.contains("[type = \"book\"]"),
            "first filter: {result}"
        );
        assert!(result.contains("[price < 20]"), "second filter: {result}");
    }

    // ── Nested indentation ───────────────────────────────────

    #[test]
    fn nested_lambda_in_block() {
        let result = fmt("($f := function($x) { $x * 2 }; $f(5))");
        assert!(result.contains('\n'), "should be multiline: {result}");
        // The lambda body should be further indented than the block body
        let lines: Vec<&str> = result.lines().collect();
        let lambda_body = lines.iter().find(|l| l.contains("$x * 2"));
        assert!(
            lambda_body.is_some(),
            "should contain lambda body: {result}"
        );
        let body_indent = lambda_body.unwrap().len() - lambda_body.unwrap().trim_start().len();
        assert!(
            body_indent >= INDENT.len() * 2,
            "lambda body should be double-indented: {result}"
        );
    }

    #[test]
    fn nested_array_in_function() {
        let result = fmt("$map([1, 2, 3, 4], function($v) { $v + 1 })");
        assert!(result.contains('\n'), "should be multiline: {result}");
    }

    // ── Comments ─────────────────────────────────────────────

    #[test]
    fn preserves_comments() {
        let result = fmt("/* header */ $x + /* inline */ $y");
        assert!(
            result.contains("/* header */"),
            "missing header comment: {result}"
        );
        assert!(
            result.contains("/* inline */"),
            "missing inline comment: {result}"
        );
        for line in result.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("/*") {
                assert!(trimmed.ends_with("*/"), "comment not on own line: {result}");
            }
        }
    }

    #[test]
    fn trailing_comment() {
        let result = fmt("$x + $y /* end */");
        assert!(
            result.contains("/* end */"),
            "missing trailing comment: {result}"
        );
    }

    #[test]
    fn multiline_comment() {
        let result = fmt("$x /* \n  ***\n  */ + $y");
        assert!(
            result.contains("/* \n  ***\n  */"),
            "missing multiline comment: {result}"
        );
    }

    #[test]
    fn comment_inside_string_not_extracted() {
        // "/* not a comment */" is a string literal, should not be treated as comment
        let result = fmt(r#""/* not a comment */""#);
        assert_eq!(result, r#""/* not a comment */""#);
    }

    // ── Partial application ──────────────────────────────────

    #[test]
    fn partial_application() {
        let result = fmt("$sum(?, 1)");
        assert!(
            result.contains("?"),
            "should preserve placeholder: {result}"
        );
        assert!(result.contains("1"), "should preserve arg: {result}");
    }

    // ── Keep-array modifier ──────────────────────────────────

    #[test]
    fn keep_array_name() {
        assert_eq!(fmt("items[]"), "items[]");
    }

    // ── Roundtrip / Idempotency ──────────────────────────────

    #[test]
    fn idempotent() {
        let exprs = [
            "$sum(a, b, c)",
            "a.b.c",
            "($x := 1; $y := 2; $x + $y)",
            "x ? 1 : 0",
            "/* comment */ $x + $y",
            "$x + $y /* trailing */",
            "function($x) { $x + 1 }",
            "{\"a\": 1, \"b\": 2}",
            "[1, 2, 3]",
            "a.b.c.d.e",
            "$foo(a, b, c, d)",
            "data^(<price, >name)",
            "|target|update, delete|",
            "$x and $y or $z",
            "items[price > 10]",
            "-x",
            "a[0]",
            "[1..5]",
            "a & b ~> c",
        ];
        for expr in exprs {
            let first = fmt(expr);
            let second = fmt(&first);
            assert_eq!(
                first, second,
                "not idempotent for: {expr}\nfirst:  {first}\nsecond: {second}"
            );
        }
    }

    #[test]
    fn roundtrip_complex() {
        // A realistic complex expression
        let expr = r#"Account.Order.Product{
  $."Product Name": $sum(Price)
}"#;
        // We can't assert exact output format for complex expressions since
        // the formatter canonicalizes, but it must be idempotent
        if let Ok(first) = format(expr) {
            let second = fmt(&first);
            assert_eq!(first, second, "complex expression not idempotent");
        }
    }

    // ── Focus / index bindings (@$var, #$var) ────────────────

    #[test]
    fn focus_binding_on_name_step() {
        assert_eq!(fmt("a@$v.$v"), "a@$v.$v");
    }

    #[test]
    fn index_binding_on_name_step() {
        assert_eq!(fmt("a#$i"), "a#$i");
    }

    #[test]
    fn focus_and_index_on_same_step() {
        assert_eq!(fmt("a@$v#$i"), "a@$v#$i");
        // Reversed source order canonicalizes to focus-then-index —
        // both attach to the same node, so the AST is identical.
        assert_eq!(fmt("a#$i@$v"), "a@$v#$i");
    }

    #[test]
    fn index_binding_on_subscript_step() {
        // The predicate binds first ([ bp 80 > # bp 75), so the index
        // owner is the Binary subscript node, not the name. (Focus on a
        // subscript is unreachable: `@` after a predicate is S0215.)
        assert_eq!(fmt("a[0]#$i.$i"), "a[0]#$i.$i");
    }

    #[test]
    fn focus_binding_on_block_step() {
        assert_eq!(fmt("(a)@$v.$v"), "(a)@$v.$v");
    }

    #[test]
    fn index_binding_on_block_step() {
        assert_eq!(fmt("a.($ * 2)#$i.$i"), "a.($ * 2)#$i.$i");
    }

    #[test]
    fn focus_binding_on_variable_step() {
        assert_eq!(fmt("$x@$v"), "$x@$v");
    }

    #[test]
    fn index_binding_on_sort_step() {
        // Focus on a sort is unreachable: `@` after `^(...)` is S0216.
        assert_eq!(fmt("data^(<price)#$i.$i"), "data^(<price)#$i.$i");
    }

    #[test]
    fn focus_binding_survives_path_break() {
        // Four steps exceed BREAK_THRESHOLD, so the path goes multiline;
        // the binding stays glued to its owning step.
        assert_eq!(
            fmt("Account.Order@$o.Product.($o)"),
            "Account\n  .Order@$o\n  .Product\n  .($o)"
        );
    }

    #[test]
    fn focus_binding_before_group() {
        let result = fmt("a@$v{\"k\": $v}");
        assert_eq!(result, "a@$v{\"k\": $v}");
    }

    /// The formatted text must compile and evaluate to the same result as
    /// the source. Expected values are Go-verified (scratch-test recipe,
    /// 2026-08-07). Results compare via `Display` (compact JSON, undefined
    /// → "") because `Value`'s PartialEq keeps `undefined != undefined`.
    #[test]
    fn bindings_are_idempotent_and_evaluate_identically() {
        use crate::Expression;
        let data = r#"{"a": [1, 2], "Account": {"Order": [{"Product": [{"p": 1}, {"p": 2}]}]}, "data": [{"price": 2}, {"price": 1}]}"#;
        let cases = [
            ("a@$v.$v", "[1,2]"),
            ("a#$i.$i", "[0,1]"),
            ("a@$v#$i.[$v, $i]", "[1,0,2,1]"),
            ("(a)@$v.$v", "[1,2]"),
            ("a.($ * 2)#$i.$i", "[0,0]"),
            ("a[0]#$i.$i", "0"),
            // Go also yields undefined for these two shapes.
            ("data^(<price)#$i.$i", ""),
            ("Account.Order@$o.Product.($o.Product.p)", ""),
        ];
        for (expr, expected) in cases {
            let first = fmt(expr);
            assert_eq!(fmt(&first), first, "not idempotent for: {expr}");
            let eval = |src: &str| {
                Expression::compile(src)
                    .unwrap_or_else(|e| panic!("compile `{src}`: {e}"))
                    .evaluate(data)
                    .unwrap_or_else(|e| panic!("eval `{src}`: {e}"))
                    .to_string()
            };
            assert_eq!(eval(expr), expected, "unexpected source result: {expr}");
            assert_eq!(
                eval(&first),
                expected,
                "formatted output changed semantics for: {expr} -> {first}"
            );
        }
    }

    // ── Error handling ───────────────────────────────────────

    #[test]
    fn parse_error_returns_err() {
        assert!(format("$foo(").is_err(), "unclosed paren should error");
        assert!(format("[1, 2,").is_err(), "unclosed bracket should error");
    }

    /// Each ternary subtree is rendered exactly once — the old
    /// measure-then-emit shape was 2^depth, hanging on ~40 nested
    /// ternaries (gnata-emj.7). 150 levels must format instantly.
    #[test]
    fn deeply_nested_ternaries_format_in_linear_time() {
        let mut expr = String::from("z");
        for i in (0..150).rev() {
            expr = format!("a{i} ? b{i} : ({expr})");
        }
        let out = format(&expr).expect("nested ternaries format");
        assert!(out.contains("a0"));
        assert!(out.contains('z'));
        // Idempotence still holds for the broken-line layout.
        assert_eq!(format(&out).expect("reformat"), out);
    }
}
