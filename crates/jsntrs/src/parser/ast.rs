// Reachable only through #[doc(hidden)] re-exports for in-repo tooling;
// not part of the documented public API.
#![expect(missing_docs)]

/// Binary operator tag — replaces String for zero-cost match dispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    Add,       // +
    Sub,       // -
    Mul,       // *
    Div,       // /
    Mod,       // %
    Pow,       // **
    Concat,    // &
    Eq,        // =
    Ne,        // !=
    Lt,        // <
    Le,        // <=
    Gt,        // >
    Ge,        // >=
    And,       // and
    Or,        // or
    In,        // in
    Chain,     // ~>
    NullCoal,  // ??
    CondTern,  // ?:
    Subscript, // [
    Range,     // ..
    ObjConst,  // {
    Dot,       // . (before process_ast flattens to Path)
    Sort,      // ^  (before process_ast converts)
    Assign,    // :=
    Pipe,      // | (transform)
}

impl BinaryOp {
    /// Return the canonical string representation of this operator.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Mod => "%",
            Self::Pow => "**",
            Self::Concat => "&",
            Self::Eq => "=",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::And => "and",
            Self::Or => "or",
            Self::In => "in",
            Self::Chain => "~>",
            Self::NullCoal => "??",
            Self::CondTern => "?:",
            Self::Subscript => "[",
            Self::Range => "..",
            Self::ObjConst => "{",
            Self::Dot => ".",
            Self::Sort => "^",
            Self::Assign => ":=",
            Self::Pipe => "|",
        }
    }
}

impl std::fmt::Display for BinaryOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Unary operator tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    Negate,    // -
    ArrayCons, // [
    ObjCons,   // {
}

impl UnaryOp {
    /// Return the canonical string representation of this operator.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Negate => "-",
            Self::ArrayCons => "[",
            Self::ObjCons => "{",
        }
    }
}

impl std::fmt::Display for UnaryOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Index into the AST arena. Lightweight, Copy, no lifetimes.
///
/// Only obtainable from [`AstArena::alloc`] (or as [`NodeId::EMPTY`]), so a
/// `NodeId` is always valid for the arena that produced it. Using it with a
/// *different* arena is a bug and may panic or address the wrong node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(u32);

impl NodeId {
    /// Sentinel for "no node" (e.g. the absent else-branch of a ternary).
    /// Never a valid arena index — check with [`NodeId::is_empty`] before
    /// resolving.
    pub const EMPTY: NodeId = NodeId(u32::MAX);

    pub fn is_empty(self) -> bool {
        self == Self::EMPTY
    }
}

/// Static per-node property flags, precomputed by `process_ast` so the
/// evaluator reads a byte instead of re-walking subtrees per evaluation.
pub(crate) mod static_flags {
    /// Set when the node's flags were computed (nodes evaluated without
    /// `process_ast` fall back to on-the-fly subtree walks).
    pub(crate) const COMPUTED: u8 = 1;
    /// Subtree contains a `%` parent reference (full-subtree property).
    pub(crate) const PARENT_REF: u8 = 1 << 1;
    /// Path-local `@$var`/`#$var` binding — propagated only through
    /// subscript operands, sort operands, and path steps, mirroring
    /// `node_has_index_binding`.
    pub(crate) const INDEX_BINDING: u8 = 1 << 2;
    /// Subtree references the `$index`/`$key` group-by bindings.
    pub(crate) const GROUP_BINDINGS: u8 = 1 << 3;
}

/// Arena-based AST storage. All nodes live in a contiguous Vec.
///
/// O(1) drop (single Vec dealloc), cache-friendly, trivially serializable,
/// and `Send + Sync` safe for sharing across threads.
#[derive(Debug, Default)]
pub struct AstArena {
    nodes: Vec<Expr>,
    /// Parallel to `nodes`: [`static_flags`] bytes filled by `process_ast`.
    /// May be shorter than `nodes` for un-processed arenas — missing
    /// entries read as "not computed".
    flags: Vec<u8>,
}

impl AstArena {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            flags: Vec::new(),
        }
    }

    /// The node's precomputed [`static_flags`] byte, or `None` when flags
    /// were never computed for it (empty id, or arena not processed).
    #[inline]
    pub(crate) fn node_static_flags(&self, id: NodeId) -> Option<u8> {
        let f = *self.flags.get(id.0 as usize)?;
        (f & static_flags::COMPUTED != 0).then_some(f)
    }

    /// Store a node's [`static_flags`] byte, growing the table as needed.
    pub(crate) fn set_node_static_flags(&mut self, id: NodeId, f: u8) {
        let idx = id.0 as usize;
        if idx >= self.flags.len() {
            self.flags.resize(self.nodes.len().max(idx + 1), 0);
        }
        self.flags[idx] = f;
    }

    /// Maximum AST nodes before the parser bails. Prevents OOM from complex
    /// expressions that produce a very large number of nodes.
    const MAX_NODES: usize = 100_000;

    /// Allocate a new AST node.
    ///
    /// # Errors
    /// Returns `S0210` if the arena exceeds the node limit.
    pub fn alloc(&mut self, expr: Expr) -> Result<NodeId, crate::error::JsonataError> {
        if self.nodes.len() >= Self::MAX_NODES {
            return Err(crate::error::JsonataError::new(
                "S0210",
                "expression too complex (too many AST nodes)",
            ));
        }
        let id = self.nodes.len() as u32;
        self.nodes.push(expr);
        Ok(NodeId(id))
    }

    /// Resolve a node id to its expression.
    ///
    /// # Panics
    /// Panics if `id` is [`NodeId::EMPTY`] or out of bounds for this arena —
    /// both are caller bugs. A foreign id that happens to be in bounds is
    /// NOT detected: it silently resolves to an unrelated node. Use
    /// [`AstArena::try_get`] to resolve ids of uncertain provenance.
    #[inline]
    #[expect(clippy::panic, reason = "documented caller-bug panic (M-PANIC-ON-BUG)")]
    pub fn get(&self, id: NodeId) -> &Expr {
        self.try_get(id).unwrap_or_else(|| {
            panic!(
                "{id:?} is not a node of this arena (len {}) — NodeIds are only \
                 valid for the arena that allocated them",
                self.nodes.len()
            )
        })
    }

    /// Resolve a node id, returning `None` for [`NodeId::EMPTY`] or an id
    /// that this arena never allocated.
    #[inline]
    pub fn try_get(&self, id: NodeId) -> Option<&Expr> {
        self.nodes.get(id.0 as usize)
    }

    /// Mutable counterpart of [`AstArena::get`].
    ///
    /// # Panics
    /// Panics if `id` is [`NodeId::EMPTY`] or was allocated by a different
    /// arena — both are caller bugs.
    #[inline]
    #[expect(clippy::panic, reason = "documented caller-bug panic (M-PANIC-ON-BUG)")]
    pub fn get_mut(&mut self, id: NodeId) -> &mut Expr {
        let len = self.nodes.len();
        self.nodes.get_mut(id.0 as usize).unwrap_or_else(|| {
            panic!(
                "{id:?} is not a node of this arena (len {len}) — NodeIds are only \
                 valid for the arena that allocated them"
            )
        })
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

/// AST node variants. Replaces Go's Node.Type string dispatch.
#[derive(Debug, Clone)]
pub enum Expr {
    /// Field name lookup.
    Name {
        value: String,
        pos: usize,
        keep_array: bool,
        stages: Vec<Stage>,
        group: Option<GroupExpr>,
        focus: Option<String>,
        index: Option<String>,
    },
    /// String literal.
    StringLit { value: String, pos: usize },
    /// Numeric literal.
    NumberLit { value: f64, raw: String, pos: usize },
    /// Boolean or null literal (true, false, null).
    ValueLit {
        value: String, // "true", "false", "null"
        pos: usize,
    },
    /// Variable reference ($name).
    Variable {
        name: String, // "" for bare $, "$" for $$
        group: Option<GroupExpr>,
        keep_array: bool,
        focus: Option<String>,
        index: Option<String>,
        pos: usize,
    },
    /// Wildcard (*).
    Wildcard { pos: usize },
    /// Descendant (**).
    Descendant { pos: usize },
    /// Parent reference (%).
    Parent { pos: usize, slot: Option<Slot> },
    /// Regex literal (/pattern/flags).
    Regex {
        pattern: String,
        flags: String,
        pos: usize,
    },
    /// Partial application placeholder (?).
    Placeholder { pos: usize },

    /// Path: flattened dot-chain (a.b.c → Path { steps: [a, b, c] }).
    /// Created by AST post-processing from nested Binary dots.
    Path {
        steps: Vec<NodeId>,
        keep_singleton_array: bool,
        group: Option<GroupExpr>,
        pos: usize,
    },

    /// Binary operator.
    Binary {
        op: BinaryOp,
        lhs: NodeId,
        rhs: NodeId,
        group: Option<GroupExpr>,
        keep_array: bool,
        focus: Option<String>,
        index: Option<String>,
        pos: usize,
    },

    /// Unary operator (negation, array constructor, object constructor).
    Unary {
        op: UnaryOp,
        operand: NodeId,          // for negation
        expressions: Vec<NodeId>, // for array constructor [...]
        lhs: Vec<NodeId>,         // for object constructor {...} — flat [k0,v0,k1,v1,...]
        group: Option<GroupExpr>, // group-by expression attached in infix position
        keep_array: bool,
        pos: usize,
    },

    /// Block: semicolon-separated or parenthesized expressions.
    Block {
        expressions: Vec<NodeId>,
        /// Focus binding (`@$var`) when the block is a path step.
        focus: Option<String>,
        /// Index binding (`#$var`) when the block is a path step.
        index: Option<String>,
        pos: usize,
    },

    /// Conditional: condition ? then : else.
    Condition {
        condition: NodeId,
        then: NodeId,
        else_: Option<NodeId>,
        pos: usize,
    },

    /// Variable binding: $var := expr.
    Bind {
        lhs: NodeId,
        rhs: NodeId,
        pos: usize,
    },

    /// Function call: procedure(args...).
    Function {
        procedure: NodeId,
        arguments: Vec<NodeId>,
        pos: usize,
        thunk: bool, // TCO flag set by post-processing
        keep_array: bool,
        group: Option<GroupExpr>,
    },

    /// Partial application: procedure(args... with ? placeholders).
    Partial {
        procedure: NodeId,
        arguments: Vec<NodeId>,
        pos: usize,
    },

    /// Lambda: `function($params) <signature> { body }`.
    Lambda {
        params: Vec<NodeId>,
        body: NodeId,
        signature: Option<Signature>,
        pos: usize,
        thunk: bool,
    },

    /// Transform: |pattern|update,delete|.
    Transform {
        pattern: NodeId,
        update: NodeId,
        delete: Option<NodeId>,
        pos: usize,
    },

    /// Sort: expr ^(terms).
    Sort {
        expr: NodeId,
        terms: Vec<SortTerm>,
        keep_array: bool,
        index: Option<String>,
        focus: Option<String>,
        pos: usize,
    },

    /// Group-by attached to a node with no inline `group` slot
    /// (Block, literals, ...): `(1+2){"k": $}` or `-3{"k": $}`.
    /// Created by the parser's `{` handler when `set_group` cannot
    /// place the group on `expr` directly.
    Grouped {
        expr: NodeId,
        group: GroupExpr,
        pos: usize,
    },
}

impl Expr {
    pub fn pos(&self) -> usize {
        match self {
            Expr::Name { pos, .. }
            | Expr::StringLit { pos, .. }
            | Expr::NumberLit { pos, .. }
            | Expr::ValueLit { pos, .. }
            | Expr::Variable { pos, .. }
            | Expr::Wildcard { pos }
            | Expr::Descendant { pos }
            | Expr::Parent { pos, .. }
            | Expr::Regex { pos, .. }
            | Expr::Placeholder { pos }
            | Expr::Path { pos, .. }
            | Expr::Binary { pos, .. }
            | Expr::Unary { pos, .. }
            | Expr::Block { pos, .. }
            | Expr::Condition { pos, .. }
            | Expr::Bind { pos, .. }
            | Expr::Function { pos, .. }
            | Expr::Partial { pos, .. }
            | Expr::Lambda { pos, .. }
            | Expr::Transform { pos, .. }
            | Expr::Sort { pos, .. }
            | Expr::Grouped { pos, .. } => *pos,
        }
    }
}

/// Sort term within a Sort expression.
#[derive(Debug, Clone)]
pub struct SortTerm {
    pub descending: bool,
    pub expression: NodeId,
}

/// Predicate or index binding attached to a path step.
#[derive(Debug, Clone)]
pub struct Stage {
    pub kind: StageKind,
    pub pos: usize,
}

#[derive(Debug, Clone)]
pub enum StageKind {
    Filter { expression: NodeId },
    Index { var_name: String },
}

/// Group-by expression attached to a path step.
#[derive(Debug, Clone)]
pub struct GroupExpr {
    pub pairs: Vec<[NodeId; 2]>, // [key_expr, value_expr]
    pub pos: usize,
}

/// Parent reference slot.
#[derive(Debug, Clone)]
pub struct Slot {
    pub label: String,
    pub level: i32,
    pub index: i32,
}

pub(crate) fn push_group_pairs(group: Option<&GroupExpr>, out: &mut Vec<NodeId>) {
    if let Some(g) = group {
        for pair in &g.pairs {
            out.push(pair[0]);
            out.push(pair[1]);
        }
    }
}

/// Push every child NodeId of `expr` onto `out` — the single place that
/// knows the AST's child edges.
pub(crate) fn push_children(expr: &Expr, out: &mut Vec<NodeId>) {
    match expr {
        Expr::Name { stages, group, .. } => {
            for s in stages {
                if let StageKind::Filter { expression } = &s.kind {
                    out.push(*expression);
                }
            }
            push_group_pairs(group.as_ref(), out);
        }
        Expr::Variable { group, .. } => push_group_pairs(group.as_ref(), out),
        Expr::Grouped { expr, group, .. } => {
            out.push(*expr);
            push_group_pairs(Some(group), out);
        }
        Expr::Path { steps, group, .. } => {
            out.extend_from_slice(steps);
            push_group_pairs(group.as_ref(), out);
        }
        Expr::Binary {
            lhs, rhs, group, ..
        } => {
            out.push(*lhs);
            out.push(*rhs);
            push_group_pairs(group.as_ref(), out);
        }
        Expr::Unary {
            operand,
            expressions,
            lhs,
            group,
            ..
        } => {
            out.push(*operand);
            out.extend_from_slice(expressions);
            out.extend_from_slice(lhs);
            push_group_pairs(group.as_ref(), out);
        }
        Expr::Block { expressions, .. } => out.extend_from_slice(expressions),
        Expr::Condition {
            condition,
            then,
            else_,
            ..
        } => {
            out.push(*condition);
            out.push(*then);
            if let Some(e) = else_ {
                out.push(*e);
            }
        }
        Expr::Bind { lhs, rhs, .. } => {
            out.push(*lhs);
            out.push(*rhs);
        }
        Expr::Function {
            procedure,
            arguments,
            group,
            ..
        } => {
            out.push(*procedure);
            out.extend_from_slice(arguments);
            push_group_pairs(group.as_ref(), out);
        }
        Expr::Partial {
            procedure,
            arguments,
            ..
        } => {
            out.push(*procedure);
            out.extend_from_slice(arguments);
        }
        Expr::Lambda { body, .. } => out.push(*body),
        Expr::Transform {
            pattern,
            update,
            delete,
            ..
        } => {
            out.push(*pattern);
            out.push(*update);
            if let Some(d) = delete {
                out.push(*d);
            }
        }
        Expr::Sort { expr, terms, .. } => {
            out.push(*expr);
            out.extend(terms.iter().map(|t| t.expression));
        }
        // Leaves.
        Expr::StringLit { .. }
        | Expr::NumberLit { .. }
        | Expr::ValueLit { .. }
        | Expr::Wildcard { .. }
        | Expr::Descendant { .. }
        | Expr::Parent { .. }
        | Expr::Regex { .. }
        | Expr::Placeholder { .. } => {}
    }
}

/// Lambda type signature.
#[derive(Debug, Clone)]
pub struct Signature {
    /// Source text including the outer `<` `>` (kept for the formatter).
    pub raw: String,
    /// Parameter specs parsed at compile time — invalid signatures are
    /// compile errors, matching the Go reference (parser/signature.go).
    /// `Arc` keeps the AST `Send + Sync`.
    pub params: std::sync::Arc<[crate::evaluator::ParamSpec]>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_get_rejects_empty_and_foreign_ids() {
        let mut arena = AstArena::new();
        let id = arena
            .alloc(Expr::StringLit {
                value: "x".into(),
                pos: 0,
            })
            .unwrap();
        assert!(arena.try_get(id).is_some());
        assert!(arena.try_get(NodeId::EMPTY).is_none());
        // An id from a larger arena is out of range for this one.
        let mut bigger = AstArena::new();
        let _ = bigger.alloc(Expr::StringLit {
            value: "a".into(),
            pos: 0,
        });
        let foreign = bigger
            .alloc(Expr::StringLit {
                value: "b".into(),
                pos: 0,
            })
            .unwrap();
        assert!(arena.try_get(foreign).is_none());
    }

    #[test]
    #[should_panic(expected = "not a node of this arena")]
    fn get_panics_with_helpful_message_on_empty_id() {
        let arena = AstArena::new();
        let _ = arena.get(NodeId::EMPTY);
    }
}
