//! The language the local backend evaluates: its tokens and its shape.
//!
//! It is small on purpose, and the purpose is [the crate note](crate): a
//! program has to be stoppable mid-loop, which in this process means an
//! evaluator that agrees to stop, which means an evaluator this crate owns.
//!
//! What a program is:
//!
//! ```text
//! let total = 0;
//! let items = tools.list({ kind: "open" });
//! let i = 0;
//! while (i < len(items)) {
//!   let item = items[i];
//!   if (item.size > 10) { total = total + item.size; log(item.name); }
//!   i = i + 1;
//! }
//! return { total: total, seen: i };
//! ```
//!
//! Statements are `let`, assignment, `if`/`else`, `while`, `return`, a block,
//! and a bare expression. Expressions are JSON literals, identifiers, member
//! and index access, calls, the usual arithmetic, comparison and logic, and
//! nothing else. There is no user-defined function, no closure, no `for`, no
//! exception handling: every one of those is a thing to add when a program
//! needs it, and each one is more surface for a hostile program to stand on.
//!
//! Parsing is one pass with no backtracking, so a pathological program costs
//! its own length and not more.

/// One token, and where it started - the offset is what a parse failure points
/// at, since a model correcting its own program needs to be told where.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub at: usize,
    pub kind: Tok,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Number(f64),
    Text(String),
    Name(String),
    True,
    False,
    Null,
    Let,
    If,
    Else,
    While,
    Return,
    /// One of `( ) { } [ ] , : ; . + - * / % ! = == != < <= > >= && ||`
    Sym(&'static str),
    End,
}

/// The symbols, longest first: `==` must be read before `=`.
const SYMBOLS: &[&str] = &[
    "==", "!=", "<=", ">=", "&&", "||", "(", ")", "{", "}", "[", "]", ",", ":", ";", ".", "+", "-",
    "*", "/", "%", "!", "=", "<", ">",
];

/// Turn a program into tokens, or say where it stopped making sense.
pub fn tokens(source: &str) -> Result<Vec<Token>, String> {
    let bytes = source.as_bytes();
    let mut out: Vec<Token> = Vec::new();
    let mut at = 0usize;

    while at < bytes.len() {
        let byte = bytes[at];
        if byte.is_ascii_whitespace() {
            at += 1;
            continue;
        }
        // A comment runs to the end of its line. `//` is what every language
        // this seam is portable to spells it as, bar Python.
        if source[at..].starts_with("//") || byte == b'#' {
            at += source[at..].find('\n').unwrap_or(source.len() - at);
            continue;
        }
        if byte.is_ascii_digit() {
            let start = at;
            while at < bytes.len() && (bytes[at].is_ascii_digit() || bytes[at] == b'.') {
                at += 1;
            }
            // An exponent, so a program can write a number big enough to stop
            // being lossless JSON - which is a case, not a curiosity.
            if at < bytes.len() && (bytes[at] == b'e' || bytes[at] == b'E') {
                at += 1;
                if at < bytes.len() && (bytes[at] == b'+' || bytes[at] == b'-') {
                    at += 1;
                }
                while at < bytes.len() && bytes[at].is_ascii_digit() {
                    at += 1;
                }
            }
            let text = &source[start..at];
            let number: f64 = text
                .parse()
                .map_err(|_| format!("{text:?} at {start} is not a number"))?;
            out.push(Token {
                at: start,
                kind: Tok::Number(number),
            });
            continue;
        }
        if byte == b'"' || byte == b'\'' {
            let quote = byte;
            let start = at;
            at += 1;
            let mut text = String::new();
            loop {
                if at >= bytes.len() {
                    return Err(format!("the string starting at {start} is never closed"));
                }
                match bytes[at] {
                    b'\\' if at + 1 < bytes.len() => {
                        let escaped = match bytes[at + 1] {
                            b'n' => '\n',
                            b't' => '\t',
                            b'r' => '\r',
                            other => char::from(other),
                        };
                        text.push(escaped);
                        at += 2;
                    }
                    end if end == quote => {
                        at += 1;
                        break;
                    }
                    _ => {
                        let char_len = source[at..].chars().next().map_or(1, char::len_utf8);
                        text.push_str(&source[at..at + char_len]);
                        at += char_len;
                    }
                }
            }
            out.push(Token {
                at: start,
                kind: Tok::Text(text),
            });
            continue;
        }
        if byte.is_ascii_alphabetic() || byte == b'_' {
            let start = at;
            while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
                at += 1;
            }
            let word = &source[start..at];
            let kind = match word {
                "true" => Tok::True,
                "false" => Tok::False,
                "null" => Tok::Null,
                "let" => Tok::Let,
                "if" => Tok::If,
                "else" => Tok::Else,
                "while" => Tok::While,
                "return" => Tok::Return,
                name => Tok::Name(name.to_string()),
            };
            out.push(Token { at: start, kind });
            continue;
        }
        match SYMBOLS
            .iter()
            .find(|symbol| source[at..].starts_with(**symbol))
        {
            Some(symbol) => {
                out.push(Token {
                    at,
                    kind: Tok::Sym(symbol),
                });
                at += symbol.len();
            }
            None => {
                return Err(format!(
                    "{:?} at {at} is not part of this language",
                    char::from(byte)
                ))
            }
        }
    }

    out.push(Token {
        at: source.len(),
        kind: Tok::End,
    });
    Ok(out)
}

/// One statement.
#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Let {
        name: String,
        value: Expr,
    },
    Assign {
        target: Expr,
        value: Expr,
    },
    If {
        test: Expr,
        then: Vec<Stmt>,
        other: Vec<Stmt>,
    },
    While {
        test: Expr,
        body: Vec<Stmt>,
    },
    Return(Option<Expr>),
    Eval(Expr),
}

/// One expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Number(f64),
    Text(String),
    Bool(bool),
    Null,
    Name(String),
    List(Vec<Expr>),
    Map(Vec<(String, Expr)>),
    Member {
        of: Box<Expr>,
        name: String,
    },
    Index {
        of: Box<Expr>,
        at: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    Unary {
        op: &'static str,
        of: Box<Expr>,
    },
    Binary {
        op: &'static str,
        left: Box<Expr>,
        right: Box<Expr>,
    },
}

/// Parse a program, or say what stopped it.
pub fn parse(source: &str) -> Result<Vec<Stmt>, String> {
    let mut parser = Parser {
        tokens: tokens(source)?,
        at: 0,
    };
    let mut program = Vec::new();
    while !parser.done() {
        program.push(parser.statement()?);
    }
    Ok(program)
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
}

impl Parser {
    fn done(&self) -> bool {
        matches!(self.peek(), Tok::End)
    }

    fn peek(&self) -> &Tok {
        &self.tokens[self.at.min(self.tokens.len() - 1)].kind
    }

    fn next(&mut self) -> Tok {
        let kind = self.tokens[self.at.min(self.tokens.len() - 1)].kind.clone();
        if self.at < self.tokens.len() - 1 {
            self.at += 1;
        }
        kind
    }

    fn here(&self) -> usize {
        self.tokens[self.at.min(self.tokens.len() - 1)].at
    }

    fn eat(&mut self, symbol: &str) -> bool {
        if matches!(self.peek(), Tok::Sym(s) if *s == symbol) {
            self.next();
            return true;
        }
        false
    }

    fn expect(&mut self, symbol: &str) -> Result<(), String> {
        if self.eat(symbol) {
            return Ok(());
        }
        Err(format!(
            "expected {symbol:?} at offset {}, found {:?}",
            self.here(),
            self.peek()
        ))
    }

    fn statement(&mut self) -> Result<Stmt, String> {
        match self.peek().clone() {
            Tok::Let => {
                self.next();
                let Tok::Name(name) = self.next() else {
                    return Err(format!("`let` needs a name at offset {}", self.here()));
                };
                self.expect("=")?;
                let value = self.expression()?;
                self.expect(";")?;
                Ok(Stmt::Let { name, value })
            }
            Tok::If => {
                self.next();
                self.expect("(")?;
                let test = self.expression()?;
                self.expect(")")?;
                let then = self.block()?;
                let other = if matches!(self.peek(), Tok::Else) {
                    self.next();
                    // `else if` chains without needing braces around the if.
                    if matches!(self.peek(), Tok::If) {
                        vec![self.statement()?]
                    } else {
                        self.block()?
                    }
                } else {
                    Vec::new()
                };
                Ok(Stmt::If { test, then, other })
            }
            Tok::While => {
                self.next();
                self.expect("(")?;
                let test = self.expression()?;
                self.expect(")")?;
                let body = self.block()?;
                Ok(Stmt::While { test, body })
            }
            Tok::Return => {
                self.next();
                if self.eat(";") {
                    return Ok(Stmt::Return(None));
                }
                let value = self.expression()?;
                self.expect(";")?;
                Ok(Stmt::Return(Some(value)))
            }
            _ => {
                let first = self.expression()?;
                if self.eat("=") {
                    let value = self.expression()?;
                    self.expect(";")?;
                    // Only a place can be assigned to. `1 = 2;` parses as an
                    // expression and is refused here, where the message can
                    // say what a place is.
                    return match &first {
                        Expr::Name(_) | Expr::Member { .. } | Expr::Index { .. } => {
                            Ok(Stmt::Assign {
                                target: first,
                                value,
                            })
                        }
                        _ => {
                            Err("only a name, a member or an index can be assigned to".to_string())
                        }
                    };
                }
                self.expect(";")?;
                Ok(Stmt::Eval(first))
            }
        }
    }

    fn block(&mut self) -> Result<Vec<Stmt>, String> {
        self.expect("{")?;
        let mut body = Vec::new();
        while !self.eat("}") {
            if self.done() {
                return Err("a block is never closed".to_string());
            }
            body.push(self.statement()?);
        }
        Ok(body)
    }

    fn expression(&mut self) -> Result<Expr, String> {
        self.binary(0)
    }

    /// Precedence climbing: one level per row of the table.
    fn binary(&mut self, level: usize) -> Result<Expr, String> {
        const LEVELS: &[&[&str]] = &[
            &["||"],
            &["&&"],
            &["==", "!=", "<", "<=", ">", ">="],
            &["+", "-"],
            &["*", "/", "%"],
        ];
        if level == LEVELS.len() {
            return self.unary();
        }
        let mut left = self.binary(level + 1)?;
        while let Tok::Sym(symbol) = self.peek() {
            let Some(op) = LEVELS[level].iter().find(|op| *op == symbol) else {
                break;
            };
            let op = *op;
            self.next();
            let right = self.binary(level + 1)?;
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        for op in ["-", "!"] {
            if self.eat(op) {
                let of = self.unary()?;
                return Ok(Expr::Unary {
                    op: if op == "-" { "-" } else { "!" },
                    of: Box::new(of),
                });
            }
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr, String> {
        let mut of = self.primary()?;
        loop {
            if self.eat(".") {
                let Tok::Name(name) = self.next() else {
                    return Err(format!("a member name follows `.` at {}", self.here()));
                };
                of = Expr::Member {
                    of: Box::new(of),
                    name,
                };
            } else if self.eat("[") {
                let at = self.expression()?;
                self.expect("]")?;
                of = Expr::Index {
                    of: Box::new(of),
                    at: Box::new(at),
                };
            } else if self.eat("(") {
                let mut args = Vec::new();
                while !self.eat(")") {
                    args.push(self.expression()?);
                    if !self.eat(",") {
                        self.expect(")")?;
                        break;
                    }
                }
                of = Expr::Call {
                    callee: Box::new(of),
                    args,
                };
            } else {
                return Ok(of);
            }
        }
    }

    fn primary(&mut self) -> Result<Expr, String> {
        let at = self.here();
        match self.next() {
            Tok::Number(value) => Ok(Expr::Number(value)),
            Tok::Text(value) => Ok(Expr::Text(value)),
            Tok::True => Ok(Expr::Bool(true)),
            Tok::False => Ok(Expr::Bool(false)),
            Tok::Null => Ok(Expr::Null),
            Tok::Name(name) => Ok(Expr::Name(name)),
            Tok::Sym("(") => {
                let inner = self.expression()?;
                self.expect(")")?;
                Ok(inner)
            }
            Tok::Sym("[") => {
                let mut items = Vec::new();
                while !self.eat("]") {
                    items.push(self.expression()?);
                    if !self.eat(",") {
                        self.expect("]")?;
                        break;
                    }
                }
                Ok(Expr::List(items))
            }
            Tok::Sym("{") => {
                let mut entries = Vec::new();
                while !self.eat("}") {
                    let key = match self.next() {
                        Tok::Name(name) => name,
                        Tok::Text(text) => text,
                        other => {
                            return Err(format!("a key is a name or a string, found {other:?}"))
                        }
                    };
                    self.expect(":")?;
                    entries.push((key, self.expression()?));
                    if !self.eat(",") {
                        self.expect("}")?;
                        break;
                    }
                }
                Ok(Expr::Map(entries))
            }
            other => Err(format!("{other:?} at offset {at} does not start a value")),
        }
    }
}
