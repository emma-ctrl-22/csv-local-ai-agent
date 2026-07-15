//! A tiny, safe, recursive-descent expression evaluator over `Decimal`.
//!
//! Grammar:
//!   expr    := term (('+' | '-') term)*
//!   term    := unary (('*' | '/' | '%') unary)*
//!   unary   := '-' unary | primary
//!   primary := NUMBER | IDENT | '[' any chars ']' | '(' expr ')'
//!
//! Identifiers resolve through a variable lookup (used by compute_column to
//! reference other columns). `[Column With Spaces]` is supported.

use crate::{CoreError, Result};
use rust_decimal::Decimal;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(Decimal),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    LParen,
    RParen,
}

fn lex(src: &str) -> Result<Vec<Tok>> {
    let mut out = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' | '\n' | '\r' => i += 1,
            '+' => { out.push(Tok::Plus); i += 1; }
            '-' => { out.push(Tok::Minus); i += 1; }
            '*' => { out.push(Tok::Star); i += 1; }
            '/' => { out.push(Tok::Slash); i += 1; }
            '%' => { out.push(Tok::Percent); i += 1; }
            '(' => { out.push(Tok::LParen); i += 1; }
            ')' => { out.push(Tok::RParen); i += 1; }
            '[' => {
                let mut j = i + 1;
                let mut name = String::new();
                while j < chars.len() && chars[j] != ']' {
                    name.push(chars[j]);
                    j += 1;
                }
                if j >= chars.len() {
                    return Err(CoreError::Expr("unclosed '[' in expression".into()));
                }
                out.push(Tok::Ident(name.trim().to_string()));
                i = j + 1;
            }
            '0'..='9' | '.' => {
                let mut j = i;
                let mut num = String::new();
                while j < chars.len() && (chars[j].is_ascii_digit() || chars[j] == '.' || chars[j] == ',') {
                    if chars[j] != ',' {
                        num.push(chars[j]);
                    }
                    j += 1;
                }
                let d: Decimal = num
                    .parse()
                    .map_err(|_| CoreError::Expr(format!("bad number '{num}'")))?;
                out.push(Tok::Num(d));
                i = j;
            }
            c if c.is_alphabetic() || c == '_' => {
                let mut j = i;
                let mut name = String::new();
                while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                    name.push(chars[j]);
                    j += 1;
                }
                out.push(Tok::Ident(name));
                i = j;
            }
            other => {
                return Err(CoreError::Expr(format!(
                    "unexpected character '{other}' — only numbers, column names, + - * / % ( ) are allowed"
                )))
            }
        }
    }
    Ok(out)
}

struct Parser<'a> {
    toks: Vec<Tok>,
    pos: usize,
    vars: &'a dyn Fn(&str) -> Option<Decimal>,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn expr(&mut self) -> Result<Decimal> {
        let mut acc = self.term()?;
        loop {
            match self.peek() {
                Some(Tok::Plus) => {
                    self.next();
                    acc += self.term()?;
                }
                Some(Tok::Minus) => {
                    self.next();
                    acc -= self.term()?;
                }
                _ => break,
            }
        }
        Ok(acc)
    }

    fn term(&mut self) -> Result<Decimal> {
        let mut acc = self.unary()?;
        loop {
            match self.peek() {
                Some(Tok::Star) => {
                    self.next();
                    acc *= self.unary()?;
                }
                Some(Tok::Slash) => {
                    self.next();
                    let rhs = self.unary()?;
                    if rhs.is_zero() {
                        return Err(CoreError::Expr("division by zero".into()));
                    }
                    acc /= rhs;
                }
                Some(Tok::Percent) => {
                    self.next();
                    let rhs = self.unary()?;
                    if rhs.is_zero() {
                        return Err(CoreError::Expr("modulo by zero".into()));
                    }
                    acc %= rhs;
                }
                _ => break,
            }
        }
        Ok(acc)
    }

    fn unary(&mut self) -> Result<Decimal> {
        if matches!(self.peek(), Some(Tok::Minus)) {
            self.next();
            return Ok(-self.unary()?);
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Decimal> {
        match self.next() {
            Some(Tok::Num(d)) => Ok(d),
            Some(Tok::Ident(name)) => (self.vars)(&name).ok_or_else(|| {
                CoreError::Expr(format!("unknown name '{name}' in expression"))
            }),
            Some(Tok::LParen) => {
                let v = self.expr()?;
                match self.next() {
                    Some(Tok::RParen) => Ok(v),
                    _ => Err(CoreError::Expr("missing ')'".into())),
                }
            }
            other => Err(CoreError::Expr(format!("unexpected token: {other:?}"))),
        }
    }
}

/// Evaluate an expression with a variable resolver (column values, etc.).
pub fn eval_with(src: &str, vars: &dyn Fn(&str) -> Option<Decimal>) -> Result<Decimal> {
    let toks = lex(src)?;
    if toks.is_empty() {
        return Err(CoreError::Expr("empty expression".into()));
    }
    let mut p = Parser { toks, pos: 0, vars };
    let v = p.expr()?;
    if p.pos != p.toks.len() {
        return Err(CoreError::Expr("trailing input after expression".into()));
    }
    Ok(v)
}

/// Evaluate a pure-numeric expression (the `calculate` tool).
pub fn eval(src: &str) -> Result<Decimal> {
    eval_with(src, &|_| None)
}

/// Convenience for compute_column: resolve from a row map.
pub fn eval_row(src: &str, row: &HashMap<String, Decimal>) -> Result<Decimal> {
    eval_with(src, &|name| {
        let want = name.trim().to_lowercase();
        row.iter()
            .find(|(k, _)| k.trim().to_lowercase() == want)
            .map(|(_, v)| *v)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[test]
    fn arithmetic_precedence() {
        assert_eq!(eval("2 + 3 * 4").unwrap(), d("14"));
        assert_eq!(eval("(2 + 3) * 4").unwrap(), d("20"));
        assert_eq!(eval("-5 + 10").unwrap(), d("5"));
        assert_eq!(eval("10 / 4").unwrap(), d("2.5"));
    }

    #[test]
    fn money_is_exact() {
        // The classic float trap: 0.1 + 0.2
        assert_eq!(eval("0.1 + 0.2").unwrap(), d("0.3"));
        // VAT at 15% on 1,234.50
        assert_eq!(eval("1,234.50 * 0.15").unwrap(), d("185.1750"));
    }

    #[test]
    fn division_by_zero_is_an_error() {
        assert!(eval("1 / 0").is_err());
        assert!(eval("1 % (2 - 2)").is_err());
    }

    #[test]
    fn variables_and_bracket_names() {
        let vars = |name: &str| match name {
            "Amount" => Some(d("100")),
            "Unit Price" => Some(d("2.5")),
            _ => None,
        };
        assert_eq!(eval_with("Amount * 1.15", &vars).unwrap(), d("115.00"));
        assert_eq!(eval_with("[Unit Price] * 4", &vars).unwrap(), d("10.0"));
        assert!(eval_with("Nope + 1", &vars).is_err());
    }

    #[test]
    fn rejects_garbage() {
        assert!(eval("1 + ").is_err());
        assert!(eval("system('rm -rf')").is_err());
        assert!(eval("").is_err());
    }
}
