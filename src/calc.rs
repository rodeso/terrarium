//! Arithmetic expression evaluator.
//!
//! Shared by the Calculator tab (runs in the browser for live previews and on
//! the server when saving) and by the LLM's `calculate` tool (server).
//!
//! Supported syntax:
//! - numbers: `12`, `3.5`, `.5`, `1e6`, `2.5E-3`
//! - operators: `+ - * / ^` (also `** × ÷ −`), unary `-`, postfix `%` (percent:
//!   `20%` = 0.2) and `!` (factorial), `mod` (remainder)
//! - comparisons `< > <= >= == !=` (also `≤ ≥ ≠`), giving 1 (true) or 0 (false)
//! - parentheses, and implicit multiplication before `(` or a name: `2(3+4)`, `2pi`
//! - constants: `pi`/`π`, `e`, `tau`
//! - functions: `sqrt`/`√`, `cbrt`, `abs`, `round(x)`/`round(x, digits)`,
//!   `floor`, `ceil`, `trunc`, `min(…)`, `max(…)`, `pow(x, y)`, `exp`, `ln`,
//!   `log(x)` (base 10) / `log(x, base)`, `log2`, `sin`, `cos`, `tan`,
//!   `asin`, `acos`, `atan` (radians), `gcd`, `lcm`,
//!   `prime(k)` (the k-th prime), `isprime(x)`, `if(condition, a, b)`
//! - series: `sum(expr, k, from, to)` and `prod(expr, k, from, to)`, e.g.
//!   `sum(prime(k), k, 1, 100)`; `sum(a, b, …)` with plain values adds them

use std::cell::RefCell;
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct CalcError(pub String);

impl fmt::Display for CalcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CalcError {}

fn err<T>(msg: impl Into<String>) -> Result<T, CalcError> {
    Err(CalcError(msg.into()))
}

/// Upper bound on work per evaluation (series steps, prime lookups), so an
/// expression like `sum(k, k, 1, 10^12)` fails fast instead of freezing the app.
const WORK_LIMIT: u64 = 2_000_000;
/// Largest k for `prime(k)` (the 100,000th prime is 1,299,709).
const MAX_PRIME_INDEX: f64 = 100_000.0;
/// Integers above 2^53 can't be represented exactly in an f64.
const MAX_EXACT_INT: f64 = 9_007_199_254_740_992.0;

/// Evaluates an expression to a finite number.
pub fn evaluate(input: &str) -> Result<f64, CalcError> {
    let tokens = tokenize(input)?;
    if tokens.is_empty() {
        return err("Enter an expression");
    }
    let mut parser = Parser { tokens, pos: 0 };
    let ast = parser.logic()?;
    if let Some(tok) = parser.peek() {
        return err(match tok {
            Tok::RParen => "Unmatched closing parenthesis".to_string(),
            other => format!("Unexpected {}", other.describe()),
        });
    }
    let mut env = Env { vars: Vec::new(), work: 0 };
    finite(eval(&ast, &mut env)?)
}

/// Evaluates and formats in one step.
pub fn evaluate_to_string(input: &str) -> Result<String, CalcError> {
    evaluate(input).map(format_number)
}

/// Formats a result for display: up to 12 significant digits, no
/// floating-point noise (`0.1 + 0.2` shows as `0.3`), scientific notation
/// for very large or very small numbers.
pub fn format_number(x: f64) -> String {
    if x == 0.0 || !x.is_finite() {
        return if x.is_nan() { "NaN".into() } else if x.is_infinite() { "∞".into() } else { "0".into() };
    }
    let abs = x.abs();
    // Whole numbers that are exact print in full (e.g. 24133, 2^50).
    if x.fract() == 0.0 && abs <= MAX_EXACT_INT {
        return format!("{x:.0}");
    }
    if !(1e-9..1e15).contains(&abs) {
        let s = format!("{x:.9e}");
        let (mantissa, exponent) = s.split_once('e').unwrap_or((&s, "0"));
        return format!("{}e{}", trim_zeros(mantissa), exponent);
    }
    let int_digits = abs.log10().floor() as i32 + 1;
    let decimals = (12 - int_digits).clamp(0, 15) as usize;
    let s = trim_zeros(&format!("{x:.decimals$}")).to_string();
    if s == "-0" { "0".into() } else { s }
}

fn trim_zeros(s: &str) -> &str {
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.') } else { s }
}

fn finite(x: f64) -> Result<f64, CalcError> {
    if x.is_nan() {
        err("The result is undefined")
    } else if x.is_infinite() {
        err("The result is too large")
    } else {
        Ok(if x == 0.0 { 0.0 } else { x }) // normalise -0
    }
}

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Name(String),
    Op(char), // + - * / ^ % !
    Cmp(Cmp),
    LParen,
    RParen,
    Comma,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Cmp {
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
}

impl Tok {
    fn describe(&self) -> String {
        match self {
            Tok::Num(n) => format!("number {}", format_number(*n)),
            Tok::Name(n) => format!("'{n}'"),
            Tok::Op(c) => format!("'{c}'"),
            Tok::Cmp(_) => "comparison".into(),
            Tok::LParen => "'('".into(),
            Tok::RParen => "')'".into(),
            Tok::Comma => "','".into(),
        }
    }
}

fn tokenize(input: &str) -> Result<Vec<Tok>, CalcError> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    let next_is = |i: usize, c: char| chars.get(i + 1) == Some(&c);

    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => i += 1,
            '0'..='9' | '.' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                // Exponent only if digits follow: `2e3` is 2000, `2e` is 2·e.
                if i < chars.len() && matches!(chars[i], 'e' | 'E') {
                    let mut j = i + 1;
                    if j < chars.len() && matches!(chars[j], '+' | '-') {
                        j += 1;
                    }
                    if j < chars.len() && chars[j].is_ascii_digit() {
                        while j < chars.len() && chars[j].is_ascii_digit() {
                            j += 1;
                        }
                        i = j;
                    }
                }
                let text: String = chars[start..i].iter().collect();
                match text.parse::<f64>() {
                    Ok(n) => tokens.push(Tok::Num(n)),
                    Err(_) => return err(format!("'{text}' is not a valid number")),
                }
            }
            'π' => {
                tokens.push(Tok::Name("pi".into()));
                i += 1;
            }
            '√' => {
                tokens.push(Tok::Name("sqrt".into()));
                i += 1;
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                tokens.push(Tok::Name(chars[start..i].iter().collect::<String>().to_lowercase()));
            }
            '&' if next_is(i, '&') => {
                tokens.push(Tok::Name("and".into()));
                i += 2;
            }
            '|' if next_is(i, '|') => {
                tokens.push(Tok::Name("or".into()));
                i += 2;
            }
            '*' if next_is(i, '*') => {
                tokens.push(Tok::Op('^'));
                i += 2;
            }
            '<' | '>' | '=' | '!' if next_is(i, '=') => {
                tokens.push(Tok::Cmp(match c {
                    '<' => Cmp::Le,
                    '>' => Cmp::Ge,
                    '=' => Cmp::Eq,
                    _ => Cmp::Ne,
                }));
                i += 2;
            }
            '<' => {
                tokens.push(Tok::Cmp(Cmp::Lt));
                i += 1;
            }
            '>' => {
                tokens.push(Tok::Cmp(Cmp::Gt));
                i += 1;
            }
            '≤' | '≥' | '≠' => {
                tokens.push(Tok::Cmp(match c {
                    '≤' => Cmp::Le,
                    '≥' => Cmp::Ge,
                    _ => Cmp::Ne,
                }));
                i += 1;
            }
            '=' => return err("Use == to compare values"),
            '+' | '-' | '*' | '/' | '^' | '%' | '!' => {
                tokens.push(Tok::Op(c));
                i += 1;
            }
            '×' | '·' | '∙' => {
                tokens.push(Tok::Op('*'));
                i += 1;
            }
            '÷' => {
                tokens.push(Tok::Op('/'));
                i += 1;
            }
            '−' | '–' => {
                tokens.push(Tok::Op('-'));
                i += 1;
            }
            '(' | '[' => {
                tokens.push(Tok::LParen);
                i += 1;
            }
            ')' | ']' => {
                tokens.push(Tok::RParen);
                i += 1;
            }
            ',' | ';' => {
                tokens.push(Tok::Comma);
                i += 1;
            }
            other => return err(format!("Unexpected character '{other}'")),
        }
    }
    Ok(tokens)
}

// ---------------------------------------------------------------------------
// Syntax tree
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Node {
    Num(f64),
    /// A constant or a series variable, resolved when evaluated.
    Name(String),
    Neg(Box<Node>),
    Percent(Box<Node>),
    Factorial(Box<Node>),
    Bin(BinOp, Box<Node>, Box<Node>),
    Compare(Cmp, Box<Node>, Box<Node>),
    And(Box<Node>, Box<Node>),
    Or(Box<Node>, Box<Node>),
    Not(Box<Node>),
    Call(String, Vec<Node>),
}

#[derive(Debug, Clone, Copy)]
enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
}

// ---------------------------------------------------------------------------
// Parser (recursive descent)
//
//   comparison = expr [ ("<" | ">" | "<=" | ">=" | "==" | "!=") expr ]
//   expr       = term { ("+" | "-") term }
//   term       = unary { ("*" | "/" | "mod") unary | <implicit *> unary }
//   unary      = ("+" | "-") unary | power
//   power      = postfix [ "^" unary ]          (right-associative; -2^2 = -4)
//   postfix    = primary { "%" | "!" }
//   primary    = number | "(" comparison ")" | name [ "(" args ")" | postfix ]
// ---------------------------------------------------------------------------

struct Parser {
    tokens: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.tokens.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn eat_op(&mut self, op: char) -> bool {
        if self.peek() == Some(&Tok::Op(op)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn eat_keyword(&mut self, word: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Name(n)) if n == word) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// logic = and_expr { "or" and_expr }
    fn logic(&mut self) -> Result<Node, CalcError> {
        let mut node = self.and_expr()?;
        while self.eat_keyword("or") {
            node = Node::Or(Box::new(node), Box::new(self.and_expr()?));
        }
        Ok(node)
    }

    /// and_expr = not_expr { "and" not_expr }
    fn and_expr(&mut self) -> Result<Node, CalcError> {
        let mut node = self.not_expr()?;
        while self.eat_keyword("and") {
            node = Node::And(Box::new(node), Box::new(self.not_expr()?));
        }
        Ok(node)
    }

    /// not_expr = "not" not_expr | comparison
    fn not_expr(&mut self) -> Result<Node, CalcError> {
        if self.eat_keyword("not") {
            Ok(Node::Not(Box::new(self.not_expr()?)))
        } else {
            self.comparison()
        }
    }

    fn comparison(&mut self) -> Result<Node, CalcError> {
        let left = self.expr()?;
        if let Some(&Tok::Cmp(cmp)) = self.peek() {
            self.pos += 1;
            let right = self.expr()?;
            return Ok(Node::Compare(cmp, Box::new(left), Box::new(right)));
        }
        Ok(left)
    }

    fn expr(&mut self) -> Result<Node, CalcError> {
        let mut node = self.term()?;
        loop {
            let op = if self.eat_op('+') {
                BinOp::Add
            } else if self.eat_op('-') {
                BinOp::Sub
            } else {
                return Ok(node);
            };
            node = Node::Bin(op, Box::new(node), Box::new(self.term()?));
        }
    }

    fn term(&mut self) -> Result<Node, CalcError> {
        let mut node = self.unary()?;
        loop {
            let op = if self.eat_op('*') {
                BinOp::Mul
            } else if self.eat_op('/') {
                BinOp::Div
            } else if matches!(self.peek(), Some(Tok::Name(n)) if n == "mod") {
                self.pos += 1;
                BinOp::Mod
            } else if matches!(self.peek(), Some(Tok::LParen))
                || matches!(self.peek(), Some(Tok::Name(n)) if !KEYWORDS.contains(&n.as_str()))
            {
                BinOp::Mul // implicit multiplication: 2(3+4), 2pi, 3sqrt(2)
            } else {
                return Ok(node);
            };
            node = Node::Bin(op, Box::new(node), Box::new(self.unary()?));
        }
    }

    fn unary(&mut self) -> Result<Node, CalcError> {
        if self.eat_op('-') {
            Ok(Node::Neg(Box::new(self.unary()?)))
        } else if self.eat_op('+') {
            self.unary()
        } else if self.eat_op('!') {
            // Prefix ! is "not" (`!isprime(k)`); after a value it's factorial (`5!`).
            Ok(Node::Not(Box::new(self.unary()?)))
        } else {
            self.power()
        }
    }

    fn power(&mut self) -> Result<Node, CalcError> {
        let base = self.postfix()?;
        if self.eat_op('^') {
            Ok(Node::Bin(BinOp::Pow, Box::new(base), Box::new(self.unary()?)))
        } else {
            Ok(base)
        }
    }

    fn postfix(&mut self) -> Result<Node, CalcError> {
        let mut node = self.primary()?;
        loop {
            if self.eat_op('%') {
                node = Node::Percent(Box::new(node));
            } else if self.eat_op('!') {
                node = Node::Factorial(Box::new(node));
            } else {
                return Ok(node);
            }
        }
    }

    fn primary(&mut self) -> Result<Node, CalcError> {
        match self.next() {
            Some(Tok::Num(n)) => Ok(Node::Num(n)),
            Some(Tok::LParen) => {
                let node = self.logic()?;
                match self.next() {
                    // Like most calculators, close parentheses left open at the end.
                    Some(Tok::RParen) | None => Ok(node),
                    _ => err("Missing closing parenthesis"),
                }
            }
            Some(Tok::Name(name)) => self.name(name),
            Some(tok) => err(format!("Unexpected {}", tok.describe())),
            None => err("The expression is incomplete"),
        }
    }

    fn name(&mut self, name: String) -> Result<Node, CalcError> {
        let name = canonical(&name).to_string();
        if KEYWORDS.contains(&name.as_str()) {
            return err(format!("'{name}' needs a value on each side, e.g. 2 > 1 {name} 3 > 2"));
        }
        if !FUNCTIONS.contains(&name.as_str()) {
            if self.peek() == Some(&Tok::LParen) && !is_constant(&name) {
                return err(format!("Unknown function '{name}'"));
            }
            return Ok(Node::Name(name)); // constant or series variable
        }
        let args = if self.peek() == Some(&Tok::LParen) {
            self.pos += 1;
            let mut args = vec![self.logic()?];
            while self.peek() == Some(&Tok::Comma) {
                self.pos += 1;
                args.push(self.logic()?);
            }
            match self.next() {
                Some(Tok::RParen) | None => args, // auto-close at the end, as above
                _ => return err(format!("Missing closing parenthesis after {name}(…")),
            }
        } else if self.peek().is_some() {
            vec![self.postfix()?] // √2, sqrt 9
        } else {
            return err(format!("{name} needs a value, e.g. {name}(2)"));
        };
        Ok(Node::Call(name, args))
    }
}

/// Spellings models and people commonly use for the same function.
fn is_constant(name: &str) -> bool {
    matches!(name, "pi" | "e" | "tau" | "true" | "false")
}

/// Words used as operators, never as names or for implicit multiplication.
const KEYWORDS: &[&str] = &["mod", "and", "or"];

fn truth(b: bool) -> f64 {
    if b { 1.0 } else { 0.0 }
}

fn canonical(name: &str) -> &str {
    match name {
        "iif" | "ifelse" => "if",
        "product" => "prod",
        "is_prime" => "isprime",
        "nthprime" | "nth_prime" => "prime",
        other => other,
    }
}

const FUNCTIONS: &[&str] = &[
    "sqrt", "cbrt", "abs", "round", "floor", "ceil", "trunc", "min", "max", "pow", "exp", "ln", "log",
    "log2", "sin", "cos", "tan", "asin", "acos", "atan", "gcd", "lcm", "prime", "isprime", "if", "sum",
    "prod",
];

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

struct Env {
    /// Series variables currently in scope (innermost last).
    vars: Vec<(String, f64)>,
    /// Work done so far, checked against WORK_LIMIT.
    work: u64,
}

impl Env {
    fn spend(&mut self, amount: u64) -> Result<(), CalcError> {
        self.work += amount;
        if self.work > WORK_LIMIT {
            err("This would take too long to calculate; try a smaller range")
        } else {
            Ok(())
        }
    }
}

fn eval(node: &Node, env: &mut Env) -> Result<f64, CalcError> {
    let value = match node {
        Node::Num(n) => *n,
        Node::Name(name) => {
            if let Some((_, v)) = env.vars.iter().rev().find(|(n, _)| n == name) {
                *v
            } else {
                match name.as_str() {
                    "pi" => std::f64::consts::PI,
                    "e" => std::f64::consts::E,
                    "tau" => std::f64::consts::TAU,
                    "true" => 1.0,
                    "false" => 0.0,
                    _ => return err(format!("Unknown name '{name}'")),
                }
            }
        }
        Node::Neg(x) => -eval(x, env)?,
        Node::Percent(x) => eval(x, env)? / 100.0,
        Node::Factorial(x) => factorial(eval(x, env)?)?,
        Node::Bin(op, a, b) => {
            let a = eval(a, env)?;
            let b = eval(b, env)?;
            match op {
                BinOp::Add => a + b,
                BinOp::Sub => a - b,
                BinOp::Mul => a * b,
                BinOp::Div if b == 0.0 => return err("Division by zero"),
                BinOp::Div => a / b,
                BinOp::Mod if b == 0.0 => return err("Division by zero"),
                BinOp::Mod => a % b,
                BinOp::Pow => a.powf(b),
            }
        }
        // Short-circuit, so `x != 0 and 1/x > 2` is safe.
        Node::And(a, b) => truth(eval(a, env)? != 0.0 && eval(b, env)? != 0.0),
        Node::Or(a, b) => truth(eval(a, env)? != 0.0 || eval(b, env)? != 0.0),
        Node::Not(x) => truth(eval(x, env)? == 0.0),
        Node::Compare(cmp, a, b) => {
            let (a, b) = (eval(a, env)?, eval(b, env)?);
            let close = (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0);
            let truth = match cmp {
                Cmp::Lt => a < b && !close,
                Cmp::Gt => a > b && !close,
                Cmp::Le => a < b || close,
                Cmp::Ge => a > b || close,
                Cmp::Eq => close,
                Cmp::Ne => !close,
            };
            if truth { 1.0 } else { 0.0 }
        }
        Node::Call(name, args) => call(name, args, env)?,
    };
    finite(value)
}

fn call(name: &str, args: &[Node], env: &mut Env) -> Result<f64, CalcError> {
    // Functions that need their arguments unevaluated.
    match name {
        "if" => {
            if args.len() != 3 {
                return err("if takes 3 values: if(condition, value if true, value if false)");
            }
            let condition = eval(&args[0], env)?;
            return eval(if condition != 0.0 { &args[1] } else { &args[2] }, env);
        }
        "sum" | "prod" => {
            if let [body, Node::Name(var), from, to] = args {
                if !is_constant(var) || env.vars.iter().any(|(n, _)| n == var) {
                    return series(name == "sum", body, var, from, to, env);
                }
            }
            // Plain list: sum(1, 2, 3)
            let values = args.iter().map(|a| eval(a, env)).collect::<Result<Vec<_>, _>>()?;
            return Ok(if name == "sum" { values.iter().sum() } else { values.iter().product() });
        }
        _ => {}
    }

    let values = args.iter().map(|a| eval(a, env)).collect::<Result<Vec<_>, _>>()?;
    let arity = |n: usize| -> Result<(), CalcError> {
        if values.len() == n {
            Ok(())
        } else {
            err(format!("{name} takes {n} value{}", if n == 1 { "" } else { "s" }))
        }
    };
    let x = values[0];
    Ok(match name {
        "sqrt" => {
            arity(1)?;
            if x < 0.0 {
                return err("Square root of a negative number");
            }
            x.sqrt()
        }
        "cbrt" => { arity(1)?; x.cbrt() }
        "abs" => { arity(1)?; x.abs() }
        "floor" => { arity(1)?; x.floor() }
        "ceil" => { arity(1)?; x.ceil() }
        "trunc" => { arity(1)?; x.trunc() }
        "exp" => { arity(1)?; x.exp() }
        "sin" => { arity(1)?; x.sin() }
        "cos" => { arity(1)?; x.cos() }
        "tan" => { arity(1)?; x.tan() }
        "asin" => { arity(1)?; x.asin() }
        "acos" => { arity(1)?; x.acos() }
        "atan" => { arity(1)?; x.atan() }
        "ln" | "log2" => {
            arity(1)?;
            if x <= 0.0 {
                return err("Logarithm of zero or a negative number");
            }
            if name == "ln" { x.ln() } else { x.log2() }
        }
        "log" => {
            if x <= 0.0 {
                return err("Logarithm of zero or a negative number");
            }
            match values.as_slice() {
                [_] => x.log10(),
                [_, base] if *base > 0.0 && *base != 1.0 => x.log(*base),
                [_, _] => return err("Logarithm base must be positive and not 1"),
                _ => return err("log takes 1 or 2 values"),
            }
        }
        "round" => match values.as_slice() {
            [_] => x.round(),
            [_, digits] if digits.fract() == 0.0 && (0.0..=15.0).contains(digits) => {
                let factor = 10f64.powi(*digits as i32);
                (x * factor).round() / factor
            }
            [_, _] => return err("round's second value must be a whole number from 0 to 15"),
            _ => return err("round takes 1 or 2 values"),
        },
        "pow" => {
            arity(2)?;
            x.powf(values[1])
        }
        "min" => values.iter().copied().fold(f64::INFINITY, f64::min),
        "max" => values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        "gcd" | "lcm" => {
            let ints = values.iter().map(|&v| whole(v, name)).collect::<Result<Vec<_>, _>>()?;
            let g = ints.iter().fold(0u64, |acc, &v| gcd(acc, v));
            if name == "gcd" {
                g as f64
            } else {
                ints.iter().try_fold(1u64, |acc, &v| {
                    if v == 0 { Some(0) } else { (acc / gcd(acc, v)).checked_mul(v) }
                })
                .ok_or_else(|| CalcError("The result is too large".into()))? as f64
            }
        }
        "isprime" => {
            arity(1)?;
            env.spend(1)?;
            if x.fract() != 0.0 || x < 2.0 {
                0.0
            } else if x > MAX_EXACT_INT {
                return err("isprime only works up to 2^53");
            } else if is_prime(x as u64) {
                1.0
            } else {
                0.0
            }
        }
        "prime" => {
            arity(1)?;
            if x.fract() != 0.0 || !(1.0..=MAX_PRIME_INDEX).contains(&x) {
                return err("prime(k) needs a whole number k from 1 to 100000");
            }
            env.spend(1)?;
            nth_prime(x as usize) as f64
        }
        _ => return err(format!("Unknown function '{name}'")),
    })
}

/// sum/prod of `body` for `var` = from..=to.
fn series(is_sum: bool, body: &Node, var: &str, from: &Node, to: &Node, env: &mut Env) -> Result<f64, CalcError> {
    let label = if is_sum { "sum" } else { "prod" };
    let from = eval(from, env)?;
    let to = eval(to, env)?;
    if from.fract() != 0.0 || to.fract() != 0.0 {
        return err(format!("{label}'s range must be whole numbers, e.g. {label}(k^2, k, 1, 10)"));
    }
    let mut acc = if is_sum { 0.0 } else { 1.0 };
    if to < from {
        return Ok(acc); // empty range
    }
    env.spend(((to - from) as u64).saturating_add(1))?;
    let mut k = from;
    while k <= to {
        env.vars.push((var.to_string(), k));
        let v = eval(body, env);
        env.vars.pop();
        let v = v?;
        acc = if is_sum { acc + v } else { acc * v };
        finite(acc)?;
        k += 1.0;
    }
    Ok(acc)
}

fn whole(v: f64, name: &str) -> Result<u64, CalcError> {
    if v.fract() != 0.0 || v < 0.0 || v > MAX_EXACT_INT {
        err(format!("{name} needs whole numbers that aren't negative"))
    } else {
        Ok(v as u64)
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn factorial(n: f64) -> Result<f64, CalcError> {
    if n < 0.0 || n.fract() != 0.0 {
        return err("Factorial needs a whole number that isn't negative");
    }
    if n > 170.0 {
        return err("The result is too large");
    }
    Ok((1..=n as u64).fold(1.0, |acc, k| acc * k as f64))
}

/// Deterministic Miller–Rabin for all 64-bit integers.
fn is_prime(n: u64) -> bool {
    if n < 2 {
        return false;
    }
    const SMALL: [u64; 12] = [2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37];
    for p in SMALL {
        if n % p == 0 {
            return n == p;
        }
    }
    let mul = |a: u64, b: u64| ((a as u128 * b as u128) % n as u128) as u64;
    let pow = |mut base: u64, mut exp: u64| {
        let mut result = 1u64;
        base %= n;
        while exp > 0 {
            if exp & 1 == 1 {
                result = mul(result, base);
            }
            base = mul(base, base);
            exp >>= 1;
        }
        result
    };
    let (mut d, mut s) = (n - 1, 0);
    while d % 2 == 0 {
        d /= 2;
        s += 1;
    }
    'witness: for a in SMALL {
        let mut x = pow(a, d);
        if x == 1 || x == n - 1 {
            continue;
        }
        for _ in 1..s {
            x = mul(x, x);
            if x == n - 1 {
                continue 'witness;
            }
        }
        return false;
    }
    true
}

thread_local! {
    /// Primes found so far, grown on demand by `nth_prime`.
    static PRIMES: RefCell<Vec<u64>> = RefCell::new(vec![2, 3]);
}

/// The k-th prime (1-based): nth_prime(1) = 2.
fn nth_prime(k: usize) -> u64 {
    PRIMES.with(|cell| {
        let mut primes = cell.borrow_mut();
        let mut candidate = *primes.last().unwrap_or(&3) + 2;
        while primes.len() < k {
            let limit = (candidate as f64).sqrt() as u64;
            if primes.iter().take_while(|&&p| p <= limit).all(|&p| candidate % p != 0) {
                primes.push(candidate);
            }
            candidate += 2;
        }
        primes[k - 1]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(s: &str) -> String {
        match evaluate(s) {
            Ok(v) => format_number(v),
            Err(e) => format!("ERR {e}"),
        }
    }

    #[test]
    fn basics_and_precedence() {
        assert_eq!(ev("50/60*13"), "10.8333333333");
        assert_eq!(ev("13*50/60"), "10.8333333333");
        assert_eq!(ev("2 + 3 * 4"), "14");
        assert_eq!(ev("(2 + 3) * 4"), "20");
        assert_eq!(ev("2^3^2"), "512");
        assert_eq!(ev("-2^2"), "-4");
        assert_eq!(ev("2^-1"), "0.5");
        assert_eq!(ev("10 mod 3"), "1");
        assert_eq!(ev("7 ÷ 2 × 3 − 1"), "9.5");
        assert_eq!(ev("2**10"), "1024");
    }

    #[test]
    fn floating_point_noise_is_hidden() {
        assert_eq!(ev("0.1 + 0.2"), "0.3");
        assert_eq!(ev("1/3"), "0.333333333333");
        assert_eq!(ev("-0"), "0");
    }

    #[test]
    fn numbers_constants_and_implicit_multiplication() {
        assert_eq!(ev("1e3"), "1000");
        assert_eq!(ev("2.5E-3"), "0.0025");
        assert_eq!(ev(".5 * 4"), "2");
        assert_eq!(ev("2pi"), ev("2*pi"));
        assert_eq!(ev("2e"), ev("2*e"));
        assert_eq!(ev("2(3+4)"), "14");
        assert_eq!(ev("π"), "3.14159265359");
    }

    #[test]
    fn percent_and_factorial() {
        assert_eq!(ev("20% * 50"), "10");
        assert_eq!(ev("200 * 15%"), "30");
        assert_eq!(ev("5!"), "120");
        assert_eq!(ev("0!"), "1");
    }

    #[test]
    fn functions() {
        assert_eq!(ev("sqrt(16)"), "4");
        assert_eq!(ev("√2 * √2"), "2");
        assert_eq!(ev("round(10.83333, 2)"), "10.83");
        assert_eq!(ev("round(2.5)"), "3");
        assert_eq!(ev("max(1, 7, 3) - min(4, 2)"), "5");
        assert_eq!(ev("log(1000)"), "3");
        assert_eq!(ev("log(8, 2)"), "3");
        assert_eq!(ev("ln(e)"), "1");
        assert_eq!(ev("pow(2, 8)"), "256");
        assert_eq!(ev("abs(-3) + floor(2.7) + ceil(2.1)"), "8");
        assert_eq!(ev("gcd(12, 18)"), "6");
        assert_eq!(ev("lcm(4, 6, 10)"), "60");
    }

    #[test]
    fn primes_series_and_conditions() {
        assert_eq!(ev("prime(1)"), "2");
        assert_eq!(ev("prime(100)"), "541");
        assert_eq!(ev("sum(prime(k), k, 1, 100)"), "24133");
        assert_eq!(ev("isprime(97) + isprime(91) + isprime(2) + isprime(1)"), "2");
        assert_eq!(ev("isprime(2147483647)"), "1"); // Mersenne prime 2^31 − 1
        // What the model actually tried: sums primes ≤ 100 (a different question)
        assert_eq!(ev("sum(iif(isprime(n), n, 0), n, 1, 100)"), "1060");
        assert_eq!(ev("sum(k^2, k, 1, 10)"), "385");
        assert_eq!(ev("prod(k, k, 1, 5)"), "120");
        assert_eq!(ev("sum(1, 2, 3)"), "6");
        assert_eq!(ev("sum(sum(i*j, j, 1, 3), i, 1, 3)"), "36");
        assert_eq!(ev("if(3 > 2, 10, 20)"), "10");
        assert_eq!(ev("if(1 == 1, 1, 1/0)"), "1"); // untaken branch isn't evaluated
        assert_eq!(ev("0.1 + 0.2 == 0.3"), "1");
        assert_eq!(ev("2 <= 2") + &ev("3 != 3"), "10");
    }

    #[test]
    fn booleans_and_logic() {
        // What the model sent for "sum of non-primes up to 100"
        assert_eq!(ev("sum(if(isprime(k) == false, k, 0), k, 1, 100)"), "3990");
        assert_eq!(ev("sum(if(not isprime(k), k, 0), k, 1, 100)"), "3990");
        assert_eq!(ev("sum(if(!isprime(k), k, 0), k, 1, 100)"), "3990");
        assert_eq!(ev("!0 + 3!"), "7"); // prefix not, postfix factorial
        assert_eq!(ev("true + true"), "2");
        assert_eq!(ev("False"), "0");
        assert_eq!(ev("1 and 0"), "0");
        assert_eq!(ev("isprime(3) && 5 > 2"), "1");
        assert_eq!(ev("false || true"), "1");
        assert_eq!(ev("not(isprime(4))"), "1");
        assert_eq!(ev("not 1 or 1"), "1"); // not binds tighter than or
        assert_eq!(ev("0 and 1/0"), "0"); // short-circuit
        assert_eq!(ev("10 mod 4 == 2 and 3 > 1"), "1");
        assert_eq!(ev("and 1"), "ERR 'and' needs a value on each side, e.g. 2 > 1 and 3 > 2");
    }

    #[test]
    fn big_and_small_numbers() {
        assert_eq!(ev("10^20"), "1e20");
        assert_eq!(ev("1/10^12"), "1e-12");
        assert_eq!(ev("123456789 * 1000"), "123456789000");
        assert_eq!(ev("2^50"), "1125899906842624");
    }

    #[test]
    fn errors_are_helpful() {
        assert_eq!(ev("1/0"), "ERR Division by zero");
        assert_eq!(ev("sqrt(-1)"), "ERR Square root of a negative number");
        assert_eq!(ev("(1+2"), "3"); // auto-closed
        assert_eq!(ev("sum(prime(k), k, 1, 100"), "24133"); // what the model actually sends
        assert_eq!(ev("2*(3+(4"), "14");
        assert_eq!(ev("(1+2 3"), "ERR Missing closing parenthesis");
        assert_eq!(ev("1+2)"), "ERR Unmatched closing parenthesis");
        assert_eq!(ev("2+"), "ERR The expression is incomplete");
        assert_eq!(ev("foo(2)"), "ERR Unknown function 'foo'");
        assert_eq!(ev("x + 1"), "ERR Unknown name 'x'");
        assert_eq!(ev("2 $ 3"), "ERR Unexpected character '$'");
        assert_eq!(ev(""), "ERR Enter an expression");
        assert_eq!(ev("1.2.3"), "ERR '1.2.3' is not a valid number");
        assert_eq!(ev("171!"), "ERR The result is too large");
        assert_eq!(ev("10^400"), "ERR The result is too large");
        assert_eq!(ev("2 3"), "ERR Unexpected number 3");
        assert_eq!(ev("2 = 2"), "ERR Use == to compare values");
        assert_eq!(ev("sum(k, k, 1, 10^9)"), "ERR This would take too long to calculate; try a smaller range");
        assert_eq!(ev("prime(0)"), "ERR prime(k) needs a whole number k from 1 to 100000");
        assert_eq!(ev("sum(k, k, 1.5, 3)"), "ERR sum's range must be whole numbers, e.g. sum(k^2, k, 1, 10)");
    }
}
