//! AST post-processing pass.
//!
//! Transforms the raw Pratt-parsed tree into a form suitable for evaluation:
//! - Flattens nested binary(".") nodes into Path nodes with steps vectors.
//! - Propagates KeepSingletonArray when any step has keep_array=true.
//! - Attaches group expressions from path-step binary("{") to the path.
//! - Marks tail-call positions in lambda bodies for TCO.
//! - Recursively processes all child nodes.
//!
//! Direct port of Go `internal/parser/process.go` and `internal/parser/tailcall.go`.

use super::ast::{AstArena, BinaryOp, Expr, GroupExpr, NodeId, push_children};
use crate::error::JsonataError;

/// Check if a path step (or any node in its LHS chain) has keep_array set.
fn step_has_keep_array(arena: &AstArena, step: NodeId) -> bool {
    let mut current = step;
    loop {
        match arena.get(current) {
            Expr::Name {
                keep_array: true, ..
            }
            | Expr::Binary {
                keep_array: true, ..
            }
            | Expr::Variable {
                keep_array: true, ..
            }
            | Expr::Function {
                keep_array: true, ..
            }
            | Expr::Sort {
                keep_array: true, ..
            }
            | Expr::Block {
                keep_array: true, ..
            }
            | Expr::Unary {
                keep_array: true, ..
            }
            | Expr::Wildcard {
                keep_array: true, ..
            }
            | Expr::Descendant {
                keep_array: true, ..
            } => {
                return true;
            }
            _ => {}
        }
        // Walk into the LHS of Binary nodes (e.g. A[][filter] pattern), and
        // into a sort's operand: the `[]` in `a[]^(b).b` is written on a step
        // of the path this sort re-orders, and the documentation puts the
        // suffix "on any step in the path expression"
        // (https://docs.jsonata.org/predicate § Singleton array and value
        // equivalence), honoured once at the end. Stopping at the sort left
        // `a[]^(b).b` unflagged while `a[].b` was flagged (jsntrs-by0).
        match arena.get(current) {
            Expr::Binary { lhs, .. } => current = *lhs,
            Expr::Sort { expr, .. } => current = *expr,
            Expr::Path {
                keep_singleton_array,
                ..
            } => return *keep_singleton_array,
            _ => break,
        }
    }
    false
}

/// Maximum recursion depth for `process_node` on wasm32, which cannot grow
/// its stack. Deep left-associative chains (`1+1+1+...`) build tree depth
/// the parser's expression-nesting cap never sees; ~1,000 frames stays
/// comfortably inside a default 1 MB wasm stack.
#[cfg(target_arch = "wasm32")]
const MAX_PROCESS_DEPTH: u32 = 1_000;

#[cfg(target_arch = "wasm32")]
thread_local! {
    static PROCESS_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Run the post-processing pass over a parsed AST.
/// Call this after `Parser::parse()` and before evaluation.
///
/// Also precomputes the [`static_flags`] side table for the processed tree,
/// so the evaluator can read per-node properties (parent refs, focus/index
/// bindings, group-binding uses) without re-walking subtrees.
///
/// # Errors
/// Returns a `JsonataError` if the AST contains structural issues, or
/// S0210 on wasm32 when the tree is nested too deeply to process.
pub fn process_ast(arena: &mut AstArena, node: NodeId) -> Result<NodeId, JsonataError> {
    let root = process_node(arena, node)?;
    compute_static_flags(arena, root);
    Ok(root)
}

/// The recursive body of [`process_ast`] — re-entered for every subnode.
///
/// Two operand positions bypass the decoration wrap and call
/// [`process_node_undecorated`] instead: see [`process_subscript_lhs`] and
/// [`process_sort_operand`].
fn process_node(arena: &mut AstArena, node: NodeId) -> Result<NodeId, JsonataError> {
    let processed = process_node_undecorated(arena, node)?;
    wrap_decorated_step(arena, processed)
}

/// [`process_node`] without the trailing [`wrap_decorated_step`] promotion.
fn process_node_undecorated(arena: &mut AstArena, node: NodeId) -> Result<NodeId, JsonataError> {
    // Every recursive call re-enters through this function, so guarding here
    // covers the whole pass. Mirrors Parser::expression: grow the stack on
    // native (left-associative spines are parsed in one frame, so
    // MAX_PARSE_DEPTH does not bound tree depth); bound the depth on wasm.
    #[cfg(not(target_arch = "wasm32"))]
    {
        stacker::maybe_grow(crate::STACK_RED_ZONE, crate::STACK_GROW_SIZE, || {
            process_node_inner(arena, node)
        })
    }
    #[cfg(target_arch = "wasm32")]
    {
        let depth = PROCESS_DEPTH.with(|d| {
            let v = d.get() + 1;
            d.set(v);
            v
        });
        let result = if depth > MAX_PROCESS_DEPTH {
            Err(JsonataError::new(
                "S0210",
                "expression is too deeply nested",
            ))
        } else {
            process_node_inner(arena, node)
        };
        PROCESS_DEPTH.with(|d| d.set(d.get() - 1));
        result
    }
}

#[expect(clippy::too_many_lines)]
fn process_node_inner(arena: &mut AstArena, node: NodeId) -> Result<NodeId, JsonataError> {
    if node.is_empty() {
        return Ok(node);
    }

    match arena.get(node).clone() {
        Expr::Binary { ref op, .. } if *op == BinaryOp::Dot => process_dot_binary(arena, node),
        Expr::Binary { op, lhs, rhs, .. } => {
            let new_lhs = if op == BinaryOp::Subscript {
                process_subscript_lhs(arena, lhs)?
            } else {
                process_node(arena, lhs)?
            };
            let new_rhs = process_node(arena, rhs)?;
            let expr = arena.get_mut(node);
            if let Expr::Binary { lhs: l, rhs: r, .. } = expr {
                *l = new_lhs;
                *r = new_rhs;
            }
            process_group(arena, node)?;
            Ok(node)
        }
        Expr::Unary {
            operand,
            expressions,
            lhs,
            ..
        } => {
            let new_operand = process_node(arena, operand)?;
            let new_exprs: Vec<NodeId> = expressions
                .iter()
                .map(|&e| process_node(arena, e))
                .collect::<Result<_, _>>()?;
            let new_lhs: Vec<NodeId> = lhs
                .iter()
                .map(|&e| process_node(arena, e))
                .collect::<Result<_, _>>()?;
            let expr = arena.get_mut(node);
            if let Expr::Unary {
                operand: o,
                expressions: es,
                lhs: l,
                ..
            } = expr
            {
                *o = new_operand;
                *es = new_exprs;
                *l = new_lhs;
            }
            process_group(arena, node)?;
            Ok(node)
        }
        Expr::Block { expressions, .. } => {
            let new_exprs: Vec<NodeId> = expressions
                .iter()
                .map(|&e| process_node(arena, e))
                .collect::<Result<_, _>>()?;
            let expr = arena.get_mut(node);
            if let Expr::Block {
                expressions: es, ..
            } = expr
            {
                *es = new_exprs;
            }
            Ok(node)
        }
        Expr::Grouped { expr, .. } => {
            let new_expr = process_node(arena, expr)?;
            if let Expr::Grouped { expr: e, .. } = arena.get_mut(node) {
                *e = new_expr;
            }
            process_group(arena, node)?;
            Ok(node)
        }
        Expr::Function {
            procedure,
            arguments,
            ..
        }
        | Expr::Partial {
            procedure,
            arguments,
            ..
        } => {
            let new_proc = process_node(arena, procedure)?;
            let new_args: Vec<NodeId> = arguments
                .iter()
                .map(|&a| process_node(arena, a))
                .collect::<Result<_, _>>()?;
            let expr = arena.get_mut(node);
            match expr {
                Expr::Function {
                    procedure: p,
                    arguments: a,
                    ..
                }
                | Expr::Partial {
                    procedure: p,
                    arguments: a,
                    ..
                } => {
                    *p = new_proc;
                    *a = new_args;
                }
                _ => {}
            }
            process_group(arena, node)?;
            Ok(node)
        }
        Expr::Lambda { body, .. } => {
            let new_body = process_node(arena, body)?;
            let expr = arena.get_mut(node);
            if let Expr::Lambda { body: b, .. } = expr {
                *b = new_body;
            }
            mark_tail_calls(arena, new_body);
            Ok(node)
        }
        Expr::Condition {
            condition,
            then,
            else_,
            ..
        } => {
            let new_cond = process_node(arena, condition)?;
            let new_then = process_node(arena, then)?;
            let new_else = match else_ {
                Some(e) => Some(process_node(arena, e)?),
                None => None,
            };
            let expr = arena.get_mut(node);
            if let Expr::Condition {
                condition: c,
                then: t,
                else_: el,
                ..
            } = expr
            {
                *c = new_cond;
                *t = new_then;
                *el = new_else;
            }
            Ok(node)
        }
        Expr::Bind { lhs, rhs, .. } => {
            let new_lhs = process_node(arena, lhs)?;
            let new_rhs = process_node(arena, rhs)?;
            let expr = arena.get_mut(node);
            if let Expr::Bind { lhs: l, rhs: r, .. } = expr {
                *l = new_lhs;
                *r = new_rhs;
            }
            Ok(node)
        }
        Expr::Transform {
            pattern,
            update,
            delete,
            ..
        } => {
            let new_pattern = process_node(arena, pattern)?;
            let new_update = process_node(arena, update)?;
            let new_delete = match delete {
                Some(d) => Some(process_node(arena, d)?),
                None => None,
            };
            let expr = arena.get_mut(node);
            if let Expr::Transform {
                pattern: p,
                update: u,
                delete: d,
                ..
            } = expr
            {
                *p = new_pattern;
                *u = new_update;
                *d = new_delete;
            }
            Ok(node)
        }
        Expr::Sort {
            expr: sort_expr,
            terms,
            ..
        } => {
            let new_expr = process_sort_operand(arena, sort_expr)?;
            let new_terms: Vec<_> = terms
                .iter()
                .map(|t| {
                    let new_e = process_node(arena, t.expression)?;
                    Ok(super::ast::SortTerm {
                        descending: t.descending,
                        expression: new_e,
                    })
                })
                .collect::<Result<_, JsonataError>>()?;
            let expr = arena.get_mut(node);
            if let Expr::Sort {
                expr: e, terms: ts, ..
            } = expr
            {
                *e = new_expr;
                *ts = new_terms;
            }
            Ok(node)
        }
        Expr::Path { steps, .. } => {
            let mut keep_singleton = false;
            let new_steps: Vec<NodeId> = steps
                .iter()
                .map(|&s| {
                    let processed = process_node(arena, s)?;
                    if step_has_keep_array(arena, processed) {
                        keep_singleton = true;
                    }
                    Ok(processed)
                })
                .collect::<Result<_, JsonataError>>()?;
            let expr = arena.get_mut(node);
            if let Expr::Path {
                steps: ss,
                keep_singleton_array: ksa,
                ..
            } = expr
            {
                *ss = new_steps;
                if keep_singleton {
                    *ksa = true;
                }
            }
            process_group(arena, node)?;
            Ok(node)
        }
        // Leaf nodes (`Name`, `Wildcard`, `Descendant`, literals …) — still
        // need to process any attached Group expression.
        _ => {
            process_group(arena, node)?;
            Ok(node)
        }
    }
}

/// Process the left operand of a subscript without promoting a decorated
/// step to a `Path`.
///
/// jsntrs models a predicate as `Binary(Subscript)` rather than a `stages`
/// list on the step, so [`step_has_keep_array`] and
/// `evaluator::binary::has_keep_array` find `A[]`'s flag by walking the
/// subscript's left chain (`Phone[][type="mobile"].number`). A `Path` in that
/// chain would hide the flag from both, and `eval_subscript_binary`
/// additionally matches a raw `Descendant` operand to prepend the context
/// node (`**[0]`), so the operand keeps its raw shape — the enclosing
/// `Binary` is what the hoists inspect.
fn process_subscript_lhs(arena: &mut AstArena, node: NodeId) -> Result<NodeId, JsonataError> {
    process_node_undecorated(arena, node)
}

/// Process the operand of a `^` (order-by) without promoting a decorated
/// `Wildcard` to a `Path`.
///
/// jsonata-js's `^` wraps a non-path operand in a *fresh* path carrying no
/// `keepSingletonArray`, so the sort's re-sequencing discards a wildcard's
/// keep-array marker: `*[]^(a)` on `{"p": {"a": 1}}` is `{"a": 1}`, not
/// `[{"a": 1}]`. A decorated `Name` goes the other way — `processAST`
/// promotes *every* name to a path that already carries
/// `keepSingletonArray`, and `^` appends the sort step to that same path, so
/// `a[]^(b)` is `[{"b": 1}]`. Suppressing only the wildcard wrap reproduces
/// both. (`Descendant` needs no arm: [`wrap_decorated_step`] never wraps it.)
fn process_sort_operand(arena: &mut AstArena, node: NodeId) -> Result<NodeId, JsonataError> {
    if !node.is_empty() && matches!(arena.get(node), Expr::Wildcard { .. }) {
        return process_node_undecorated(arena, node);
    }
    process_node(arena, node)
}

/// Promote a decorated lone step to a single-step `Path`.
///
/// jsonata-js `processAST` turns *every* `name` into `{type:'path',
/// steps:[name]}` and hoists `keepArray` onto the path's
/// `keepSingletonArray`. jsntrs keeps a bare `Name` as itself — it is the
/// hottest node kind in the engine and the `Name` evaluator is a single
/// field lookup — and wraps only when the node carries a decoration the
/// evaluator cannot honour on its own:
///
/// - the `[]` suffix — without a path there is no `keep_singleton_array`
///   to set, so `a[]` on `{"a": 1}` collapsed to `1` instead of `[1]`
///   (jsntrs-ews). A name that *also* carries a group-by is left alone
///   here: jsonata-js applies the group after marking the sequence, so the
///   group result replaces it and the `[]` never shows
///   (`a[]{"k": $}` → `{"k": 1}`) — which is what the un-wrapped `Name`
///   already does;
/// - an `@$v` focus or `#$i` index binding — the variable is bound by the
///   path's tuple machinery, which only runs over a `Path`. A single-step
///   `books@$b{$b.g: $b.t}` routed through the standard group evaluator
///   with `$b` unbound and produced `{}` (jsntrs-p0v.8). Multi-step forms
///   were unaffected because the dot chain already built a path.
///
/// `Wildcard` and `Sort` join `Name` for the `[]` suffix (jsntrs-p0v.20):
/// neither evaluator can mark a singleton on its own, so `*[]` on
/// `{"a": 1}` collapsed to `1` and `a^(b)[]` on `{"a": {"b": 1}}` to
/// `{"b": 1}`. `Descendant` deliberately stays out: jsonata-js's
/// `evaluateDescendants` unwraps a one-item result *before* the
/// sequence-level keep-array marker applies, so a bare `**[]` is exactly
/// `**` (`**[]` on `5` → `5`). Its `keep_array` flag still matters one level
/// up, in a path (`x.**[]` on `{"x": 5}` → `[5]`) or a subscript chain.
///
/// Inside a dot chain the wrapper is transparent: `collect_path_steps`
/// splices a single-step path back into the enclosing path and
/// [`step_has_keep_array`] re-reads the flag off the step.
fn wrap_decorated_step(arena: &mut AstArena, node: NodeId) -> Result<NodeId, JsonataError> {
    if node.is_empty() {
        return Ok(node);
    }
    let (keep_singleton_array, pos) = match arena.get(node) {
        Expr::Name {
            keep_array,
            group,
            focus,
            index,
            pos,
            ..
        } => {
            let binds_tuple_var = focus.is_some() || index.is_some();
            let keeps_singleton = *keep_array && group.is_none();
            if !binds_tuple_var && !keeps_singleton {
                return Ok(node);
            }
            (*keep_array, *pos)
        }
        Expr::Wildcard {
            keep_array: true,
            pos,
        }
        | Expr::Sort {
            keep_array: true,
            pos,
            ..
        } => (true, *pos),
        _ => return Ok(node),
    };
    arena.alloc(Expr::Path {
        steps: vec![node],
        keep_singleton_array,
        group: None,
        pos,
    })
}

/// Process group-by key/value pairs on a node (if it has a group).
fn process_group(arena: &mut AstArena, node: NodeId) -> Result<(), JsonataError> {
    let group = match arena.get(node) {
        Expr::Name { group, .. } => group.clone(),
        Expr::Variable { group, .. } => group.clone(),
        Expr::Path { group, .. } => group.clone(),
        Expr::Function { group, .. } => group.clone(),
        Expr::Binary { group, .. } => group.clone(),
        Expr::Unary { group, .. } => group.clone(),
        Expr::Grouped { group, .. } => Some(group.clone()),
        _ => None,
    };
    if let Some(mut g) = group {
        for pair in &mut g.pairs {
            pair[0] = process_node(arena, pair[0])?;
            pair[1] = process_node(arena, pair[1])?;
        }
        match arena.get_mut(node) {
            Expr::Name { group: gr, .. } => *gr = Some(g),
            Expr::Variable { group: gr, .. } => *gr = Some(g),
            Expr::Path { group: gr, .. } => *gr = Some(g),
            Expr::Function { group: gr, .. } => *gr = Some(g),
            Expr::Binary { group: gr, .. } => *gr = Some(g),
            Expr::Unary { group: gr, .. } => *gr = Some(g),
            Expr::Grouped { group: gr, .. } => *gr = g,
            _ => {}
        }
    }
    Ok(())
}

/// Flatten a binary(".") node into a Path node.
fn process_dot_binary(arena: &mut AstArena, node: NodeId) -> Result<NodeId, JsonataError> {
    // Collect the steps together with any group-by attached to a dot node
    // in the chain: `{` binds looser than `.`, so `a.b{"k": $}.c` parses
    // with the group on the *inner* dot. Like jsonata-js, every group in a
    // chain hoists onto the flattened path and applies once, to the result
    // of the whole path (jsntrs-5lw.3).
    let mut steps = Vec::new();
    let mut chain_group = None;
    collect_path_steps(arena, node, &mut steps, &mut chain_group)?;

    let pos = arena.get(node).pos();

    // Check for KeepSingletonArray propagation.
    // Any step (or a subscript step's left chain) with keep_array=true
    // forces the entire path to preserve singletons as arrays.
    let mut keep_singleton = false;
    for &step in &steps {
        if step_has_keep_array(arena, step) {
            keep_singleton = true;
            break;
        }
    }

    // Process group-by key/value pairs so nested dot expressions within them are resolved.
    let processed_group = if let Some(mut g) = chain_group {
        for pair in &mut g.pairs {
            pair[0] = process_node(arena, pair[0])?;
            pair[1] = process_node(arena, pair[1])?;
        }
        Some(g)
    } else {
        None
    };

    // Reuse the node slot by replacing it with a Path.
    *arena.get_mut(node) = Expr::Path {
        steps,
        keep_singleton_array: keep_singleton,
        group: processed_group,
        pos,
    };

    Ok(node)
}

/// Recursively collect steps from nested binary(".") nodes, hoisting any
/// group-by found on a dot node into `group` (S0210 if the chain carries
/// more than one).
fn collect_path_steps(
    arena: &mut AstArena,
    node: NodeId,
    steps: &mut Vec<NodeId>,
    group: &mut Option<GroupExpr>,
) -> Result<(), JsonataError> {
    if let Expr::Binary {
        op: BinaryOp::Dot,
        lhs,
        rhs,
        group: dot_group,
        ..
    } = arena.get(node)
    {
        // In our Rust AST, Focus/Index live on Name nodes directly
        // (set by the parser's @ and # LED handlers), not on Binary "."
        // nodes. No propagation needed here.
        let (lhs, rhs) = (*lhs, *rhs);
        let dot_group = dot_group.clone();
        if let Some(g) = dot_group {
            if group.is_some() {
                // The parser's own check only sees groups on one node
                // (`a.b{...}{...}`); a chain spreads them over several.
                return Err(JsonataError::new(
                    "S0210",
                    "each step can only have one grouping expression",
                ));
            }
            *group = Some(g);
        }
        collect_path_steps(arena, lhs, steps, group)?;
        collect_path_steps(arena, rhs, steps, group)?;
        return Ok(());
    }

    // Leaf step — process it.
    let processed = process_node(arena, node)?;

    // If the processed node is itself a Path, splice its steps.
    if let Expr::Path { steps: ps, .. } = arena.get(processed) {
        let ps = ps.clone();
        steps.extend(ps);
        return Ok(());
    }

    // In a path context, a string literal is a field name lookup.
    let string_lit = if let Expr::StringLit { value, pos } = arena.get(processed) {
        Some((value.clone(), *pos))
    } else {
        None
    };
    if let Some((value, pos)) = string_lit {
        let name_node = arena.alloc(Expr::Name {
            value,
            pos,
            keep_array: false,
            stages: Vec::new(),
            group: None,
            focus: None,
            index: None,
        })?;
        steps.push(name_node);
        return Ok(());
    }

    steps.push(processed);
    Ok(())
}

// ── Tail-call optimization marking ──────────────────────────────────

/// Mark tail-call positions in a lambda body.
/// Sets `thunk = true` on Function nodes in tail position.
fn mark_tail_calls(arena: &mut AstArena, node: NodeId) {
    if node.is_empty() {
        return;
    }
    mark_tail_position(arena, node);
}

/// Recursively mark nodes in tail position.
fn mark_tail_position(arena: &mut AstArena, node: NodeId) {
    if node.is_empty() {
        return;
    }
    match arena.get(node).clone() {
        // A call with a group-by attached is NOT in tail position: the group
        // reduction still has to run on the returned value, so the frame
        // cannot be discarded. Thunking it would hand the `TailCall` sentinel
        // to `eval_group_by`, which calls `eval_function` directly and has no
        // trampoline — the sentinel would be grouped as an ordinary item
        // (jsntrs-5lw.1). Such a node falls through to the `_` arm below.
        Expr::Function { group: None, .. } => {
            if let Expr::Function { thunk, .. } = arena.get_mut(node) {
                *thunk = true;
            }
        }
        Expr::Condition { then, else_, .. } => {
            mark_tail_position(arena, then);
            if let Some(e) = else_ {
                mark_tail_position(arena, e);
            }
        }
        Expr::Block { expressions, .. } => {
            if let Some(&last) = expressions.last() {
                mark_tail_position(arena, last);
            }
        }
        Expr::Lambda { .. } => {
            // Nested lambdas get their own tail-call analysis.
        }
        // Everything else — `Expr::Bind` included — falls through unmarked.
        // The reference's `tailCallOptimize` recognises exactly three shapes
        // (call, condition, block); a `:=` right-hand side is *not* one of
        // them. Marking it handed the `TailCall` sentinel to `eval_bind`,
        // which stores the sentinel under the variable name and passes it on
        // as the bind's value, so the enclosing lambda looked like it had a
        // tail-call body: `$g := function($x){ $y := $f($x) }` then returned
        // `$f`'s raw sequence instead of the collapsed value the reference
        // produces, and `$g(…)[]` re-wrapped it (jsntrs-p0v.15). The
        // matching walk in `functions::body_is_tail_call` already declined to
        // copy this arm (jsntrs-p0v.6); the two now agree.
        _ => {}
    }
}

// ── Static property flags ───────────────────────────────────────────

/// Fill the arena's [`static_flags`] side table for every node reachable
/// from `root`, bottom-up (children before parents), so the evaluator reads
/// a byte per query instead of re-walking subtrees per evaluation.
///
/// Full-subtree properties (parent refs, group-binding uses) propagate over
/// every child edge from [`push_children`]. The focus/index-binding bit
/// mirrors the evaluator's deliberately path-local reachability: it
/// propagates only through subscript operands, sort operands, and path
/// steps.
fn compute_static_flags(arena: &mut AstArena, root: NodeId) {
    use super::ast::static_flags as f;

    // Iterative post-order DFS; slot-reuse during processing means child
    // ids are not always smaller than their parent's, so a plain forward
    // scan cannot substitute.
    let mut stack: Vec<(NodeId, bool)> = vec![(root, false)];
    let mut kids: Vec<NodeId> = Vec::new();
    while let Some((id, expanded)) = stack.pop() {
        if id.is_empty() || (!expanded && arena.node_static_flags(id).is_some()) {
            continue;
        }
        if !expanded {
            stack.push((id, true));
            kids.clear();
            push_children(arena.get(id), &mut kids);
            for &k in &kids {
                if !k.is_empty() && arena.node_static_flags(k).is_none() {
                    stack.push((k, false));
                }
            }
            continue;
        }

        let expr = arena.get(id);
        let mut flags = f::COMPUTED;
        match expr {
            Expr::Parent { .. } => flags |= f::PARENT_REF,
            Expr::Variable { name, .. } if name == "index" || name == "key" => {
                flags |= f::GROUP_BINDINGS;
            }
            _ => {}
        }
        // Own focus/index binding on the five step variants.
        let own_binding = matches!(
            expr,
            Expr::Name { index: Some(_), .. }
                | Expr::Name { focus: Some(_), .. }
                | Expr::Variable { index: Some(_), .. }
                | Expr::Variable { focus: Some(_), .. }
                | Expr::Binary { index: Some(_), .. }
                | Expr::Binary { focus: Some(_), .. }
                | Expr::Sort { index: Some(_), .. }
                | Expr::Sort { focus: Some(_), .. }
                | Expr::Block { index: Some(_), .. }
                | Expr::Block { focus: Some(_), .. }
        );
        if own_binding {
            flags |= f::INDEX_BINDING;
        }

        let child_flag = |k: NodeId| arena.node_static_flags(k).unwrap_or(0);

        // Full-subtree bits from every child edge.
        kids.clear();
        push_children(expr, &mut kids);
        for &k in &kids {
            flags |= child_flag(k) & (f::PARENT_REF | f::GROUP_BINDINGS);
        }

        // Path-local index-binding propagation.
        match expr {
            Expr::Binary {
                op: BinaryOp::Subscript,
                lhs,
                rhs,
                ..
            } => {
                flags |= (child_flag(*lhs) | child_flag(*rhs)) & f::INDEX_BINDING;
            }
            Expr::Sort { expr, .. } => {
                flags |= child_flag(*expr) & f::INDEX_BINDING;
            }
            Expr::Path { steps, .. } => {
                for &s in steps {
                    flags |= child_flag(s) & f::INDEX_BINDING;
                }
            }
            _ => {}
        }

        arena.set_node_static_flags(id, flags);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    /// Helper: parse and process, return (arena, root).
    fn parse_and_process(src: &str) -> (AstArena, NodeId) {
        let (mut arena, root) = Parser::parse(src).expect("parse failed");
        let root = process_node(&mut arena, root).expect("process failed");
        (arena, root)
    }

    /// Left-associative chains build tree depth in a single parser frame, so
    /// MAX_PARSE_DEPTH never sees it — process_node must grow the stack
    /// (native) instead of overflowing. ~25k terms ≈ 50k nodes.
    #[test]
    fn deep_left_assoc_chain_processes_without_overflow() {
        let mut src = String::with_capacity(2 + 2 * 25_000);
        src.push('1');
        for _ in 0..25_000 {
            src.push_str("+1");
        }
        let (_arena, root) = parse_and_process(&src);
        assert!(!root.is_empty());
    }

    #[test]
    fn dot_chain_flattened_to_path() {
        let (arena, root) = parse_and_process("a.b.c");
        match arena.get(root) {
            Expr::Path { steps, .. } => {
                assert_eq!(steps.len(), 3);
                for &s in steps {
                    assert!(matches!(arena.get(s), Expr::Name { .. }));
                }
            }
            other => panic!("expected Path, got {:?}", other),
        }
    }

    #[test]
    fn single_name_stays_name() {
        let (arena, root) = parse_and_process("foo");
        assert!(matches!(arena.get(root), Expr::Name { value, .. } if value == "foo"));
    }

    /// jsntrs-ews: a lone `a[]` had nowhere to record the keep-array flag,
    /// so the singleton collapsed. It becomes a one-step path instead.
    #[test]
    fn lone_keep_array_name_becomes_a_single_step_path() {
        let (arena, root) = parse_and_process("a[]");
        match arena.get(root) {
            Expr::Path {
                steps,
                keep_singleton_array: true,
                group: None,
                ..
            } => {
                assert_eq!(steps.len(), 1);
                assert!(matches!(arena.get(steps[0]), Expr::Name { value, .. } if value == "a"));
            }
            other => panic!("expected single-step Path, got {other:?}"),
        }
    }

    /// jsntrs-p0v.8: `@$v`/`#$i` on a lone name needs the path tuple
    /// machinery to bind the variable, so the name becomes a one-step path
    /// while the binding stays on the step.
    #[test]
    fn lone_focus_bound_name_becomes_a_single_step_path() {
        for src in ["books@$b{$b.g: $b.t}", "books#$i{$string($i): t}"] {
            let (arena, root) = parse_and_process(src);
            match arena.get(root) {
                Expr::Path {
                    steps,
                    keep_singleton_array: false,
                    ..
                } => {
                    assert_eq!(steps.len(), 1, "{src}");
                    match arena.get(steps[0]) {
                        Expr::Name {
                            focus,
                            index,
                            group: Some(_),
                            ..
                        } => assert!(focus.is_some() || index.is_some(), "{src}"),
                        other => panic!("{src}: expected bound Name step, got {other:?}"),
                    }
                }
                other => panic!("{src}: expected single-step Path, got {other:?}"),
            }
        }
    }

    /// A group-by is applied after the sequence is marked, so it replaces
    /// the value the `[]` would have wrapped — leave such a name alone.
    #[test]
    fn keep_array_name_with_group_stays_a_name() {
        let (arena, root) = parse_and_process(r#"a[]{"k": $}"#);
        assert!(matches!(arena.get(root), Expr::Name { value, .. } if value == "a"));
    }

    /// `step_has_keep_array` finds `A[]` in `A[][pred]` by walking the
    /// subscript's left chain, so the operand must stay a `Name`.
    #[test]
    fn subscript_operand_name_is_not_wrapped() {
        let (arena, root) = parse_and_process("a[][b=1].c");
        match arena.get(root) {
            Expr::Path {
                steps,
                keep_singleton_array: true,
                ..
            } => match arena.get(steps[0]) {
                Expr::Binary { lhs, .. } => {
                    assert!(matches!(arena.get(*lhs), Expr::Name { value, .. } if value == "a"));
                }
                other => panic!("expected subscript step, got {other:?}"),
            },
            other => panic!("expected Path with keep_singleton_array, got {other:?}"),
        }
    }

    /// jsntrs-p0v.20: `*[]` and `a^(b)[]` had nowhere to record the
    /// keep-array flag either, so both singletons collapsed.
    #[test]
    fn lone_keep_array_wildcard_and_sort_become_single_step_paths() {
        for (src, want_wildcard) in [("*[]", true), ("a^(b)[]", false)] {
            let (arena, root) = parse_and_process(src);
            match arena.get(root) {
                Expr::Path {
                    steps,
                    keep_singleton_array: true,
                    group: None,
                    ..
                } => {
                    assert_eq!(steps.len(), 1, "{src}");
                    if want_wildcard {
                        assert!(
                            matches!(
                                arena.get(steps[0]),
                                Expr::Wildcard {
                                    keep_array: true,
                                    ..
                                }
                            ),
                            "{src}"
                        );
                    } else {
                        assert!(
                            matches!(
                                arena.get(steps[0]),
                                Expr::Sort {
                                    keep_array: true,
                                    ..
                                }
                            ),
                            "{src}"
                        );
                    }
                }
                other => panic!("{src}: expected single-step Path, got {other:?}"),
            }
        }
    }

    /// jsonata-js never promotes `**` to a path, and `evaluateDescendants`
    /// unwraps a one-item result before the sequence-level keep-array marker
    /// applies — so a bare `**[]` is exactly `**` and must stay a
    /// `Descendant` (the flag still rides along for enclosing paths).
    #[test]
    fn lone_keep_array_descendant_stays_a_descendant() {
        let (arena, root) = parse_and_process("**[]");
        assert!(matches!(
            arena.get(root),
            Expr::Descendant {
                keep_array: true,
                ..
            }
        ));
    }

    /// `x.*[]` / `x.**[]`: inside a dot chain the flag rides on the step and
    /// `step_has_keep_array` hoists it onto the enclosing path.
    #[test]
    fn keep_array_wildcard_and_descendant_steps_hoist_to_the_path() {
        for src in ["x.*[]", "x.**[]"] {
            let (arena, root) = parse_and_process(src);
            match arena.get(root) {
                Expr::Path {
                    steps,
                    keep_singleton_array: true,
                    ..
                } => assert_eq!(steps.len(), 2, "{src}"),
                other => panic!("{src}: expected 2-step Path with keep-singleton, got {other:?}"),
            }
        }
    }

    /// jsonata-js's `^` re-sequences a non-path operand through a fresh path
    /// with no `keepSingletonArray`, so a wildcard operand keeps its raw
    /// shape and `*[]^(a)` collapses — unlike `a[]^(b)`, whose operand is a
    /// name and therefore already a keep-singleton path.
    #[test]
    fn sort_operand_wildcard_is_not_wrapped_but_a_name_is() {
        let (arena, root) = parse_and_process("*[]^(a)");
        match arena.get(root) {
            Expr::Sort { expr, .. } => assert!(matches!(
                arena.get(*expr),
                Expr::Wildcard {
                    keep_array: true,
                    ..
                }
            )),
            other => panic!("expected Sort over a raw Wildcard, got {other:?}"),
        }
        let (arena, root) = parse_and_process("a[]^(b)");
        match arena.get(root) {
            Expr::Sort { expr, .. } => assert!(matches!(
                arena.get(*expr),
                Expr::Path {
                    keep_singleton_array: true,
                    ..
                }
            )),
            other => panic!("expected Sort over a keep-singleton Path, got {other:?}"),
        }
    }

    /// jsntrs-ews: `[]` on a parenthesised path step is recorded on the
    /// block and hoisted to the enclosing path.
    #[test]
    fn keep_array_on_a_block_step_hoists_to_the_path() {
        let (arena, root) = parse_and_process("foo.($sum(bar))[]");
        match arena.get(root) {
            Expr::Path {
                steps,
                keep_singleton_array: true,
                ..
            } => {
                assert_eq!(steps.len(), 2);
                assert!(matches!(
                    arena.get(steps[1]),
                    Expr::Block {
                        keep_array: true,
                        ..
                    }
                ));
            }
            other => panic!("expected Path with keep_singleton_array, got {other:?}"),
        }
    }

    #[test]
    fn keep_singleton_array_propagated() {
        let (arena, root) = parse_and_process("a[].b");
        match arena.get(root) {
            Expr::Path {
                keep_singleton_array: true,
                ..
            } => {}
            other => panic!("expected Path with keep_singleton_array, got {:?}", other),
        }
    }

    #[test]
    fn string_in_path_becomes_name() {
        let (arena, root) = parse_and_process(r#"a."Product Name""#);
        match arena.get(root) {
            Expr::Path { steps, .. } => {
                assert_eq!(steps.len(), 2);
                match arena.get(steps[1]) {
                    Expr::Name { value, .. } => assert_eq!(value, "Product Name"),
                    other => panic!("expected Name, got {:?}", other),
                }
            }
            other => panic!("expected Path, got {:?}", other),
        }
    }

    #[test]
    fn tail_call_marked_on_recursive_function() {
        // Lambda body is a conditional with function calls in tail position
        let (arena, root) = parse_and_process("$f := function($n){$n > 0 ? $f($n - 1) : 0}");
        // root is Bind, rhs is Lambda
        match arena.get(root) {
            Expr::Bind { rhs, .. } => match arena.get(*rhs) {
                Expr::Lambda { body, .. } => {
                    // body is Condition, then branch is Function with thunk=true
                    match arena.get(*body) {
                        Expr::Condition { then, .. } => match arena.get(*then) {
                            Expr::Function { thunk, .. } => {
                                assert!(thunk, "function in tail position should have thunk=true");
                            }
                            other => panic!("expected Function, got {:?}", other),
                        },
                        other => panic!("expected Condition, got {:?}", other),
                    }
                }
                other => panic!("expected Lambda, got {:?}", other),
            },
            other => panic!("expected Bind, got {:?}", other),
        }
    }

    #[test]
    fn nested_path_spliced() {
        // (a.b).c should flatten to a single Path [a, b, c]
        let (arena, root) = parse_and_process("(a.b).c");
        match arena.get(root) {
            Expr::Path { steps, .. } => {
                // The parenthesized (a.b) becomes a Block containing a Path,
                // which gets spliced into the outer path.
                // Actual step count depends on how blocks in paths are handled.
                assert!(
                    steps.len() >= 2,
                    "expected at least 2 steps, got {}",
                    steps.len()
                );
            }
            other => panic!("expected Path, got {:?}", other),
        }
    }

    #[test]
    fn binary_non_dot_children_processed() {
        let (arena, root) = parse_and_process("a.b + c.d");
        match arena.get(root) {
            Expr::Binary { op, lhs, rhs, .. } => {
                assert_eq!(*op, BinaryOp::Add);
                assert!(matches!(arena.get(*lhs), Expr::Path { .. }));
                assert!(matches!(arena.get(*rhs), Expr::Path { .. }));
            }
            other => panic!("expected Binary +, got {:?}", other),
        }
    }

    #[test]
    fn block_children_processed() {
        let (arena, root) = parse_and_process("(a.b; c.d)");
        match arena.get(root) {
            Expr::Block { expressions, .. } => {
                assert_eq!(expressions.len(), 2);
                assert!(matches!(arena.get(expressions[0]), Expr::Path { .. }));
                assert!(matches!(arena.get(expressions[1]), Expr::Path { .. }));
            }
            other => panic!("expected Block, got {:?}", other),
        }
    }

    #[test]
    fn tail_call_in_block_last_expr() {
        let (arena, root) = parse_and_process("$f := function($n){($x := 1; $f($n - 1))}");
        match arena.get(root) {
            Expr::Bind { rhs, .. } => match arena.get(*rhs) {
                Expr::Lambda { body, .. } => match arena.get(*body) {
                    Expr::Block { expressions, .. } => {
                        let last = expressions.last().unwrap();
                        match arena.get(*last) {
                            Expr::Function { thunk, .. } => {
                                assert!(thunk, "last expr in block should be tail-call");
                            }
                            other => panic!("expected Function, got {:?}", other),
                        }
                    }
                    other => panic!("expected Block, got {:?}", other),
                },
                other => panic!("expected Lambda, got {:?}", other),
            },
            other => panic!("expected Bind, got {:?}", other),
        }
    }

    /// Does any node reachable from `root` carry `thunk = true`?
    fn has_thunked_call(arena: &AstArena, root: NodeId) -> bool {
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            let Some(expr) = arena.try_get(id) else {
                continue;
            };
            if matches!(expr, Expr::Function { thunk: true, .. }) {
                return true;
            }
            push_children(expr, &mut stack);
        }
        false
    }

    /// jsntrs-p0v.15: the reference's `tailCallOptimize` rewrites calls,
    /// conditions and blocks — never a `:=` right-hand side. Marking one
    /// handed the `TailCall` sentinel to `eval_bind`, which stored it under
    /// the variable name and passed it on as the bind's value, so the
    /// enclosing lambda behaved as if it had a tail-call body.
    #[test]
    fn a_bind_right_hand_side_is_not_a_tail_position() {
        // Control: the same three routes with the call left in tail position.
        for src in [
            "$g := function($n){$f($n)}",
            "$g := function($n){($x := 1; $f($n))}",
            "$g := function($n){$n > 0 ? $f($n) : 0}",
        ] {
            let (arena, root) = parse_and_process(src);
            assert!(
                has_thunked_call(&arena, root),
                "expected a tail call: {src}"
            );
        }
        // Wrapping the very same call in a bind takes it out of tail position.
        for src in [
            "$g := function($n){$y := $f($n)}",
            "$g := function($n){($x := 1; $y := $f($n))}",
            "$g := function($n){$n > 0 ? ($y := $f($n)) : 0}",
        ] {
            let (arena, root) = parse_and_process(src);
            assert!(
                !has_thunked_call(&arena, root),
                "a bind right-hand side must not be thunked: {src}"
            );
        }
    }

    #[test]
    fn unary_children_processed() {
        let (arena, root) = parse_and_process("[a.b, c.d]");
        match arena.get(root) {
            Expr::Unary { expressions, .. } => {
                assert_eq!(expressions.len(), 2);
                assert!(matches!(arena.get(expressions[0]), Expr::Path { .. }));
                assert!(matches!(arena.get(expressions[1]), Expr::Path { .. }));
            }
            other => panic!("expected Unary (array), got {:?}", other),
        }
    }

    #[test]
    fn condition_children_processed() {
        let (arena, root) = parse_and_process("x.y ? a.b : c.d");
        match arena.get(root) {
            Expr::Condition {
                condition,
                then,
                else_,
                ..
            } => {
                assert!(matches!(arena.get(*condition), Expr::Path { .. }));
                assert!(matches!(arena.get(*then), Expr::Path { .. }));
                assert!(matches!(arena.get(else_.unwrap()), Expr::Path { .. }));
            }
            other => panic!("expected Condition, got {:?}", other),
        }
    }
}
