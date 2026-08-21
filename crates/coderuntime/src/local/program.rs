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
///
/// One scanner per kind of thing a program can start with, so each of them is
/// readable on its own and this is only the dispatch.
pub fn tokens(source: &str) -> Result<Vec<Token>, String> {
    let bytes = source.as_bytes();
    let mut out: Vec<Token> = Vec::new();
    let mut at = 0usize;

    while at < bytes.len() {
        at = match skip_trivia(source, at) {
            Some(next) => next,
            None => at,
        };
        if at >= bytes.len() {
            break;
        }
        let (token, next) = match bytes[at] {
            byte if byte.is_ascii_digit() => scan_number(source, at)?,
            b'"' | b'\'' => scan_text(source, at)?,
            byte if byte.is_ascii_alphabetic() || byte == b'_' => scan_word(source, at),
            _ => scan_symbol(source, at)?,
        };
        out.push(token);
        at = next;
    }

    out.push(Token {
        at: source.len(),
        kind: Tok::End,
    });
    Ok(out)
}

/// Skip whitespace and comments. `None` when there was nothing to skip.
fn skip_trivia(source: &str, from: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut at = from;
    loop {
        let before = at;
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        // A comment runs to the end of its line. `//` is what every language
        // this seam is portable to spells it as, bar Python, which is why `#`
        // is read the same way.
        if at < bytes.len() && (source[at..].starts_with("//") || bytes[at] == b'#') {
            at += source[at..].find('\n').unwrap_or(source.len() - at);
        }
        if at == before {
            return (at != from).then_some(at);
        }
    }
}

/// A number, including an exponent - so a program can write one big enough to
/// stop being lossless JSON, which is a case rather than a curiosity.
fn scan_number(source: &str, from: usize) -> Result<(Token, usize), String> {
    let bytes = source.as_bytes();
    let mut at = from;
    while at < bytes.len() && (bytes[at].is_ascii_digit() || bytes[at] == b'.') {
        at += 1;
    }
    if at < bytes.len() && (bytes[at] == b'e' || bytes[at] == b'E') {
        at += 1;
        if at < bytes.len() && (bytes[at] == b'+' || bytes[at] == b'-') {
            at += 1;
        }
        while at < bytes.len() && bytes[at].is_ascii_digit() {
            at += 1;
        }
    }
    let text = &source[from..at];
    let number: f64 = text
        .parse()
        .map_err(|_| format!("{text:?} at {from} is not a number"))?;
    Ok((
        Token {
            at: from,
            kind: Tok::Number(number),
        },
        at,
    ))
}

/// A quoted string, with the escapes a program is likely to write.
fn scan_text(source: &str, from: usize) -> Result<(Token, usize), String> {
    let bytes = source.as_bytes();
    let quote = bytes[from];
    let mut at = from + 1;
    let mut text = String::new();
    loop {
        if at >= bytes.len() {
            return Err(format!("the string starting at {from} is never closed"));
        }
        match bytes[at] {
            b'\\' if at + 1 < bytes.len() => {
                text.push(match bytes[at + 1] {
                    b'n' => '\n',
                    b't' => '\t',
                    b'r' => '\r',
                    other => char::from(other),
                });
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
    Ok((
        Token {
            at: from,
            kind: Tok::Text(text),
        },
        at,
    ))
}

/// A word: a keyword if it is one, a name otherwise.
fn scan_word(source: &str, from: usize) -> (Token, usize) {
    let bytes = source.as_bytes();
    let mut at = from;
    while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
        at += 1;
    }
    let kind = match &source[from..at] {
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
    (Token { at: from, kind }, at)
}

/// One of the punctuation tokens, longest first.
fn scan_symbol(source: &str, from: usize) -> Result<(Token, usize), String> {
    match SYMBOLS
        .iter()
        .find(|symbol| source[from..].starts_with(**symbol))
    {
        Some(symbol) => Ok((
            Token {
                at: from,
                kind: Tok::Sym(symbol),
            },
            from + symbol.len(),
        )),
        None => Err(format!(
            "{:?} at {from} is not part of this language",
            source[from..].chars().next().unwrap_or('?')
        )),
    }
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
            Tok::Let => self.let_statement(),
            Tok::If => self.if_statement(),
            Tok::While => self.while_statement(),
            Tok::Return => self.return_statement(),
            _ => self.expression_statement(),
        }
    }

    fn let_statement(&mut self) -> Result<Stmt, String> {
        self.next();
        let Tok::Name(name) = self.next() else {
            return Err(format!("`let` needs a name at offset {}", self.here()));
        };
        self.expect("=")?;
        let value = self.expression()?;
        self.expect(";")?;
        Ok(Stmt::Let { name, value })
    }

    fn if_statement(&mut self) -> Result<Stmt, String> {
        self.next();
        self.expect("(")?;
        let test = self.expression()?;
        self.expect(")")?;
        let then = self.block()?;
        let other = self.else_arm()?;
        Ok(Stmt::If { test, then, other })
    }

    /// The `else` half, which is also where `else if` chains without needing
    /// braces around the inner `if`.
    fn else_arm(&mut self) -> Result<Vec<Stmt>, String> {
        if !matches!(self.peek(), Tok::Else) {
            return Ok(Vec::new());
        }
        self.next();
        if matches!(self.peek(), Tok::If) {
            return Ok(vec![self.statement()?]);
        }
        self.block()
    }

    fn while_statement(&mut self) -> Result<Stmt, String> {
        self.next();
        self.expect("(")?;
        let test = self.expression()?;
        self.expect(")")?;
        let body = self.block()?;
        Ok(Stmt::While { test, body })
    }

    fn return_statement(&mut self) -> Result<Stmt, String> {
        self.next();
        if self.eat(";") {
            return Ok(Stmt::Return(None));
        }
        let value = self.expression()?;
        self.expect(";")?;
        Ok(Stmt::Return(Some(value)))
    }

    /// An expression, and then either `= value;` - which makes it an
    /// assignment - or `;`.
    fn expression_statement(&mut self) -> Result<Stmt, String> {
        let first = self.expression()?;
        if !self.eat("=") {
            self.expect(";")?;
            return Ok(Stmt::Eval(first));
        }
        let value = self.expression()?;
        self.expect(";")?;
        // Only a place can be assigned to. `1 = 2;` parses as an expression
        // and is refused here, where the message can say what a place is.
        match &first {
            Expr::Name(_) | Expr::Member { .. } | Expr::Index { .. } => Ok(Stmt::Assign {
                target: first,
                value,
            }),
            _ => Err("only a name, a member or an index can be assigned to".to_string()),
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
            Tok::Sym("[") => self.list_literal(),
            Tok::Sym("{") => self.map_literal(),
            other => Err(format!("{other:?} at offset {at} does not start a value")),
        }
    }

    fn list_literal(&mut self) -> Result<Expr, String> {
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

    fn map_literal(&mut self) -> Result<Expr, String> {
        let mut entries = Vec::new();
        while !self.eat("}") {
            let key = match self.next() {
                Tok::Name(name) => name,
                Tok::Text(text) => text,
                other => return Err(format!("a key is a name or a string, found {other:?}")),
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
}
