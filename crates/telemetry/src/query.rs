//! The Logs page's filter language (structured_logging.md 5.1 row 1), and
//! its translation to SQL over the log store.
//!
//! ```text
//! level >= warn and store.id = 's_1'
//! order.id = 'o_9' or message contains 'timeout'
//! not (service = 'engine') and has error
//! 'payment'                         -- a quoted string alone: message contains it
//! payment not seen                  -- words that don't parse: the same, for the whole text
//! ```
//!
//! Grammar, lowest precedence first:
//!
//! ```text
//! query      = or
//! or         = and ("or" and)*
//! and        = not ("and" not)*
//! not        = "not" not | atom
//! atom       = "(" or ")" | "has" field | field op value | field ("contains" | "like") string | string
//! op         = "=" | "==" | "!=" | "<>" | "<" | "<=" | ">" | ">="
//! value      = string | number | "true" | "false" | "null" | word
//! field      = word ("." word)*          -- level, service, target, message, trace_id, span_id, or an attribute
//! ```
//!
//! Keywords are case-insensitive. A bare word as a value is a string
//! (`network = Stagenet`), except for `level`, whose values are level names.
//!
//! SQL is never built from query text: field names become `json_extract`
//! paths passed as parameters (and are restricted to `[A-Za-z0-9_.]`
//! anyway), and every value is a parameter.

use std::fmt;

use rusqlite::types::Value as SqlValue;

/// A parsed query.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Compare {
        field: String,
        op: Op,
        value: Value,
    },
    Contains {
        field: String,
        text: String,
    },
    Like {
        field: String,
        pattern: String,
    },
    Has(String),
    /// Free text: the message contains it.
    Text(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Text(String),
    Integer(i64),
    Real(f64),
    Bool(bool),
    Null,
    /// A level name, only as `level`'s value.
    Level(Severity),
}

/// OpenTelemetry severity numbers for the five `tracing` levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Trace = 1,
    Debug = 5,
    Info = 9,
    Warn = 13,
    Error = 17,
}

impl Severity {
    pub fn from_name(name: &str) -> Option<Severity> {
        match name.to_ascii_lowercase().as_str() {
            "trace" => Some(Severity::Trace),
            "debug" => Some(Severity::Debug),
            "info" | "information" => Some(Severity::Info),
            "warn" | "warning" => Some(Severity::Warn),
            "error" => Some(Severity::Error),
            _ => None,
        }
    }

    pub fn from_number(number: i64) -> Severity {
        match number {
            ..=4 => Severity::Trace,
            5..=8 => Severity::Debug,
            9..=12 => Severity::Info,
            13..=16 => Severity::Warn,
            _ => Severity::Error,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Severity::Trace => "trace",
            Severity::Debug => "debug",
            Severity::Info => "info",
            Severity::Warn => "warn",
            Severity::Error => "error",
        }
    }

    /// As `tracing` writes it: `INFO`.
    pub fn upper(self) -> &'static str {
        match self {
            Severity::Trace => "TRACE",
            Severity::Debug => "DEBUG",
            Severity::Info => "INFO",
            Severity::Warn => "WARN",
            Severity::Error => "ERROR",
        }
    }
}

impl From<&tracing::Level> for Severity {
    fn from(level: &tracing::Level) -> Self {
        match *level {
            tracing::Level::TRACE => Severity::Trace,
            tracing::Level::DEBUG => Severity::Debug,
            tracing::Level::INFO => Severity::Info,
            tracing::Level::WARN => Severity::Warn,
            tracing::Level::ERROR => Severity::Error,
        }
    }
}

/// A query that doesn't parse: what is wrong, and where (character
/// offsets into the query).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub start: usize,
    pub end: usize,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (at character {})", self.message, self.start + 1)
    }
}

/// Longest query accepted, in characters.
pub const MAX_QUERY_CHARS: usize = 4096;

/// Deepest nesting of brackets and `not`s accepted. The parser, and the
/// expression it builds (its display, SQL and drop), recurse once per
/// level: a query of a few thousand `(` would otherwise overflow the stack
/// and abort the process.
pub const MAX_DEPTH: usize = 64;

/// Parses a query. Empty text is `None` (everything matches).
pub fn parse(text: &str) -> Result<Option<Expr>, ParseError> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    let len = text.chars().count();
    if len > MAX_QUERY_CHARS {
        return Err(ParseError {
            message: format!("a query can be at most {MAX_QUERY_CHARS} characters"),
            start: MAX_QUERY_CHARS,
            end: len,
        });
    }
    match Parser::new(text).and_then(|mut p| p.query()) {
        Ok(expr) => Ok(Some(expr)),
        // Plain words that aren't a query are a text search, as in Seq -
        // unless they look like an attempt at one.
        Err(_) if looks_like_words(text) => Ok(Some(Expr::Text(text.trim().to_string()))),
        Err(e) => Err(e),
    }
}

fn looks_like_words(text: &str) -> bool {
    !text.contains(|c: char| "=<>!()'\"".contains(c))
        && !text
            .split_whitespace()
            .any(|w| ["contains", "like", "has"].iter().any(|k| is_keyword(w, k)))
}

// --- Tokens ----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(String),
    Str(String),
    Integer(i64),
    Real(f64),
    Op(Op),
    Open,
    Close,
}

#[derive(Debug, Clone)]
struct Spanned {
    token: Token,
    start: usize,
    end: usize,
}

fn tokenize(text: &str) -> Result<Vec<Spanned>, ParseError> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    let error = |message: &str, start: usize, end: usize| ParseError {
        message: message.to_string(),
        start,
        end,
    };
    while i < chars.len() {
        let c = chars[i];
        let start = i;
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let token = match c {
            '(' => {
                i += 1;
                Token::Open
            }
            ')' => {
                i += 1;
                Token::Close
            }
            '=' => {
                i += if chars.get(i + 1) == Some(&'=') { 2 } else { 1 };
                Token::Op(Op::Eq)
            }
            '!' if chars.get(i + 1) == Some(&'=') => {
                i += 2;
                Token::Op(Op::Ne)
            }
            '<' => match chars.get(i + 1) {
                Some('=') => {
                    i += 2;
                    Token::Op(Op::Le)
                }
                Some('>') => {
                    i += 2;
                    Token::Op(Op::Ne)
                }
                _ => {
                    i += 1;
                    Token::Op(Op::Lt)
                }
            },
            '>' => {
                if chars.get(i + 1) == Some(&'=') {
                    i += 2;
                    Token::Op(Op::Ge)
                } else {
                    i += 1;
                    Token::Op(Op::Gt)
                }
            }
            '\'' | '"' => {
                let quote = c;
                let mut value = String::new();
                i += 1;
                loop {
                    match chars.get(i) {
                        None => {
                            return Err(error(
                                "this string has no closing quote",
                                start,
                                chars.len(),
                            ))
                        }
                        Some('\\') => {
                            match chars.get(i + 1) {
                                Some(&next) => value.push(next),
                                None => {
                                    return Err(error(
                                        "this string has no closing quote",
                                        start,
                                        chars.len(),
                                    ))
                                }
                            }
                            i += 2;
                        }
                        Some(&q) if q == quote => {
                            i += 1;
                            break;
                        }
                        Some(&other) => {
                            value.push(other);
                            i += 1;
                        }
                    }
                }
                Token::Str(value)
            }
            c if c.is_ascii_digit()
                || (c == '-' && chars.get(i + 1).is_some_and(char::is_ascii_digit)) =>
            {
                i += 1;
                while chars
                    .get(i)
                    .is_some_and(|c| c.is_ascii_digit() || *c == '.')
                {
                    i += 1;
                }
                let number: String = chars[start..i].iter().collect();
                if let Ok(n) = number.parse::<i64>() {
                    Token::Integer(n)
                } else if let Ok(n) = number.parse::<f64>() {
                    Token::Real(n)
                } else {
                    return Err(error("not a number", start, i));
                }
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                while chars
                    .get(i)
                    .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
                {
                    i += 1;
                }
                Token::Word(chars[start..i].iter().collect())
            }
            _ => return Err(error(&format!("unexpected '{c}'"), start, start + 1)),
        };
        tokens.push(Spanned {
            token,
            start,
            end: i,
        });
    }
    Ok(tokens)
}

// --- Parser ----------------------------------------------------------------

struct Parser {
    tokens: Vec<Spanned>,
    at: usize,
    len: usize,
    /// Brackets and `not`s open around the current position.
    depth: usize,
}

fn is_keyword(word: &str, keyword: &str) -> bool {
    word.eq_ignore_ascii_case(keyword)
}

const KEYWORDS: &[&str] = &[
    "and", "or", "not", "has", "contains", "like", "true", "false", "null",
];

impl Parser {
    fn new(text: &str) -> Result<Parser, ParseError> {
        Ok(Parser {
            tokens: tokenize(text)?,
            at: 0,
            len: text.chars().count(),
            depth: 0,
        })
    }

    fn peek(&self) -> Option<&Spanned> {
        self.tokens.get(self.at)
    }

    fn peek_keyword(&self, keyword: &str) -> bool {
        matches!(self.peek(), Some(Spanned { token: Token::Word(w), .. }) if is_keyword(w, keyword))
    }

    fn next(&mut self) -> Option<Spanned> {
        let token = self.tokens.get(self.at).cloned();
        self.at += 1;
        token
    }

    fn error_here(&self, message: &str) -> ParseError {
        match self.peek() {
            Some(t) => ParseError {
                message: message.to_string(),
                start: t.start,
                end: t.end,
            },
            None => ParseError {
                message: message.to_string(),
                start: self.len,
                end: self.len,
            },
        }
    }

    fn query(&mut self) -> Result<Expr, ParseError> {
        let expr = self.or()?;
        if self.peek().is_some() {
            return Err(self.error_here("expected 'and', 'or' or the end of the query"));
        }
        Ok(expr)
    }

    fn or(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.and()?;
        while self.peek_keyword("or") {
            self.next();
            left = Expr::Or(Box::new(left), Box::new(self.and()?));
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.not()?;
        while self.peek_keyword("and") {
            self.next();
            left = Expr::And(Box::new(left), Box::new(self.not()?));
        }
        Ok(left)
    }

    /// One more level of nesting, refused past [`MAX_DEPTH`].
    fn descend(&mut self) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.error_here(&format!(
                "a query can nest brackets and nots at most {MAX_DEPTH} deep"
            )));
        }
        Ok(())
    }

    fn not(&mut self) -> Result<Expr, ParseError> {
        if self.peek_keyword("not") {
            self.next();
            self.descend()?;
            let inner = self.not()?;
            self.depth -= 1;
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.atom()
    }

    fn atom(&mut self) -> Result<Expr, ParseError> {
        let Some(first) = self.next() else {
            return Err(self.error_here("expected a condition"));
        };
        match first.token {
            Token::Open => {
                self.descend()?;
                let inner = self.or()?;
                self.depth -= 1;
                match self.next() {
                    Some(Spanned {
                        token: Token::Close,
                        ..
                    }) => Ok(inner),
                    _ => Err(ParseError {
                        message: "this bracket is never closed".into(),
                        start: first.start,
                        end: first.end,
                    }),
                }
            }
            Token::Str(text) => Ok(Expr::Text(text)),
            Token::Word(word) if is_keyword(&word, "has") => {
                let field = self.field()?;
                Ok(Expr::Has(field))
            }
            Token::Word(word) if !KEYWORDS.iter().any(|k| is_keyword(&word, k)) => {
                let field = canonical_field(&word);
                self.after_field(field)
            }
            _ => Err(ParseError {
                message: "expected a property name, a quoted text or '('".into(),
                start: first.start,
                end: first.end,
            }),
        }
    }

    fn field(&mut self) -> Result<String, ParseError> {
        match self.next() {
            Some(Spanned {
                token: Token::Word(word),
                ..
            }) if !KEYWORDS.iter().any(|k| is_keyword(&word, k)) => Ok(canonical_field(&word)),
            _ => {
                self.at -= 1;
                Err(self.error_here("expected a property name"))
            }
        }
    }

    fn after_field(&mut self, field: String) -> Result<Expr, ParseError> {
        let Some(next) = self.next() else {
            return Err(self
                .error_here("expected =, !=, <, >, 'contains' or 'like' after the property name"));
        };
        match next.token {
            Token::Op(op) => {
                let value = self.value(&field, op)?;
                Ok(Expr::Compare { field, op, value })
            }
            Token::Word(word) if is_keyword(&word, "contains") || is_keyword(&word, "like") => {
                let Some(Spanned {
                    token: Token::Str(text),
                    ..
                }) = self.next()
                else {
                    self.at -= 1;
                    return Err(self.error_here("expected a quoted text"));
                };
                if is_keyword(&word, "contains") {
                    Ok(Expr::Contains { field, text })
                } else {
                    Ok(Expr::Like {
                        field,
                        pattern: text,
                    })
                }
            }
            _ => Err(ParseError {
                message: "expected =, !=, <, >, 'contains' or 'like' after the property name"
                    .into(),
                start: next.start,
                end: next.end,
            }),
        }
    }

    fn value(&mut self, field: &str, op: Op) -> Result<Value, ParseError> {
        let Some(token) = self.next() else {
            return Err(self.error_here("expected a value"));
        };
        let value = match token.token {
            Token::Str(s) => Value::Text(s),
            Token::Integer(n) => Value::Integer(n),
            Token::Real(n) => Value::Real(n),
            Token::Word(w) if is_keyword(&w, "true") => Value::Bool(true),
            Token::Word(w) if is_keyword(&w, "false") => Value::Bool(false),
            Token::Word(w) if is_keyword(&w, "null") => Value::Null,
            Token::Word(w) if !KEYWORDS.iter().any(|k| is_keyword(&w, k)) => Value::Text(w),
            _ => {
                return Err(ParseError {
                    message: "expected a value".into(),
                    start: token.start,
                    end: token.end,
                })
            }
        };
        if field == "level" {
            let severity = match &value {
                Value::Text(name) => Severity::from_name(name),
                _ => None,
            };
            return match severity {
                Some(s) => Ok(Value::Level(s)),
                None => Err(ParseError {
                    message: "a level is one of trace, debug, info, warn, error".into(),
                    start: token.start,
                    end: token.end,
                }),
            };
        }
        if value == Value::Null && !matches!(op, Op::Eq | Op::Ne) {
            return Err(ParseError {
                message: "null can only be compared with = or !=".into(),
                start: token.start,
                end: token.end,
            });
        }
        Ok(value)
    }
}

/// Built-in names in one spelling.
fn canonical_field(word: &str) -> String {
    match word.to_ascii_lowercase().as_str() {
        "level" | "severity" => "level".into(),
        "service" => "service".into(),
        "target" => "target".into(),
        "message" | "msg" => "message".into(),
        "trace_id" | "traceid" => "trace_id".into(),
        "span_id" | "spanid" => "span_id".into(),
        _ => word.to_string(),
    }
}

// --- Printing --------------------------------------------------------------

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Op::Eq => "=",
            Op::Ne => "!=",
            Op::Lt => "<",
            Op::Le => "<=",
            Op::Gt => ">",
            Op::Ge => ">=",
        })
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Text(s) => f.write_str(&quote(s)),
            Value::Integer(n) => write!(f, "{n}"),
            Value::Real(n) => write!(f, "{n}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Null => f.write_str("null"),
            Value::Level(s) => f.write_str(s.name()),
        }
    }
}

impl Expr {
    fn precedence(&self) -> u8 {
        match self {
            Expr::Or(..) => 1,
            Expr::And(..) => 2,
            _ => 3,
        }
    }

    fn write_child(&self, child: &Expr, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if child.precedence() < self.precedence() {
            write!(f, "({child})")
        } else {
            write!(f, "{child}")
        }
    }
}

/// Prints a query back as text that parses to the same query.
impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expr::And(a, b) => {
                self.write_child(a, f)?;
                f.write_str(" and ")?;
                self.write_child(b, f)
            }
            Expr::Or(a, b) => {
                self.write_child(a, f)?;
                f.write_str(" or ")?;
                self.write_child(b, f)
            }
            Expr::Not(inner) => {
                f.write_str("not ")?;
                if inner.precedence() < 3 {
                    write!(f, "({inner})")
                } else {
                    write!(f, "{inner}")
                }
            }
            Expr::Compare { field, op, value } => write!(f, "{field} {op} {value}"),
            Expr::Contains { field, text } => write!(f, "{field} contains {}", quote(text)),
            Expr::Like { field, pattern } => write!(f, "{field} like {}", quote(pattern)),
            Expr::Has(field) => write!(f, "has {field}"),
            Expr::Text(text) => f.write_str(&quote(text)),
        }
    }
}

/// `query and extra`, for the Logs page's "find" links; `extra` alone when
/// there is no query yet.
pub fn and_also(query: Option<&Expr>, extra: Expr) -> Expr {
    match query {
        Some(q) => Expr::And(Box::new(q.clone()), Box::new(extra)),
        None => extra,
    }
}

// --- SQL -------------------------------------------------------------------

/// The column or expression a field is read from, over the `logs` table.
fn column(field: &str, params: &mut Vec<SqlValue>) -> String {
    match field {
        "level" => "level".into(),
        "service" | "target" | "message" | "trace_id" | "span_id" => field.into(),
        // Generated, indexed columns (see `store::SCHEMA`).
        "store.id" => "store_id".into(),
        "order.id" => "order_id".into(),
        "session.id" => "session_id".into(),
        _ => {
            params.push(SqlValue::Text(json_path(field)));
            "json_extract(attributes, ?)".into()
        }
    }
}

/// `$."store.id"`: the whole name is one key (attribute names contain dots).
pub(crate) fn json_path(field: &str) -> String {
    format!("$.\"{}\"", field.replace('"', ""))
}

fn escape_like(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len() + 2);
    for c in text.chars() {
        if matches!(c, '%' | '_' | '\\') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

impl Expr {
    /// A SQL condition over the `logs` table, with its parameters appended
    /// to `params` in order.
    pub fn to_sql(&self, params: &mut Vec<SqlValue>) -> String {
        match self {
            Expr::And(a, b) => format!("({} AND {})", a.to_sql(params), b.to_sql(params)),
            Expr::Or(a, b) => format!("({} OR {})", a.to_sql(params), b.to_sql(params)),
            // `IS NOT 1` rather than `NOT`, so a condition on a missing
            // property (NULL) counts as not matching, and `not` of it as
            // matching, as a reader would expect.
            Expr::Not(inner) => format!("(({}) IS NOT 1)", inner.to_sql(params)),
            Expr::Compare { field, op, value } => {
                let column = column(field, params);
                match value {
                    Value::Null if *op == Op::Eq => format!("{column} IS NULL"),
                    Value::Null => format!("{column} IS NOT NULL"),
                    _ => {
                        params.push(match value {
                            Value::Text(s) => SqlValue::Text(s.clone()),
                            Value::Integer(n) => SqlValue::Integer(*n),
                            Value::Real(n) => SqlValue::Real(*n),
                            Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
                            Value::Level(s) => SqlValue::Integer(*s as i64),
                            Value::Null => SqlValue::Null,
                        });
                        let op = match op {
                            Op::Eq => "=",
                            Op::Ne => "!=",
                            Op::Lt => "<",
                            Op::Le => "<=",
                            Op::Gt => ">",
                            Op::Ge => ">=",
                        };
                        format!("{column} {op} ?")
                    }
                }
            }
            Expr::Contains { field, text } => {
                let column = column(field, params);
                params.push(SqlValue::Text(format!("%{}%", escape_like(text))));
                format!("{column} LIKE ? ESCAPE '\\'")
            }
            Expr::Like { field, pattern } => {
                let column = column(field, params);
                params.push(SqlValue::Text(pattern.clone()));
                format!("{column} LIKE ?")
            }
            Expr::Has(field) => {
                let column = column(field, params);
                format!("{column} IS NOT NULL")
            }
            Expr::Text(text) => {
                params.push(SqlValue::Text(format!("%{}%", escape_like(text))));
                "message LIKE ? ESCAPE '\\'".to_string()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(text: &str) -> Expr {
        parse(text).unwrap().unwrap()
    }

    /// Deep nesting is refused, not recursed into until the stack runs out.
    #[test]
    fn nesting_past_the_limit_is_an_error_not_a_stack_overflow() {
        let deep = format!("{}level = warn{}", "(".repeat(5000), ")".repeat(5000));
        assert!(parse(&deep).is_err());
        let nots = format!("{}level = warn", "not ".repeat(5000));
        assert!(parse(&nots).is_err());
        let fine = format!(
            "{}level = warn{}",
            "(".repeat(MAX_DEPTH),
            ")".repeat(MAX_DEPTH)
        );
        assert!(parse(&fine).is_ok());
        assert!(parse(&"a".repeat(MAX_QUERY_CHARS + 1)).is_err());
    }

    #[test]
    fn precedence_and_printing_round_trip() {
        for (input, printed) in [
            (
                "level >= warn and store.id = 's_1'",
                "level >= warn and store.id = 's_1'",
            ),
            ("a = 1 or b = 2 and c = 3", "a = 1 or b = 2 and c = 3"),
            ("(a = 1 or b = 2) and c = 3", "(a = 1 or b = 2) and c = 3"),
            ("not (a = 1 or b = 2)", "not (a = 1 or b = 2)"),
            ("NOT a = 1 AND has error", "not a = 1 and has error"),
            ("msg contains \"it's\"", "message contains 'it\\'s'"),
            ("network = Stagenet", "network = 'Stagenet'"),
            ("attempts > 2.5", "attempts > 2.5"),
            ("x != null", "x != null"),
            ("x <> -3", "x != -3"),
            ("'payment'", "'payment'"),
            ("target like 'engine::%'", "target like 'engine::%'"),
        ] {
            let expr = parse(input)
                .unwrap_or_else(|e| panic!("{input}: {e}"))
                .unwrap();
            let text = expr.to_string();
            assert_eq!(text, printed, "{input}");
            assert_eq!(
                p(&text),
                expr,
                "printing then parsing gives the same query: {input}"
            );
        }
    }

    #[test]
    fn words_that_are_not_a_query_are_a_text_search() {
        assert_eq!(p("payment not seen"), Expr::Text("payment not seen".into()));
        assert_eq!(parse("   ").unwrap(), None);
    }

    #[test]
    fn errors_say_what_and_where() {
        for (input, message, start) in [
            ("level = loud", "a level is one of", 8),
            ("a = 'open", "no closing quote", 4),
            ("(a = 1", "never closed", 0),
            ("a = 1 b = 2", "expected 'and', 'or'", 6),
            ("a =", "expected a value", 3),
            ("a > null", "null can only", 4),
            ("a = 1 and", "expected a condition", 9),
            ("a = ~", "unexpected '~'", 4),
            ("a contains 3", "expected a quoted text", 11),
        ] {
            let error = parse(input).unwrap_err();
            assert!(error.message.contains(message), "{input}: {error:?}");
            assert_eq!(error.start, start, "{input}: {error:?}");
        }
    }

    #[test]
    fn sql_uses_parameters_for_every_value_and_field_name() {
        let mut params = Vec::new();
        let sql = p(
            "store.id = 's_1' and network = 'x\\' OR 1=1 --' and level >= warn and not has error",
        )
        .to_sql(&mut params);
        assert_eq!(
            sql,
            "(((store_id = ? AND json_extract(attributes, ?) = ?) AND level >= ?) AND ((json_extract(attributes, ?) IS NOT NULL) IS NOT 1))"
        );
        assert_eq!(
            params,
            vec![
                SqlValue::Text("s_1".into()),
                SqlValue::Text("$.\"network\"".into()),
                SqlValue::Text("x' OR 1=1 --".into()),
                SqlValue::Integer(13),
                SqlValue::Text("$.\"error\"".into()),
            ]
        );
    }

    #[test]
    fn text_searches_escape_like_wildcards() {
        let mut params = Vec::new();
        assert_eq!(
            p("'100%_done'").to_sql(&mut params),
            "message LIKE ? ESCAPE '\\'"
        );
        assert_eq!(params, vec![SqlValue::Text("%100\\%\\_done%".into())]);
    }
}
