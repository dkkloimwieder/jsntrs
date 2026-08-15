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

/// The characters the JSONata lexer skips between tokens — and the only ones
/// `format` may strip from the end of its output.
///
/// `String::trim_end` uses Unicode's much wider notion of whitespace, which
/// includes characters the lexer treats as ordinary text: `\x0c`, `\u{a0}`,
/// `\u{2028}` and friends are not stop characters, so one at the end of the
/// expression belongs to the last *token* — a field name, say — and trimming
/// it changed the expression (jsntrs-ecq.12). Everything `emit` adds as
/// padding is in this set.
const LEXER_SPACE: [char; 5] = [' ', '\t', '\n', '\r', '\u{b}'];

/// A block comment extracted from source: `/* text */` with its byte offset.
#[derive(Debug, Clone)]
struct Comment {
    text: String, // includes /* and */
    pos: usize,   // byte offset of the /*
}

/// Source spans of the tokens a `/*` can hide inside.
///
/// The comment scan works on raw bytes, so anything that only *looks* like
/// a comment marker has to be stepped over. Skipping string literals was
/// not enough: a `/*` inside a backtick-quoted name came back out as a
/// stray comment line — one more copy on every reformat — and a lone quote
/// inside such a name made the scan read the rest of the source as an
/// unclosed string, silently dropping every later comment (jsntrs-ecq.10).
/// `highlight.rs` already treats the AST's own token positions as opaque
/// (gnata-0mb.2); this is the same rule.
///
/// Only the four token kinds whose text can *contain* a `/`, `*`, quote or
/// backtick are collected, and each is re-lexed from its node's position so
/// the extent is the lexer's rather than a guess. All four begin with a
/// byte the lexer reads the same way in either context, so lexing them in
/// prefix position is exact — which is what makes the `Regex` span, the one
/// that hinges on a leading `/`, correct.
fn token_spans(src: &str, arena: &AstArena) -> Vec<(usize, usize)> {
    let mut lexer = crate::lexer::Lexer::new(src);
    let mut spans = Vec::new();
    for node in arena.nodes() {
        let pos = match node {
            Expr::Name { pos, .. }
            | Expr::StringLit { pos, .. }
            | Expr::Variable { pos, .. }
            | Expr::Regex { pos, .. } => *pos,
            _ => continue,
        };
        lexer.seek(pos);
        // A node whose position is not a token start — there is none today —
        // contributes nothing rather than a span covering the wrong bytes.
        if matches!(lexer.next(false), Ok(tok) if tok.pos == pos) {
            spans.push((pos, lexer.offset()));
        }
    }
    spans.sort_unstable();
    spans
}

/// Extract all block comments from source with their positions, skipping
/// the token spans from [`token_spans`].
///
/// The scan walks token by token, so every position it tests is a token
/// *start* — which is what makes the quote test sound. A quote only opens a
/// string literal there; anywhere else it is ordinary identifier text, since
/// [`crate::lexer::is_stop_char`] (the lexer's only name boundary) does not
/// stop a run at `'`, `"` or `` ` ``. Testing the quote without that
/// distinction read `$'…'` as a quoted name and lifted the `/*` inside it
/// out as a comment, or — the other way round — swallowed a later real
/// comment into a "string" that was really the tail of a `$name`
/// (jsntrs-5xh). Stepping over the whole run instead cannot hide a comment:
/// `/` and `*` are both stop characters, so no run contains `/*`.
///
/// This is a byte-level scan, so char-boundary safety is a precondition of
/// every slice below. Each delimiter it looks for (`/`, `*`, `"`, `'`) is
/// ASCII and every byte of a multi-byte UTF-8 character is `>= 0x80`, so a
/// matched delimiter is always at a char boundary — with one exception: a
/// comment that is never closed ends wherever the source ran out, which may
/// be mid-character. Slicing that range panicked on input as small as
/// `/*€` (jsntrs-ecq.2), so an unterminated comment ends the scan instead.
/// Nothing is lost by dropping it: the lexer rejects an unclosed `/*` with
/// S0106, so `format` has already returned that error.
fn extract_comments(src: &str, spans: &[(usize, usize)]) -> Vec<Comment> {
    let bytes = src.as_bytes();
    let mut comments = Vec::new();
    let mut i = 0;
    let mut span = 0;
    while i + 1 < bytes.len() {
        // Step over any token covering this position. Spans are sorted by
        // start, and `i` only ever moves forward, so one cursor suffices.
        while span < spans.len() && spans[span].1 <= i {
            span += 1;
        }
        if span < spans.len() && spans[span].0 <= i {
            i = spans[span].1;
            continue;
        }
        if bytes[i] == b'/' && bytes[i + 1] == b'*' {
            let start = i;
            i += 2;
            let mut closed = false;
            while i + 1 < bytes.len() {
                if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    i += 2;
                    closed = true;
                    break;
                }
                i += 1;
            }
            if !closed {
                // `i` may be mid-character; never slice with it. The caller
                // surfaces the lexer's S0106 for this source.
                return comments;
            }
            comments.push(Comment {
                text: src[start..i].to_string(),
                pos: start,
            });
        } else if bytes[i] == b'"' || bytes[i] == b'\'' {
            // A quote *at a token start* opens a string literal. Belt and
            // braces: such a literal always has a `StringLit` span, but
            // skipping it here too costs nothing.
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
        } else if !crate::lexer::is_stop_char(bytes[i]) {
            // A name, `$variable` or number token: opaque up to the next
            // stop character, quotes and backticks included. Every stop
            // character is ASCII, so the run also ends on a char boundary.
            while i < bytes.len() && !crate::lexer::is_stop_char(bytes[i]) {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    comments
}

/// Format a JSONata expression string.
///
/// # Errors
/// Returns `JsonataError` if the expression fails to parse, or if it holds a
/// field name that has no JSONata spelling (see [`name_spelling`]) — emitting
/// text that does not parse back would be worse than reporting it.
pub fn format(expr: &str) -> Result<String, JsonataError> {
    // Parse first: the comment scan needs the AST's token positions, and a
    // source the lexer rejects has no comments worth recovering anyway.
    let (mut arena, root) = Parser::parse(expr)?;
    let root = process_ast(&mut arena, root)?;
    let comments = extract_comments(expr, &token_spans(expr, &arena));
    let mut f = Formatter::new(&arena, &comments);
    f.emit(root, 0);
    f.emit_trailing_comments();
    match f.error {
        Some(e) => Err(e),
        None => Ok(f.out.trim_end_matches(LEXER_SPACE).to_string()),
    }
}

struct Formatter<'a> {
    arena: &'a AstArena,
    comments: &'a [Comment],
    comment_idx: usize, // next comment to emit
    out: String,
    /// First unwritable construct met while walking, if any: `emit` cannot
    /// fail mid-walk, so the failure travels here and `format` returns it
    /// instead of the (unusable) text.
    error: Option<JsonataError>,
}

impl<'a> Formatter<'a> {
    fn new(arena: &'a AstArena, comments: &'a [Comment]) -> Self {
        Self {
            arena,
            comments,
            comment_idx: 0,
            out: String::new(),
            error: None,
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

    /// Write one node.
    ///
    /// # Decoration audit
    ///
    /// Five slots decorate a node *after* its own text — `stages`
    /// (`[pred]`, `#$i`), `keep_array` (`[]`), `focus` (`@$v`), `index`
    /// (`#$i`) and `group` (`{…}`) — and each is carried only by the node
    /// kinds the parser's `set_*` helpers can reach. Dropping one is silent
    /// semantic loss, so this table is the contract; `every_decorated_node_kind_round_trips`
    /// exercises one expression per ✓ (jsntrs-ecq.9).
    ///
    /// | node        | stages | keep_array | focus | index | group |
    /// |-------------|--------|------------|-------|-------|-------|
    /// | `Name`      | ✓      | ✓          | ✓     | ✓     | ✓     |
    /// | `Variable`  | –      | ✓          | ✓     | ✓     | ✓     |
    /// | `Binary`    | –      | ✓          | ✓     | ✓     | ✓     |
    /// | `Unary`     | –      | ✓          | –     | –     | ✓     |
    /// | `Block`     | –      | ✓          | ✓     | ✓     | –     |
    /// | `Function`  | –      | ✓          | –     | –     | ✓     |
    /// | `Sort`      | –      | ✓          | ✓     | ✓     | –     |
    /// | `Path`      | –      | (derived)  | –     | –     | ✓     |
    /// | `Grouped`   | –      | –          | –     | –     | ✓     |
    /// | `Wildcard`  | –      | ✓          | –     | –     | –     |
    /// | `Descendant`| –      | ✓          | –     | –     | –     |
    ///
    /// Every other kind (`StringLit`, `NumberLit`, `ValueLit`, `Parent`,
    /// `Regex`, `Placeholder`, `Condition`, `Bind`, `Partial`, `Lambda`,
    /// `Transform`) has no decoration slot at all: the parser's `set_group`
    /// wraps such a node in `Grouped` and `set_focus` / `set_index` /
    /// `set_keep_array` drop the decoration outright, so there is nothing
    /// for `emit` to write.
    ///
    /// That drop is upstream of the formatter and not its to undo: by the
    /// time `emit` runs, `a.-0#$i` *is* `a.-0`, so the printed text differs
    /// from the source (jsntrs-89v) while the AST it re-parses to does not.
    /// On a *number* it costs no meaning — a numeric step is S0213 either
    /// way — but two of the slot-less kinds are valid steps, and there the
    /// dropped flag did mean something. Both are parser bugs, not
    /// formatter ones; the documented rule they break is that "the `[]` can
    /// be placed either side of the predicates and on any step in the path
    /// expression" (<https://docs.jsonata.org/predicate>):
    ///
    /// - `Parent`: `a.b.%[]` answers `[{…}]` in jsonata 2.2.2 and `{…}`
    ///   here; `%@$v` and `%#$i` are dropped the same way.
    /// - `StringLit`: a string step *is* a field name — `collect_path_steps`
    ///   rebuilds it as a fresh `Name` — so `a."b"[]` should be `a.b[]`,
    ///   and the `[]` is thrown away before that rebuild: `[1]` in jsonata
    ///   2.2.2, `1` here.
    ///
    /// `Path::keep_singleton_array` is *derived*: `process_ast` raises it
    /// when any step carries `keep_array`, and the step keeps its own flag,
    /// so emitting the step's `[]` reproduces it.
    ///
    /// Slot order is fixed — `[]`, the step's own stages, `@$v`, `#$i`,
    /// `{…}` — and for the ones a source can reorder by hand it is the
    /// *canonical* order rather than the written one. The parser drops each
    /// decoration into its own slot and keeps no record of which came
    /// first, so `a#$i@$v` and `a@$v#$i` are one node and both print as
    /// `a@$v#$i` (jsntrs-k56). Nothing rides on the order — both spellings
    /// of each pair answer alike here and in jsonata 2.2.2 — and the
    /// canonical one is the shape the documentation writes, which says of
    /// `@`: "It can only be used directly following a map stage, not a
    /// filter or order-by stage."
    /// (<https://docs.jsonata.org/path-operators>).
    ///
    /// `[]` before `{…}` is not a choice at all. The reverse spelling is an
    /// S0209 ("a predicate cannot follow a grouping expression"), which
    /// also makes `keep_array` unreachable on a negated `Unary` — `-a[]`
    /// puts the `[]` on `a`, and the only spelling that would not,
    /// `-a{}[]`, is that same S0209.
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
                self.emit_name(value);
                if keep_array {
                    self.out.push_str("[]");
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
            Expr::Wildcard { keep_array, .. } => {
                self.out.push('*');
                if keep_array {
                    self.out.push_str("[]");
                }
            }
            Expr::Descendant { keep_array, .. } => {
                self.out.push_str("**");
                if keep_array {
                    self.out.push_str("[]");
                }
            }
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
                // The lexer accepts only `i` and `m` in the source and then
                // appends a synthetic `g` (spec.md §"Valid flags"); printing
                // that `g` back produces text the lexer rejects with S0302,
                // so emit only the source flags (jsntrs-ecq.6).
                self.out.push_str(source_regex_flags(flags));
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
                keep_array,
                ref focus,
                ref index,
                ..
            } => {
                self.emit_binary(
                    op,
                    lhs,
                    rhs,
                    group.as_ref(),
                    keep_array,
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
                keep_array,
                ..
            } => {
                match op {
                    UnaryOp::Negate => {
                        self.out.push('-');
                        self.emit(operand, depth);
                    }
                    UnaryOp::ArrayCons => self.emit_array(expressions, depth),
                    UnaryOp::ObjCons => self.emit_object(lhs, depth),
                }
                if keep_array {
                    self.out.push_str("[]");
                }
                self.emit_group(group.as_ref(), depth);
            }

            Expr::Block {
                ref expressions,
                ref focus,
                ref index,
                keep_array,
                ..
            } => {
                self.emit_block(expressions, depth);
                if keep_array {
                    self.out.push_str("[]");
                }
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
                keep_array,
                ..
            } => {
                self.emit(procedure, depth);
                self.emit_args(arguments, depth);
                if keep_array {
                    self.out.push_str("[]");
                }
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
                keep_array,
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
                if keep_array {
                    self.out.push_str("[]");
                }
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
        if self.error.is_none() {
            self.error = f.error;
        }
        f.out
    }

    /// Emit a field name in a spelling that lexes back to the same name.
    ///
    /// A name with no such spelling is recorded as an error rather than
    /// written out broken (jsntrs-ecq.8); the first one wins, and `format`
    /// returns it in place of the text.
    fn emit_name(&mut self, name: &str) {
        if let Some(text) = name_spelling(name) {
            self.out.push_str(&text);
        } else if self.error.is_none() {
            self.error = Some(JsonataError::new(
                "S0105",
                format!(
                    "field name has no JSONata spelling (backtick quoting has no escape): {name:?}"
                ),
            ));
        }
    }

    /// Emit the steps of a path, then its group-by.
    ///
    /// The group normally goes last: `process_ast` hoists a group written
    /// mid-chain onto the path itself (`a.b{"k": $}.c` → `a.b.c{"k": $}`),
    /// exactly as jsonata-js does, and that hoisted form is the canonical
    /// spelling — pinned by the `rust-path-group-hoist` conformance group.
    ///
    /// One step kind cannot be followed by a bare `.` though. A `-` parses
    /// its operand at binding power 70, below the dot's 75, so a re-read of
    /// `2.a.--b.c` folds the tail into the operand (`--(b.c)`) and yields a
    /// *shorter* path than the one being printed. Such a step exists only
    /// because something at binding power ≤ 70 ended the operand in the
    /// source, and inside a dot chain the only such token is the group's
    /// `{` — so writing the group directly after that step both restores the
    /// terminator and keeps the group on the same node (jsntrs-ecq.9).
    ///
    /// The joining `.` itself is padded where the steps around it would
    /// otherwise weld it into a number (see [`dot_needs_padding`]) — decided
    /// *after* the following step is written, because the deciding fact is
    /// its printed text; on the broken-line layout the newline and indent
    /// already separate them.
    fn emit_path(&mut self, steps: &[NodeId], group: Option<&GroupExpr>, depth: usize) {
        let anchor = self.group_anchor(steps, group);
        let emit_step = |f: &mut Self, i: usize, step: NodeId, depth: usize| {
            f.emit(step, depth);
            if anchor == Some(i) {
                f.emit_group(group, depth);
            }
        };
        if steps.len() > BREAK_THRESHOLD {
            emit_step(self, 0, steps[0], depth);
            for (i, &step) in steps.iter().enumerate().skip(1) {
                self.out.push('\n');
                self.indent(depth + 1);
                self.out.push('.');
                emit_step(self, i, step, depth + 1);
            }
        } else {
            for (i, &step) in steps.iter().enumerate() {
                // Write the joining `.` bare, then widen it once the step's
                // own text is there to be read: what welds to the `.` is the
                // *printed* text of the following step, which is not a
                // property of its node kind (jsntrs-y3t).
                let dot = i.checked_sub(1).map(|prev| {
                    let at = self.out.len();
                    self.out.push('.');
                    (at, prev)
                });
                emit_step(self, i, step, depth);
                // The group written after the previous step (if any) ends in
                // `}`, which no `.` can be absorbed into.
                if let Some((at, prev)) = dot
                    && anchor != Some(prev)
                    && dot_needs_padding(self.arena, steps[prev], &self.out[at + 1..])
                {
                    self.out.replace_range(at..=at, " . ");
                }
            }
        }
        if anchor.is_none() {
            self.emit_group(group, depth);
        }
    }

    /// The step index the path's group must follow, or `None` for the
    /// default "after the last step" placement (see [`Formatter::emit_path`]).
    ///
    /// Records an error when a step needs a terminator that no group can
    /// supply. That is unreachable today — every dot-absorbing step comes
    /// from a `{…}` in the chain, and a chain carries at most one (S0210) —
    /// but reporting beats printing a shorter path.
    fn group_anchor(&mut self, steps: &[NodeId], group: Option<&GroupExpr>) -> Option<usize> {
        let last = steps.len().checked_sub(1)?;
        let mut anchor = None;
        for (i, &step) in steps.iter().enumerate().take(last) {
            if !self.step_absorbs_dot(step) {
                continue;
            }
            if group.is_none() || anchor.is_some() {
                if self.error.is_none() {
                    self.error = Some(JsonataError::new(
                        "S0210",
                        "path step has no JSONata spelling: a negated step needs a \
                         following group expression to end it",
                    ));
                }
                return anchor;
            }
            anchor = Some(i);
        }
        anchor
    }

    /// True when re-reading this step followed by `.` would swallow the
    /// rest of the path into the step. Only a leading `-` does: it parses
    /// its operand at binding power 70, below the dot's 75, unless the
    /// step's own `{…}` group already ends it.
    ///
    /// Both spellings of a leading `-` count. A `-` in front of a number
    /// literal is *folded into the literal* — jsonata 2.2.2 `processAST`,
    /// case `unary`: `if (expr.value === '-' && result.expression.type ===
    /// 'number') { result = result.expression; result.value =
    /// -result.value; }`, which jsntrs mirrors in `Parser::nud` — so the
    /// step is an `Expr::NumberLit` whose text still begins with `-` and
    /// still absorbs the dot. Missing that half printed `0.-0{0:0}.0.a` as
    /// `0\n  .-0\n  .0\n  .a{0: 0}`, which re-reads as the two-step
    /// `0.-(0.0.a{0: 0})` (jsntrs-qhh). A `NumberLit` never carries a group
    /// of its own — `set_group` wraps such a node in `Grouped` — so, unlike
    /// `Unary`, there is no self-terminating form to exempt.
    fn step_absorbs_dot(&self, step: NodeId) -> bool {
        match self.arena.try_get(step) {
            Some(Expr::Unary {
                op: UnaryOp::Negate,
                group: None,
                ..
            }) => true,
            Some(Expr::NumberLit { raw, .. }) => raw.starts_with('-'),
            _ => false,
        }
    }

    #[expect(clippy::too_many_arguments)]
    fn emit_binary(
        &mut self,
        op: BinaryOp,
        lhs: NodeId,
        rhs: NodeId,
        group: Option<&GroupExpr>,
        keep_array: bool,
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
        if keep_array {
            self.out.push_str("[]");
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

    fn emit_object(&mut self, lhs: &[NodeId], depth: usize) {
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
            // `Signature::raw` already carries the outer angle brackets (it
            // is the source text of `<…>`); adding another pair produced
            // `<<n:n>>`, which the signature parser rejects on the way back
            // in with S0402 (jsntrs-ecq.7).
            self.out.push_str(&sig.raw);
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

/// True when the `.` joining the step `prev` to the text `next` has to be
/// padded with spaces to stay a path separator.
///
/// `scan_number` takes a `.` followed by a digit as the start of a fraction,
/// so the two-step path `0 . 0` printed as `0.0` lexes back as the single
/// number `0.0`. That was invisible until something depended on the fold:
/// `1 - --0 . 0` became `1 - --0.0` and then `1 - 0.0`, because unary minus
/// folds into a number literal but not into a path (jsntrs-ecq.11).
///
/// The two halves are asymmetric, and getting that wrong is what made the
/// test miss half its cases. The step *before* the `.` is exact from its
/// node kind: a step's text ends in a number token only when the step **is**
/// a `NumberLit` — every other kind ends in `]`, `}`, `)`, a flag letter or
/// a name character, and a name that would end in a bare number cannot be
/// spelled bare in the first place. The step *after* it is not: what welds
/// is the leading digit of the printed text, and a step of any kind can
/// print one — `Binary{Subscript}` prints `2[0]`, and testing the node kind
/// there wrote `1e2 . 2` + `.` + `2[0]`, whose `2.2` re-lexes as one number
/// and drops a step (jsntrs-y3t). So the caller passes the text it just
/// emitted rather than the node.
///
/// Padding a `.` that did not strictly need it (after `1.5`, say, where the
/// fraction is already spent) is harmless, so the test does not look at the
/// digits already in `prev`.
fn dot_needs_padding(arena: &AstArena, prev: NodeId, next: &str) -> bool {
    matches!(arena.try_get(prev), Some(Expr::NumberLit { .. }))
        && next.starts_with(|c: char| c.is_ascii_digit())
}

/// The flags of a regex literal as they were written in the source.
///
/// `Expr::Regex::flags` holds the *effective* flags: the lexer collects the
/// source flags (`i`, `m` only) and appends a `g` that no source ever
/// contains — and that the lexer itself rejects on the way back in (S0302,
/// as does jsonata-js with S0201). Dropping that one trailing `g` is exact
/// because it is the last thing pushed and no source flag is `g`.
fn source_regex_flags(flags: &str) -> &str {
    flags.strip_suffix('g').unwrap_or(flags)
}

/// How to write `name` so the lexer reads back exactly this name, or `None`
/// when no spelling does that.
///
/// JSONata gives a field name two spellings: bare (a run of characters the
/// lexer does not treat as a token boundary) and backtick-quoted. Quoting has
/// **no escape syntax** — the lexer takes everything up to the next backtick
/// and errors with S0105 if there is none, and jsonata-js rejects an escaped
/// backtick inside a quoted name too — so a name that itself contains a
/// backtick can only be written bare. That is also the only way such a name
/// reaches the formatter: the lexer treats a backtick inside an identifier
/// run as an ordinary character, so `a` backtick `b` is a single name (as in
/// jsonata-js). Quoting it produced an unterminated quote — S0105 on
/// re-parse — or, worse, a different expression (jsntrs-ecq.8).
///
/// The bare spelling is verified rather than assumed: [`lexes_back_as_name`]
/// runs the real lexer over it.
fn name_spelling(name: &str) -> Option<String> {
    if name.contains('`') {
        // No quoting is possible; bare is the only candidate.
        return lexes_back_as_name(name).then(|| name.to_string());
    }
    Some(if name.is_empty() || needs_backtick(name) {
        format!("`{name}`")
    } else {
        name.to_string()
    })
}

/// True when lexing `text` on its own yields exactly one `Name` token whose
/// value is `text` — i.e. writing it bare is faithful.
fn lexes_back_as_name(text: &str) -> bool {
    let mut lexer = crate::lexer::Lexer::new(text);
    // Prefix position: the same state a field name is emitted in.
    match lexer.next(false) {
        Ok(tok)
            if tok.typ == crate::lexer::TokenType::Name && tok.pos == 0 && tok.value == text =>
        {
            matches!(lexer.next(true), Ok(t) if t.typ == crate::lexer::TokenType::EOF)
        }
        _ => false,
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

    /// The lexer appends a synthetic `g` to every regex's flags and then
    /// rejects a source `g` with S0302, so printing the effective flags
    /// produced output that no longer parsed. This test used to assert the
    /// broken spelling (`/test/i` → `/test/ig`); it now pins the round-trip
    /// (jsntrs-ecq.6).
    #[test]
    fn regex_literals() {
        assert_eq!(fmt("/abc/"), "/abc/");
        assert_eq!(fmt("/test/i"), "/test/i");
        assert_eq!(fmt("/^foo.*bar$/m"), "/^foo.*bar$/m");
        assert_eq!(fmt("/x/im"), "/x/im");
        assert_eq!(fmt(r#"$match("a", /a/i)"#), r#"$match("a", /a/i)"#);
    }

    /// Formatted regex output must parse — and keep its flags. The `g` the
    /// lexer adds is invisible in the source, so the formatted text carries
    /// only `i`/`m` and re-parsing restores the same effective flags.
    #[test]
    fn regex_output_reparses_with_same_flags() {
        use crate::Expression;
        for src in [
            "/abc/", "/abc/i", "/abc/m", "/abc/im", "/abc/mi", r"/a\/b/i",
        ] {
            let once = fmt(src);
            let twice = format(&once)
                .unwrap_or_else(|e| panic!("formatted `{src}` -> `{once}` does not parse: {e}"));
            assert_eq!(once, twice, "not idempotent for: {src}");
            assert!(
                !once.trim_start_matches('/').ends_with('g'),
                "implicit g leaked into output for `{src}`: {once}"
            );
        }
        // The effective flags survive: a case-insensitive match still matches.
        let out = fmt(r#"$match("ABC", /abc/i).match"#);
        let eval = |src: &str| {
            Expression::compile(src)
                .unwrap_or_else(|e| panic!("compile `{src}`: {e}"))
                .evaluate("{}")
                .unwrap_or_else(|e| panic!("eval `{src}`: {e}"))
                .to_string()
        };
        assert_eq!(eval(&out), "\"ABC\"", "flags lost in: {out}");
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

    /// `Signature::raw` includes the outer angle brackets, so wrapping it in
    /// another pair emitted `<<n:n>>` — S0402 on re-parse (jsntrs-ecq.7).
    /// Plain and nested (higher-order) signatures both round-trip now.
    #[test]
    fn lambda_signature_is_not_double_wrapped() {
        for (src, sig) in [
            ("function($x)<n:n>{$x}", "<n:n>"),
            ("function($f)<f<n:n>:n>{$f(1)}", "<f<n:n>:n>"),
            ("function($a, $b)<nn:n>{$a}", "<nn:n>"),
            ("function($x)<a<n>:n>{$x}", "<a<n>:n>"),
            ("function($x)<x-:x>{$x}", "<x-:x>"),
            ("function($x)<(sao)?:s>{$x}", "<(sao)?:s>"),
        ] {
            let once = fmt(src);
            assert!(
                once.contains(&format!("){sig} {{")),
                "signature not emitted verbatim for `{src}`: {once}"
            );
            assert!(!once.contains("<<"), "signature double-wrapped: {once}");
            let twice = format(&once)
                .unwrap_or_else(|e| panic!("formatted `{src}` -> `{once}` does not parse: {e}"));
            assert_eq!(once, twice, "not idempotent for: {src}");
        }
    }

    /// A formatted signature must still be *enforced*: the round-tripped
    /// lambda rejects the wrong argument type with T0410, as the source does.
    #[test]
    fn formatted_signature_still_type_checks() {
        use crate::Expression;
        let src = r#"($f := function($x)<n:n>{$x}; $f("s"))"#;
        let once = fmt(src);
        for text in [src, &once] {
            let err = Expression::compile(text)
                .unwrap_or_else(|e| panic!("compile `{text}`: {e}"))
                .evaluate("{}")
                .expect_err("signature must reject a string argument");
            assert_eq!(err.code, "T0410", "wrong code for `{text}`: {err}");
        }
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

    /// The lexer reads a backtick inside an identifier run as an ordinary
    /// character, so `` a`b `` is one name (jsonata-js agrees). Quoting such a
    /// name left the quote unterminated — S0105 on re-parse — and for the
    /// second repro below the output even re-parsed as a *different*
    /// expression. Both must now round-trip (jsntrs-ecq.8).
    #[test]
    fn name_containing_backtick_round_trips() {
        for src in [
            "a`b",
            "a`b.c",
            "a``b",
            "a`",
            "\u{0}`",
            "x + a`b",
            "a`b[0]",
            "a`b@$v.$v",
            "`plain name`.a`b",
            "{\"k\": a`b}",
            // The fuzzer's idempotence repro. It was found as
            // `($t;2\u{0}c222222222…)`, with no separator between `2` and the
            // name; jsntrs-0jv made that a syntax error, so the `;` the
            // documented block grammar requires is spelled out here. The
            // backtick-quoting path under test is unchanged.
            "($t;2;\u{0}c222222222`% $222`% $y)",
        ] {
            let once = fmt(src);
            let twice = format(&once)
                .unwrap_or_else(|e| panic!("formatted `{src}` -> `{once:?}` does not parse: {e}"));
            assert_eq!(
                once, twice,
                "not idempotent for: {src}\nfirst:  {once:?}\nsecond: {twice:?}"
            );
        }
    }

    /// Round-tripping must preserve the *name*, not just parse: the
    /// formatted text still selects the same field.
    #[test]
    fn backtick_name_keeps_its_meaning() {
        use crate::Expression;
        let data = r#"{"a`b": 1, "c": {"d`": 2}}"#;
        let eval = |src: &str| {
            Expression::compile(src)
                .unwrap_or_else(|e| panic!("compile `{src}`: {e}"))
                .evaluate(data)
                .unwrap_or_else(|e| panic!("eval `{src}`: {e}"))
                .to_string()
        };
        for (src, expected) in [("a`b", "1"), ("c.d`", "2"), ("a`b + c.d`", "3")] {
            assert_eq!(eval(src), expected, "unexpected source result: {src}");
            let once = fmt(src);
            assert_eq!(
                eval(&once),
                expected,
                "formatted output changed semantics: {src} -> {once:?}"
            );
        }
    }

    /// Names the parser cannot produce today — a leading backtick, or a
    /// backtick next to a token boundary — have no spelling at all, and are
    /// reported instead of being written out broken. The bare spelling is
    /// never guessed: it is checked against the real lexer.
    #[test]
    fn unwritable_names_have_no_spelling() {
        assert_eq!(name_spelling("`x"), None, "leading backtick");
        assert_eq!(name_spelling("a b`"), None, "space is a token boundary");
        assert_eq!(name_spelling("a.b`"), None, "dot is a token boundary");
        assert_eq!(name_spelling("a`b").as_deref(), Some("a`b"));
        assert_eq!(name_spelling("plain").as_deref(), Some("plain"));
        assert_eq!(name_spelling("a b").as_deref(), Some("`a b`"));
        assert_eq!(name_spelling("").as_deref(), Some("``"));
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

    /// `extract_comments` scans raw bytes; an unterminated `/*` used to leave
    /// the cursor mid-character and panic when slicing the comment text out
    /// (jsntrs-ecq.2). The lexer's S0106 must surface instead.
    #[test]
    fn unterminated_comment_ending_in_multibyte_char_errors() {
        let err = format("/*\u{20AC}").expect_err("unterminated comment must error");
        assert_eq!(err.code, "S0106", "wrong code: {err}");
    }

    #[test]
    fn unterminated_comment_ending_in_ascii_errors() {
        for src in ["/*", "/* oops", "$x + /* oops", "$x /* oops\n  more"] {
            let err = format(src).expect_err("unterminated comment must error");
            assert_eq!(err.code, "S0106", "wrong code for `{src}`: {err}");
        }
    }

    /// Multi-byte characters *inside* a closed comment are fine — the slice
    /// boundaries are the ASCII delimiters — but pin it so a future rewrite
    /// of the scanner keeps them intact.
    #[test]
    fn multibyte_chars_inside_closed_comment_preserved() {
        let result = fmt("/* héllo \u{20AC} \u{1F600} */ $x");
        assert!(
            result.contains("/* héllo \u{20AC} \u{1F600} */"),
            "comment text mangled: {result}"
        );
        assert!(result.contains("$x"), "expression lost: {result}");
        // Multi-byte text before a comment must not shift its placement.
        let after = fmt("\"\u{20AC}\u{20AC}\" & /* tail */ $x");
        assert!(after.contains("/* tail */"), "comment lost: {after}");
    }

    /// A `/*` inside a string literal is not a comment, so it can never make
    /// the scanner think the source ends inside one.
    #[test]
    fn unterminated_comment_marker_inside_string_is_not_a_comment() {
        assert_eq!(fmt(r#""/* unclosed""#), r#""/* unclosed""#);
        // …including one whose closing quote follows a multi-byte character.
        let multibyte = "\"\u{20AC} /*\"";
        assert_eq!(fmt(multibyte), multibyte);
        // …and a real comment after such a string is still picked up.
        let result = fmt(r#""/*" & $x /* real */"#);
        assert!(result.contains("/* real */"), "comment lost: {result}");
    }

    /// A regex literal may contain an escaped `/*` (`\/*`), which the byte
    /// scanner reads as a comment that never closes. It used to emit the
    /// dangling `/*` as a comment, producing output that no longer parsed;
    /// now the partial comment is dropped.
    #[test]
    fn slash_star_inside_regex_emits_no_dangling_comment() {
        // The point here is that nothing extra — no `/*` on a line of its
        // own — is appended. (The trailing `g` these assertions used to
        // carry was the implicit flag, dropped by jsntrs-ecq.6.)
        assert_eq!(fmt(r"/a\/*/"), r"/a\/*/");
        assert_eq!(fmt(r#"$match("a", /a\/*/)"#), r#"$match("a", /a\/*/)"#);
    }

    /// A `/*` inside a backtick-quoted name is not a comment. The scan only
    /// skipped string literals, so it lifted one out and re-emitted it on a
    /// line of its own — and appended one more copy on every pass, since the
    /// stray line is itself a `/*` the next scan finds (jsntrs-ecq.10).
    #[test]
    fn comment_marker_inside_a_backtick_name_is_not_a_comment() {
        for src in [
            "`a/*b*/c`",
            "`/*`",
            "`a/*b`",
            "x.`a/*b*/c`",
            // The fuzzer's spelling of the same thing: a regex literal
            // holding an escaped `/*`, next to a real comment.
            "λ/*/6*/*/\\/*\u{8}*/",
        ] {
            let once = fmt(src);
            let twice = format(&once)
                .unwrap_or_else(|e| panic!("formatted `{src}` -> {once:?} does not parse: {e}"));
            assert_eq!(
                once, twice,
                "not idempotent for: {src}\nfirst:  {once:?}\nsecond: {twice:?}"
            );
        }
        assert_eq!(fmt("`a/*b*/c`"), "`a/*b*/c`", "stray comment line emitted");
    }

    /// The other half: a lone quote inside a backtick name (or a variable
    /// name — the lexer does not stop an identifier run at a quote) made the
    /// scan treat everything after it as an unclosed string literal, so a
    /// real comment further on was dropped (jsntrs-ecq.10).
    #[test]
    fn quote_inside_a_name_does_not_swallow_later_comments() {
        for src in [
            "`a'b` /*c*/",
            "`a\"b` /*c*/",
            "$a'b /*c*/",
            "`it's` & /*c*/ x",
        ] {
            let once = fmt(src);
            assert!(
                once.contains("/*c*/"),
                "comment dropped from `{src}`: {once:?}"
            );
            assert_eq!(
                format(&once).expect("re-parse"),
                once,
                "not idempotent: {src}"
            );
        }
    }

    /// A quote is only a string delimiter at a *token start*. Everywhere
    /// else it is ordinary name text, because [`crate::lexer::is_stop_char`]
    /// — the lexer's only identifier boundary — does not stop a run at `'`,
    /// `"` or `` ` ``. Testing the byte without that distinction made the
    /// scan disagree with the lexer in both directions: it lifted the `/*`
    /// *inside* `$'//*'` out as a comment, and it read the `'` that ends
    /// `$0'` as opening a string that then swallowed a real comment. Either
    /// way the second pass lost the comment (jsntrs-5xh).
    ///
    /// jsonata 2.2.2 draws the boundary in the same place (`tokenizer`: the
    /// name scan stops only on `' \t\n\r\v'` or a single-character key of
    /// `operators`) and gives each source and its formatted output below the
    /// same `ast()` — `$'` is `{value: "'", type: "variable"}` in both.
    #[test]
    fn a_quote_next_to_a_dollar_token_is_name_text() {
        for (src, want) in [
            // The scan used to rip a comment out of the variable name …
            ("a@$'//*'/**/a", "a@$' / \n/*'/**/\na"),
            ("a@$\"//*\"/**/a", "a@$\" / \n/*\"/**/\na"),
            // … and, the other way round, to read the quote that *ends* a
            // name as opening a string over the real comment.
            ("a@$'?a'*/**/0-0", "a@$'\n  ?   /**/\n  `a'` * 0 - 0"),
            ("a@$0'@$0'()?/**/0", "a@$0'()\n  ?   /**/\n  0"),
        ] {
            let once = fmt(src);
            assert_eq!(once, want, "unexpected formatting of {src:?}");
            assert!(
                once.contains("/*"),
                "comment dropped from {src:?}: {once:?}"
            );
            assert_eq!(
                format(&once).expect("re-parse"),
                once,
                "not idempotent: {src:?}"
            );
        }
        // The `/*` after `$'` opens a comment for the lexer too, so an
        // unclosed one is its S0106 and not a token to step over; and a
        // quote that really is at a token start still opens a string.
        assert_eq!(format("$'//****'").unwrap_err().code, "S0106");
        assert_eq!(format("$'/**/'").unwrap_err().code, "S0101");
    }

    /// And the relocated comment leaves the meaning alone: the canonical
    /// spelling evaluates to what the source did (0, as in jsonata 2.2.2).
    #[test]
    fn quoted_variable_round_trip_keeps_its_meaning() {
        use crate::Expression;
        let data = r#"{"a": 6, "a'": 3}"#;
        let eval = |src: &str| {
            Expression::compile(src)
                .unwrap_or_else(|e| panic!("compile `{src}`: {e}"))
                .evaluate(data)
                .unwrap_or_else(|e| panic!("eval `{src}`: {e}"))
                .to_string()
        };
        let src = "a@$'?a'*/**/0-0";
        assert_eq!(eval(src), "0", "unexpected source result");
        assert_eq!(eval(&fmt(src)), "0", "formatted output changed semantics");
    }

    /// Regex literals are opaque to the scan for the same reason, and the
    /// `/` that opens one must not be mistaken for a division (or the span
    /// would end in the wrong place and expose the comment marker again).
    #[test]
    fn comment_marker_inside_a_regex_is_not_a_comment() {
        assert_eq!(fmt(r"/a\/*/"), r"/a\/*/");
        assert_eq!(fmt(r#"$match("a", /a\/*/)"#), r#"$match("a", /a\/*/)"#);
        // A division `/` is not a regex: the comment beside it still lands.
        assert_eq!(fmt("a /* c */ / b"), "/* c */\na / b");
        assert_eq!(fmt("a / b /* c */"), "a / b\n/* c */");
        assert_eq!(fmt("/* x */ /re/"), "/* x */\n/re/");
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

    // ── Decoration slots (group, focus, index, keep_array, stages) ──

    /// The audit on [`Formatter::emit`] made executable: one expression per
    /// decoration slot the parser can fill, each of which must survive a
    /// round trip. `emit` used to ignore the `Unary` group and every
    /// `keep_array` outside `Name`/`Variable`/`Block`, so `-a{"k": 1}` came
    /// back as `-a` and `a[0][]` as `a[0]` (jsntrs-ecq.9).
    #[test]
    fn every_decorated_node_kind_round_trips() {
        // (source, the decoration text that must survive)
        let cases = [
            // Name: stages, keep_array, focus, index, group.
            ("a[b > 3]", "[b > 3]"),
            ("a[]", "[]"),
            ("a@$v", "@$v"),
            ("a#$i", "#$i"),
            (r#"a{"k": $}"#, r#"{"k": $}"#),
            // Variable: keep_array, focus, index, group.
            ("$x[]", "[]"),
            ("$x@$v", "@$v"),
            ("$x#$i", "#$i"),
            (r#"$x{"k": $}"#, r#"{"k": $}"#),
            // Binary: keep_array, index, group. (`@` after a predicate is
            // S0215, so `focus` on a Binary has no spelling.)
            ("a[0][]", "[]"),
            ("a[0]#$i", "#$i"),
            (r#"a[0]{"k": $}"#, r#"{"k": $}"#),
            // Unary: keep_array and group, for all three operators.
            ("[1, 2][]", "[]"),
            (r#"-a{"k": $}"#, r#"{"k": $}"#),
            (r#"[1, 2]{"k": $}"#, r#"{"k": $}"#),
            (r#"{"a": 1}{"k": $}"#, r#"{"k": $}"#),
            // Block: keep_array, focus, index.
            ("(a)[]", "[]"),
            ("(a)@$v", "@$v"),
            ("(a)#$i", "#$i"),
            // Function: keep_array and group.
            ("$f()[]", "[]"),
            (r#"$f(){"k": $}"#, r#"{"k": $}"#),
            // Sort: keep_array and index. (`@` after `^(…)` is S0216.)
            ("a^(<b)[]", "[]"),
            ("a^(<b)#$i", "#$i"),
            // Path: its own group; `keep_singleton_array` is derived from
            // the steps, so the step's own `[]` carries it.
            (r#"a.b{"k": $}"#, r#"{"k": $}"#),
            ("a.b[]", "[]"),
            // Grouped: the fallback wrapper for a node with no group slot.
            (r#"(1){"k": $}"#, r#"{"k": $}"#),
            (r#"-1{"k": $}"#, r#"{"k": $}"#),
        ];
        for (src, decoration) in cases {
            let once = fmt(src);
            assert!(
                once.contains(decoration),
                "decoration {decoration:?} dropped from `{src}`: {once:?}"
            );
            let twice = format(&once)
                .unwrap_or_else(|e| panic!("formatted `{src}` -> {once:?} does not parse: {e}"));
            assert_eq!(once, twice, "not idempotent for: {src}");
        }
    }

    /// The dropped decorations were real meaning, not decoration: each of
    /// these evaluates to something the stripped spelling does not.
    /// Reference-verified against jsonata-js 2.x (2026-08-14).
    #[test]
    fn decorations_keep_their_meaning_through_a_round_trip() {
        use crate::Expression;
        let data = r#"{"a": [{"b": 3}, {"b": 4}], "d": [{"p": 2}, {"p": 1}]}"#;
        let cases = [
            (r#"-a[0].b{"k": $}"#, r#"{"k":-3}"#),
            (r#"[1, 2]{"k": $}"#, r#"{"k":[1,2]}"#),
            (r#"[1, 2][]{"k": $}"#, r#"{"k":[1,2]}"#),
            ("a[0][]", r#"[{"b":3}]"#),
            ("a[0][].b", "[3]"),
            ("a.b[]", "[3,4]"),
            ("d^(<p)[]", r#"[{"p":1},{"p":2}]"#),
        ];
        let eval = |src: &str| {
            Expression::compile(src)
                .unwrap_or_else(|e| panic!("compile `{src}`: {e}"))
                .evaluate(data)
                .unwrap_or_else(|e| panic!("eval `{src}`: {e}"))
                .to_string()
        };
        for (src, expected) in cases {
            assert_eq!(eval(src), expected, "unexpected source result: {src}");
            let once = fmt(src);
            assert_eq!(
                eval(&once),
                expected,
                "formatted output changed semantics: {src} -> {once:?}"
            );
        }
    }

    /// The arena's nodes with their byte offsets blanked: two spellings of
    /// one tree differ only in where their tokens sat.
    fn ast_shape(src: &str) -> String {
        let (mut arena, root) = Parser::parse(src).expect("parse");
        process_ast(&mut arena, root).expect("process");
        let dump = format!("{:?}", arena.nodes());
        let mut out = String::with_capacity(dump.len());
        let mut rest = dump.as_str();
        while let Some(i) = rest.find("pos: ") {
            out.push_str(&rest[..i]);
            rest = &rest[i + 5..];
            rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
        }
        out.push_str(rest);
        out
    }

    /// A decoration the *parser* drops never reaches the formatter.
    ///
    /// `set_focus` / `set_index` / `set_keep_array` fill a slot only on the
    /// node kinds that have one (the audit table on [`Formatter::emit`]);
    /// on a literal there is no slot, so the decoration is dropped where it
    /// is read and `a.-0#$i` is already the same tree as `a.-0` by the time
    /// `format` sees it. The formatter prints the tree it is given, so the
    /// *source text* is not reproduced (jsntrs-89v) — but what a formatter
    /// owes, an output that re-parses to the same AST and a first pass that
    /// is already canonical, is intact, and so is the answer: every
    /// spelling below puts a *number* where a path step belongs, which is
    /// S0213 either way. Restoring the text would mean giving numeric
    /// literals decoration slots for a step that can never be evaluated.
    ///
    /// The same parser drop on a `%` or a string step is a different
    /// matter and a real bug — see the audit on [`Formatter::emit`].
    #[test]
    fn a_decoration_the_parser_drops_is_not_the_formatters_to_restore() {
        use crate::Expression;
        let eval = |src: &str| {
            Expression::compile(src)
                .unwrap_or_else(|e| panic!("compile `{src}`: {e}"))
                .evaluate(r#"{"a": 1}"#)
                .map_or_else(|e| e.code.to_string(), |v| v.to_string())
        };
        for (decorated, plain) in [
            ("a.-0#$i", "a.-0"),
            ("a.0#$i", "a.0"),
            ("a.-0@$v", "a.-0"),
            ("0[].0", "0 . 0"),
            ("a.0[]", "a.0"),
        ] {
            assert_eq!(
                ast_shape(decorated),
                ast_shape(plain),
                "the decoration reached the AST: {decorated}"
            );
            assert_eq!(
                fmt(decorated),
                fmt(plain),
                "one tree, two spellings: {decorated}"
            );
            let once = fmt(decorated);
            assert_eq!(
                ast_shape(&once),
                ast_shape(decorated),
                "AST changed: {once}"
            );
            assert_eq!(fmt(&once), once, "not idempotent: {decorated}");
            assert_eq!(eval(decorated), "S0213", "unexpected result: {decorated}");
            assert_eq!(eval(&once), "S0213", "unexpected result: {once}");
        }
    }

    /// The decoration slots print in a fixed order whatever order the
    /// source wrote them in, because the order is not in the AST either:
    /// the parser drops each decoration into its own slot and keeps no
    /// record of which came first, so `a#$i@$v` and `a@$v#$i` are one node
    /// and `format` prints the one spelling both of them mean. Choosing a
    /// canonical spelling is what a formatter is for — it already hoists a
    /// path group and re-spaces every operator — and there is no meaning
    /// here to canonicalize away: both spellings of each pair answer the
    /// same, in jsntrs and in jsonata 2.2.2 (checked 2026-08-15). The
    /// documentation says nothing about the order of two bindings on one
    /// step; the order chosen is the one it writes, saying of `@` that "It
    /// can only be used directly following a map stage, not a filter or
    /// order-by stage." (<https://docs.jsonata.org/path-operators>).
    /// (jsntrs-k56.)
    #[test]
    fn decoration_slots_print_in_a_canonical_order() {
        use crate::Expression;
        for (written, canonical) in [
            ("a#$i@$v", "a@$v#$i"),
            ("a@$v[]", "a[]@$v"),
            ("a#$i[]", "a[]#$i"),
            ("a@$v#$i[]", "a[]@$v#$i"),
        ] {
            assert_eq!(
                ast_shape(written),
                ast_shape(canonical),
                "slot order reached the AST: {written}"
            );
            assert_eq!(fmt(written), canonical, "not the canonical order");
            assert_eq!(fmt(canonical), canonical, "not idempotent: {canonical}");
        }
        // And the order the source used changes nothing to evaluate.
        let data = r#"{"a": [1, 2]}"#;
        let eval = |src: &str| {
            Expression::compile(src)
                .unwrap_or_else(|e| panic!("compile `{src}`: {e}"))
                .evaluate(data)
                .unwrap_or_else(|e| panic!("eval `{src}`: {e}"))
                .to_string()
        };
        assert_eq!(eval("a@$v#$i.[$v, $i]"), "[1,0,2,1]");
        assert_eq!(eval("a#$i@$v.[$v, $i]"), "[1,0,2,1]");
        assert_eq!(eval("a[]@$v.$v"), "[1,2]");
        assert_eq!(eval("a@$v[].$v"), "[1,2]");
    }

    /// A `-` step parses its operand below the dot's binding power, so a
    /// path group written after the last step would let the step swallow
    /// the tail: `2.a.--b{}.c@$v` printed `2.a.--b.c@$v{}`, which re-reads
    /// as `--(b.c@$v)` — one step short, and so not idempotent either. The
    /// group goes back where it ends the operand (jsntrs-ecq.9).
    #[test]
    fn path_group_ends_a_negated_step() {
        assert_eq!(fmt("2.a.--b{}.c@$v"), "2\n  .a\n  .--b{}\n  .c@$v");
        assert_eq!(fmt("-a{}.--b{}.c"), "-a{}.--b{}.c");
        // A negated step with its own group already ends itself, and the
        // path group keeps the canonical trailing position.
        assert_eq!(fmt("-a{}.b"), "-a{}.b");
        assert_eq!(fmt(r#"x.--y{"k": $}.k"#), r#"x.--y{"k": $}.k"#);
        // A negated *last* step needs no terminator.
        assert_eq!(fmt("a.-b"), "a.-b");
        for src in ["2.a.--b{}.c@$v", "-a{}.--b{}.c", r#"x.--y{"k": $}.k"#] {
            let once = fmt(src);
            assert_eq!(
                format(&once).expect("re-parse"),
                once,
                "not idempotent: {src}"
            );
        }
    }

    /// The other spelling of a leading `-`: in front of a number literal it
    /// is folded *into* the literal (jsonata 2.2.2 `processAST`, case
    /// `unary`), so the step is a `NumberLit` whose text still starts with
    /// `-` and still swallows the following `.`-chain. Treating only the
    /// `Unary` node as dot-absorbing put the group at the end, and
    /// `0.-0{0:0}.0.a` printed `0\n  .-0\n  .0\n  .a{0: 0}` — the two-step
    /// `0.-(0.0.a{0: 0})`, which then re-formatted single-line, so the
    /// layout flipped between passes as well (jsntrs-qhh).
    #[test]
    fn path_group_ends_a_negative_number_step() {
        assert_eq!(fmt("0.-0{0:0}.0.a"), "0\n  .-0{0: 0}\n  .0\n  .a");
        assert_eq!(fmt("0.-0{0:0}.a"), "0.-0{0: 0}.a");
        assert_eq!(fmt("a.-0{0:0}.b"), "a.-0{0: 0}.b");
        assert_eq!(fmt("0.-1e2{0:0}.a.b"), "0\n  .-1e2{0: 0}\n  .a\n  .b");
        assert_eq!(fmt("0.-0{}.a"), "0.-0{}.a");
        // Two folds cancel, leaving a positive literal that ends itself —
        // the group goes back to the canonical trailing position, and the
        // joining `.` still needs its padding (jsntrs-ecq.11).
        assert_eq!(fmt("0.--0{0:0}.a"), "0 . 0.a{0: 0}");
        // A negative literal as the *last* step needs no terminator.
        assert_eq!(fmt("0 . -1"), "0.-1");
        assert_eq!(fmt("0.-0{0: 0}.-1"), "0.-0{0: 0}.-1");
        for src in [
            "0.-0{0:0}.0.a",
            "0.-0{0:0}.a",
            "a.-0{0:0}.b",
            "0.-1e2{0:0}.a.b",
            "0.--0{0:0}.a",
            "-1{0:0}.a.b.c.d",
            r#"x.-1{"k": $}.y"#,
        ] {
            let once = fmt(src);
            assert_eq!(
                format(&once).expect("re-parse"),
                once,
                "not idempotent: {src}"
            );
        }
    }

    /// And the printed path is the *same* path: the step count a re-parse
    /// yields must match the one being printed, in both layouts. The
    /// pre-fix output collapsed four steps into two.
    #[test]
    fn negated_number_step_keeps_the_step_count() {
        let steps = |src: &str| {
            let (mut arena, root) = Parser::parse(src).expect("parse");
            let root = process_ast(&mut arena, root).expect("process");
            match arena.get(root) {
                Expr::Path { steps, .. } => steps.len(),
                other => panic!("not a path: {other:?}"),
            }
        };
        for src in ["0.-0{0:0}.0.a", "0.-0{0:0}.a", "0.-1e2{0:0}.a.b"] {
            assert_eq!(
                steps(&fmt(src)),
                steps(src),
                "step count changed: {src} -> {:?}",
                fmt(src)
            );
        }
        // The shape the old formatter produced, for contrast.
        assert_eq!(steps("0.-0{0:0}.0.a"), 4);
        assert_eq!(steps("0\n  .-0\n  .0\n  .a{0: 0}"), 2);
    }

    /// A `.` between two numeric steps was written bare, so `0 . 0` came
    /// back as the single number `0.0`. Invisible until a fold depended on
    /// it: `1 - --0 . 0` printed `1 - --0.0`, which folds to `1 - 0.0` = 1,
    /// where the path is an S0213 (jsntrs-ecq.11).
    #[test]
    fn numeric_path_steps_keep_their_joining_dot() {
        assert_eq!(fmt("0 . 0"), "0 . 0");
        assert_eq!(fmt("1 - --0 . 0"), "1 - --0 . 0");
        assert_eq!(fmt("0 . 0 . 0"), "0 . 0 . 0");
        assert_eq!(fmt("-1 . 0"), "-1 . 0");
        assert_eq!(fmt(r#"0 . 0{"k": $}"#), r#"0 . 0{"k": $}"#);
        // The broken-line layout separates them already.
        assert_eq!(fmt("0 . 0 . 0 . 0"), "0\n  .0\n  .0\n  .0");
        // A step whose text cannot start a fraction still joins bare.
        assert_eq!(fmt("0 . a"), "0.a");
        assert_eq!(fmt("a . 0"), "a.0");
        assert_eq!(fmt("0 . -1"), "0.-1");
        for src in [
            "0 . 0",
            "1 - --0 . 0",
            "0 . 0 . 0 . 0",
            "1.5 . 0",
            "1e5 . 0",
        ] {
            let once = fmt(src);
            assert_eq!(
                format(&once).expect("re-parse"),
                once,
                "not idempotent: {src}"
            );
        }
    }

    /// And the round trip keeps the *meaning*: a numeric path step is an
    /// S0213 at evaluation, where the welded number is a plain value.
    #[test]
    fn welded_numeric_steps_would_change_the_result() {
        use crate::Expression;
        let eval = |src: &str| {
            Expression::compile(src)
                .unwrap_or_else(|e| panic!("compile `{src}`: {e}"))
                .evaluate("{}")
                .map_or_else(|e| e.code.to_string(), |v| v.to_string())
        };
        assert_eq!(eval("1 - --0 . 0"), "S0213");
        assert_eq!(eval(&fmt("1 - --0 . 0")), "S0213");
        // The spelling the old formatter produced, for contrast.
        assert_eq!(eval("1 - --0.0"), "1");
    }

    /// The padding is decided by the *printed text* of the following step,
    /// not by its node kind. Any step can print a leading digit — a
    /// `Binary{Subscript}` prints `2[0]` — and the bare `.` in front of one
    /// welds into the number before it exactly as a `NumberLit` would. The
    /// node-kind test missed every such step: `1e2.2@$w.2[0]` printed
    /// `1e2 . 2.2[0]`, whose `2.2` re-lexes as one number, so the three-step
    /// path came back with two steps and the *second* pass printed
    /// `1e2.2.2[0]` (jsntrs-y3t).
    #[test]
    fn a_step_printing_a_leading_digit_needs_the_padding_too() {
        assert_eq!(fmt("1e2.2@$w.2[0]"), "1e2 . 2 . 2[0]");
        assert_eq!(fmt("0 . 2[0]"), "0 . 2[0]");
        assert_eq!(fmt("0 . 0[1][2]"), "0 . 0[1][2]");
        // …and the rule cuts the other way too, which is the half a
        // node-kind test could never get right: this step's value starts
        // with a digit but its *printed text* starts with `[`, so the `.`
        // needs no padding and welds on. (The line used to read
        // `0 . 2..3`, which stopped parsing when `..` left the general
        // expression grammar for the array constructor — jsntrs-uql.)
        assert_eq!(fmt("0 . [2..3]"), "0.[2..3]");
        assert_eq!(fmt("0.[2..3]"), "0.[2..3]");
        assert_eq!(fmt("0 . 2^(<a)"), "0 . 2^(<a)");
        assert_eq!(fmt(r#"0 . 2{"k": $}"#), r#"0 . 2{"k": $}"#);
        // The group hoisted to the end of the path leaves the `.` in front
        // of `2[0]` to be padded like any other.
        assert_eq!(fmt("0 . 0{} . 2[0]"), "0 . 0 . 2[0]{}");
        // …and a group written *on* the previous step already ends it with
        // `}`, which no `.` can be absorbed into.
        assert_eq!(fmt("0.-0{0:0}.2[0]"), "0.-0{0: 0}.2[0]");
        for src in [
            "1e2.2@$w.2[0]",
            "0 . 2[0]",
            "0 . 0{} . 2[0]",
            "0.-0{0:0}.2[0]",
            "1 - --0 . 2[0]",
        ] {
            let once = fmt(src);
            assert_eq!(
                format(&once).expect("re-parse"),
                once,
                "not idempotent: {src}"
            );
        }
    }

    /// And the same meaning test as for two numeric steps: the welded
    /// spelling is a number with a predicate, not a path, so it answers
    /// where the path is an S0213. Both of these were *idempotent* while
    /// wrong — the round trip changed the expression on the first pass and
    /// then held still — so only the result pins them.
    #[test]
    fn a_welded_subscript_step_would_change_the_result() {
        use crate::Expression;
        let eval = |src: &str| {
            Expression::compile(src)
                .unwrap_or_else(|e| panic!("compile `{src}`: {e}"))
                .evaluate("{}")
                .map_or_else(|e| e.code.to_string(), |v| v.to_string())
        };
        assert_eq!(eval("0 . 2[0]"), "S0213");
        assert_eq!(eval(&fmt("0 . 2[0]")), "S0213");
        assert_eq!(eval("1 - --0 . 2[0]"), "S0213");
        assert_eq!(eval(&fmt("1 - --0 . 2[0]")), "S0213");
        // The spellings the old formatter produced, for contrast.
        assert_eq!(eval("0.2[0]"), "0.2");
        assert_eq!(eval("1 - --0.2[0]"), "0.8");
    }

    /// The printed path is the *same* path: a step count that survives the
    /// round trip in both layouts.
    #[test]
    fn a_subscript_step_keeps_the_step_count() {
        let steps = |src: &str| {
            let (mut arena, root) = Parser::parse(src).expect("parse");
            let root = process_ast(&mut arena, root).expect("process");
            match arena.get(root) {
                Expr::Path { steps, .. } => steps.len(),
                other => panic!("not a path: {other:?}"),
            }
        };
        for src in ["1e2.2@$w.2[0]", "0 . 2[0]", "0 . 0[1][2]", "0 . 0 . 2[0]"] {
            assert_eq!(steps(&fmt(src)), steps(src), "step count changed: {src}");
        }
        // The shape the old formatter produced, for contrast.
        assert_eq!(steps("1e2.2@$w.2[0]"), 3);
        assert_eq!(steps("1e2 . 2.2[0]"), 2);
    }

    /// Wrapping the step in parentheses instead would not do: a block step
    /// adds an ancestry level, so `%` inside it points one step further out
    /// (`a.b.-%.n` → `[-1, -2]`, `a.b.(-%).n` → D1002). The formatter must
    /// therefore never parenthesize a step.
    #[test]
    fn negated_step_is_not_parenthesized() {
        assert_eq!(fmt("a.b.-%.n"), "a.b.-%.n");
        assert!(!fmt("2.a.--b{}.c@$v").contains("(-"));
    }

    /// A placeholder is a complete operand, so the token after it is lexed
    /// in infix context: the `/` in `?/2` is a division and not the start of
    /// a regex (jsntrs-ecq.9).
    ///
    /// Since jsntrs-9un a `?` may only stand for a whole argument, so these
    /// spellings are rejected — but with **S0211**, not the S0302
    /// "unterminated regex" a prefix-context `/` would have produced, which
    /// is exactly the lexer property this guards.
    #[test]
    fn placeholder_is_an_operand_for_the_lexer() {
        for src in ["-?{}/0", "o#$>?#$i/x", "?/2", "$f(?/2)"] {
            let err = format(src).expect_err("a stray ? is not an expression");
            assert_eq!(err.code, "S0211", "{src}");
        }
        // The one position the documentation gives `?` still round-trips.
        let once = fmt("$substring(?,0,5)");
        assert_eq!(
            format(&once).expect("re-parse"),
            once,
            "not idempotent: {once}"
        );
    }

    // ── Trailing whitespace ──────────────────────────────────

    /// `String::trim_end` strips Unicode whitespace, which is wider than the
    /// set the lexer skips: `\x0c`, `\u{a0}`, `\u{2028}`, `\u{85}` are all
    /// ordinary characters to it, so one at the end of the expression is part
    /// of the last *token* and trimming it silently renamed the field
    /// (jsntrs-ecq.12).
    ///
    /// The output has to *end* in one for the trim to reach it, which rules
    /// out a backtick-quoted name (the closing backtick shields it): the two
    /// spellings that reach the end are a `$variable` and the bare spelling
    /// forced by a name that itself holds a backtick.
    #[test]
    fn trailing_whitespace_the_lexer_does_not_skip_is_token_text() {
        for space in ['\u{c}', '\u{a0}', '\u{2028}', '\u{85}', '\u{2003}'] {
            for src in [format!("$x{space}"), format!("a`b{space}")] {
                let once = fmt(&src);
                assert!(
                    once.ends_with(space),
                    "trailing {space:?} eaten out of {src:?}: {once:?}"
                );
                assert_eq!(format(&once).expect("re-parse"), once, "not idempotent");
            }
        }
    }

    /// And the name it belongs to still selects the same field: `` a`b\u{a0} ``
    /// trimmed down to `` a`b ``, which matches nothing.
    #[test]
    fn trimmed_name_would_select_a_different_field() {
        use crate::Expression;
        let data = "{\"a`b\u{a0}\": 5, \"a`b\": 6}";
        let eval = |text: &str| {
            Expression::compile(text)
                .unwrap_or_else(|e| panic!("compile `{text}`: {e}"))
                .evaluate(data)
                .unwrap_or_else(|e| panic!("eval `{text}`: {e}"))
                .to_string()
        };
        let src = "a`b\u{a0}";
        assert_eq!(eval(src), "5", "unexpected source result");
        assert_eq!(eval(&fmt(src)), "5", "formatted output changed the name");
        // The spelling the old trim produced picks the other field.
        assert_eq!(eval("a`b"), "6");
    }

    /// The whitespace the lexer *does* skip is still stripped — including the
    /// indent `emit_comments_before` leaves behind.
    #[test]
    fn trailing_lexer_whitespace_is_still_trimmed() {
        for src in ["a  ", "a\n\t", "a\r\n", "a\u{b}", "  a \t\n"] {
            assert_eq!(fmt(src), "a", "not trimmed: {src:?}");
        }
        assert!(
            !fmt("$x /* end */").ends_with(' '),
            "comment indent left dangling"
        );
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
