//! Evaluation environment: lexical scoping chain with shared call counter.
//!
//! Port of Go `internal/evaluator/env.go`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use compact_str::CompactString;

use crate::error::JsonataError;
use crate::value::Value;

/// Variable bindings keyed by name. foldhash, like `ObjectMap`: binding
/// names are short and lookup sits on the evaluator's hot path
/// (M-FAST-HASHER).
type BindingsMap = HashMap<CompactString, Value, foldhash::fast::RandomState>;

/// Maximum recursive call depth before U1001.
/// Matches the JSONata reference implementation's default.
pub const DEFAULT_MAX_CALL_DEPTH: u32 = 100;

/// Shared recursive call depth counter across all child environments.
#[derive(Debug)]
pub struct CallCounter {
    pub depth: Cell<u32>,
    pub eval_depth: Cell<u32>,
    pub max: u32,
    /// Closure environments created during evaluation. A lambda bound into
    /// its own defining scope forms an `Rc` cycle (env -> binding ->
    /// lambda -> closure env) that would leak; the public API boundary
    /// drains this list and breaks surviving cycles. Shared chain-wide,
    /// like the counters.
    pub(crate) closure_envs: RefCell<Vec<std::rc::Weak<Environment>>>,
}

impl Default for CallCounter {
    fn default() -> Self {
        Self::new()
    }
}

impl CallCounter {
    pub fn new() -> Self {
        Self {
            depth: Cell::new(0),
            eval_depth: Cell::new(0),
            max: DEFAULT_MAX_CALL_DEPTH,
            closure_envs: RefCell::new(Vec::new()),
        }
    }
}

/// Evaluation environment holding variable bindings with lexical scoping.
///
/// Forms a linked chain via `parent`. All environments in a chain share
/// the same `CallCounter` (via Rc) and cancellation token (via Arc).
/// Bindings use `RefCell` for interior mutability, allowing `bind()` through `Rc`.
///
/// Non-local lookups are memoized in a per-environment cache so that
/// repeated references to the same variable (e.g. a builtin like `$sum`
/// inside a HOF callback) only walk the parent chain once.
#[derive(Debug)]
pub struct Environment {
    parent: Option<Rc<Environment>>,
    bindings: RefCell<BindingsMap>,
    /// Lazy cache of non-local lookups. Populated on first parent-chain hit.
    /// Vec-based for cache-line friendliness at typical sizes (0-5 entries).
    cache: RefCell<Vec<(CompactString, Value)>>,
    /// Chain-shared bind counter: every `bind` anywhere in the chain bumps
    /// it, and `lookup` discards a cache filled at an older generation —
    /// so an ancestor rebind can never serve a stale cached value.
    generation: Rc<Cell<u64>>,
    /// The generation this environment's cache was filled at.
    cache_gen: Cell<u64>,
    calls: Rc<CallCounter>,
    cancel: Option<Arc<AtomicBool>>,
}

impl Environment {
    /// Create a root environment with no bindings.
    pub fn new() -> Self {
        Self {
            parent: None,
            bindings: RefCell::new(BindingsMap::default()),
            cache: RefCell::new(Vec::new()),
            generation: Rc::new(Cell::new(0)),
            cache_gen: Cell::new(0),
            calls: Rc::new(CallCounter::new()),
            cancel: None,
        }
    }

    /// Pre-size the bindings map for a known number of upcoming binds
    /// (M-INITIAL-CAPACITY). Used by `stdlib::register_all`, which inserts
    /// the full builtin registry in one burst.
    pub(crate) fn reserve_bindings(&self, additional: usize) {
        self.bindings.borrow_mut().reserve(additional);
    }

    /// Child scope for one public evaluation on the thread-cached stdlib
    /// root.
    ///
    /// Unlike [`Environment::new_child`], the call counter is FRESH: call
    /// depth, `$eval` depth, and the closure-env suspects list drained by
    /// [`Environment::teardown_cycles`] are per-evaluation state and must
    /// not accumulate on the shared root. The generation chain is inherited
    /// (caches snapshot it at creation; the root is set up once and never
    /// bound into afterwards), and the cancel slot starts empty so each
    /// evaluation carries its own token.
    pub(crate) fn new_eval_child(parent: Rc<Environment>) -> Self {
        let generation = Rc::clone(&parent.generation);
        let cache_gen = Cell::new(generation.get());
        Self {
            parent: Some(parent),
            bindings: RefCell::new(BindingsMap::default()),
            cache: RefCell::new(Vec::new()),
            generation,
            cache_gen,
            calls: Rc::new(CallCounter::new()),
            cancel: None,
        }
    }

    /// Create a child scope inheriting from parent.
    pub fn new_child(parent: Rc<Environment>) -> Self {
        let calls = Rc::clone(&parent.calls);
        let cancel = parent.cancel.clone();
        let generation = Rc::clone(&parent.generation);
        let cache_gen = Cell::new(generation.get());
        Self {
            parent: Some(parent),
            bindings: RefCell::new(BindingsMap::default()),
            cache: RefCell::new(Vec::new()),
            generation,
            cache_gen,
            calls,
            cancel,
        }
    }

    /// Set a variable in this environment.
    /// Uses interior mutability so this works through `Rc<Environment>`.
    pub fn bind(&self, name: impl Into<CompactString>, value: Value) {
        self.bindings.borrow_mut().insert(name.into(), value);
        self.generation.set(self.generation.get().wrapping_add(1));
    }

    /// Look up a variable, walking the parent chain iteratively.
    /// Non-local results are cached so repeated lookups of the same name
    /// from this environment are O(1) after the first hit.
    pub fn lookup(&self, name: &str) -> Option<Value> {
        // 1. Check local bindings
        if let Some(v) = self.bindings.borrow().get(name) {
            return Some(v.clone());
        }
        // 2. Check cache (only for non-local lookups); a bind anywhere in
        // the chain since the cache was filled invalidates it wholesale.
        let generation = self.generation.get();
        if self.cache_gen.get() == generation {
            for (k, v) in &*self.cache.borrow() {
                if k == name {
                    return Some(v.clone());
                }
            }
        } else {
            self.cache.borrow_mut().clear();
            self.cache_gen.set(generation);
        }
        // 3. Walk parent chain iteratively
        let mut current = self.parent.as_ref();
        while let Some(env) = current {
            if let Some(v) = env.bindings.borrow().get(name) {
                let result = v.clone();
                self.cache
                    .borrow_mut()
                    .push((CompactString::from(name), result.clone()));
                return Some(result);
            }
            current = env.parent.as_ref();
        }
        None
    }

    /// Look up a variable and return both the value and the environment that holds it.
    /// Used by the parent operator (%) to walk the env chain for chained %.% navigation.
    /// The `self_rc` parameter must be the `Rc` wrapping this environment.
    pub(crate) fn lookup_with_env(
        self_rc: &Rc<Environment>,
        name: &str,
    ) -> Option<(Value, Rc<Environment>)> {
        let mut current = self_rc;
        loop {
            if let Some(v) = current.bindings.borrow().get(name) {
                return Some((v.clone(), Rc::clone(current)));
            }
            match &current.parent {
                Some(parent) => current = parent,
                None => return None,
            }
        }
    }

    /// Check only the direct bindings (no parent chain walk).
    pub(crate) fn lookup_direct(&self, name: &str) -> Option<Value> {
        self.bindings.borrow().get(name).cloned()
    }

    /// Get the parent environment.
    pub(crate) fn parent(&self) -> Option<&Rc<Environment>> {
        self.parent.as_ref()
    }

    /// Get a reference to the shared call counter.
    pub(crate) fn call_counter(&self) -> &Rc<CallCounter> {
        &self.calls
    }

    /// Increment the $eval nesting counter.
    ///
    /// # Errors
    /// Returns `D3121` if nesting depth exceeds `max_depth`.
    pub(crate) fn incr_eval_depth(&self, max_depth: u32) -> Result<(), JsonataError> {
        let d = self.calls.eval_depth.get() + 1;
        if d > max_depth {
            return Err(JsonataError::new(
                "D3121",
                "$eval: maximum nesting depth exceeded",
            ));
        }
        self.calls.eval_depth.set(d);
        Ok(())
    }

    /// Decrement the $eval nesting counter.
    pub(crate) fn decr_eval_depth(&self) {
        let d = self.calls.eval_depth.get();
        if d > 0 {
            self.calls.eval_depth.set(d - 1);
        }
    }

    /// Set the cancellation token.
    pub(crate) fn set_cancel(&mut self, cancel: Arc<AtomicBool>) {
        self.cancel = Some(cancel);
    }

    /// Check if evaluation has been cancelled.
    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    }

    /// Poll the cancellation flag from a hot loop, checking every 1024th
    /// iteration (`i` is the loop index) to keep the per-item cost to a
    /// branch. Function-free loops (fast HOF paths, auto-map, predicate
    /// filters) call this so they stay cancellable without going through
    /// a call boundary.
    ///
    /// # Errors
    /// Returns `D3001` when evaluation has been cancelled.
    #[inline]
    pub(crate) fn poll_cancelled(&self, i: usize) -> Result<(), JsonataError> {
        if i.is_multiple_of(1024) && self.is_cancelled() {
            return Err(JsonataError::new("D3001", "evaluation cancelled"));
        }
        Ok(())
    }

    /// Record `closure_env` as a potential cycle participant.
    ///
    /// Called when a lambda captures its defining environment; see
    /// `CallCounter::closure_envs`.
    pub(crate) fn note_closure_env(&self, closure_env: &Rc<Environment>) {
        let mut suspects = self.calls.closure_envs.borrow_mut();
        // Repeated lambdas in the same scope register once.
        if let Some(last) = suspects.last()
            && let Some(last) = last.upgrade()
            && Rc::ptr_eq(&last, closure_env)
        {
            return;
        }
        suspects.push(Rc::downgrade(closure_env));
    }

    /// Break `Rc` cycles left behind by lambdas bound into their own
    /// scope chain. Called at the public API boundary, after evaluation
    /// has produced its (fully materialized) result: any closure env
    /// still alive is only reachable through such a cycle — or through a
    /// `Value::Function` in the result, which the public API cannot call.
    pub(crate) fn teardown_cycles(&self) {
        let suspects = std::mem::take(&mut *self.calls.closure_envs.borrow_mut());
        for weak in suspects {
            if let Some(env) = weak.upgrade() {
                env.bindings.borrow_mut().clear();
                env.cache.borrow_mut().clear();
            }
        }
    }

    /// Iterate over direct bindings (no parent chain walk).
    /// Calls the provided closure with each (name, value) pair.
    pub(crate) fn for_each_direct<F: FnMut(&str, &Value)>(&self, mut f: F) {
        for (k, v) in self.bindings.borrow().iter() {
            f(k, v);
        }
    }

    /// Clone this environment (shallow copy of bindings, shared parent/calls).
    #[must_use]
    pub(crate) fn shallow_clone(&self) -> Self {
        Self {
            parent: self.parent.clone(),
            bindings: RefCell::new(self.bindings.borrow().clone()),
            cache: RefCell::new(Vec::new()),
            generation: Rc::clone(&self.generation),
            cache_gen: Cell::new(self.generation.get()),
            calls: Rc::clone(&self.calls),
            cancel: self.cancel.clone(),
        }
    }
}

impl Default for Environment {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_lookup_missing() {
        let env = Environment::new();
        assert!(env.lookup("x").is_none());
    }

    #[test]
    fn bind_and_lookup() {
        let env = Environment::new();
        env.bind("x", Value::Number(42.0));
        assert_eq!(env.lookup("x"), Some(Value::Number(42.0)));
    }

    /// A rebind anywhere in the chain invalidates descendants' lookup
    /// caches — the second lookup used to serve the stale cached value
    /// (gnata-eci.7).
    #[test]
    fn ancestor_rebind_invalidates_descendant_cache() {
        let root = Rc::new({
            let e = Environment::new();
            e.bind("x", Value::Number(1.0));
            e
        });
        let child = Rc::new(Environment::new_child(Rc::clone(&root)));
        assert_eq!(child.lookup("x"), Some(Value::Number(1.0)));
        root.bind("x", Value::Number(2.0));
        assert_eq!(child.lookup("x"), Some(Value::Number(2.0)));

        // A fresh shadow in an intermediate scope also wins over a
        // grandparent value the grandchild had cached.
        let grandchild = Rc::new(Environment::new_child(Rc::clone(&child)));
        assert_eq!(grandchild.lookup("x"), Some(Value::Number(2.0)));
        child.bind("x", Value::Number(3.0));
        assert_eq!(grandchild.lookup("x"), Some(Value::Number(3.0)));
    }

    #[test]
    fn child_inherits_parent() {
        let root = Environment::new();
        root.bind("x", Value::Number(1.0));
        let parent = Rc::new(root);
        let child = Environment::new_child(Rc::clone(&parent));
        assert_eq!(child.lookup("x"), Some(Value::Number(1.0)));
    }

    #[test]
    fn child_shadows_parent() {
        let root = Environment::new();
        root.bind("x", Value::Number(1.0));
        let parent = Rc::new(root);
        let child = Environment::new_child(Rc::clone(&parent));
        child.bind("x", Value::Number(2.0));
        assert_eq!(child.lookup("x"), Some(Value::Number(2.0)));
    }

    #[test]
    fn shared_call_counter() {
        let root = Environment::new();
        let parent = Rc::new(root);
        let child = Environment::new_child(Rc::clone(&parent));
        parent.calls.depth.set(5);
        assert_eq!(child.calls.depth.get(), 5);
    }

    #[test]
    fn lookup_direct_no_parent() {
        let root = Environment::new();
        root.bind("x", Value::Number(1.0));
        let parent = Rc::new(root);
        let child = Environment::new_child(parent);
        assert!(child.lookup_direct("x").is_none());
    }

    #[test]
    fn cancellation() {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut env = Environment::new();
        env.set_cancel(Arc::clone(&cancel));
        assert!(!env.is_cancelled());
        cancel.store(true, Ordering::Relaxed);
        assert!(env.is_cancelled());
    }

    #[test]
    fn bind_through_rc() {
        let env = Rc::new(Environment::new());
        env.bind("x", Value::Number(42.0));
        assert_eq!(env.lookup("x"), Some(Value::Number(42.0)));
    }

    #[test]
    fn lookup_cache_populated_on_parent_hit() {
        let root = Environment::new();
        root.bind("builtin", Value::Number(99.0));
        let e1 = Rc::new(root);
        let e2 = Rc::new(Environment::new_child(Rc::clone(&e1)));
        let child = Environment::new_child(Rc::clone(&e2));

        // First lookup walks the chain (depth 2)
        assert_eq!(child.lookup("builtin"), Some(Value::Number(99.0)));
        // Cache should now have the entry
        assert_eq!(child.cache.borrow().len(), 1);
        assert_eq!(child.cache.borrow()[0].0, "builtin");
        // Second lookup hits the cache (no chain walk)
        assert_eq!(child.lookup("builtin"), Some(Value::Number(99.0)));
    }

    #[test]
    fn lookup_cache_not_used_for_local() {
        let root = Environment::new();
        root.bind("x", Value::Number(1.0));
        let env = Rc::new(root);
        let child = Environment::new_child(env);
        child.bind("x", Value::Number(2.0));

        // Local binding should be returned, not cached parent value
        assert_eq!(child.lookup("x"), Some(Value::Number(2.0)));
        // Cache should be empty (local hit doesn't populate cache)
        assert!(child.cache.borrow().is_empty());
    }

    #[test]
    fn lookup_with_env_iterative() {
        let root = Environment::new();
        root.bind("x", Value::Number(1.0));
        let e1 = Rc::new(root);
        let e2 = Rc::new(Environment::new_child(Rc::clone(&e1)));
        let e3 = Rc::new(Environment::new_child(Rc::clone(&e2)));

        let (val, env) = Environment::lookup_with_env(&e3, "x").unwrap();
        assert_eq!(val, Value::Number(1.0));
        // Should find it in e1 (root)
        assert!(Rc::ptr_eq(&env, &e1));
    }
}
