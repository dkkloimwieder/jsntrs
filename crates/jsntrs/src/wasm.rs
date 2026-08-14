//! WebAssembly entry points via wasm-bindgen.
//!
//! Exports mirror the Go gnata WASM API, with the `jsntrs` prefix:
//!   jsntrsEval(expr, jsonData) → string (JSON) or throws
//!   jsntrsCompile(expr) → handle (number) or throws
//!   jsntrsEvalHandle(handle, jsonData) → string (JSON) or throws
//!   jsntrsReleaseHandle(handle)

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use wasm_bindgen::prelude::*;

use crate::expression::Expression;
use crate::value::Value;

/// Maximum number of compiled expressions kept in the source-string cache.
///
/// Each entry holds a full AST arena (typically a few KB), so this bounds
/// the cache to low single-digit MB while still covering any realistic
/// working set of distinct expressions in a long-running instance. At the
/// cap, the oldest entry is evicted.
const EXPR_CACHE_CAP: usize = 256;

thread_local! {
    static COMPILED: RefCell<HashMap<u32, Expression>> = RefCell::new(HashMap::new());
    static EXPR_CACHE: RefCell<ExprCache> = RefCell::new(ExprCache::new());
    static NEXT_HANDLE: RefCell<u32> = const { RefCell::new(0) };
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn console_error(msg: &str);
}

/// Module initializer, run automatically by wasm-bindgen at instantiation.
///
/// Installs a panic hook that reports the panic message and location to
/// `console.error` — without it, a Rust panic surfaces in JS only as an
/// opaque `RuntimeError: unreachable`. Hand-rolled rather than pulling in
/// the `console_error_panic_hook` crate for a single function.
#[wasm_bindgen(start)]
pub fn init() {
    std::panic::set_hook(Box::new(|info| {
        console_error(&format!("jsntrs (wasm) panicked: {info}"));
    }));
}

/// FIFO-bounded map from expression source to its compiled form, so
/// long-running instances calling `jsntrsEval` with many distinct
/// expressions cannot grow memory without bound.
struct ExprCache {
    map: HashMap<String, Expression>,
    order: VecDeque<String>,
}

impl ExprCache {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&self, expr: &str) -> Option<&Expression> {
        self.map.get(expr)
    }

    fn insert(&mut self, expr: String, compiled: Expression) {
        if self.map.len() >= EXPR_CACHE_CAP
            && let Some(oldest) = self.order.pop_front()
        {
            self.map.remove(&oldest);
        }
        if self.map.insert(expr.clone(), compiled).is_none() {
            self.order.push_back(expr);
        }
    }
}

/// Compile and evaluate a JSONata expression against JSON data.
/// Returns the result as a JSON string, or empty string for undefined.
#[wasm_bindgen(js_name = jsntrsEval)]
pub fn eval(expr: &str, json_data: &str) -> Result<String, JsError> {
    // Check cache first
    let cached_result = EXPR_CACHE.with(|cache| {
        let cache = cache.borrow();
        cache.get(expr).map(|e| eval_expression(e, json_data))
    });

    if let Some(result) = cached_result {
        return result;
    }

    let compiled = Expression::compile(expr).map_err(|e| JsError::new(&e.to_string()))?;
    let result = eval_expression(&compiled, json_data);

    EXPR_CACHE.with(|cache| {
        cache.borrow_mut().insert(expr.to_string(), compiled);
    });

    result
}

/// Compile a JSONata expression and return a numeric handle.
#[wasm_bindgen(js_name = jsntrsCompile)]
pub fn compile(expr: &str) -> Result<u32, JsError> {
    let compiled = Expression::compile(expr).map_err(|e| JsError::new(&e.to_string()))?;

    COMPILED.with(|cache| {
        let mut cache = cache.borrow_mut();
        // After u32 wrap-around a fresh counter value could collide with a
        // still-live handle and silently replace its expression; skip past
        // live handles (and 0, so handles stay non-zero). Terminates while
        // fewer than u32::MAX handles are live.
        let handle = NEXT_HANDLE.with(|h| {
            let mut h = h.borrow_mut();
            loop {
                *h = h.wrapping_add(1);
                if *h != 0 && !cache.contains_key(&*h) {
                    break *h;
                }
            }
        });
        cache.insert(handle, compiled);
        Ok(handle)
    })
}

/// Evaluate a previously compiled expression (by handle) against JSON data.
#[wasm_bindgen(js_name = jsntrsEvalHandle)]
pub fn eval_handle(handle: u32, json_data: &str) -> Result<String, JsError> {
    COMPILED.with(|cache| {
        let cache = cache.borrow();
        let expr = cache
            .get(&handle)
            .ok_or_else(|| JsError::new(&format!("unknown handle {handle}")))?;
        eval_expression(expr, json_data)
    })
}

/// Release a compiled expression handle.
#[wasm_bindgen(js_name = jsntrsReleaseHandle)]
pub fn release_handle(handle: u32) {
    COMPILED.with(|cache| {
        cache.borrow_mut().remove(&handle);
    });
}

/// Format a JSONata expression with consistent indentation and line breaks.
#[wasm_bindgen(js_name = jsntrsFormat)]
pub fn format(expr: &str) -> Result<String, JsError> {
    crate::formatter::format(expr).map_err(|e| JsError::new(&e.to_string()))
}

/// Tokenize a JSONata expression for syntax highlighting.
/// Returns JSON: `[[start, end, "type"], ...]`
#[wasm_bindgen(js_name = jsntrsHighlight)]
pub fn highlight(expr: &str) -> Result<String, JsError> {
    crate::highlight::highlight(expr).map_err(|e| JsError::new(&e.to_string()))
}

fn eval_expression(expr: &Expression, json_data: &str) -> Result<String, JsError> {
    let result = if json_data.is_empty() || json_data == "null" {
        expr.evaluate_value(&Value::Undefined)
    } else {
        expr.evaluate(json_data)
    }
    .map_err(|e| JsError::new(&e.to_string()))?;

    if result.is_undefined() {
        return Ok(String::new());
    }

    Ok(result.to_json_string())
}
