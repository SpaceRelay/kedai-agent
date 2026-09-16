// 词法分析:EJS JS 子集的 token 化(Tok/Kw/P 枚举与 lex 系列函数)。
// 负责把源码字符串切成 token 流:数字/字符串(含模板字符串)/标识符/关键字/运算符;
// 模板字符串 `${...}` 内部递归 lex 到顶层 `}` 为止。

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Tok {
    Num(f64),
    Str(String),
    /// 模板字符串:parts 与 exprs 交错(parts[0] + exprs[0] + parts[1] + ...)
    Tpl {
        parts: Vec<String>,
        exprs: Vec<Vec<Tok>>,
    },
    Ident(String),
    Kw(Kw),
    P(P),
    Eof,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Kw {
    If,
    Else,
    For,
    In,
    Of,
    Var,
    Let,
    Const,
    Function,
    Return,
    Break,
    Continue,
    While,
    True,
    False,
    Null,
    Undefined,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum P {
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Semi,
    Dot,
    Question,
    Colon,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Bang,
    EqEq,
    NotEq,
    EqEqEq,
    NotEqEq,
    Lt,
    Le,
    Gt,
    Ge,
    AndAnd,
    OrOr,
    Assign,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PlusPlus,
    MinusMinus,
    Arrow,
}

/// 对子表达式 lex(模板字符串 `${...}` 内部),遇到顶层 `}` 停止
fn lex_until_rbrace(src: &str, pos: &mut usize) -> Result<Vec<Tok>, String> {
    let mut toks = Vec::new();
    let mut depth = 0usize;
    loop {
        if *pos >= src.len() {
            return Err("模板表达式未闭合".into());
        }
        // 上方已返回错误或持续推进,pos < len 保证必有下一字符
        let ch = src[*pos..].chars().next().expect("pos 在界内,必有下一字符");
        if ch == '}' && depth == 0 {
            *pos += 1;
            return Ok(toks);
        }
        let (tok, end) = lex_one(src, *pos)?;
        if matches!(tok, Tok::P(P::RBrace)) {
            depth = depth.saturating_sub(1);
        } else if matches!(tok, Tok::P(P::LBrace)) {
            depth += 1;
        }
        toks.push(tok);
        *pos = end;
    }
}

/// 模板字符串字面量(当前字符是反引号)
fn lex_template(src: &str, pos: &mut usize) -> Result<Tok, String> {
    *pos += 1; // 吃掉 `
    let mut parts = vec![String::new()];
    let mut exprs = Vec::new();
    loop {
        if *pos >= src.len() {
            return Err("未闭合的模板字符串".into());
        }
        // 上方越界检查保证 pos < len,必有下一字符
        let ch = src[*pos..].chars().next().expect("pos 在界内,必有下一字符");
        match ch {
            '`' => {
                *pos += 1;
                break;
            }
            '\\' => {
                *pos += 1;
                if *pos < src.len() {
                    let e = src[*pos..].chars().next().expect("pos 在界内,必有下一字符");
                    // parts 初始化即含一段且只增不减,必有尾段
                    parts
                        .last_mut()
                        .expect("parts 至少一段")
                        .push(unescape_char(e));
                    *pos += e.len_utf8();
                }
            }
            '$' if src[*pos..].starts_with("${") => {
                *pos += 2;
                let inner = lex_until_rbrace(src, pos)?;
                exprs.push(inner);
                parts.push(String::new());
            }
            _ => {
                parts.last_mut().expect("parts 至少一段").push(ch);
                *pos += ch.len_utf8();
            }
        }
    }
    Ok(Tok::Tpl { parts, exprs })
}

fn unescape_char(c: char) -> char {
    match c {
        'n' => '\n',
        't' => '\t',
        'r' => '\r',
        '0' => '\0',
        'b' => '\u{0008}',
        'f' => '\u{000C}',
        'v' => '\u{000B}',
        other => other,
    }
}

/// 解析一个 token;返回 (token, 结束位置(绝对索引))
fn lex_one(src: &str, start: usize) -> Result<(Tok, usize), String> {
    let bytes = src.as_bytes();
    let mut i = start;
    // 跳过空白与注释
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            match src[i + 2..].find("*/") {
                Some(idx) => i += 2 + idx + 2,
                None => return Err("块注释未闭合".into()),
            }
            continue;
        }
        break;
    }
    if i >= bytes.len() {
        return Ok((Tok::Eof, src.len()));
    }
    // 上方 i >= len 已返回 Eof,此处必有下一字符
    let ch = src[i..].chars().next().expect("i 在界内,必有下一字符");
    match ch {
        '0'..='9' => {
            let mut j = i;
            let mut seen_dot = false;
            while j < bytes.len() {
                let c = src[j..].chars().next().expect("j 在界内,必有下一字符");
                if c.is_ascii_digit() {
                    j += 1;
                } else if c == '.' && !seen_dot {
                    seen_dot = true;
                    j += 1;
                } else {
                    break;
                }
            }
            // 指数 1e5
            if j < bytes.len() && (src[j..].starts_with('e') || src[j..].starts_with('E')) {
                let mut k = j + 1;
                if k < bytes.len() && (src[k..].starts_with('+') || src[k..].starts_with('-')) {
                    k += 1;
                }
                let mut digits = 0;
                while k < bytes.len()
                    && src[k..]
                        .chars()
                        .next()
                        .expect("k 在界内,必有下一字符")
                        .is_ascii_digit()
                {
                    k += 1;
                    digits += 1;
                }
                if digits > 0 {
                    j = k;
                }
            }
            let text = &src[i..j];
            // 有意丢弃 ParseFloatError:其唯一信息就是「这段文本不是数字」,
            // 而 text 原值已带进消息
            let n: f64 = text.parse().map_err(|_| format!("数字格式错误: {text}"))?;
            Ok((Tok::Num(n), j))
        }
        '\'' | '"' => {
            let quote = ch;
            let mut j = i + 1;
            let mut out = String::new();
            let mut closed = false;
            while j < bytes.len() {
                let c = src[j..].chars().next().expect("j 在界内,必有下一字符");
                match c {
                    '\\' => {
                        j += 1;
                        if j < bytes.len() {
                            let e = src[j..].chars().next().expect("j 在界内,必有下一字符");
                            if e == 'u' && j + 5 < bytes.len() {
                                let hex = &src[j + 1..j + 5];
                                if let Ok(code) = u32::from_str_radix(hex, 16) {
                                    if let Some(chr) = char::from_u32(code) {
                                        out.push(chr);
                                        j += 5;
                                        continue;
                                    }
                                }
                            }
                            out.push(unescape_char(e));
                            j += e.len_utf8();
                        }
                    }
                    c if c == quote => {
                        j += 1;
                        closed = true;
                        break;
                    }
                    _ => {
                        out.push(c);
                        j += c.len_utf8();
                    }
                }
            }
            if !closed {
                return Err("字符串未闭合".into());
            }
            Ok((Tok::Str(out), j))
        }
        '`' => {
            let tok = lex_template(src, &mut i)?;
            Ok((tok, i))
        }
        // 标识符:ASCII 字母/下划线/$ 开头;支持 Unicode 字母(中文等)——
        // JS 规范中 IdentifierName 允许 Unicode ID_Start/ID_Continue,酒馆助手卡
        // 常用中文属性访问(如 data.是否在场、cg.角色),缺此支持会报词法错误
        c if c.is_alphabetic() || c == '_' || c == '$' => {
            let mut j = i + c.len_utf8();
            while j < bytes.len() {
                let c = src[j..].chars().next().expect("j 在界内,必有下一字符");
                if c.is_alphanumeric() || c == '_' || c == '$' {
                    j += c.len_utf8();
                } else {
                    break;
                }
            }
            let word = &src[i..j];
            let kw = match word {
                "if" => Some(Kw::If),
                "else" => Some(Kw::Else),
                "for" => Some(Kw::For),
                "in" => Some(Kw::In),
                "of" => Some(Kw::Of),
                "var" => Some(Kw::Var),
                "let" => Some(Kw::Let),
                "const" => Some(Kw::Const),
                "function" => Some(Kw::Function),
                "return" => Some(Kw::Return),
                "break" => Some(Kw::Break),
                "continue" => Some(Kw::Continue),
                "while" => Some(Kw::While),
                "true" => Some(Kw::True),
                "false" => Some(Kw::False),
                "null" => Some(Kw::Null),
                "undefined" => Some(Kw::Undefined),
                _ => None,
            };
            match kw {
                Some(k) => Ok((Tok::Kw(k), j)),
                None => Ok((Tok::Ident(word.to_string()), j)),
            }
        }
        _ => {
            let two: Option<(P, usize)> =
                if i + 1 < bytes.len() && bytes[i].is_ascii() && bytes[i + 1].is_ascii() {
                    let two_chars = &src[i..i + 2];
                    match two_chars {
                        "==" => Some((P::EqEq, i + 2)),
                        "!=" => Some((P::NotEq, i + 2)),
                        "===" => Some((P::EqEqEq, i + 2)), // 下面三字符优先处理
                        "!==" => Some((P::NotEqEq, i + 2)),
                        "<=" => Some((P::Le, i + 2)),
                        ">=" => Some((P::Ge, i + 2)),
                        "&&" => Some((P::AndAnd, i + 2)),
                        "||" => Some((P::OrOr, i + 2)),
                        "+=" => Some((P::PlusEq, i + 2)),
                        "-=" => Some((P::MinusEq, i + 2)),
                        "*=" => Some((P::StarEq, i + 2)),
                        "/=" => Some((P::SlashEq, i + 2)),
                        "++" => Some((P::PlusPlus, i + 2)),
                        "--" => Some((P::MinusMinus, i + 2)),
                        "=>" => Some((P::Arrow, i + 2)),
                        _ => None,
                    }
                } else {
                    None
                };
            if let Some((p, end)) = two {
                return Ok((Tok::P(p), end));
            }
            let three: Option<(P, usize)> = if i + 2 < bytes.len()
                && bytes[i].is_ascii()
                && bytes[i + 1].is_ascii()
                && bytes[i + 2].is_ascii()
            {
                match &src[i..i + 3] {
                    "===" => Some((P::EqEqEq, i + 3)),
                    "!==" => Some((P::NotEqEq, i + 3)),
                    _ => None,
                }
            } else {
                None
            };
            if let Some((p, end)) = three {
                return Ok((Tok::P(p), end));
            }
            let one = match ch {
                '{' => P::LBrace,
                '}' => P::RBrace,
                '(' => P::LParen,
                ')' => P::RParen,
                '[' => P::LBracket,
                ']' => P::RBracket,
                ',' => P::Comma,
                ';' => P::Semi,
                '.' => P::Dot,
                '?' => P::Question,
                ':' => P::Colon,
                '+' => P::Plus,
                '-' => P::Minus,
                '*' => P::Star,
                '/' => P::Slash,
                '%' => P::Percent,
                '!' => P::Bang,
                '=' => P::Assign,
                '<' => P::Lt,
                '>' => P::Gt,
                _ => return Err(format!("无法识别的字符: {ch}")),
            };
            Ok((Tok::P(one), i + ch.len_utf8()))
        }
    }
}

/// 整段源码 → token 流(末尾追加 Eof)
pub(super) fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let mut toks = Vec::new();
    let mut pos = 0usize;
    loop {
        let (tok, end) = lex_one(src, pos)?;
        if matches!(tok, Tok::Eof) {
            break;
        }
        pos = end;
        toks.push(tok);
    }
    toks.push(Tok::Eof);
    Ok(toks)
}
