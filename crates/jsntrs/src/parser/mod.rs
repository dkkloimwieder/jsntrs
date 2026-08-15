pub mod ast;
pub mod process;

pub use ast::{AstArena, BinaryOp, Expr, GroupExpr, NodeId, Signature, SortTerm, UnaryOp};
pub use process::process_ast;

use crate::error::JsonataError;
use crate::lexer::{Lexer, Token, TokenType};

/// Maximum nesting depth for expressions. Prevents stack overflow from deeply
/// nested input like `-(-(-(-(...)))) `.
const MAX_PARSE_DEPTH: usize = 500;

/// Hand-written Pratt parser for JSONata expressions.
///
/// Direct port of Go `internal/parser/parser.go`.
/// Takes `&mut AstArena`, returns `NodeId` for the root expression.
pub struct Parser {
    lex: Lexer,
    token: Token,
    infix: bool,
    arena: AstArena,
    depth: usize,
}

impl Parser {
    /// Parse a JSONata expression string into an AST arena.
    /// Returns the arena and the root node ID.
    ///
    /// # Errors
    /// Returns a `JsonataError` with an S0xxx code for syntax errors.
    pub fn parse(src: &str) -> Result<(AstArena, NodeId), JsonataError> {
        let mut parser = Self {
            lex: Lexer::new(src),
            token: Token::eof(),
            infix: false,
            arena: AstArena::new(),
            depth: 0,
        };
        // Prime the token stream
        parser.advance()?;
        if parser.token.typ == TokenType::EOF {
            return Ok((parser.arena, NodeId::EMPTY));
        }
        let root = parser.expression(0)?;
        if parser.token.typ != TokenType::EOF {
            return Err(parse_error(
                "S0201",
                &format!("unexpected token: {}", parser.token.value),
                parser.token.pos,
            )
            .with_token(parser.token.value.clone()));
        }
        Ok((parser.arena, root))
    }

    // ── Token management ─────────────────────────────────────────────

    fn advance(&mut self) -> Result<(), JsonataError> {
        self.token = self.lex.next(self.infix)?;
        self.infix = false;
        Ok(())
    }

    fn advance_prefix(&mut self) -> Result<(), JsonataError> {
        self.infix = false;
        self.token = self.lex.next(false)?;
        Ok(())
    }

    fn consume(&mut self, expected: TokenType) -> Result<Token, JsonataError> {
        if self.token.typ != expected {
            return Err(parse_error(
                "S0202",
                &format!("expected {:?}, got {:?}", expected, self.token.typ),
                self.token.pos,
            )
            .with_token(self.token.value.clone()));
        }
        let tok = std::mem::take(&mut self.token);
        self.advance()?;
        Ok(tok)
    }

    fn consume_prefix(&mut self, expected: TokenType) -> Result<Token, JsonataError> {
        if self.token.typ != expected {
            return Err(parse_error(
                "S0202",
                &format!("expected {:?}, got {:?}", expected, self.token.typ),
                self.token.pos,
            )
            .with_token(self.token.value.clone()));
        }
        let tok = std::mem::take(&mut self.token);
        self.advance_prefix()?;
        Ok(tok)
    }

    // ── Core Pratt parser ────────────────────────────────────────────

    fn expression(&mut self, rbp: i32) -> Result<NodeId, JsonataError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            stacker::maybe_grow(crate::STACK_RED_ZONE, crate::STACK_GROW_SIZE, || {
                self.expression_inner(rbp)
            })
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.expression_inner(rbp)
        }
    }

    fn expression_inner(&mut self, rbp: i32) -> Result<NodeId, JsonataError> {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            return Err(parse_error(
                "S0210",
                "expression is too deeply nested",
                self.token.pos,
            ));
        }
        let mut left = self.nud()?;
        while binding_power(self.token.typ) > rbp {
            left = self.led(left)?;
        }
        self.depth -= 1;
        Ok(left)
    }

    /// Parse RHS of a binary operator, converting EOF to S0207.
    fn binary_rhs(&mut self, bp: i32, op: &str) -> Result<NodeId, JsonataError> {
        if self.token.typ == TokenType::EOF {
            return Err(parse_error(
                "S0207",
                &format!("nothing follows the \"{op}\" operator"),
                self.token.pos,
            ));
        }
        self.expression(bp)
    }

    // ── NUD (prefix) handlers ────────────────────────────────────────

    #[expect(clippy::too_many_lines)]
    fn nud(&mut self) -> Result<NodeId, JsonataError> {
        let tok = std::mem::take(&mut self.token);
        match tok.typ {
            TokenType::Name => {
                self.infix = true;
                self.advance()?;
                // Check for lambda: "function" or "λ" followed by "("
                if (tok.value == "function" || tok.value == "λ")
                    && self.token.typ == TokenType::LParen
                {
                    return self.parse_lambda(tok.pos);
                }
                Ok(self.arena.alloc(Expr::Name {
                    value: tok.value,
                    pos: tok.pos,
                    keep_array: false,
                    stages: Vec::new(),
                    group: None,
                    focus: None,
                    index: None,
                })?)
            }
            // Keywords can appear as field names in prefix position
            TokenType::And | TokenType::Or | TokenType::In => {
                self.infix = true;
                self.advance()?;
                Ok(self.arena.alloc(Expr::Name {
                    value: tok.value,
                    pos: tok.pos,
                    keep_array: false,
                    stages: Vec::new(),
                    group: None,
                    focus: None,
                    index: None,
                })?)
            }
            TokenType::Variable => {
                self.infix = true;
                self.advance()?;
                Ok(self.arena.alloc(Expr::Variable {
                    name: tok.value,
                    group: None,
                    keep_array: false,
                    focus: None,
                    index: None,
                    pos: tok.pos,
                })?)
            }
            TokenType::String => {
                self.infix = true;
                self.advance()?;
                Ok(self.arena.alloc(Expr::StringLit {
                    value: tok.value,
                    pos: tok.pos,
                })?)
            }
            TokenType::Number => {
                self.infix = true;
                self.advance()?;
                Ok(self.arena.alloc(Expr::NumberLit {
                    value: tok.num_val,
                    raw: tok.value,
                    pos: tok.pos,
                })?)
            }
            TokenType::Value => {
                self.infix = true;
                self.advance()?;
                Ok(self.arena.alloc(Expr::ValueLit {
                    value: tok.value,
                    pos: tok.pos,
                })?)
            }
            TokenType::Regex => {
                self.infix = true;
                self.advance()?;
                Ok(self.arena.alloc(Expr::Regex {
                    pattern: tok.regex_pat,
                    flags: tok.regex_flg,
                    pos: tok.pos,
                })?)
            }
            TokenType::Minus => {
                // Unary minus (bp=70)
                self.advance_prefix()?;
                let rhs = self.expression(70)?;
                // If RHS is a number literal, fold the negation directly
                if let Expr::NumberLit { value, raw, pos } = self.arena.get(rhs).clone() {
                    let neg = -value;
                    let neg_raw = if let Some(stripped) = raw.strip_prefix('-') {
                        stripped.to_string()
                    } else {
                        format!("-{raw}")
                    };
                    *self.arena.get_mut(rhs) = Expr::NumberLit {
                        value: neg,
                        raw: neg_raw,
                        pos,
                    };
                    Ok(rhs)
                } else {
                    Ok(self.arena.alloc(Expr::Unary {
                        op: UnaryOp::Negate,
                        operand: rhs,
                        expressions: Vec::new(),
                        lhs: Vec::new(),
                        group: None,
                        keep_array: false,
                        pos: tok.pos,
                    })?)
                }
            }
            TokenType::Star => {
                self.infix = true;
                self.advance()?;
                Ok(self.arena.alloc(Expr::Wildcard {
                    keep_array: false,
                    pos: tok.pos,
                })?)
            }
            TokenType::StarStar => {
                self.infix = true;
                self.advance()?;
                Ok(self.arena.alloc(Expr::Descendant {
                    keep_array: false,
                    pos: tok.pos,
                })?)
            }
            TokenType::Percent => {
                self.infix = true;
                self.advance()?;
                Ok(self.arena.alloc(Expr::Parent {
                    pos: tok.pos,
                    slot: None,
                })?)
            }
            TokenType::LBracket => {
                // Array constructor [...]
                self.advance_prefix()?;
                let mut exprs = Vec::new();
                while self.token.typ != TokenType::RBracket {
                    if self.token.typ == TokenType::EOF {
                        return Err(parse_error("S0202", "expected ]", self.token.pos));
                    }
                    let expr = self.expression(0)?;
                    exprs.push(expr);
                    if self.token.typ == TokenType::Comma {
                        self.advance_prefix()?;
                    } else if self.token.typ != TokenType::RBracket {
                        // Structural close tokens (wrong delimiter) → S0202; operator-level tokens → S0204.
                        match self.token.typ {
                            TokenType::RParen | TokenType::RBrace | TokenType::Colon => {
                                return Err(parse_error(
                                    "S0202",
                                    &format!(
                                        "unexpected token in array constructor: {}",
                                        self.token.value
                                    ),
                                    self.token.pos,
                                ));
                            }
                            _ => {
                                return Err(parse_error(
                                    "S0204",
                                    &format!(
                                        "expected , or ] in array constructor: {}",
                                        self.token.value
                                    ),
                                    self.token.pos,
                                ));
                            }
                        }
                    }
                }
                self.infix = true;
                self.advance()?; // consume ]
                Ok(self.arena.alloc(Expr::Unary {
                    op: UnaryOp::ArrayCons,
                    operand: NodeId::EMPTY,
                    expressions: exprs,
                    lhs: Vec::new(),
                    group: None,
                    keep_array: false,
                    pos: tok.pos,
                })?)
            }
            TokenType::LBrace => {
                // Object constructor {...}
                self.advance_prefix()?;
                let pairs = self.parse_object_pairs()?;
                Ok(self.arena.alloc(Expr::Unary {
                    op: UnaryOp::ObjCons,
                    operand: NodeId::EMPTY,
                    expressions: Vec::new(),
                    lhs: pairs,
                    group: None,
                    keep_array: false,
                    pos: tok.pos,
                })?)
            }
            TokenType::LParen => {
                // Block or parenthesized expression. The documentation gives
                // the block exactly one shape — "'code blocks' — multiple
                // expressions, separated by semicolons: (expr1; expr2;
                // expr3)" (docs.jsonata.org/composition) — so an expression
                // that is not followed by `;` ends the block, and anything
                // other than `)` there is a syntax error (jsntrs-0jv).
                self.advance_prefix()?;
                let mut exprs = Vec::new();
                while self.token.typ != TokenType::RParen {
                    if self.token.typ == TokenType::EOF {
                        return Err(parse_error(
                            "S0203",
                            "expected ) before end of expression",
                            self.token.pos,
                        ));
                    }
                    let expr = self.expression(0)?;
                    exprs.push(expr);
                    if self.token.typ != TokenType::Semicolon {
                        break;
                    }
                    self.advance_prefix()?;
                }
                if self.token.typ == TokenType::EOF {
                    return Err(parse_error(
                        "S0203",
                        "expected ) before end of expression",
                        self.token.pos,
                    ));
                }
                if self.token.typ != TokenType::RParen {
                    return Err(parse_error(
                        "S0202",
                        &format!(
                            "expected ; or ) in block, got {:?}: {}",
                            self.token.typ, self.token.value
                        ),
                        self.token.pos,
                    )
                    .with_token(self.token.value.clone()));
                }
                self.infix = true;
                self.advance()?; // consume )
                Ok(self.arena.alloc(Expr::Block {
                    expressions: exprs,
                    focus: None,
                    index: None,
                    keep_array: false,
                    pos: tok.pos,
                })?)
            }
            TokenType::Question => {
                // A placeholder is a complete operand, like every other NUD
                // value above: what follows it is infix context, so `?/x` is
                // a division and not the start of a regex literal. Reading
                // it as a regex made `? / x` an S0302 "unterminated regex"
                // (jsntrs-ecq.9); jsonata-js rejects `?` in this position
                // outright (S0211), so nothing can regress.
                self.infix = true;
                self.advance()?;
                Ok(self.arena.alloc(Expr::Placeholder { pos: tok.pos })?)
            }
            TokenType::Pipe | TokenType::Tilde => self.parse_transform(tok.pos),
            TokenType::Chain => Err(parse_error(
                "S0211",
                "the ~> operator cannot be used as a prefix",
                tok.pos,
            )),
            TokenType::EOF => Err(parse_error(
                "S0201",
                "unexpected end of expression",
                tok.pos,
            )),
            _ => Err(parse_error(
                "S0211",
                &format!("unexpected token: {}", tok.value),
                tok.pos,
            )
            .with_token(tok.value)),
        }
    }

    // ── LED (infix) handlers ─────────────────────────────────────────

    #[expect(clippy::too_many_lines)]
    fn led(&mut self, left: NodeId) -> Result<NodeId, JsonataError> {
        let tok = std::mem::take(&mut self.token);
        match tok.typ {
            TokenType::Dot => {
                self.advance_prefix()?;
                let rhs = self.expression(74)?; // right-associative
                Ok(self.arena.alloc(Expr::Binary {
                    op: BinaryOp::Dot,
                    lhs: left,
                    rhs,
                    group: None,
                    keep_array: false,
                    focus: None,
                    index: None,
                    pos: tok.pos,
                })?)
            }
            TokenType::LParen => {
                // Function call
                self.advance_prefix()?;
                let mut args = Vec::new();
                let mut has_placeholder = false;
                while self.token.typ != TokenType::RParen {
                    if self.token.typ == TokenType::EOF {
                        return Err(parse_error(
                            "S0203",
                            "expected ) before end of expression",
                            self.token.pos,
                        ));
                    }
                    let arg = self.expression(0)?;
                    if matches!(self.arena.get(arg), Expr::Placeholder { .. }) {
                        has_placeholder = true;
                    }
                    args.push(arg);
                    if self.token.typ == TokenType::Comma {
                        self.advance_prefix()?;
                    } else if self.token.typ != TokenType::RParen {
                        if self.token.typ == TokenType::EOF {
                            return Err(parse_error(
                                "S0203",
                                "expected ) before end of expression",
                                self.token.pos,
                            ));
                        }
                        return Err(parse_error(
                            "S0202",
                            &format!("expected , or ) in argument list: {}", self.token.value),
                            self.token.pos,
                        ));
                    }
                }
                self.infix = true;
                self.advance()?; // consume )
                if has_placeholder {
                    Ok(self.arena.alloc(Expr::Partial {
                        procedure: left,
                        arguments: args,
                        pos: tok.pos,
                    })?)
                } else {
                    Ok(self.arena.alloc(Expr::Function {
                        procedure: left,
                        arguments: args,
                        pos: tok.pos,
                        thunk: false,
                        keep_array: false,
                        group: None,
                    })?)
                }
            }
            TokenType::LBracket => {
                // Subscript/predicate or empty [] (keep array)
                // S0209: A predicate/subscript cannot follow a group-by expression.
                if self.has_group(left) {
                    return Err(parse_error(
                        "S0209",
                        "a predicate cannot follow a grouping expression in a step",
                        tok.pos,
                    ));
                }
                self.advance_prefix()?;
                if self.token.typ == TokenType::RBracket {
                    // Empty [] → set KeepArray on left
                    self.infix = true;
                    self.advance()?;
                    self.set_keep_array(left);
                    Ok(left)
                } else {
                    let rhs = self.expression(0)?;
                    self.infix = true;
                    self.consume(TokenType::RBracket)?;
                    Ok(self.arena.alloc(Expr::Binary {
                        op: BinaryOp::Subscript,
                        lhs: left,
                        rhs,
                        group: None,
                        keep_array: false,
                        focus: None,
                        index: None,
                        pos: tok.pos,
                    })?)
                }
            }
            TokenType::LBrace => {
                // Group-by expression
                // S0210: A step can only have one group-by expression.
                if self.has_group(left) {
                    return Err(parse_error(
                        "S0210",
                        "each step can only have one grouping expression",
                        tok.pos,
                    ));
                }
                self.advance_prefix()?;
                let pairs_flat = self.parse_object_pairs()?;
                let pairs = pairs_to_group_pairs(&pairs_flat);
                self.infix = true;
                // Attach group to the left node; nodes without a group
                // slot (Block, literals, ...) get a Grouped wrapper.
                match self.set_group(
                    left,
                    GroupExpr {
                        pairs,
                        pos: tok.pos,
                    },
                ) {
                    None => Ok(left),
                    Some(group) => Ok(self.arena.alloc(Expr::Grouped {
                        expr: left,
                        group,
                        pos: tok.pos,
                    })?),
                }
            }
            TokenType::At => {
                // S0215: @ cannot follow a predicate (subscript) expression.
                if matches!(self.arena.get(left), Expr::Binary { op, .. } if *op == BinaryOp::Subscript)
                {
                    return Err(parse_error(
                        "S0215",
                        "the @ operator cannot follow a predicate expression",
                        tok.pos,
                    ));
                }
                // S0216: @ cannot follow a sort expression.
                if matches!(self.arena.get(left), Expr::Sort { .. }) {
                    return Err(parse_error(
                        "S0216",
                        "the @ operator cannot follow a sort expression",
                        tok.pos,
                    ));
                }
                // Focus binding @$var
                self.advance()?;
                // S0214: the token after @ must be a variable ($name), not a plain name.
                // The reference attributes it to the operator itself
                // (`token: "@"`, jsonata 2.2.2 jsonata.js:6574), not to the
                // offending right-hand token.
                if self.token.typ == TokenType::Name {
                    return Err(parse_error(
                        "S0214",
                        "the @ operator must be followed by a $variable, not a plain name",
                        tok.pos,
                    )
                    .with_token("@"));
                }
                if self.token.typ != TokenType::Variable {
                    return Err(parse_error(
                        "S0214",
                        "the @ operator must be followed by a $variable",
                        tok.pos,
                    )
                    .with_token("@"));
                }
                let var_name = self.token.value.clone();
                // The bound variable ends an operand: lex what follows in
                // infix context so `/` is division, not a regex start.
                self.infix = true;
                self.advance()?;
                self.set_focus(left, var_name);
                Ok(left)
            }
            TokenType::Hash => {
                // Index binding #$var
                self.advance()?;
                // S0214: the token after # must be a variable ($name), not a plain name.
                // Attributed to `#` itself (jsonata 2.2.2 jsonata.js:6590).
                if self.token.typ == TokenType::Name {
                    return Err(parse_error(
                        "S0214",
                        "the # operator must be followed by a $variable, not a plain name",
                        tok.pos,
                    )
                    .with_token("#"));
                }
                if self.token.typ != TokenType::Variable {
                    return Err(parse_error(
                        "S0214",
                        "the # operator must be followed by a $variable",
                        tok.pos,
                    )
                    .with_token("#"));
                }
                let var_name = self.token.value.clone();
                // Same as `@`: the token after the variable is infix context.
                self.infix = true;
                self.advance()?;
                self.set_index(left, var_name);
                Ok(left)
            }
            TokenType::Question => {
                // Ternary conditional
                self.advance_prefix()?;
                let then = self.expression(0)?;
                let else_ = if self.token.typ == TokenType::Colon {
                    self.advance_prefix()?;
                    Some(self.expression(0)?)
                } else {
                    None
                };
                Ok(self.arena.alloc(Expr::Condition {
                    condition: left,
                    then,
                    else_,
                    pos: tok.pos,
                })?)
            }
            TokenType::Assign => {
                // Binding :=
                // LHS must be a variable
                if !matches!(self.arena.get(left), Expr::Variable { .. }) {
                    return Err(parse_error(
                        "S0212",
                        "the left side of := must be a $variable name",
                        tok.pos,
                    ));
                }
                self.advance_prefix()?;
                let rhs = self.expression(9)?; // right-associative (bp-1)
                Ok(self.arena.alloc(Expr::Bind {
                    lhs: left,
                    rhs,
                    pos: tok.pos,
                })?)
            }
            TokenType::Caret => {
                // Sort expression
                self.parse_sort(left, tok.pos)
            }
            // Standard binary operators
            TokenType::Chain => {
                self.advance_prefix()?;
                let rhs = self.expression(44)?; // bp-1 for right-associativity
                Ok(self.arena.alloc(Expr::Binary {
                    op: BinaryOp::Chain,
                    lhs: left,
                    rhs,
                    group: None,
                    keep_array: false,
                    focus: None,
                    index: None,
                    pos: tok.pos,
                })?)
            }
            TokenType::Elvis => {
                self.advance_prefix()?;
                let rhs = self.expression(19)?;
                Ok(self.arena.alloc(Expr::Binary {
                    op: BinaryOp::CondTern,
                    lhs: left,
                    rhs,
                    group: None,
                    keep_array: false,
                    focus: None,
                    index: None,
                    pos: tok.pos,
                })?)
            }
            TokenType::Coalesce => {
                self.advance_prefix()?;
                let rhs = self.expression(19)?;
                Ok(self.arena.alloc(Expr::Binary {
                    op: BinaryOp::NullCoal,
                    lhs: left,
                    rhs,
                    group: None,
                    keep_array: false,
                    focus: None,
                    index: None,
                    pos: tok.pos,
                })?)
            }
            TokenType::DotDot => {
                self.advance_prefix()?;
                let rhs = self.expression(19)?;
                Ok(self.arena.alloc(Expr::Binary {
                    op: BinaryOp::Range,
                    lhs: left,
                    rhs,
                    group: None,
                    keep_array: false,
                    focus: None,
                    index: None,
                    pos: tok.pos,
                })?)
            }
            TokenType::And => {
                self.advance_prefix()?;
                let rhs = self.expression(30)?;
                Ok(self.arena.alloc(Expr::Binary {
                    op: BinaryOp::And,
                    lhs: left,
                    rhs,
                    group: None,
                    keep_array: false,
                    focus: None,
                    index: None,
                    pos: tok.pos,
                })?)
            }
            TokenType::Or => {
                self.advance_prefix()?;
                let rhs = self.expression(25)?;
                Ok(self.arena.alloc(Expr::Binary {
                    op: BinaryOp::Or,
                    lhs: left,
                    rhs,
                    group: None,
                    keep_array: false,
                    focus: None,
                    index: None,
                    pos: tok.pos,
                })?)
            }
            TokenType::In => {
                self.advance_prefix()?;
                let rhs = self.expression(40)?;
                Ok(self.arena.alloc(Expr::Binary {
                    op: BinaryOp::In,
                    lhs: left,
                    rhs,
                    group: None,
                    keep_array: false,
                    focus: None,
                    index: None,
                    pos: tok.pos,
                })?)
            }
            TokenType::Equals => self.binary_op(left, BinaryOp::Eq, "=", 40, tok.pos),
            TokenType::NE => self.binary_op(left, BinaryOp::Ne, "!=", 40, tok.pos),
            TokenType::LT => self.binary_op(left, BinaryOp::Lt, "<", 40, tok.pos),
            TokenType::GT => self.binary_op(left, BinaryOp::Gt, ">", 40, tok.pos),
            TokenType::LE => self.binary_op(left, BinaryOp::Le, "<=", 40, tok.pos),
            TokenType::GE => self.binary_op(left, BinaryOp::Ge, ">=", 40, tok.pos),
            TokenType::Plus => self.binary_op(left, BinaryOp::Add, "+", 50, tok.pos),
            TokenType::Minus => self.binary_op(left, BinaryOp::Sub, "-", 50, tok.pos),
            TokenType::Star => self.binary_op(left, BinaryOp::Mul, "*", 60, tok.pos),
            TokenType::Slash => self.binary_op(left, BinaryOp::Div, "/", 60, tok.pos),
            TokenType::Percent => {
                self.advance_prefix()?;
                let rhs = self.binary_rhs(60, "%")?;
                Ok(self.arena.alloc(Expr::Binary {
                    op: BinaryOp::Mod,
                    lhs: left,
                    rhs,
                    group: None,
                    keep_array: false,
                    focus: None,
                    index: None,
                    pos: tok.pos,
                })?)
            }
            TokenType::StarStar => self.binary_op(left, BinaryOp::Pow, "**", 60, tok.pos),
            TokenType::Amp => self.binary_op(left, BinaryOp::Concat, "&", 50, tok.pos),
            TokenType::Pipe => {
                self.advance_prefix()?;
                let rhs = self.expression(19)?;
                Ok(self.arena.alloc(Expr::Binary {
                    op: BinaryOp::Pipe,
                    lhs: left,
                    rhs,
                    group: None,
                    keep_array: false,
                    focus: None,
                    index: None,
                    pos: tok.pos,
                })?)
            }
            _ => Err(parse_error(
                "S0201",
                &format!("unexpected token: {}", tok.value),
                tok.pos,
            )),
        }
    }

    /// Helper for standard binary operators.
    fn binary_op(
        &mut self,
        left: NodeId,
        op: BinaryOp,
        op_str: &str,
        bp: i32,
        pos: usize,
    ) -> Result<NodeId, JsonataError> {
        self.advance_prefix()?;
        let rhs = self.binary_rhs(bp, op_str)?;
        self.arena.alloc(Expr::Binary {
            op,
            lhs: left,
            rhs,
            group: None,
            keep_array: false,
            focus: None,
            index: None,
            pos,
        })
    }

    // ── Special parsers ──────────────────────────────────────────────

    fn parse_lambda(&mut self, pos: usize) -> Result<NodeId, JsonataError> {
        // Current token should be (
        self.consume_prefix(TokenType::LParen)?;

        // Parse parameter list
        let mut params = Vec::new();
        while self.token.typ != TokenType::RParen {
            if self.token.typ != TokenType::Variable {
                // The reference names the offending parameter token
                // (`token: arg.value`, jsonata 2.2.2 jsonata.js:6384).
                return Err(parse_error(
                    "S0208",
                    "expected $parameter name in lambda",
                    self.token.pos,
                )
                .with_token(self.token.value.clone()));
            }
            let param = self.arena.alloc(Expr::Variable {
                name: self.token.value.clone(),
                group: None,
                keep_array: false,
                focus: None,
                index: None,
                pos: self.token.pos,
            })?;
            params.push(param);
            self.advance()?;
            if self.token.typ == TokenType::Comma {
                self.advance_prefix()?;
            }
        }
        self.consume(TokenType::RParen)?;

        // Optional type signature <...>
        let signature = if self.token.typ == TokenType::LT {
            Some(self.parse_signature()?)
        } else {
            None
        };

        // Body: { expr }
        self.consume_prefix(TokenType::LBrace)?;
        let body = self.expression(0)?;
        self.infix = true;
        self.consume(TokenType::RBrace)?;

        self.arena.alloc(Expr::Lambda {
            params,
            body,
            signature,
            pos,
            thunk: false,
        })
    }

    fn parse_signature(&mut self) -> Result<Signature, JsonataError> {
        // Consume the < token
        let sig_pos = self.token.pos;
        let mut depth = 1;
        let mut raw = String::from("<");
        self.advance()?;

        while depth > 0 {
            if self.token.typ == TokenType::EOF {
                return Err(parse_error(
                    "S0202",
                    "unterminated signature",
                    self.token.pos,
                ));
            }
            if self.token.typ == TokenType::LT {
                depth += 1;
            } else if self.token.typ == TokenType::GT {
                depth -= 1;
            }
            raw.push_str(&self.token.value);
            if depth > 0 {
                self.advance()?;
            }
        }
        self.advance()?;
        // Parse the content now — malformed signatures (S0401/S0402) are
        // compile errors, and call sites reuse the parsed specs instead of
        // re-parsing per invocation.
        let inner = raw
            .strip_prefix('<')
            .and_then(|r| r.strip_suffix('>'))
            .unwrap_or(&raw);
        let params = crate::evaluator::parse_signature(inner)
            .map_err(|e| e.with_position(sig_pos))?
            .into();
        Ok(Signature { raw, params })
    }

    fn parse_transform(&mut self, pos: usize) -> Result<NodeId, JsonataError> {
        // Current token is | or ~
        self.advance_prefix()?;
        let pattern = self.expression(20)?;
        self.consume_prefix(TokenType::Pipe)?;
        let update = self.expression(20)?;
        let delete = if self.token.typ == TokenType::Comma {
            self.advance_prefix()?;
            Some(self.expression(20)?)
        } else {
            None
        };
        self.infix = true;
        self.consume(TokenType::Pipe)?;
        self.arena.alloc(Expr::Transform {
            pattern,
            update,
            delete,
            pos,
        })
    }

    fn parse_sort(&mut self, left: NodeId, pos: usize) -> Result<NodeId, JsonataError> {
        self.advance()?;
        self.consume_prefix(TokenType::LParen)?;

        let mut terms = Vec::new();
        while self.token.typ != TokenType::RParen {
            if self.token.typ == TokenType::EOF {
                return Err(parse_error(
                    "S0202",
                    "expected ) in sort expression",
                    self.token.pos,
                ));
            }
            let descending = match self.token.typ {
                TokenType::GT => {
                    self.advance_prefix()?;
                    true
                }
                TokenType::LT => {
                    self.advance_prefix()?;
                    false
                }
                _ => false,
            };
            let expression = self.expression(0)?;
            terms.push(SortTerm {
                descending,
                expression,
            });
            if self.token.typ == TokenType::Comma {
                self.advance_prefix()?;
            }
        }
        self.infix = true;
        self.advance()?; // consume )
        self.arena.alloc(Expr::Sort {
            expr: left,
            terms,
            keep_array: false,
            index: None,
            focus: None,
            pos,
        })
    }

    fn parse_object_pairs(&mut self) -> Result<Vec<NodeId>, JsonataError> {
        let mut pairs = Vec::new();
        while self.token.typ != TokenType::RBrace {
            if self.token.typ == TokenType::EOF {
                return Err(parse_error(
                    "S0202",
                    "expected } in object constructor",
                    self.token.pos,
                ));
            }
            let key = self.expression(0)?;
            self.consume_prefix(TokenType::Colon)?;
            let value = self.expression(0)?;
            pairs.push(key);
            pairs.push(value);
            if self.token.typ == TokenType::Comma {
                self.advance_prefix()?;
            }
        }
        self.infix = true;
        self.advance()?; // consume }
        Ok(pairs)
    }

    // ── Node modification helpers ────────────────────────────────────

    /// Record the `[]` (keep-array) suffix on `id`.
    ///
    /// The `Block` arm carries `(…)[]`: jsonata-js stores `keepArray` on
    /// whatever node the suffix follows, so a parenthesised path step keeps
    /// it too and `process_ast` hoists it to the path's
    /// `keep_singleton_array` (`foo.($sum(bar))[]` → `[3]`). A *top-level*
    /// block never produces a sequence, so the flag stays inert there and
    /// `($sum(prices))[]` remains `60`.
    ///
    /// `Wildcard`/`Descendant` carry it too (jsntrs-p0v.20): jsonata-js keeps
    /// `keepArray` on the raw node and honours it wherever that node's result
    /// is still a sequence, so `*[]` on `{"a": 1}` is `[1]` and `x.**[]` on
    /// `{"x": 5}` is `[5]`. Dropping the flag collapsed both.
    fn set_keep_array(&mut self, id: NodeId) {
        match self.arena.get_mut(id) {
            Expr::Name { keep_array, .. }
            | Expr::Binary { keep_array, .. }
            | Expr::Variable { keep_array, .. }
            | Expr::Function { keep_array, .. }
            | Expr::Sort { keep_array, .. }
            | Expr::Block { keep_array, .. }
            | Expr::Unary { keep_array, .. }
            | Expr::Wildcard { keep_array, .. }
            | Expr::Descendant { keep_array, .. } => {
                *keep_array = true;
            }
            _ => {}
        }
    }

    fn has_group(&self, id: NodeId) -> bool {
        match self.arena.get(id) {
            Expr::Name { group, .. } => group.is_some(),
            Expr::Binary { group, .. } => group.is_some(),
            Expr::Variable { group, .. } => group.is_some(),
            Expr::Unary { group, .. } => group.is_some(),
            Expr::Function { group, .. } => group.is_some(),
            Expr::Grouped { .. } => true,
            _ => false,
        }
    }

    /// Attach a group-by to `id` if the node has a `group` slot; otherwise
    /// hand the group back so the caller can wrap the node in
    /// [`Expr::Grouped`] — silently dropping it loses `(1+2){"k": $}`.
    fn set_group(&mut self, id: NodeId, group: GroupExpr) -> Option<GroupExpr> {
        match self.arena.get_mut(id) {
            Expr::Name { group: g, .. } => *g = Some(group),
            Expr::Binary { group: g, .. } => *g = Some(group),
            Expr::Variable { group: g, .. } => *g = Some(group),
            Expr::Unary { group: g, .. } => *g = Some(group),
            Expr::Function { group: g, .. } => *g = Some(group),
            _ => return Some(group),
        }
        None
    }

    fn set_focus(&mut self, id: NodeId, name: String) {
        match self.arena.get_mut(id) {
            Expr::Name { focus, .. } => *focus = Some(name),
            Expr::Binary { focus, .. } => *focus = Some(name),
            Expr::Variable { focus, .. } => *focus = Some(name),
            Expr::Sort { focus, .. } => *focus = Some(name),
            Expr::Block { focus, .. } => *focus = Some(name),
            _ => {}
        }
    }

    fn set_index(&mut self, id: NodeId, name: String) {
        match self.arena.get_mut(id) {
            Expr::Name { index, .. } => *index = Some(name),
            Expr::Binary { index, .. } => *index = Some(name),
            Expr::Variable { index, .. } => *index = Some(name),
            Expr::Sort { index, .. } => *index = Some(name),
            Expr::Block { index, .. } => *index = Some(name),
            _ => {}
        }
    }
}

/// Convert flat [k0, v0, k1, v1, ...] pairs to [[k0, v0], [k1, v1], ...].
fn pairs_to_group_pairs(flat: &[NodeId]) -> Vec<[NodeId; 2]> {
    flat.chunks_exact(2).map(|c| [c[0], c[1]]).collect()
}

/// Binding power table for the Pratt parser.
fn binding_power(tt: TokenType) -> i32 {
    match tt {
        TokenType::LParen | TokenType::LBracket => 80,
        TokenType::Dot | TokenType::At | TokenType::Hash => 75,
        TokenType::LBrace => 70,
        TokenType::StarStar | TokenType::Star | TokenType::Slash | TokenType::Percent => 60,
        TokenType::Plus | TokenType::Minus | TokenType::Amp => 50,
        TokenType::Chain => 45,
        TokenType::NE
        | TokenType::LE
        | TokenType::GE
        | TokenType::Equals
        | TokenType::LT
        | TokenType::GT
        | TokenType::In
        | TokenType::Caret => 40,
        TokenType::And => 30,
        TokenType::Or => 25,
        TokenType::DotDot
        | TokenType::Pipe
        | TokenType::Question
        | TokenType::Elvis
        | TokenType::Coalesce => 20,
        TokenType::Assign => 10,
        _ => 0,
    }
}

fn parse_error(code: &'static str, msg: &str, pos: usize) -> JsonataError {
    JsonataError::new(code, msg).with_position(pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> (AstArena, NodeId) {
        Parser::parse(src).unwrap()
    }

    #[test]
    fn parse_name() {
        let (arena, root) = parse("foo");
        assert!(matches!(arena.get(root), Expr::Name { value, .. } if value == "foo"));
    }

    #[test]
    fn parse_dotted_path() {
        let (arena, root) = parse("a.b.c");
        // Should be Binary { ".", Binary { ".", a, b }, c } (left-assoc via bp)
        // Actually with bp=74 for RHS, it's right-associative:
        // Binary { ".", a, Binary { ".", b, c } }
        assert!(matches!(arena.get(root), Expr::Binary { op, .. } if *op == BinaryOp::Dot));
    }

    #[test]
    fn parse_number() {
        let (arena, root) = parse("42");
        assert!(matches!(arena.get(root), Expr::NumberLit { value, .. } if *value == 42.0));
    }

    #[test]
    fn parse_negative_number() {
        let (arena, root) = parse("-42");
        // Should fold into a single negative number literal
        assert!(matches!(arena.get(root), Expr::NumberLit { value, .. } if *value == -42.0));
    }

    #[test]
    fn parse_string() {
        let (arena, root) = parse(r#""hello""#);
        assert!(matches!(arena.get(root), Expr::StringLit { value, .. } if value == "hello"));
    }

    #[test]
    fn parse_variable() {
        let (arena, root) = parse("$x");
        assert!(matches!(arena.get(root), Expr::Variable { name, .. } if name == "x"));
    }

    /// Parse errors carry their source position, and unexpected-token
    /// errors carry the token text (gnata-0mb.4).
    #[test]
    fn parse_errors_carry_position_and_token() {
        let err = Parser::parse("1 ⊕ 2").unwrap_err();
        assert!(err.position.is_some());

        let err = Parser::parse("a b").unwrap_err();
        assert_eq!(err.position, Some(2));
        assert_eq!(err.token, "b");
    }

    #[test]
    fn focus_and_index_bindings_keep_infix_context() {
        // `/` after `@$var` / `#$var` is division, not a regex start
        // (Go parses both; they only fail later at eval with T2001).
        for src in ["Nums@$n / 2", "Nums#$i / 2"] {
            assert!(
                Parser::parse(src).is_ok(),
                "expected {src:?} to parse without S0302"
            );
        }
        // `/regex/` after the binding also lexes as division — the dangling
        // `/` hits EOF (S0207, this port's code for `1 +` too; Go says
        // S0201 here), never S0302 unterminated regex.
        let err = Parser::parse("Nums@$n /regex/").unwrap_err();
        assert_eq!(err.code, "S0207");
    }

    #[test]
    fn parse_binary_plus() {
        let (arena, root) = parse("1 + 2");
        match arena.get(root) {
            Expr::Binary { op, lhs, rhs, .. } => {
                assert_eq!(*op, BinaryOp::Add);
                assert!(matches!(arena.get(*lhs), Expr::NumberLit { value, .. } if *value == 1.0));
                assert!(matches!(arena.get(*rhs), Expr::NumberLit { value, .. } if *value == 2.0));
            }
            other => panic!("expected Binary, got {:?}", other),
        }
    }

    #[test]
    fn parse_precedence() {
        // 1 + 2 * 3 should parse as 1 + (2 * 3)
        let (arena, root) = parse("1 + 2 * 3");
        match arena.get(root) {
            Expr::Binary { op, rhs, .. } => {
                assert_eq!(*op, BinaryOp::Add);
                assert!(matches!(arena.get(*rhs), Expr::Binary { op, .. } if *op == BinaryOp::Mul));
            }
            other => panic!("expected Binary, got {:?}", other),
        }
    }

    #[test]
    fn parse_function_call() {
        let (arena, root) = parse("$sum(a, b)");
        match arena.get(root) {
            Expr::Function { arguments, .. } => {
                assert_eq!(arguments.len(), 2);
            }
            other => panic!("expected Function, got {:?}", other),
        }
    }

    #[test]
    fn parse_lambda() {
        let (arena, root) = parse("function($x){ $x + 1 }");
        assert!(matches!(arena.get(root), Expr::Lambda { params, .. } if params.len() == 1));
    }

    #[test]
    fn parse_array_constructor() {
        let (arena, root) = parse("[1, 2, 3]");
        match arena.get(root) {
            Expr::Unary {
                op, expressions, ..
            } => {
                assert_eq!(*op, UnaryOp::ArrayCons);
                assert_eq!(expressions.len(), 3);
            }
            other => panic!("expected Unary array, got {:?}", other),
        }
    }

    #[test]
    fn parse_object_constructor() {
        let (arena, root) = parse(r#"{"a": 1, "b": 2}"#);
        match arena.get(root) {
            Expr::Unary { op, lhs, .. } => {
                assert_eq!(*op, UnaryOp::ObjCons);
                assert_eq!(lhs.len(), 4); // flat [k0, v0, k1, v1]
            }
            other => panic!("expected Unary object, got {:?}", other),
        }
    }

    #[test]
    fn parse_conditional() {
        let (arena, root) = parse("x ? 1 : 2");
        assert!(matches!(
            arena.get(root),
            Expr::Condition { else_: Some(_), .. }
        ));
    }

    #[test]
    fn parse_conditional_no_else() {
        let (arena, root) = parse("x ? 1");
        assert!(matches!(
            arena.get(root),
            Expr::Condition { else_: None, .. }
        ));
    }

    #[test]
    fn parse_binding() {
        let (arena, root) = parse("$x := 42");
        assert!(matches!(arena.get(root), Expr::Bind { .. }));
    }

    #[test]
    fn parse_wildcard() {
        let (arena, root) = parse("*");
        assert!(matches!(arena.get(root), Expr::Wildcard { .. }));
    }

    #[test]
    fn parse_descendant() {
        let (arena, root) = parse("**");
        assert!(matches!(arena.get(root), Expr::Descendant { .. }));
    }

    #[test]
    fn parse_parent() {
        let (arena, root) = parse("%");
        assert!(matches!(arena.get(root), Expr::Parent { .. }));
    }

    #[test]
    fn parse_block() {
        let (arena, root) = parse("(a; b; c)");
        match arena.get(root) {
            Expr::Block { expressions, .. } => {
                assert_eq!(expressions.len(), 3);
            }
            other => panic!("expected Block, got {:?}", other),
        }
    }

    #[test]
    fn parse_subscript() {
        let (arena, root) = parse("a[0]");
        assert!(matches!(arena.get(root), Expr::Binary { op, .. } if *op == BinaryOp::Subscript));
    }

    #[test]
    fn parse_keep_array() {
        let (arena, root) = parse("a[]");
        assert!(matches!(
            arena.get(root),
            Expr::Name {
                keep_array: true,
                ..
            }
        ));
    }

    #[test]
    fn parse_sort() {
        let (arena, root) = parse("data^(>price)");
        match arena.get(root) {
            Expr::Sort { terms, .. } => {
                assert_eq!(terms.len(), 1);
                assert!(terms[0].descending);
            }
            other => panic!("expected Sort, got {:?}", other),
        }
    }

    #[test]
    fn parse_transform() {
        let (arena, root) = parse("|a|b|");
        assert!(matches!(
            arena.get(root),
            Expr::Transform { delete: None, .. }
        ));
    }

    #[test]
    fn parse_transform_with_delete() {
        let (arena, root) = parse("|a|b,c|");
        assert!(matches!(
            arena.get(root),
            Expr::Transform {
                delete: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn parse_partial_application() {
        let (arena, root) = parse("$f(?, 2)");
        assert!(matches!(arena.get(root), Expr::Partial { .. }));
    }

    #[test]
    fn parse_chain() {
        let (arena, root) = parse("a ~> b");
        assert!(matches!(arena.get(root), Expr::Binary { op, .. } if *op == BinaryOp::Chain));
    }

    #[test]
    fn parse_range() {
        let (arena, root) = parse("[1..5]");
        match arena.get(root) {
            Expr::Unary { expressions, .. } => {
                assert_eq!(expressions.len(), 1);
                assert!(
                    matches!(arena.get(expressions[0]), Expr::Binary { op, .. } if *op == BinaryOp::Range)
                );
            }
            other => panic!("expected array with range, got {:?}", other),
        }
    }

    #[test]
    fn parse_empty_expression() {
        let (arena, root) = Parser::parse("").unwrap();
        assert!(root.is_empty());
        assert!(arena.is_empty());
    }

    #[test]
    fn error_unexpected_eof() {
        let result = Parser::parse("1 +");
        assert!(result.is_err());
    }

    #[test]
    fn error_binding_lhs_not_variable() {
        let result = Parser::parse("a := 1");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, "S0212");
    }

    #[test]
    fn parse_complex_jsonata() {
        // Real-world JSONata: sum of order prices
        let (arena, root) = parse("$sum(Account.Order.Product.Price)");
        assert!(matches!(arena.get(root), Expr::Function { .. }));
    }

    #[test]
    fn parse_lambda_with_signature() {
        let (arena, root) = parse("function($x)<s:n>{ $x }");
        match arena.get(root) {
            Expr::Lambda { signature, .. } => {
                assert!(signature.is_some());
                assert!(signature.as_ref().unwrap().raw.starts_with('<'));
            }
            other => panic!("expected Lambda, got {:?}", other),
        }
    }
}
