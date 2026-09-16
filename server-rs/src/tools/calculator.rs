// 计算器工具:白名单递归下降解析器,不使用 eval(与 Node 版 calculator.ts 对齐)
use serde_json::{json, Value};

struct Parser {
    s: Vec<u8>,
    pos: usize,
}

impl Parser {
    fn new(s: &str) -> Self {
        Parser {
            s: s.as_bytes().to_vec(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    // expr := term (('+' | '-') term)*
    fn expr(&mut self) -> Result<f64, String> {
        let mut left = self.term()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b'+') => {
                    self.pos += 1;
                    let right = self.term()?;
                    left += right;
                }
                Some(b'-') => {
                    self.pos += 1;
                    let right = self.term()?;
                    left -= right;
                }
                _ => break,
            }
        }
        Ok(left)
    }

    // term := factor (('*' | '/') factor)*
    fn term(&mut self) -> Result<f64, String> {
        let mut left = self.factor()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b'*') => {
                    self.pos += 1;
                    let right = self.factor()?;
                    left *= right;
                }
                Some(b'/') => {
                    self.pos += 1;
                    let right = self.factor()?;
                    if right == 0.0 {
                        return Err("除数不能为 0".into());
                    }
                    left /= right;
                }
                _ => break,
            }
        }
        Ok(left)
    }

    // factor := '-' factor | '(' expr ')' | number
    fn factor(&mut self) -> Result<f64, String> {
        self.skip_ws();
        match self.peek() {
            Some(b'-') => {
                self.pos += 1;
                let v = self.factor()?;
                Ok(-v)
            }
            Some(b'(') => {
                self.pos += 1;
                let v = self.expr()?;
                self.skip_ws();
                if self.peek() != Some(b')') {
                    return Err("缺少右括号".into());
                }
                self.pos += 1;
                Ok(v)
            }
            Some(c) if c.is_ascii_digit() || c == b'.' => self.number(),
            Some(c) => Err(format!(
                "无法解析的字符 \"{}\" 位于 {}",
                c as char, self.pos
            )),
            None => Err(format!("位置 {} 处期望数字", self.pos)),
        }
    }

    // number := \d+\.?\d* | \.\d+
    fn number(&mut self) -> Result<f64, String> {
        let start = self.pos;
        let mut seen_dot = false;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                self.pos += 1;
            } else if c == b'.' && !seen_dot {
                seen_dot = true;
                self.pos += 1;
            } else {
                break;
            }
        }
        let text = String::from_utf8_lossy(&self.s[start..self.pos]).to_string();
        // 有意丢弃 ParseFloatError:唯一信息是「这段不是数字」,位置 start 已在消息中,
        // 由用户修正表达式
        text.parse::<f64>()
            .map_err(|_| format!("位置 {} 处期望数字", start))
    }
}

/// 解析表达式并求值;返回 {"expression": ..., "result": ...}
pub fn calculate(expression: &str) -> Result<Value, String> {
    if expression.trim().is_empty() {
        return Err("缺少 expression 参数".into());
    }
    let mut p = Parser::new(expression);
    let result = p.expr()?;
    p.skip_ws();
    if p.pos < p.s.len() {
        return Err(format!(
            "无法解析的字符 \"{}\" 位于 {}",
            p.s[p.pos] as char, p.pos
        ));
    }
    Ok(json!({ "expression": expression, "result": result }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic() {
        let v = calculate("12*34").unwrap();
        assert_eq!(v["result"], json!(408.0));
    }

    #[test]
    fn test_parens() {
        let v = calculate("(1+2)*3").unwrap();
        assert_eq!(v["result"], json!(9.0));
    }

    #[test]
    fn test_unary_minus() {
        let v = calculate("-5+3").unwrap();
        assert_eq!(v["result"], json!(-2.0));
    }

    #[test]
    fn test_decimal() {
        let v = calculate("0.5*2").unwrap();
        assert_eq!(v["result"], json!(1.0));
    }

    #[test]
    fn test_div_by_zero() {
        let e = calculate("1/0").unwrap_err();
        assert!(e.contains("除数不能为 0"));
    }

    #[test]
    fn test_bad_char() {
        let e = calculate("1+abc").unwrap_err();
        assert!(e.contains("无法解析的字符"));
    }

    #[test]
    fn test_missing_paren() {
        let e = calculate("(1+2").unwrap_err();
        assert!(e.contains("缺少右括号"));
    }

    #[test]
    fn test_empty() {
        let e = calculate("  ").unwrap_err();
        assert!(e.contains("缺少 expression 参数"));
    }

    #[test]
    fn test_whitespace() {
        let v = calculate("1 + 2").unwrap();
        assert_eq!(v["result"], json!(3.0));
    }
}
