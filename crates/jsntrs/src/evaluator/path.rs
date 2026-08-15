//! Path evaluation: simple, tuple, and binding-aware step traversal.
//!
//! Split from `evaluator/mod.rs`; port of Go `internal/evaluator` path
//! walking, tuple-stream steps, and the `@`/`#` binding helpers.

use std::rc::Rc;

use crate::error::{JsonataError, JsonataResult};
use crate::parser::ast::{push_children, static_flags};
use crate::parser::{AstArena, Expr, NodeId};
use crate::parser::{BinaryOp, SortTerm, UnaryOp};
use crate::value::{Sequence, Value};

use super::environment::Environment;
use super::group::eval_tuple_group;
use super::{
    FunctionValue, JOIN_FLAG, PARENT_BINDING, PARENT_SHADOW, call_function, compare_sort_terms,
    descendant_lookup, eval_name, eval_no_stack_check, eval_variable, parent_out_of_context,
};

// ── Path evaluation (simplified for Phase 5) ────────────────────────

pub(super) fn eval_path(
    arena: &AstArena,
    node: NodeId,
    input: &Value,
    env: &Rc<Environment>,
) -> JsonataResult {
    let (steps, keep_singleton_array, group) = match arena.get(node) {
        Expr::Path {
            steps,
            keep_singleton_array,
            group,
            ..
        } => (steps.clone(), *keep_singleton_array, group.clone()),
        _ => unreachable!("eval_path is dispatched only for Expr::Path nodes"),
    };

    if steps.is_empty() {
        return Ok(Value::Undefined);
    }

    if path_has_tuple_step(arena, &steps) {
        return eval_path_tuple(
            arena,
            &steps,
            keep_singleton_array,
            group.as_ref(),
            input,
            env,
        );
    }
    eval_path_simple(arena, &steps, keep_singleton_array, input, env)
}

/// Check whether any path step requires tuple-aware evaluation (#$var index bindings, @$var focus, or % parent refs).
pub(super) fn path_has_tuple_step(arena: &AstArena, steps: &[NodeId]) -> bool {
    steps
        .iter()
        .any(|&step| node_has_index_binding(arena, step) || node_has_parent_ref(arena, step))
}

/// Check if a node's path-local structure contains an index (#$var) or focus (@$var) binding.
///
/// Deliberately NOT a full-subtree search: only subscript operands, sort
/// operands, and path steps are descended. Bindings inside lambda bodies,
/// function arguments, etc. belong to those inner paths, not to the path
/// being classified here.
///
/// Answered from the precomputed side table when `process_ast` filled it;
/// the walk below is the fallback for un-processed arenas.
pub(super) fn node_has_index_binding(arena: &AstArena, node: NodeId) -> bool {
    if node.is_empty() {
        return false;
    }
    if let Some(f) = arena.node_static_flags(node) {
        return f & static_flags::INDEX_BINDING != 0;
    }
    match arena.get(node) {
        Expr::Name { index: Some(_), .. } | Expr::Name { focus: Some(_), .. } => true,
        Expr::Variable { index: Some(_), .. } | Expr::Variable { focus: Some(_), .. } => true,
        Expr::Binary { index: Some(_), .. } | Expr::Binary { focus: Some(_), .. } => true,
        Expr::Sort { index: Some(_), .. } | Expr::Sort { focus: Some(_), .. } => true,
        Expr::Block { index: Some(_), .. } | Expr::Block { focus: Some(_), .. } => true,
        Expr::Binary { op, lhs, rhs, .. } if *op == BinaryOp::Subscript => {
            let lhs = *lhs;
            let rhs = *rhs;
            node_has_index_binding(arena, lhs) || node_has_index_binding(arena, rhs)
        }
        Expr::Sort { expr, .. } => {
            // Check the expression inside the sort.
            node_has_index_binding(arena, *expr)
        }
        Expr::Path { steps, .. } => steps.iter().any(|&s| node_has_index_binding(arena, s)),
        _ => false,
    }
}

/// Check if an AST subtree contains a % (Parent) reference anywhere.
/// Table-first; the subtree walk is the un-processed-arena fallback.
pub(super) fn node_has_parent_ref(arena: &AstArena, node: NodeId) -> bool {
    if !node.is_empty()
        && let Some(f) = arena.node_static_flags(node)
    {
        return f & static_flags::PARENT_REF != 0;
    }
    subtree_any(arena, node, |e| matches!(e, Expr::Parent { .. }))
}

/// Short-circuiting search over an AST subtree: does `pred` match the node
/// or any descendant?
///
/// Descends every child edge — operands, path steps, group pairs, stage
/// filters, and sort terms — via an explicit worklist (no recursion, no
/// clones). Lambda params are skipped: they declare names, not uses.
pub(super) fn subtree_any(arena: &AstArena, root: NodeId, pred: impl Fn(&Expr) -> bool) -> bool {
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if id.is_empty() {
            continue;
        }
        let expr = arena.get(id);
        if pred(expr) {
            return true;
        }
        push_children(expr, &mut stack);
    }
    false
}

/// Extract a step-level group from a node, if it has one.
fn extract_step_group(arena: &AstArena, step: NodeId) -> Option<crate::parser::GroupExpr> {
    match arena.get(step) {
        Expr::Name { group, .. } => group.clone(),
        Expr::Variable { group, .. } => group.clone(),
        Expr::Function { group, .. } => group.clone(),
        _ => None,
    }
}

/// Evaluate a path step, bypassing any group expression attached to it.
/// This is used in tuple-aware evaluation where groups are applied at the end.
fn eval_path_step_no_group(
    arena: &AstArena,
    step: NodeId,
    input: &Value,
    env: &Rc<Environment>,
    prev_was_mapper: bool,
    keep_singleton_array: bool,
) -> JsonataResult {
    // Check if step has a group; if so, evaluate the step WITHOUT the group.
    let has_group = matches!(
        arena.get(step),
        Expr::Name { group: Some(_), .. }
            | Expr::Variable { group: Some(_), .. }
            | Expr::Function { group: Some(_), .. }
    );
    if !has_group {
        return eval_path_step(
            arena,
            step,
            input,
            env,
            prev_was_mapper,
            keep_singleton_array,
        );
    }

    // For Name nodes with groups, evaluate as a plain name lookup.
    match arena.get(step) {
        Expr::Name { value, .. } => eval_name(value, input),
        Expr::Variable { name, .. } => Ok(eval_variable(name, input, env)),
        _ => eval_path_step(
            arena,
            step,
            input,
            env,
            prev_was_mapper,
            keep_singleton_array,
        ),
    }
}

/// Handle a Sort step within tuple-aware path evaluation.
///
/// Sorts all tuples simultaneously using the sort terms. If the sort expression
/// contains navigation (non-variable inner expr), expands it in tuple mode first
/// to preserve parent bindings.
fn eval_tuple_sort_step(
    arena: &AstArena,
    expr: NodeId,
    terms: &[SortTerm],
    ctxs: Vec<(Value, Rc<Environment>)>,
) -> Result<Vec<(Value, Rc<Environment>)>, JsonataError> {
    let needs_navigation = !expr.is_empty() && !matches!(arena.get(expr), Expr::Variable { .. });
    let sorted_ctxs = if needs_navigation {
        let mut inner_ctxs: Vec<(Value, Rc<Environment>)> = Vec::new();
        for (val, ctx_env) in &ctxs {
            if let Expr::Path {
                steps: inner_steps, ..
            } = arena.get(expr)
            {
                let inner_steps = inner_steps.clone();
                let expanded =
                    expand_path_tuple(arena, &inner_steps, &[(val.clone(), ctx_env.clone())])?;
                inner_ctxs.extend(expanded);
            } else {
                let result = eval_no_stack_check(arena, expr, val, ctx_env)?;
                if !result.is_undefined() {
                    let items = flatten_to_vec(result);
                    let (index_var, focus_var) = get_step_bindings(arena, expr);
                    let is_join = focus_var.is_some();
                    for (j, elem) in items.iter().enumerate() {
                        let child_env = Environment::new_child(Rc::clone(ctx_env));
                        child_env.bind(PARENT_BINDING, val.clone());
                        if is_join {
                            child_env.bind(JOIN_FLAG, Value::Bool(true));
                        }
                        if let Some(ref var_name) = index_var {
                            child_env.bind(var_name.clone(), Value::Number(j as f64));
                        }
                        if let Some(ref var_name) = focus_var {
                            child_env.bind(var_name.clone(), elem.clone());
                        }
                        let ctx_value = if is_join { val.clone() } else { elem.clone() };
                        inner_ctxs.push((ctx_value, Rc::new(child_env)));
                    }
                }
            }
        }
        inner_ctxs
    } else {
        ctxs
    };

    crate::try_sort::try_sort_by(sorted_ctxs, |a, b| {
        compare_sort_terms(arena, terms, &a.0, &b.0, &a.1, &b.1).map(|c| c.cmp(&0))
    })
}

/// Handle a Parent (%) step within tuple-aware path evaluation.
///
/// Navigates up the parent chain using `%%` env bindings. For join steps,
/// skips through ancestor envs that are also join bindings with the same parent.
fn eval_tuple_parent_step(
    ctxs: &[(Value, Rc<Environment>)],
) -> Result<Vec<(Value, Rc<Environment>)>, JsonataError> {
    let mut next_ctxs: Vec<(Value, Rc<Environment>)> = Vec::new();
    for (_, ctx_env) in ctxs {
        if let Some((parent_val, binding_env)) =
            Environment::lookup_with_env(ctx_env, PARENT_BINDING)
        {
            if parent_val.is_null() || parent_val.is_undefined() {
                // A group-by pair shadows `%%`: `%` there is undefined, so
                // the context simply drops out of the tuple stream.
                if binding_env.lookup_direct(PARENT_SHADOW).is_some() {
                    continue;
                }
                return Err(parent_out_of_context());
            }
            let mut parent_env = binding_env
                .parent()
                .cloned()
                .unwrap_or_else(|| binding_env.clone());
            if binding_env.lookup_direct(JOIN_FLAG).is_some() {
                loop {
                    if parent_env.lookup_direct(JOIN_FLAG).is_none() {
                        break;
                    }
                    if let Some((pv, pe)) =
                        Environment::lookup_with_env(&parent_env, PARENT_BINDING)
                    {
                        if value_ptr_eq(&pv, &parent_val) {
                            parent_env = pe.parent().cloned().unwrap_or_else(|| pe.clone());
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
            }
            next_ctxs.push((parent_val, parent_env));
        } else {
            return Err(parent_out_of_context());
        }
    }
    Ok(next_ctxs)
}

/// Handle a compound join-filter subscript within tuple-aware path evaluation.
///
/// Processes `books@$b[pred][idx]`: first applies the inner join-filter to collect
/// tuples, then applies the outer subscript to the entire tuple collection.
fn eval_tuple_compound_join(
    arena: &AstArena,
    ctxs: &[(Value, Rc<Environment>)],
    inner_lhs: NodeId,
    inner_rhs: NodeId,
    outer_rhs: NodeId,
    focus_name: &str,
    index_var: Option<&String>,
) -> Result<Vec<(Value, Rc<Environment>)>, JsonataError> {
    let mut next_ctxs = eval_join_filter(
        arena,
        ctxs,
        Vec::new(),
        inner_lhs,
        inner_rhs,
        focus_name,
        index_var,
    )?;

    if !next_ctxs.is_empty() {
        let outer_result = eval_no_stack_check(arena, outer_rhs, &next_ctxs[0].0, &next_ctxs[0].1)?;
        if let Some(idx) = outer_result.as_f64() {
            let mut i = idx as i64;
            if i < 0 {
                i += next_ctxs.len() as i64;
            }
            if i >= 0 && (i as usize) < next_ctxs.len() {
                next_ctxs = vec![next_ctxs[i as usize].clone()];
            } else {
                next_ctxs = vec![];
            }
        }
    }
    Ok(next_ctxs)
}

/// A (value, environment) pair flowing through tuple-aware path evaluation.
type TupleCtx = (Value, Rc<Environment>);

/// Tuple-aware path evaluation for paths containing #$var index bindings or % parent refs.
///
/// Maintains a list of (value, env) contexts so that position variables bound
/// at one step remain accessible in all subsequent steps.
fn eval_path_tuple(
    arena: &AstArena,
    steps: &[NodeId],
    keep_singleton_array: bool,
    group: Option<&crate::parser::GroupExpr>,
    input: &Value,
    env: &Rc<Environment>,
) -> JsonataResult {
    // Each context pairs a value with the env it was produced under.
    let mut ctxs: Vec<TupleCtx> = vec![(input.clone(), env.clone())];

    // Detect the first step-level group expression; it will be applied at the
    // end via eval_tuple_group instead of during per-element evaluation.
    let final_group = steps.iter().find_map(|&s| extract_step_group(arena, s));

    for (step_idx, &step) in steps.iter().enumerate() {
        // Sort steps apply globally to all tuples at once and may legitimately
        // continue with an empty context set.
        if let Expr::Sort { expr, terms, .. } = arena.get(step) {
            let expr = *expr;
            let terms = terms.clone();
            ctxs = eval_tuple_sort_step(arena, expr, &terms, ctxs)?;
            continue;
        }
        ctxs = eval_tuple_step(arena, step, step_idx, &ctxs, keep_singleton_array)?;
        if ctxs.is_empty() {
            return Ok(Value::Undefined);
        }
    }

    // Determine which group expression to apply (step-level or path-level).
    let effective_group = final_group.as_ref().or(group);
    if let Some(grp) = effective_group {
        return eval_tuple_group(arena, grp, &ctxs);
    }

    Ok(collect_tuple_results(&ctxs, keep_singleton_array))
}

/// Evaluate one non-sort path step across all tuple contexts, dispatching to
/// the special forms (%, %[pred], block forms, joins) before the general
/// per-element expansion.
fn eval_tuple_step(
    arena: &AstArena,
    step: NodeId,
    step_idx: usize,
    ctxs: &[TupleCtx],
    keep_singleton_array: bool,
) -> JsonataResult<Vec<TupleCtx>> {
    // Parent (%) steps navigate up the parent chain using the %% env bindings.
    if matches!(arena.get(step), Expr::Parent { .. }) {
        return eval_tuple_parent_step(ctxs);
    }

    if let Expr::Binary {
        op,
        lhs,
        rhs,
        index: post_filter_index,
        ..
    } = arena.get(step)
    {
        let (op, lhs, rhs) = (*op, *lhs, *rhs);
        let post_filter_index = post_filter_index.clone();
        if op == BinaryOp::Subscript && !lhs.is_empty() {
            // %[predicate]: navigate to the parent first, then filter.
            if matches!(arena.get(lhs), Expr::Parent { .. }) {
                return eval_tuple_parent_pred_step(arena, rhs, ctxs);
            }
            // (block)[pred-with-%]: preserve per-element parent bindings.
            if matches!(arena.get(lhs), Expr::Block { .. }) && node_has_parent_ref(arena, rhs) {
                return eval_tuple_block_pred_step(arena, lhs, rhs, ctxs);
            }
            // Join operator: lhs@$focus[pred], e.g. Contact@$c[$c.ssn = $e.SSN].
            if let Expr::Name {
                focus: Some(focus_name),
                index,
                ..
            } = arena.get(lhs)
            {
                let focus_name = focus_name.clone();
                let index_var = index.clone();
                return eval_tuple_join_step(
                    arena,
                    ctxs,
                    lhs,
                    rhs,
                    &focus_name,
                    index_var.as_ref(),
                    post_filter_index.as_ref(),
                );
            }
            // Compound subscript after a join-filter: books@$b[pred][1].
            if let Expr::Binary {
                op: inner_op,
                lhs: inner_lhs,
                rhs: inner_rhs,
                ..
            } = arena.get(lhs)
            {
                let (inner_op, inner_lhs, inner_rhs) = (*inner_op, *inner_lhs, *inner_rhs);
                if inner_op == BinaryOp::Subscript
                    && !inner_lhs.is_empty()
                    && let Expr::Name {
                        focus: Some(focus_name),
                        index,
                        ..
                    } = arena.get(inner_lhs)
                {
                    let focus_name = focus_name.clone();
                    let index_var = index.clone();
                    return eval_tuple_compound_join(
                        arena,
                        ctxs,
                        inner_lhs,
                        inner_rhs,
                        rhs,
                        &focus_name,
                        index_var.as_ref(),
                    );
                }
            }
        }
    }

    // Block containing a single Path: expand the inner path in tuple mode to
    // preserve parent bindings for % (e.g., Account.(Order.Product).{%.OrderID}).
    if let Expr::Block { expressions, .. } = arena.get(step) {
        let expressions = expressions.clone();
        if expressions.len() == 1
            && let Expr::Path {
                steps: inner_steps, ..
            } = arena.get(expressions[0])
        {
            let inner_steps = inner_steps.clone();
            return expand_path_tuple(arena, &inner_steps, ctxs);
        }
    }

    eval_tuple_general_step(arena, step, step_idx, ctxs, keep_singleton_array)
}

/// %[predicate] step: navigate to the parent first, then apply the predicate
/// in the parent's own env (so nested % inside it refers to the grandparent).
fn eval_tuple_parent_pred_step(
    arena: &AstArena,
    rhs: NodeId,
    ctxs: &[TupleCtx],
) -> JsonataResult<Vec<TupleCtx>> {
    let mut next_ctxs = Vec::new();
    for (_, ctx_env) in ctxs {
        let Some((parent_val, binding_env)) = Environment::lookup_with_env(ctx_env, PARENT_BINDING)
        else {
            return Err(parent_out_of_context());
        };
        if parent_val.is_null() || parent_val.is_undefined() {
            // Shadowed by a group-by pair: drop the context (see
            // `eval_tuple_parent_step`).
            if binding_env.lookup_direct(PARENT_SHADOW).is_some() {
                continue;
            }
            return Err(parent_out_of_context());
        }
        let parent_env = binding_env
            .parent()
            .cloned()
            .unwrap_or_else(|| binding_env.clone());
        let pred_result = eval_no_stack_check(arena, rhs, &parent_val, &parent_env)?;
        if pred_result.to_boolean() {
            next_ctxs.push((parent_val, parent_env));
        }
    }
    Ok(next_ctxs)
}

/// (block)[pred-with-%] step: the block would normally discard per-element
/// parent context, so expand its inner path in tuple mode to preserve parent
/// bindings, then apply the predicate per-tuple.
/// Example: (Account.Order.Product)[%.OrderID='order104'].SKU
fn eval_tuple_block_pred_step(
    arena: &AstArena,
    lhs: NodeId,
    rhs: NodeId,
    ctxs: &[TupleCtx],
) -> JsonataResult<Vec<TupleCtx>> {
    let mut next_ctxs = Vec::new();
    for (val, ctx_env) in ctxs {
        let mut tuple_ctxs: Vec<TupleCtx> = Vec::new();
        if let Expr::Block { expressions, .. } = arena.get(lhs) {
            let expressions = expressions.clone();
            if expressions.len() == 1
                && let Expr::Path {
                    steps: inner_steps, ..
                } = arena.get(expressions[0])
            {
                let inner_steps = inner_steps.clone();
                tuple_ctxs =
                    expand_path_tuple(arena, &inner_steps, &[(val.clone(), ctx_env.clone())])?;
            }
            if tuple_ctxs.is_empty() {
                // Fallback: evaluate block normally.
                let block_result = eval_no_stack_check(arena, lhs, val, ctx_env)?;
                if block_result.is_undefined() {
                    continue;
                }
                match block_result {
                    Value::Array(arr) => {
                        for item in arr.iter() {
                            tuple_ctxs.push((item.clone(), ctx_env.clone()));
                        }
                    }
                    other => tuple_ctxs.push((other, ctx_env.clone())),
                }
            }
        }
        for (tval, tenv) in &tuple_ctxs {
            let pred_result = eval_no_stack_check(arena, rhs, tval, tenv)?;
            if pred_result.to_boolean() {
                next_ctxs.push((tval.clone(), tenv.clone()));
            }
        }
    }
    Ok(next_ctxs)
}

/// Focus-join subscript step (lhs@$focus[pred]): delegate to the join filter
/// and bind the post-filter #$var index if the subscript node carries one.
fn eval_tuple_join_step(
    arena: &AstArena,
    ctxs: &[TupleCtx],
    lhs: NodeId,
    rhs: NodeId,
    focus_name: &str,
    index_var: Option<&String>,
    post_filter_index: Option<&String>,
) -> JsonataResult<Vec<TupleCtx>> {
    let next_ctxs = eval_join_filter(arena, ctxs, Vec::new(), lhs, rhs, focus_name, index_var)?;
    if let Some(pfi_name) = post_filter_index {
        for (k, (_, env)) in next_ctxs.iter().enumerate() {
            env.bind(pfi_name.clone(), Value::Number(k as f64));
        }
    }
    Ok(next_ctxs)
}

/// General tuple step: evaluate the step for every context, flatten the
/// results, and create a child env per element carrying the % parent binding
/// and any #$var/@$var bindings.
fn eval_tuple_general_step(
    arena: &AstArena,
    step: NodeId,
    step_idx: usize,
    ctxs: &[TupleCtx],
    keep_singleton_array: bool,
) -> JsonataResult<Vec<TupleCtx>> {
    let mut next_ctxs = Vec::new();
    for (val, ctx_env) in ctxs {
        // Collapse sequences between steps.
        let val = collapse_val(val);
        if step_idx > 0 && val.is_undefined() {
            continue;
        }

        let result =
            eval_path_step_no_group(arena, step, &val, ctx_env, false, keep_singleton_array)?;
        if result.is_undefined() {
            continue;
        }

        // Skip parent binding for step 0 when it is $ or $$
        // (root references don't have a parent context).
        let skip_parent = step_idx == 0
            && matches!(
                arena.get(step),
                Expr::Variable { name, .. } if name.is_empty() || name == "$"
            );

        // Get index var and focus var from this step.
        let (index_var, focus_var) = get_step_bindings(arena, step);
        let is_join = focus_var.is_some();

        // Flatten the result into individual (value, env) contexts.
        let items = flatten_to_vec(result);

        for (j, elem) in items.iter().enumerate() {
            let child_env = Environment::new_child(Rc::clone(ctx_env));
            // Bind parent context (for % operator), unless this is a root step.
            if !skip_parent {
                child_env.bind(PARENT_BINDING, val.clone());
                if is_join {
                    child_env.bind(JOIN_FLAG, Value::Bool(true));
                }
            }
            // Bind index variable if present.
            if let Some(ref var_name) = index_var {
                child_env.bind(var_name.clone(), Value::Number(j as f64));
            }
            // Bind focus variable if present (join @$var).
            if let Some(ref var_name) = focus_var {
                child_env.bind(var_name.clone(), elem.clone());
            }
            // For join steps, the context value stays at parent level.
            let ctx_value = if is_join { val.clone() } else { elem.clone() };
            next_ctxs.push((ctx_value, Rc::new(child_env)));
        }
    }
    Ok(next_ctxs)
}

/// Collapse final tuple contexts into the path result value.
fn collect_tuple_results(ctxs: &[TupleCtx], keep_singleton_array: bool) -> Value {
    let mut seq = Sequence::new();
    for (val, _) in ctxs {
        seq.append(val.clone());
    }
    let result = seq.into_value();
    if keep_singleton_array {
        return match result {
            Value::Array(_) | Value::Undefined => result,
            other => Value::Array(Rc::from(vec![other])),
        };
    }
    result
}

/// Expand a sequence of path steps in tuple mode, preserving parent bindings.
/// Port of Go's `expandPathTuple`.
fn expand_path_tuple(
    arena: &AstArena,
    steps: &[NodeId],
    ctxs: &[(Value, Rc<Environment>)],
) -> Result<Vec<(Value, Rc<Environment>)>, JsonataError> {
    let mut current = ctxs.to_vec();
    for &step in steps {
        let mut next: Vec<(Value, Rc<Environment>)> = Vec::new();
        for (val, ctx_env) in &current {
            let val = collapse_val(val);
            if val.is_undefined() {
                continue;
            }
            let result = eval_path_step(arena, step, &val, ctx_env, false, false)?;
            if result.is_undefined() {
                continue;
            }
            let (index_var, focus_var) = get_step_bindings(arena, step);
            let is_join = focus_var.is_some();
            let items = flatten_to_vec(result);
            for (j, elem) in items.iter().enumerate() {
                let child_env = Environment::new_child(Rc::clone(ctx_env));
                child_env.bind(PARENT_BINDING, val.clone());
                if is_join {
                    child_env.bind(JOIN_FLAG, Value::Bool(true));
                }
                if let Some(ref var_name) = index_var {
                    child_env.bind(var_name.clone(), Value::Number(j as f64));
                }
                if let Some(ref var_name) = focus_var {
                    child_env.bind(var_name.clone(), elem.clone());
                }
                let ctx_value = if is_join { val.clone() } else { elem.clone() };
                next.push((ctx_value, Rc::new(child_env)));
            }
        }
        current = next;
        if current.is_empty() {
            return Ok(vec![]);
        }
    }
    Ok(current)
}

/// Evaluate a join-filter step: walk ctxs, evaluate left_node against each context,
/// bind focus_var (and optionally index_var) in a child env, then keep only contexts
/// whose predicate evaluates to true. Port of Go's evalJoinFilter.
fn eval_join_filter(
    arena: &AstArena,
    ctxs: &[(Value, Rc<Environment>)],
    mut dst: Vec<(Value, Rc<Environment>)>,
    left_node: NodeId,
    predicate: NodeId,
    focus_var: &str,
    index_var: Option<&String>,
) -> Result<Vec<(Value, Rc<Environment>)>, JsonataError> {
    for (val, ctx_env) in ctxs {
        let val = collapse_val(val);
        if val.is_undefined() {
            continue;
        }
        let left_result = eval_path_step(arena, left_node, &val, ctx_env, false, false)?;
        if left_result.is_undefined() {
            continue;
        }
        let items = flatten_to_vec(left_result);
        for (j, item) in items.iter().enumerate() {
            let child_env = Environment::new_child(Rc::clone(ctx_env));
            child_env.bind(PARENT_BINDING, val.clone());
            child_env.bind(JOIN_FLAG, Value::Bool(true));
            child_env.bind(focus_var, item.clone());
            if let Some(idx_name) = index_var {
                child_env.bind(idx_name.clone(), Value::Number(j as f64));
            }
            let child_rc = Rc::new(child_env);
            let pred_result = eval_no_stack_check(arena, predicate, item, &child_rc)?;
            if pred_result.to_boolean() {
                dst.push((val.clone(), child_rc));
            }
        }
    }
    Ok(dst)
}

/// Collapse a value (unwrap Sequence), returning the inner value.
pub(super) fn collapse_val(val: &Value) -> Value {
    match val {
        Value::Sequence(seq) => seq.collapse(),
        other => other.clone(),
    }
}

/// Flatten a result value into a Vec of individual items.
pub(super) fn flatten_to_vec(val: Value) -> Vec<Value> {
    match val {
        Value::Array(a) => a.to_vec(),
        Value::Sequence(s) => {
            let collapsed = s.into_value();
            match collapsed {
                Value::Array(a) => a.to_vec(),
                Value::Undefined => vec![],
                other => vec![other],
            }
        }
        other => vec![other],
    }
}

/// Extract index and focus variable names from a path step.
pub(super) fn get_step_bindings(
    arena: &AstArena,
    step: NodeId,
) -> (Option<String>, Option<String>) {
    match arena.get(step) {
        Expr::Name { index, focus, .. } => (index.clone(), focus.clone()),
        Expr::Variable { index, focus, .. } => (index.clone(), focus.clone()),
        Expr::Sort { index, focus, .. } => (index.clone(), focus.clone()),
        Expr::Block { index, focus, .. } => (index.clone(), focus.clone()),
        Expr::Binary {
            op,
            lhs,
            index,
            focus,
            ..
        } if *op == BinaryOp::Subscript => {
            // If the Binary node itself has index/focus (e.g. from `#$var` after `]`), use those.
            // Otherwise look at the lhs.
            let mut idx = index.clone();
            let mut foc = focus.clone();
            let lhs = *lhs;
            match arena.get(lhs) {
                Expr::Name {
                    index: li,
                    focus: lf,
                    ..
                }
                | Expr::Variable {
                    index: li,
                    focus: lf,
                    ..
                }
                | Expr::Sort {
                    index: li,
                    focus: lf,
                    ..
                } => {
                    if idx.is_none() {
                        idx.clone_from(li);
                    }
                    if foc.is_none() {
                        foc.clone_from(lf);
                    }
                }
                Expr::Binary {
                    index: li,
                    focus: lf,
                    ..
                } => {
                    if idx.is_none() {
                        idx.clone_from(li);
                    }
                    if foc.is_none() {
                        foc.clone_from(lf);
                    }
                }
                _ => {}
            }
            (idx, foc)
        }
        Expr::Binary { index, focus, .. } => (index.clone(), focus.clone()),
        _ => (None, None),
    }
}

/// Shallow equality check for Values (used for join flag parent comparison).
/// This approximates Go's pointer equality by checking structural equality.
pub(super) fn value_ptr_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => Rc::ptr_eq(a, b),
        (Value::Array(a), Value::Array(b)) => Rc::ptr_eq(a, b),
        (Value::String(a), Value::String(b)) => a == b,
        _ => a == b,
    }
}

/// Simple (non-tuple) path evaluation: thread each step's result into the next.
fn eval_path_simple(
    arena: &AstArena,
    steps: &[NodeId],
    keep_singleton_array: bool,
    input: &Value,
    env: &Rc<Environment>,
) -> JsonataResult {
    let mut result = input.clone();
    let mut prev_was_mapper = false;

    for (i, &step) in steps.iter().enumerate() {
        if i > 0 && result.is_undefined() {
            return Ok(Value::Undefined);
        }
        // Collapse sequences between steps.
        if let Value::Sequence(seq) = result {
            result = seq.into_value();
            if i > 0 && result.is_undefined() {
                return Ok(Value::Undefined);
            }
        }

        result = eval_path_step(
            arena,
            step,
            &result,
            env,
            prev_was_mapper,
            keep_singleton_array,
        )?;

        // Empty arrays produced by auto-mapping mean "nothing found" → undefined,
        // UNLESS the previous step was NOT a mapper (the empty array is a genuine field value).
        if let Value::Array(ref arr) = result
            && arr.is_empty()
            && prev_was_mapper
            && !matches!(arena.get(step), Expr::Name { .. } | Expr::StringLit { .. })
        {
            return Ok(Value::Undefined);
        }

        prev_was_mapper = matches!(result, Value::Array(_) | Value::Sequence(_));
    }

    if keep_singleton_array {
        match result {
            // A `**` last step hands back an un-collapsed sequence; collapsing
            // it here is what keeps `x.**[]` a flat `[{…}, 1]` instead of the
            // sequence re-wrapped whole (jsntrs-p0v.20).
            Value::Sequence(seq) => return Ok(seq.collapse_and_keep(true)),
            Value::Array(_) => return Ok(result),
            Value::Undefined => return Ok(Value::Undefined),
            _ => return Ok(Value::Array(Rc::from(vec![result]))),
        }
    }
    Ok(result)
}

/// Evaluate a single path step, handling auto-mapping over arrays.
pub(super) fn eval_path_step(
    arena: &AstArena,
    step: NodeId,
    input: &Value,
    env: &Rc<Environment>,
    prev_was_mapper: bool,
    keep_singleton_array: bool,
) -> JsonataResult {
    let expr = arena.get(step);

    // Steps that natively handle array inputs (field lookup, wildcard, variable, etc.)
    match expr {
        Expr::NumberLit { raw, .. } => {
            return Err(JsonataError::new(
                "S0213",
                format!("invalid step in path: numeric literal {raw} is not a field name"),
            ));
        }
        Expr::Name { .. }
        | Expr::Wildcard { .. }
        | Expr::Variable { .. }
        | Expr::StringLit { .. }
        | Expr::ValueLit { .. }
        | Expr::Sort { .. } => {
            return eval_no_stack_check(arena, step, input, env);
        }
        Expr::Block { .. } if !prev_was_mapper => {
            return eval_no_stack_check(arena, step, input, env);
        }
        Expr::Descendant { .. } => {
            return Ok(eval_descendant_step(input));
        }
        Expr::Binary { op, lhs, .. }
            if *op == BinaryOp::Subscript && !lhs.is_empty() && !prev_was_mapper =>
        {
            return eval_no_stack_check(arena, step, input, env);
        }
        _ => {}
    }

    // Array constructor steps not preceded by mapper are literal expressions.
    if let Expr::Unary { op, .. } = expr
        && *op == UnaryOp::ArrayCons
        && !prev_was_mapper
    {
        return eval_no_stack_check(arena, step, input, env);
    }

    // For all other step types, map over array input.
    let arr = if let Value::Array(a) = input {
        a.clone()
    } else {
        // Single item — check for function step with path-element prepend.
        if matches!(expr, Expr::Function { .. }) {
            return eval_path_function_step(arena, step, input, env);
        }
        return eval_no_stack_check(arena, step, input, env);
    };

    let is_group_step = matches!(expr, Expr::Unary { op, .. } if *op == UnaryOp::ArrayCons);

    // Lifted dispatch: analyze the step once, execute N times.
    // In .() path mapping, there's no explicit param — fields are resolved from scope.
    if !is_group_step
        && let Some(mc) = crate::stdlib::hof_fast::analyze_mapped_call(step, arena, None, env)
    {
        return eval_mapped_call_step(&mc, &arr, env, arena);
    }

    let mut seq = Sequence::with_capacity(arr.len());

    for (i, item) in arr.iter().enumerate() {
        env.poll_cancelled(i)?;
        let val = if matches!(arena.get(step), Expr::Function { .. }) {
            eval_path_function_step(arena, step, item, env)?
        } else {
            eval_no_stack_check(arena, step, item, env)?
        };
        if val.is_undefined() {
            continue;
        }
        if is_group_step {
            seq.values.push(val);
            continue;
        }
        match val {
            Value::Array(inner) => seq.values.extend(inner.iter().cloned()),
            Value::Sequence(s) => seq.values.extend(s.values),
            other => seq.append(other),
        }
    }

    if seq.values.is_empty() {
        return Ok(Value::Undefined);
    }
    if is_group_step && keep_singleton_array {
        return Ok(Value::Array(Rc::from(seq.values)));
    }
    Ok(seq.into_value())
}

/// Descendant (`**`): the current node itself, then every recursive
/// descendant — the root is skipped only for an array input, whose members
/// are collected instead (jsonata-js `recurseDescendants` pushes a non-array
/// input before iterating).
///
/// Shared with the standalone `Expr::Descendant` evaluator: jsonata-js runs
/// one `evaluateDescendants` for both positions, so `**` and `x.**` must
/// agree on including the context node (jsntrs-p0v.22).
pub(super) fn eval_descendant_step(input: &Value) -> Value {
    let mut seq = Sequence::new();
    if !matches!(input, Value::Array(_)) {
        seq.append(input.clone());
    }
    match descendant_lookup(input) {
        Value::Array(arr) => {
            for item in arr.iter() {
                seq.append(item.clone());
            }
        }
        Value::Sequence(s) => {
            for item in s.values {
                seq.append(item);
            }
        }
        Value::Undefined => {}
        other => seq.append(other),
    }
    if seq.values.is_empty() {
        Value::Undefined
    } else {
        Value::Sequence(Box::new(seq))
    }
}

/// Run a lifted mapped call (analyzed once) over each array element,
/// flattening results path-style.
fn eval_mapped_call_step(
    mc: &crate::stdlib::hof_fast::MappedCall,
    arr: &[Value],
    env: &Rc<Environment>,
    arena: &AstArena,
) -> JsonataResult {
    let mut seq = Sequence::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        env.poll_cancelled(i)?;
        let val = crate::stdlib::hof_fast::exec_mapped_call(mc, item, env, arena)?;
        if val.is_undefined() {
            continue;
        }
        match val {
            Value::Array(inner) => seq.values.extend(inner.iter().cloned()),
            Value::Sequence(s) => seq.values.extend(s.values),
            other => seq.append(other),
        }
    }
    if seq.values.is_empty() {
        return Ok(Value::Undefined);
    }
    Ok(seq.into_value())
}

/// Evaluate a function call step in path context.
/// For lambdas, prepend the path element as the first argument.
///
/// A bare function path step (`items.$uppercase(name)`) is a *call site*, so
/// it owes a `SignedBuiltin` callee the same signature validation and
/// coercion that [`eval_function`] performs for every other call site — the
/// reference implementation validates in `apply()`, which every invocation
/// funnels through. Skipping it let `items.$uppercase(name, 1)` return
/// `"ALICE"` instead of raising `T0410` (jsntrs-p0v.7).
fn eval_path_function_step(
    arena: &AstArena,
    step: NodeId,
    item: &Value,
    env: &Rc<Environment>,
) -> JsonataResult {
    let (procedure, arguments) = match arena.get(step) {
        Expr::Function {
            procedure,
            arguments,
            ..
        } => (*procedure, arguments.clone()),
        _ => return eval_no_stack_check(arena, step, item, env),
    };

    let fn_val = eval_no_stack_check(arena, procedure, item, env)?;
    let func = match &fn_val {
        Value::Function(f) => f.clone(),
        _ => {
            return Err(JsonataError::new(
                "T1006",
                "attempted to invoke undefined function",
            ));
        }
    };

    let mut args = Vec::with_capacity(arguments.len());
    for &arg_node in &arguments {
        if matches!(arena.get(arg_node), Expr::Placeholder { .. }) {
            args.push(Value::Undefined);
            continue;
        }
        args.push(super::eval_operand(arena, arg_node, item, env)?);
    }

    // For lambdas, prepend path element when fewer args than params.
    if let FunctionValue::Lambda(ref lam) = *func
        && args.len() < lam.params.len()
    {
        args.insert(0, item.clone());
    }

    // Same signature gate as `eval_function` (a lambda never reaches it —
    // `call_function` validates a lambda's own signature further in).
    if let FunctionValue::SignedBuiltin { signature, .. } = &*func {
        let (coerced, return_undefined) = super::process_call_args(signature, &args)?;
        if return_undefined {
            return Ok(Value::Undefined);
        }
        if let Some(coerced) = coerced {
            args = coerced;
        }
    }

    call_function(&func, &args, item, env, arena)
}
