//! A [JSONata](https://jsonata.org) 2.x query and transformation engine.
//!
//! Compile an expression once with [`Expression::compile`], then evaluate it
//! against any number of inputs:
//!
//! ```
//! use jsntrs::Expression;
//!
//! # fn main() -> Result<(), jsntrs::JsonataError> {
//! let expr = Expression::compile("$sum(Order.Product.(Price * Quantity))")?;
//! let result = expr.evaluate(r#"{
//!     "Order": [
//!         {"Product": [{"Price": 34.5, "Quantity": 2}]},
//!         {"Product": [{"Price": 21.5, "Quantity": 1}]}
//!     ]
//! }"#)?;
//! assert_eq!(result.as_f64(), Some(90.5));
//! # Ok(())
//! # }
//! ```
//!
//! # API overview
//!
//! - [`Expression`] — a compiled expression; `Send + Sync` and cheap to
//!   clone, so compile once and share across threads.
//! - [`Value`] — the JSON-plus-`undefined` value type results come in.
//!   Deliberately `!Send` (reference-counted, copy-on-write); build inputs
//!   per thread.
//! - [`JsonataError`] / [`JsonataResult`] — structured errors carrying
//!   JSONata spec codes, and the `Result` alias every API returns.
//! - [`new_custom_env`] / [`CustomFunc`] — register Rust functions callable
//!   from expressions.
//! - [`StreamEvaluator`] — run many expressions over a stream of inputs.
//! - [`format()`](fn@format) / [`highlight`] — pretty-print or
//!   syntax-highlight JSONata source.
//!
//! # Semantics
//!
//! This engine preserves JSONata reference semantics: `undefined` and
//! `null` are distinct, object key order is insertion order, number
//! formatting matches JavaScript's `Number.toString()`, and tail calls are
//! trampolined so deep recursion cannot overflow the stack.
//!
//! # Cargo features
//!
//! - `regex` *(default)* — full Unicode regex backend.
//! - `regex-lite` — lighter backend for smaller WASM builds (at least one
//!   backend must be enabled; if both are, `regex` wins).
//! - `mimalloc-alloc` *(default)* — links mimalloc; it is set as the global
//!   allocator only in the bundled `jsntrs-bench` binary (a library cannot
//!   set a consumer's allocator).
//! - `internals` — re-exports unstable internal items for the in-repo
//!   tests, benches, and fuzz targets. Never enable this in downstream
//!   code; the items carry no stability guarantee.

// Pedantic by default, with targeted allows
#![warn(clippy::pedantic)]
#![warn(missing_docs)]
// Cast family -- too noisy for f64-based numeric engine
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_precision_loss)]
// Not useful during active development
#![allow(clippy::doc_markdown)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::used_underscore_items)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::float_cmp)]
#![allow(clippy::many_single_char_names)]
// Restriction lints -- opt in
#![warn(clippy::unwrap_used)]
#![warn(clippy::expect_used)]
#![warn(clippy::panic)]

// Modules are crate-private; the supported public API is the curated set of
// re-exports below. `wasm` stays public — it is the wasm-bindgen surface.
pub(crate) mod error;
pub(crate) mod evaluator;
pub(crate) mod expression;
pub(crate) mod fast_path;
pub(crate) mod formatter;
pub(crate) mod highlight;
pub(crate) mod lexer;
pub(crate) mod parser;
pub(crate) mod stdlib;
pub(crate) mod stream;
pub(crate) mod try_sort;
pub(crate) mod value;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub mod wasm;

pub use error::{JsonataError, JsonataResult};
pub use evaluator::Environment;
pub use expression::{CustomFunc, Expression, new_custom_env};
pub use fast_path::FastPath;
pub use formatter::format;
pub use highlight::highlight;
pub use stream::{MetricsHook, StreamEvaluator, StreamStats};
pub use value::{CompareOp, ObjectMap, Value};

// Internal types re-exported unconditionally to keep everything named in
// hidden `Value` variants and hidden `Expression` accessors publicly
// reachable. Not part of the supported API — no stability guarantees.
#[doc(hidden)]
pub use evaluator::{BuiltinFn, EnvAwareBuiltinFn, FunctionValue, Lambda, ParamSpec, TailCall};
#[doc(hidden)]
pub use parser::{AstArena, Expr, NodeId};
#[doc(hidden)]
pub use value::Sequence;

// Internal hooks for the in-repo bins, benches, integration tests, and fuzz
// targets (all separate crates). Gated behind the `internals` feature so
// downstream code cannot reach them without opting into instability.
#[cfg(feature = "internals")]
#[doc(hidden)]
pub use evaluator::{eval, parse_signature};
#[cfg(feature = "internals")]
#[doc(hidden)]
pub use fast_path::testing as fast_path_testing;
#[cfg(feature = "internals")]
#[doc(hidden)]
pub use lexer::{Lexer, Token, TokenType};
#[cfg(feature = "internals")]
#[doc(hidden)]
pub use parser::{Parser, process_ast};
#[cfg(feature = "internals")]
#[doc(hidden)]
pub use stdlib::register_all;
#[cfg(feature = "internals")]
#[doc(hidden)]
pub use value::format_float;

/// Remaining-stack threshold (bytes) below which `stacker::maybe_grow`
/// allocates a new segment before recursing.
///
/// 128 KiB comfortably covers the deepest run of parser/evaluator frames
/// between two stack checks, so growth triggers before genuine overflow.
/// Absent on wasm32, where the stack cannot be grown.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const STACK_RED_ZONE: usize = 128 * 1024;

/// Size (bytes) of each stack segment `stacker::maybe_grow` allocates.
///
/// 1 MiB holds thousands of recursion frames per segment, amortizing
/// allocation cost across deeply nested expressions.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const STACK_GROW_SIZE: usize = 1024 * 1024;

/// Compare two values with JSONata equality semantics.
///
/// Equivalent to Go's `gnata.DeepEqual(a, b)`. Follows JSONata rules:
/// `undefined = undefined` is `false`, `null = null` is `true`,
/// numbers compare by value, arrays/objects compare recursively.
pub fn deep_equal(a: &Value, b: &Value) -> bool {
    a.deep_equal(b)
}
