// SPDX-License-Identifier: Apache-2.0
// Copyright 2022-2025 Arm Limited (original Python implementation)
// Copyright 2026 pkt-lab contributors (Rust reimplementation)

//! Safe arithmetic formula evaluator with variable substitution.
//!
//! Ported from `simple_maths.py`. Supports: `+`, `-`, `*`, `/`, `<<`, `>>`,
//! unary `-`, parentheses, numeric literals, and named variables.
//!
//! # Example
//! ```
//! use std::collections::HashMap;
//! use telemetry_core::formula::evaluate;
//!
//! let vars: HashMap<String, f64> = [
//!     ("CPU_CYCLES".into(), 1000.0),
//!     ("INST_RETIRED".into(), 500.0),
//! ].into_iter().collect();
//!
//! let result = evaluate("INST_RETIRED / CPU_CYCLES * 100", &vars).unwrap();
//! assert!((result - 50.0).abs() < 1e-9);
//! ```

use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FormulaError {
    #[error("unexpected character: '{0}'")]
    UnexpectedChar(char),
    #[error("unexpected end of expression")]
    UnexpectedEnd,
    #[error("undefined variable: '{0}'")]
    UndefinedVariable(String),
    #[error("unexpected token: {0}")]
    UnexpectedToken(String),
}

/// Evaluate a formula string with the given variable bindings.
/// Returns `f64::NAN` on division by zero (matching Python behavior).
pub fn evaluate(expr: &str, vars: &HashMap<String, f64>) -> Result<f64, FormulaError> {
    let tokens = tokenize(expr)?;
    let mut parser = Parser::new(&tokens, vars);
    let result = parser.parse_expr(0)?;
    Ok(result)
}

// ─── Tokenizer ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f64),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    LShift,
    RShift,
    LParen,
    RParen,
}

fn tokenize(input: &str) -> Result<Vec<Token>, FormulaError> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();

    while let Some(&ch) = chars.peek() {
        match ch {
            ' ' | '\t' | '\n' | '\r' => {
                chars.next();
            }
            '+' => {
                chars.next();
                tokens.push(Token::Plus);
            }
            '-' => {
                chars.next();
                tokens.push(Token::Minus);
            }
            '*' => {
                chars.next();
                tokens.push(Token::Star);
            }
            '/' => {
                chars.next();
                tokens.push(Token::Slash);
            }
            '<' => {
                chars.next();
                if chars.peek() == Some(&'<') {
                    chars.next();
                    tokens.push(Token::LShift);
                } else {
                    return Err(FormulaError::UnexpectedChar('<'));
                }
            }
            '>' => {
                chars.next();
                if chars.peek() == Some(&'>') {
                    chars.next();
                    tokens.push(Token::RShift);
                } else {
                    return Err(FormulaError::UnexpectedChar('>'));
                }
            }
            '(' => {
                chars.next();
                tokens.push(Token::LParen);
            }
            ')' => {
                chars.next();
                tokens.push(Token::RParen);
            }
            c if c.is_ascii_digit() || c == '.' => {
                let mut num_str = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' {
                        num_str.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let val: f64 = num_str
                    .parse()
                    .map_err(|_| FormulaError::UnexpectedToken(num_str))?;
                tokens.push(Token::Number(val));
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let mut ident = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_alphanumeric() || c == '_' {
                        ident.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                tokens.push(Token::Ident(ident));
            }
            other => return Err(FormulaError::UnexpectedChar(other)),
        }
    }

    Ok(tokens)
}

// ─── Pratt Parser ────────────────────────────────────────────────────────────

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    vars: &'a HashMap<String, f64>,
}

impl<'a> Parser<'a> {
    fn new(tokens: &'a [Token], vars: &'a HashMap<String, f64>) -> Self {
        Self {
            tokens,
            pos: 0,
            vars,
        }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> Option<&Token> {
        let tok = self.tokens.get(self.pos);
        self.pos += 1;
        tok
    }

    fn parse_expr(&mut self, min_bp: u8) -> Result<f64, FormulaError> {
        let mut lhs = self.parse_prefix()?;

        while let Some(tok) = self.peek() {
            let (l_bp, r_bp) = match infix_binding_power(tok) {
                Some(bp) => bp,
                None => break,
            };
            if l_bp < min_bp {
                break;
            }

            let op = self.advance().unwrap().clone();
            let rhs = self.parse_expr(r_bp)?;

            lhs = apply_binop(&op, lhs, rhs);
        }

        Ok(lhs)
    }

    fn parse_prefix(&mut self) -> Result<f64, FormulaError> {
        let tok = self.advance().cloned();
        match tok {
            Some(Token::Number(n)) => Ok(n),
            Some(Token::Ident(ref name)) => self
                .vars
                .get(name)
                .copied()
                .ok_or_else(|| FormulaError::UndefinedVariable(name.clone())),
            Some(Token::Minus) => {
                let val = self.parse_expr(PREFIX_BP)?;
                Ok(-val)
            }
            Some(Token::LParen) => {
                let val = self.parse_expr(0)?;
                match self.advance() {
                    Some(Token::RParen) => Ok(val),
                    _ => Err(FormulaError::UnexpectedToken("expected ')'".into())),
                }
            }
            Some(tok) => Err(FormulaError::UnexpectedToken(format!("{tok:?}"))),
            None => Err(FormulaError::UnexpectedEnd),
        }
    }
}

const PREFIX_BP: u8 = 9;

fn infix_binding_power(tok: &Token) -> Option<(u8, u8)> {
    match tok {
        Token::LShift | Token::RShift => Some((1, 2)),
        Token::Plus | Token::Minus => Some((3, 4)),
        Token::Star | Token::Slash => Some((5, 6)),
        _ => None,
    }
}

fn apply_binop(op: &Token, lhs: f64, rhs: f64) -> f64 {
    match op {
        Token::Plus => lhs + rhs,
        Token::Minus => lhs - rhs,
        Token::Star => lhs * rhs,
        Token::Slash => {
            if rhs == 0.0 {
                f64::NAN
            } else {
                lhs / rhs
            }
        }
        Token::LShift => ((lhs as i64) << (rhs as i64)) as f64,
        Token::RShift => ((lhs as i64) >> (rhs as i64)) as f64,
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(expr: &str) -> f64 {
        evaluate(expr, &HashMap::new()).unwrap()
    }

    fn eval_vars(expr: &str, vars: &[(&str, f64)]) -> f64 {
        let map: HashMap<String, f64> = vars.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        evaluate(expr, &map).unwrap()
    }

    #[test]
    fn test_basic_arithmetic() {
        assert!((eval("2 + 3") - 5.0).abs() < 1e-9);
        assert!((eval("10 - 4") - 6.0).abs() < 1e-9);
        assert!((eval("3 * 4") - 12.0).abs() < 1e-9);
        assert!((eval("10 / 4") - 2.5).abs() < 1e-9);
    }

    #[test]
    fn test_precedence() {
        assert!((eval("2 + 3 * 4") - 14.0).abs() < 1e-9);
        assert!((eval("(2 + 3) * 4") - 20.0).abs() < 1e-9);
    }

    #[test]
    fn test_unary_minus() {
        assert!((eval("-5") - (-5.0)).abs() < 1e-9);
        assert!((eval("-(2 + 3)") - (-5.0)).abs() < 1e-9);
    }

    #[test]
    fn test_shift() {
        assert!((eval("1 << 3") - 8.0).abs() < 1e-9);
        assert!((eval("16 >> 2") - 4.0).abs() < 1e-9);
    }

    #[test]
    fn test_division_by_zero() {
        assert!(eval("1 / 0").is_nan());
    }

    #[test]
    fn test_variables() {
        let result = eval_vars(
            "(STALL_SLOT_BACKEND - 5 * IMP_WFX_CLOCK_CYCLES) / (5 * (CPU_CYCLES - IMP_WFX_CLOCK_CYCLES)) * 100",
            &[
                ("STALL_SLOT_BACKEND", 500.0),
                ("IMP_WFX_CLOCK_CYCLES", 10.0),
                ("CPU_CYCLES", 1000.0),
            ],
        );
        // (500 - 50) / (5 * 990) * 100 = 450/4950*100 = 9.0909...
        assert!((result - 9.090909090909092).abs() < 1e-6);
    }

    #[test]
    fn test_undefined_variable() {
        let result = evaluate("X + 1", &HashMap::new());
        assert!(result.is_err());
    }
}
