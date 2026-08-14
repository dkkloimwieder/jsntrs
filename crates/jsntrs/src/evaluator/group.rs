//! Group-by evaluation: `{key: value}` over sequences and tuple streams.
//!
//! Split from `evaluator/mod.rs`; port of Go `internal/evaluator` group-by,
//! including the tuple-group buckets and key/value strategy analysis.

use std::rc::Rc;

use crate::error::{JsonataError, JsonataResult};
use crate::parser::ast::static_flags;
use crate::parser::{AstArena, Expr, NodeId};
use crate::value::Value;

use super::binary::{apply_keep_array, eval_binary};
use super::environment::Environment;
use super::path::{eval_path, subtree_any};
use super::{
    JOIN_FLAG, PARENT_BINDING, eval_function, eval_name, eval_no_stack_check, eval_unary,
    eval_variable,
};

/// Group buckets for `eval_tuple_group`: key → (members, member envs).
type TupleGroups = std::collections::HashMap<
    compact_str::CompactString,
    (Vec<Value>, Vec<Rc<Environment>>),
    foldhash::fast::RandomState,
>;

/// Apply a group-by expression to tuple contexts, using per-element environments.
///
/// JSONata group-by semantics: records are grouped by key, then the value
/// expression is evaluated once per group with the context set to the array
/// of all group members (or a single value when the group has one member).
/// This allows aggregate functions like $join or $sum to operate on the
/// full group rather than individual records.
pub(super) fn eval_tuple_group(
    arena: &AstArena,
    group: &crate::parser::GroupExpr,
    ctxs: &[(Value, Rc<Environment>)],
) -> JsonataResult {
    let mut result_map = crate::value::ObjectMap::default();

    for pair in &group.pairs {
        let key_node = pair[0];
        let val_node = pair[1];

        // Phase 1: group ctxs by key.
        let mut groups = TupleGroups::default();
        let mut key_order: Vec<compact_str::CompactString> = Vec::new();

        for (item, item_env) in ctxs {
            let key_val = eval_no_stack_check(arena, key_node, item, item_env)?;
            // Undefined keys skip the item; null (and any other non-string) is
            // T1003. Same rule as the standard group path in
            // `collect_group_items` — tuple mode is selected by the mere
            // presence of `#$v`/`@$v`/`%` in the path, so it must not change
            // the group semantics (jsonata-js-verified, jsntrs-p0v.1).
            if key_val.is_undefined() {
                continue;
            }
            let key: compact_str::CompactString = match &key_val {
                Value::String(s) => s.clone(),
                _ => {
                    return Err(JsonataError::new(
                        "T1003",
                        format!("key expression must evaluate to a string, got {key_val:?}"),
                    ));
                }
            };
            if let Some(g) = groups.get_mut(key.as_str()) {
                g.0.push(item.clone());
                g.1.push(Rc::clone(item_env));
            } else {
                key_order.push(key.clone());
                groups.insert(key, (vec![item.clone()], vec![Rc::clone(item_env)]));
            }
        }

        // Phase 2: evaluate value expression per group.
        for key in &key_order {
            let (values, envs) = groups.get(key.as_str()).ok_or_else(|| {
                JsonataError::new("D0000", "key from key_order must exist in groups")
            })?;
            let (group_ctx, group_env) = if values.len() == 1 {
                (values[0].clone(), Rc::clone(&envs[0]))
            } else {
                let merged = merge_group_envs(envs);
                (Value::Array(Rc::from(values.clone())), Rc::new(merged))
            };
            let val = if val_node.is_empty() {
                group_ctx
            } else {
                eval_no_stack_check(arena, val_node, &group_ctx, &group_env)?
            };
            if !val.is_undefined() {
                result_map.insert(key.clone(), val);
            }
        }
    }

    // A group over a non-empty tuple stream whose keys (or values) all resolve
    // to undefined yields `{}`, exactly as the standard group path does — an
    // empty tuple stream never reaches here (`eval_path_tuple` returns
    // undefined first). That is deliberately *not* aligned with the standard
    // path's "empty base stands in for one undefined item" rule: jsonata-js
    // reaches the same point with a tuple stream, then dereferences `item['@']`
    // on the pushed `undefined` and throws a raw TypeError (jsntrs-6wr.9).
    Ok(Value::Object(Rc::new(result_map)))
}

/// Merge environments from a group of records.
/// Variables that differ across records are collected into arrays.
fn merge_group_envs(envs: &[Rc<Environment>]) -> Environment {
    if envs.is_empty() {
        return Environment::new();
    }
    if envs.len() == 1 {
        return envs[0].shallow_clone();
    }

    let merged = Environment::new_child(
        envs[0]
            .parent()
            .cloned()
            .unwrap_or_else(|| Rc::new(Environment::new())),
    );

    // Collect variable names from tuple-specific envs (those with %%).
    let mut var_names: Vec<compact_str::CompactString> = Vec::new();
    let mut seen: std::collections::HashSet<
        compact_str::CompactString,
        foldhash::fast::RandomState,
    > = std::collections::HashSet::default();
    for env in envs {
        let mut current: Option<&Rc<Environment>> = Some(env);
        while let Some(e) = current {
            if e.lookup_direct(PARENT_BINDING).is_none() {
                break;
            }
            e.for_each_direct(|name, _| {
                let cs = compact_str::CompactString::from(name);
                if seen.insert(cs.clone()) {
                    var_names.push(cs);
                }
            });
            current = e.parent();
        }
    }

    // For each variable, collect values from each env via full lookup.
    for name in &var_names {
        if name == PARENT_BINDING || name == JOIN_FLAG {
            if let Some(v) = envs[0].lookup(name) {
                merged.bind(name.clone(), v);
            }
            continue;
        }
        let mut vals: Vec<Value> = Vec::new();
        for env in envs {
            if let Some(v) = env.lookup(name) {
                vals.push(v);
            }
        }
        if vals.len() == 1 {
            merged.bind(
                name.clone(),
                vals.into_iter().next().unwrap_or(Value::Undefined),
            );
        } else if !vals.is_empty() {
            // Check if all values are identical.
            let all_same = vals.windows(2).all(|w| w[0] == w[1]);
            if all_same {
                merged.bind(
                    name.clone(),
                    vals.into_iter().next().unwrap_or(Value::Undefined),
                );
            } else {
                merged.bind(name.clone(), Value::Array(Rc::from(vals)));
            }
        }
    }

    merged
}

// ── Group-by expression ({key:val}) ─────────────────────────────────

/// Check whether an AST subtree references `$index` or `$key` variables.
/// When it doesn't, we can skip `Environment::new_child` for group-by pairs.
/// Over-detection merely creates an unneeded child env; missing a use would
/// evaluate the pair without its bindings.
pub(super) fn uses_group_bindings(arena: &AstArena, node: NodeId) -> bool {
    if !node.is_empty()
        && let Some(f) = arena.node_static_flags(node)
    {
        return f & static_flags::GROUP_BINDINGS != 0;
    }
    subtree_any(
        arena,
        node,
        |e| matches!(e, Expr::Variable { name, .. } if name == "index" || name == "key"),
    )
}

/// Fast-path key evaluation strategy, determined once per group pair.
enum KeyStrategy {
    /// key_node is a simple Name — use direct field lookup, no dispatch.
    FieldAccess(String),
    /// key_node requires full evaluation.
    FullEval(NodeId),
}

/// Fast-path value evaluation strategy, determined once per group pair.
enum ValStrategy {
    /// val_node is empty — return group input as-is.
    Identity,
    /// val_node is a simple Name — use direct field lookup.
    FieldAccess(String),
    /// val_node requires full evaluation, but doesn't use $index/$key.
    FullEvalNoBindings(NodeId),
    /// val_node requires full evaluation AND uses $index/$key.
    FullEvalWithBindings(NodeId),
}

// Large dispatch function for group-by evaluation.
pub(super) fn eval_group_by(
    arena: &AstArena,
    node: NodeId,
    input: &Value,
    env: &Rc<Environment>,
) -> JsonataResult {
    // Extract group pairs from the node.
    let group = match arena.get(node) {
        Expr::Name { group: Some(g), .. } => g.clone(),
        Expr::Path { group: Some(g), .. } => g.clone(),
        Expr::Variable { group: Some(g), .. } => g.clone(),
        Expr::Function { group: Some(g), .. } => g.clone(),
        Expr::Binary { group: Some(g), .. } => g.clone(),
        Expr::Unary { group: Some(g), .. } => g.clone(),
        Expr::Grouped { group, .. } => group.clone(),
        _ => return eval_no_stack_check(arena, node, input, env),
    };

    // Evaluate the base expression without the group-by reduction.
    // We dispatch based on node type to avoid recursion back into eval_group_by.
    let base = match arena.get(node) {
        Expr::Name { value, .. } => eval_name(value, input)?,
        Expr::Path { .. } => eval_path(arena, node, input, env)?,
        Expr::Variable { name, .. } => eval_variable(name, input, env),
        Expr::Function { .. } => eval_function(arena, node, input, env)?,
        Expr::Binary { .. } => eval_binary(arena, node, input, env)?,
        Expr::Unary { .. } => eval_unary(arena, node, input, env)?,
        Expr::Grouped { expr, .. } => eval_no_stack_check(arena, *expr, input, env)?,
        _ => eval_no_stack_check(arena, node, input, env)?,
    };
    // Normalize the base to the item list the pairs are grouped over.
    //
    // An undefined or empty base stands in for ONE undefined item rather
    // than for no items at all — `evaluateGroupExpression` does exactly this
    // (`createSequence(input)` on a non-array, then `input.push(undefined)`
    // when the sequence came out empty), and it is what lets a group with
    // literal pairs still produce an object: `Missing{'k': 'v'}` is
    // `{"k": "v"}`, `Missing{'k': $}` is `{}` (the key is defined, the value
    // is not, so only the pair drops). Returning undefined here instead
    // swallowed the object, which is how `items.($string(x){'k': $})` lost
    // the trailing `{}` for an item with no `x` (jsntrs-6wr.9). Note this
    // departs from the Go reference, which propagated the nil base; see
    // docs/spec.md §4.12.1.
    let items: Vec<Value> = match base {
        Value::Undefined => vec![Value::Undefined],
        Value::Array(a) if a.is_empty() => vec![Value::Undefined],
        Value::Array(a) => a.to_vec(),
        Value::Sequence(seq) => match seq.into_value() {
            Value::Undefined => vec![Value::Undefined],
            Value::Array(a) if a.is_empty() => vec![Value::Undefined],
            Value::Array(a) => a.to_vec(),
            other => vec![other],
        },
        other => vec![other],
    };

    let mut out_obj = crate::value::ObjectMap::default();
    let mut key_set: std::collections::HashSet<
        compact_str::CompactString,
        foldhash::fast::RandomState,
    > = std::collections::HashSet::default();

    for pair in &group.pairs {
        let key_node = pair[0];
        let val_node = pair[1];
        let (key_strategy, val_strategy, val_keep_array) =
            analyze_group_pair(arena, key_node, val_node);

        let groups = collect_group_items(arena, &items, &key_strategy, env)?;

        // Iterate groups in insertion order (IndexMap guarantees this).
        for (key, (group_items, first_idx)) in &groups {
            if key_set.contains(key.as_str()) {
                return Err(JsonataError::new(
                    "D1009",
                    format!("duplicate key: \"{key}\""),
                ));
            }
            let group_input = if group_items.len() == 1 {
                group_items[0].clone()
            } else {
                Value::Array(Rc::from(group_items.clone()))
            };

            let mut val_result =
                eval_group_value(arena, &val_strategy, group_input, key, *first_idx, env)?;

            if val_keep_array {
                val_result = apply_keep_array(val_result, Value::Array(Rc::from(vec![])));
            }

            if !val_result.is_undefined() {
                key_set.insert(key.clone());
                out_obj.insert(key.clone(), val_result);
            }
        }
    }

    Ok(Value::Object(Rc::new(out_obj)))
}

/// Analyze a group-by pair once: pick the key/value evaluation strategies and
/// the value's keep-array flag.
fn analyze_group_pair(
    arena: &AstArena,
    key_node: NodeId,
    val_node: NodeId,
) -> (KeyStrategy, ValStrategy, bool) {
    // Simple Name key → direct field lookup.
    let key_strategy = match arena.get(key_node) {
        Expr::Name {
            value,
            stages,
            group: None,
            focus: None,
            index: None,
            ..
        } if stages.is_empty() => KeyStrategy::FieldAccess(value.clone()),
        _ => KeyStrategy::FullEval(key_node),
    };

    // Value expression: determine if we need env bindings.
    let val_strategy = if val_node.is_empty() {
        ValStrategy::Identity
    } else {
        match arena.get(val_node) {
            Expr::Name {
                value,
                stages,
                group: None,
                focus: None,
                index: None,
                ..
            } if stages.is_empty() && !uses_group_bindings(arena, val_node) => {
                ValStrategy::FieldAccess(value.clone())
            }
            _ if uses_group_bindings(arena, val_node) => {
                ValStrategy::FullEvalWithBindings(val_node)
            }
            _ => ValStrategy::FullEvalNoBindings(val_node),
        }
    };

    let val_keep_array = match arena.get(val_node) {
        Expr::Name { keep_array, .. }
        | Expr::Binary { keep_array, .. }
        | Expr::Variable { keep_array, .. }
        | Expr::Function { keep_array, .. }
        | Expr::Sort { keep_array, .. }
        | Expr::Unary { keep_array, .. } => *keep_array,
        Expr::Path {
            keep_singleton_array,
            ..
        } => *keep_singleton_array,
        _ => false,
    };

    (key_strategy, val_strategy, val_keep_array)
}

/// Bucket items by their evaluated key, preserving first-seen order and each
/// group's first item index (for the $index binding).
fn collect_group_items(
    arena: &AstArena,
    items: &[Value],
    key_strategy: &KeyStrategy,
    env: &Rc<Environment>,
) -> JsonataResult<
    indexmap::IndexMap<
        compact_str::CompactString,
        (Vec<Value>, usize),
        foldhash::fast::RandomState,
    >,
> {
    let mut groups: indexmap::IndexMap<
        compact_str::CompactString,
        (Vec<Value>, usize),
        foldhash::fast::RandomState,
    > = indexmap::IndexMap::default();
    for (i, item) in items.iter().enumerate() {
        // Fast-path: direct field lookup for simple Name key expressions.
        let key_val = match key_strategy {
            KeyStrategy::FieldAccess(field) => match item {
                Value::Object(obj) => match obj.get(field.as_str()) {
                    Some(v) => v.clone(),
                    None => Value::Undefined,
                },
                _ => Value::Undefined,
            },
            KeyStrategy::FullEval(node) => eval_no_stack_check(arena, *node, item, env)?,
        };
        // Undefined keys skip the item; null (and any other non-string)
        // is T1003, matching Go (nil continues, jsonNullType errors).
        if key_val.is_undefined() {
            continue;
        }
        let key: compact_str::CompactString = match &key_val {
            Value::String(s) => s.clone(),
            _ => {
                return Err(JsonataError::new(
                    "T1003",
                    format!("key expression must evaluate to a string, got {key_val:?}"),
                ));
            }
        };
        if let Some(entry) = groups.get_mut(key.as_str()) {
            entry.0.push(item.clone());
        } else {
            groups.insert(key, (vec![item.clone()], i));
        }
    }
    Ok(groups)
}

/// Evaluate one group's value using the pre-analyzed strategy.
fn eval_group_value(
    arena: &AstArena,
    val_strategy: &ValStrategy,
    group_input: Value,
    key: &compact_str::CompactString,
    first_idx: usize,
    env: &Rc<Environment>,
) -> JsonataResult {
    match val_strategy {
        ValStrategy::Identity => Ok(group_input),
        ValStrategy::FieldAccess(field) => eval_name(field, &group_input),
        ValStrategy::FullEvalNoBindings(vn) => {
            // No $index/$key used — skip Environment::new_child.
            eval_no_stack_check(arena, *vn, &group_input, env)
        }
        ValStrategy::FullEvalWithBindings(vn) => {
            let child_env = Environment::new_child(Rc::clone(env));
            child_env.bind("index", Value::Number(first_idx as f64));
            child_env.bind("key", Value::String(key.clone()));
            let child_env = Rc::new(child_env);
            eval_no_stack_check(arena, *vn, &group_input, &child_env)
        }
    }
}
