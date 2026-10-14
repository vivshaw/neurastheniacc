//! A rejecting front end, not a code generator. Only this validated C reaches Clang.
use anyhow::{Context, Result, bail, ensure};
use std::collections::HashSet;

#[derive(Debug)]
pub struct Source {
    pub name: String,
    pub canonical: String,
}

pub fn parse(input: &str) -> Result<Source> {
    ensure!(input.len() <= 32_768, "source exceeds 32 KiB");
    ensure!(
        !input.contains('\\') && !input.contains("??"),
        "C line splicing and trigraphs are unsupported"
    );
    let tokens = lex(input)?;
    ensure!(tokens.len() <= 4096, "source exceeds 4096 tokens");
    let mut p = Parser {
        tokens,
        pos: 0,
        scopes: vec![HashSet::new()],
        depth: 0,
    };
    p.expect("uint32_t")?;
    let name = p.ident()?;
    p.expect("(")?;
    for i in 0..2 {
        if i != 0 {
            p.expect(",")?;
        }
        p.expect("uint32_t")?;
        let param = p.ident()?;
        p.declare(param)?;
    }
    p.expect(")")?;
    // Parameters and top-level locals share a scope in C.
    ensure!(p.block(false)?, "every path must return a uint32_t value");
    ensure!(p.pos == p.tokens.len(), "only one function is supported");
    Ok(Source {
        name,
        canonical: p.tokens.join(" "),
    })
}

fn lex(input: &str) -> Result<Vec<String>> {
    let input = input
        .lines()
        .filter(|l| l.trim() != "#include <stdint.h>")
        .collect::<Vec<_>>()
        .join("\n");
    let b = input.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b[i..].starts_with(b"//") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b[i..].starts_with(b"/*") {
            i += 2;
            while i + 1 < b.len() && !b[i..].starts_with(b"*/") {
                i += 1;
            }
            ensure!(i + 1 < b.len(), "unterminated comment");
            i += 2;
            continue;
        }
        let start = i;
        ensure!(
            !b[i..].starts_with(b"++") && !b[i..].starts_with(b"--"),
            "increment/decrement operators are unsupported"
        );
        if b[i].is_ascii_alphanumeric() || b[i] == b'_' {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
        } else if i + 1 < b.len()
            && [b"<<", b">>", b"<=", b">=", b"==", b"!="]
                .iter()
                .any(|op| b[i..i + 2] == op[..])
        {
            i += 2;
        } else {
            ensure!(
                b"(){};,=+-*~&|^<>".contains(&b[i]),
                "unsupported character at byte {i}"
            );
            i += 1;
        }
        out.push(input[start..i].to_string());
    }
    Ok(out)
}

struct Parser {
    tokens: Vec<String>,
    pos: usize,
    scopes: Vec<HashSet<String>>,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> &str {
        self.tokens
            .get(self.pos)
            .map(String::as_str)
            .unwrap_or("<eof>")
    }
    fn take(&mut self, s: &str) -> bool {
        if self.peek() == s {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, s: &str) -> Result<()> {
        ensure!(
            self.take(s),
            "token {}: expected {s:?}, found {:?}",
            self.pos,
            self.peek()
        );
        Ok(())
    }
    fn ident(&mut self) -> Result<String> {
        let s = self.peek();
        let reserved = [
            "uint32_t", "if", "else", "return", "int", "void", "while", "for", "do", "switch",
            "case", "break", "continue", "goto", "sizeof", "typedef", "static", "const",
            "volatile", "extern", "struct", "union", "enum", "unsigned", "signed", "char", "short",
            "long", "float", "double", "auto", "register", "restrict", "inline", "default", "main",
            "printf", "scanf",
        ];
        ensure!(
            s.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
                && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                && !reserved.contains(&s),
            "expected a nonreserved identifier, found {s:?}"
        );
        let s = s.to_string();
        self.pos += 1;
        Ok(s)
    }
    fn declare(&mut self, s: String) -> Result<()> {
        ensure!(
            self.scopes.last_mut().unwrap().insert(s.clone()),
            "duplicate declaration: {s}"
        );
        Ok(())
    }
    fn known(&self, s: &str) -> Result<()> {
        ensure!(
            self.scopes.iter().rev().any(|scope| scope.contains(s)),
            "undeclared variable: {s}"
        );
        Ok(())
    }
    fn enter(&mut self) -> Result<()> {
        self.depth += 1;
        ensure!(self.depth <= 48, "nesting exceeds 48 levels");
        Ok(())
    }
    fn block(&mut self, scoped: bool) -> Result<bool> {
        self.enter()?;
        self.expect("{")?;
        if scoped {
            self.scopes.push(HashSet::new());
        }
        let mut returns = false;
        while !self.take("}") {
            returns |= self.statement()?;
        }
        if scoped {
            self.scopes.pop();
        }
        self.depth -= 1;
        Ok(returns)
    }
    fn statement(&mut self) -> Result<bool> {
        if self.take("uint32_t") {
            let name = self.ident()?;
            // Reject self-references, including references to a shadowed outer name.
            self.expect("=")?;
            let start = self.pos;
            self.expr(0)?;
            ensure!(
                !self.tokens[start..self.pos].contains(&name),
                "self-reference in initializer: {name}"
            );
            self.declare(name)?;
            self.expect(";")?;
        } else if self.take("return") {
            self.expr(0)?;
            self.expect(";")?;
            return Ok(true);
        } else if self.take("if") {
            self.expect("(")?;
            self.expr(0)?;
            if ["==", "!=", "<", "<=", ">", ">="].contains(&self.peek()) {
                self.pos += 1;
                self.expr(0)?;
            }
            self.expect(")")?;
            let yes = self.block(true)?;
            let no = if self.take("else") {
                self.block(true)?
            } else {
                false
            };
            return Ok(yes && no);
        } else if self.peek() == "{" {
            return self.block(true);
        } else {
            let name = self.ident()?;
            self.known(&name)?;
            self.expect("=")?;
            self.expr(0)?;
            self.expect(";")?;
        }
        Ok(false)
    }
    fn expr(&mut self, min: u8) -> Result<()> {
        self.enter()?;
        if self.take("~") || self.take("-") || self.take("+") {
            self.expr(7)?;
        } else if self.take("(") {
            self.expr(0)?;
            self.expect(")")?;
        } else if self
            .peek()
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_digit)
        {
            literal(self.peek())?;
            self.pos += 1;
        } else {
            let name = self.ident()?;
            self.known(&name)?;
        }
        loop {
            let (prec, shift) = match self.peek() {
                "|" => (1, false),
                "^" => (2, false),
                "&" => (3, false),
                "<<" | ">>" => (4, true),
                "+" | "-" => (5, false),
                "*" => (6, false),
                _ => break,
            };
            if prec < min {
                break;
            }
            self.pos += 1;
            if shift {
                ensure!(
                    literal(self.peek())? < 32,
                    "shift count must be a literal from 0u to 31u"
                );
                self.pos += 1;
                // Do not accidentally admit `x << 31u + y` (the whole RHS is the count in C).
                ensure!(
                    !["+", "-", "*"].contains(&self.peek()),
                    "parenthesize the shifted expression before further arithmetic"
                );
            } else {
                self.expr(prec + 1)?;
            }
        }
        self.depth -= 1;
        Ok(())
    }
}

fn literal(s: &str) -> Result<u32> {
    let Some(digits) = s.strip_suffix('u').or_else(|| s.strip_suffix('U')) else {
        bail!("integer literals require a u suffix: {s}");
    };
    if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        Ok(u32::from_str_radix(hex, 16)?)
    } else {
        ensure!(
            digits == "0" || !digits.starts_with('0'),
            "octal literals are unsupported"
        );
        Ok(digits.parse()?)
    }
}

pub fn read(path: &std::path::Path) -> Result<Source> {
    parse(&std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?)
        .with_context(|| format!("unsupported source: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wrap(body: &str) -> String {
        format!("uint32_t f(uint32_t x, uint32_t y) {{ {body} }}")
    }
    #[test]
    fn accepts_unsigned_branches_and_scopes() {
        parse(&wrap("uint32_t z = x * 33u + y; if (z > 100u) { uint32_t x = y; return z ^ x; } return z + 7u;")).unwrap();
    }
    #[test]
    fn rejects_undefined_and_unsupported_c() {
        for body in [
            "return x << 32u;",
            "return x << 1u + y;",
            "return x / y;",
            "return x + 1;",
            "uint32_t z = z; return z;",
            "uint32_t x = x; return x;",
            "if (x) { return y; }",
            "return system(x);",
            "return 010u;",
            "uint32_t z = x; uint32_t z = y; return z;",
            "{ uint32_t z = x; } return z;",
        ] {
            assert!(parse(&wrap(body)).is_err(), "accepted {body}");
        }
    }
    #[test]
    fn strips_comments_and_only_the_stdint_include() {
        let s = parse(&format!(
            "#include <stdint.h>\n// hello\n{}",
            wrap("return x /* a */ + y;")
        ))
        .unwrap();
        assert!(!s.canonical.contains("hello"));
        assert!(parse("#include <stdio.h>").is_err());
    }

    #[test]
    fn rejects_c_lexical_traps_instead_of_changing_their_meaning() {
        for body in [
            "return ++x;",
            "return --x;",
            "return x--y;",
            "return x+++y;",
            "// \\\nreturn x;",
            "// ??/\nreturn y;",
        ] {
            assert!(parse(&wrap(body)).is_err(), "accepted {body}");
        }
        parse(&wrap("return x - -y;")).unwrap();
    }
}
