// 输出协议解析与应用(L1 老层 · parsing/assistant):
// <UpdateVariable>/<JSONPatch> 输出协议解析。
// 兼容 JSON Patch(RFC 6902 子集 + delta 扩展)与 MagVarUpdate 的 _.set(...) 两种协议。
use serde_json::Value;

// ===================== JSON Patch 操作 =====================

/// <JSONPatch> 支持的操作(附件角色卡协议:replace/delta/insert/remove/move)。
/// reason 为 MagVarUpdate 风格语句(_.set(...))携带的注释原因(行注释 // 与块注释 /* */);
/// JSON Patch 数组无注释概念,各变体 reason 恒为 None。
/// Insert 已并入 Replace(前端 replace/set/insert 统一映射为 set,两者语义相同)。
#[derive(Debug, Clone, PartialEq)]
pub enum PatchOp {
    Replace {
        path: String,
        value: Value,
        reason: Option<String>,
    },
    Delta {
        path: String,
        value: f64,
        reason: Option<String>,
    },
    Remove {
        path: String,
        reason: Option<String>,
    },
    Move {
        from: String,
        to: String,
        reason: Option<String>,
    },
}

impl PatchOp {
    /// 操作携带的原因注释(仅 MagVarUpdate 语句解析产生,JSON Patch 恒为 None)
    pub fn reason(&self) -> Option<&str> {
        match self {
            PatchOp::Replace { reason, .. }
            | PatchOp::Delta { reason, .. }
            | PatchOp::Remove { reason, .. }
            | PatchOp::Move { reason, .. } => reason.as_deref(),
        }
    }
}

// ===================== 输出协议解析 =====================

/// 从 AI 输出中提取 <UpdateVariable> 块:
/// 返回 (剥离块后的文本, 解析出的补丁操作)。
/// 兼容两种块内格式:
///   1. assistant 格式:<Analysis>…</Analysis><JSONPatch>[{...}]</JSONPatch>
///   2. MagVarUpdate 格式:_.set('path', old, new);//原因
pub fn parse_update_variable(text: &str) -> (String, Vec<PatchOp>) {
    let mut ops = Vec::new();
    // 正则为写死字面量,编译必然成功
    let re = regex::Regex::new(r"(?is)<UpdateVariable\b[^>]*>([\s\S]*?)</UpdateVariable\s*>")
        .expect("UpdateVariable 块正则为常量,编译必然成功");
    for cap in re.captures_iter(text) {
        let block = &cap[1];
        if let Some(p) = extract_json_patch(block) {
            ops.extend(p);
        } else {
            ops.extend(parse_set_statements(block));
        }
    }
    let cleaned = re.replace_all(text, "").to_string();
    (cleaned, ops)
}

/// 提取块内 <JSONPatch> 数组并解析为操作;无该标签或解析失败返回 None
fn extract_json_patch(block: &str) -> Option<Vec<PatchOp>> {
    // 正则为写死字面量,编译必然成功
    let re = regex::Regex::new(r"(?is)<JSONPatch\b[^>]*>([\s\S]*?)</JSONPatch\s*>")
        .expect("JSONPatch 块正则为常量,编译必然成功");
    let body = re.captures(block)?.get(1)?.as_str().to_string();
    let v: Value = serde_json::from_str(&body).ok()?;
    parse_patch_array(&v)
}

/// 解析 JSON Patch 数组(JSON 值形式)为操作序列;非数组或全部无效返回 None。
/// 供 <JSONPatch> 文本协议与工具调用(update_variables 的 patches 参数)复用。
pub fn parse_patch_array(v: &Value) -> Option<Vec<PatchOp>> {
    let arr = v.as_array()?;
    let mut ops = Vec::new();
    for item in arr {
        let obj = match item.as_object() {
            Some(o) => o,
            None => continue,
        };
        let Some(op) = obj.get("op").and_then(|x| x.as_str()) else {
            continue;
        };
        let path = obj
            .get("path")
            .or_else(|| obj.get("from"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let value = || obj.get("value").cloned().unwrap_or(Value::Null);
        match op {
            // insert 与 replace/set 语义相同(前端统一映射为 set),并入 Replace
            "replace" | "set" | "insert" => ops.push(PatchOp::Replace {
                path,
                value: value(),
                reason: None,
            }),
            "delta" | "add" => {
                // 无效值(非数字)跳过整条:与前端 applyUpdate 的宽容 Number() 不同,
                // 本协议规范定为「无效则跳过」,故 as_f64 失败即 continue
                let Some(n) = obj.get("value").and_then(|x| x.as_f64()) else {
                    continue;
                };
                ops.push(PatchOp::Delta {
                    path,
                    value: n,
                    reason: None,
                });
            }
            "remove" => ops.push(PatchOp::Remove { path, reason: None }),
            "move" => {
                let from = obj
                    .get("from")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                if from.is_empty() {
                    continue;
                }
                ops.push(PatchOp::Move {
                    from,
                    to: path,
                    reason: None,
                });
            }
            _ => continue,
        }
    }
    if ops.is_empty() {
        None
    } else {
        Some(ops)
    }
}

/// 解析 MagVarUpdate 风格 _.set('path', old, new);//原因 语句(移植前端 parser.ts)
/// 被 initvar 模块([InitVar] 初始变量)复用,故为 pub(super)
pub(super) fn parse_set_statements(block: &str) -> Vec<PatchOp> {
    let mut ops = Vec::new();
    // 正则为写死字面量,编译必然成功
    let re = regex::Regex::new(r"[_.]\s*set\s*\(").expect("set 语句正则为常量,编译必然成功");
    let mut i = 0usize;
    while let Some(m) = re.find(&block[i..]) {
        let open_idx = i + m.end() - 1; // '(' 位置
        let Some(close_idx) = scan_balanced(block, open_idx) else {
            break;
        };
        if close_idx <= open_idx {
            i = open_idx + 1;
            continue;
        }
        // 语句结束:到 ; 或行尾;分号后跟 // 注释时吸收到行尾
        let mut stmt_end = close_idx + 1;
        while stmt_end < block.len() && !matches!(block.as_bytes()[stmt_end], b';' | b'\n' | b'\r')
        {
            stmt_end += 1;
        }
        if stmt_end < block.len() && block.as_bytes()[stmt_end] == b';' {
            let mut k = stmt_end + 1;
            while k < block.len() && block.as_bytes()[k].is_ascii_whitespace() {
                k += 1;
            }
            // 分号后(跳过空白)紧跟 // 注释 → 吸收到行尾(与前端 findSetCalls 一致:
            // 判断位置是跳过空白后的 k,而非分号位置,兼容 ';//原因' 与 ';// 原因')
            if block[k..].starts_with("//") {
                stmt_end = block[stmt_end..]
                    .find('\n')
                    .map(|i| i + stmt_end)
                    .unwrap_or(block.len());
            }
        }
        let body = &block[open_idx..stmt_end];
        if let Some((path, value, reason)) = parse_set_body(body) {
            ops.push(PatchOp::Replace {
                path,
                value,
                reason,
            });
        }
        // 继续扫描下一段;stmt_end 可能已到块末尾(注释吸收到行尾),需 clamp 到 len,
        // 否则 block[i..] 越界 panic(前端 JS slice 越界返回空,find 即终止)
        i = (stmt_end + 1).min(block.len());
    }
    ops
}

/// 解析 _.set 语句体(含 '(' 起、含 ');//原因' 或 ';//原因'),返回 (path, value, reason)。
/// reason 提取与前端 parseSetStatement 一致:行注释优先,其次块注释;
/// 行注释截断代码,块注释保留在代码中参与切分(前端 findBlockReason 不截断,行为逐字对齐)。
fn parse_set_body(body: &str) -> Option<(String, Value, Option<String>)> {
    let mut code = body.to_string();
    let reason = if let Some(idx) = find_line_comment(&code) {
        // 先取注释内容为 owned 字符串,再截断代码(避免借用冲突)
        let comment = code[idx + 2..]
            .trim()
            .trim_end_matches(';')
            .trim()
            .to_string();
        code.truncate(idx);
        if comment.is_empty() {
            None
        } else {
            Some(comment)
        }
    } else {
        find_block_reason(&code)
    };
    let code = code.trim().trim_end_matches(';').trim();
    let code = if code.starts_with('(') && code.ends_with(')') {
        &code[1..code.len() - 1]
    } else {
        code
    };
    let parts = split_top_level(code);
    if parts.len() < 2 {
        return None;
    }
    let path = strip_quotes(parts[0].trim()).to_string();
    if path.is_empty() {
        return None;
    }
    let new_v = parse_js_value(&parts[parts.len() - 1]);
    Some((path, new_v, reason))
}

/// 解析 JS 风格字面量:字符串(单/双引号)、数字、布尔、null、对象、数组;其余按字符串。
/// 字符串分支做转义还原(unescape_js_string),与前端 parseJsValue 一致。
fn parse_js_value(raw: &str) -> Value {
    let t = raw.trim();
    if t.starts_with('\'') || t.starts_with('"') {
        return Value::String(unescape_js_string(&strip_quotes(t)));
    }
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        return v;
    }
    Value::String(t.to_string())
}

/// JS 字符串转义还原(移植前端 parseJsString):
/// \' → '、\" → "、\\ → \、\n → 换行、\r → 回车、\t → 制表符;未知转义保留原样。
/// 替换顺序与前端逐字一致(\' 与 \" 先于 \\,再 \n/\r/\t),保证 '\\n'、'\\'n' 等
/// 组合序列跨端结果相同(如 'a\\nb' 三个字符最终还原为真实换行)。
fn unescape_js_string(s: &str) -> String {
    s.replace("\\'", "'")
        .replace("\\\"", "\"")
        .replace("\\\\", "\\")
        .replace("\\n", "\n")
        .replace("\\r", "\r")
        .replace("\\t", "\t")
}

/// 块注释内容(移植前端 findBlockReason:提取首个 /* ... */ 的内容并 trim;无或为空返回 None)
fn find_block_reason(s: &str) -> Option<String> {
    let start = s.find("/*")?;
    let rest = &s[start + 2..];
    let end = rest.find("*/")?;
    let content = rest[..end].trim();
    if content.is_empty() {
        None
    } else {
        Some(content.to_string())
    }
}

fn strip_quotes(t: &str) -> String {
    let t = t.trim();
    let bytes = t.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return t[1..t.len() - 1].to_string();
        }
    }
    t.to_string()
}

/// 从 start(指向 '(')扫描到配对 ')' 的索引;未配对返回 None
fn scan_balanced(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0usize;
    let mut i = start;
    let mut quote: Option<u8> = None;
    let mut esc = false;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = quote {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' | b'\'' => quote = Some(c),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                if depth == 0 {
                    // 无匹配的开括号:容错返回当前位置
                    return Some(i);
                }
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// 顶层逗号切分(忽略引号与嵌套结构)
fn split_top_level(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut esc = false;
    let mut cur = String::new();
    for c in s.chars() {
        if let Some(q) = quote {
            cur.push(c);
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c as u8 == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c as u8);
                cur.push(c);
            }
            '(' | '[' | '{' => {
                depth += 1;
                cur.push(c);
            }
            ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
                cur.push(c);
            }
            ',' if depth == 0 => {
                parts.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        parts.push(cur.trim().to_string());
    }
    parts
}

/// 行注释起点(忽略引号内)
fn find_line_comment(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut quote: Option<u8> = None;
    let mut esc = false;
    for i in 0..bytes.len() {
        let c = bytes[i];
        if let Some(q) = quote {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            b'"' | b'\'' => quote = Some(c),
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => return Some(i),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 任务1:JS 字符串转义还原(与前端 parseJsString 逐字符一致)。
    /// 关键:模型输出 'a\nb' 应还原为真实换行(此前只 strip_quotes 存字面两字符)。
    #[test]
    fn unescape_string_escapes() {
        assert_eq!(
            parse_js_value(r"'a\nb'"),
            json!("a\nb"),
            "\\n 应还原为真实换行"
        );
        assert_eq!(parse_js_value(r"'a\rb'"), json!("a\rb"));
        assert_eq!(parse_js_value(r"'a\tb'"), json!("a\tb"));
        assert_eq!(parse_js_value(r"'a\\b'"), json!(r"a\b"));
        assert_eq!(parse_js_value(r#"'a\'b'"#), json!("a'b"));
        assert_eq!(parse_js_value(r#"'a\"b'"#), json!("a\"b"));
        // 顺序组合:序列 \ 与 n 一起还原(前端 \' \" 先于 \\,再 \n,故 'a\\nb' → 换行)
        assert_eq!(parse_js_value(r"'a\\nb'"), json!("a\nb"));
        // 未知转义保留原样
        assert_eq!(parse_js_value(r"'\x41'"), json!(r"\x41"));
    }

    /// 任务2:行注释 reason('//')提取(前端 findLineComment 语义)。
    #[test]
    fn parse_set_statement_line_reason() {
        // 分号后紧跟 //(无空格)
        let ops = parse_set_statements("_.set('x', 0, 1);//好感度+1");
        assert_eq!(ops.len(), 1);
        assert_eq!(
            ops[0],
            PatchOp::Replace {
                path: "x".into(),
                value: json!(1),
                reason: Some("好感度+1".into()),
            }
        );
        // 分号后空格再 //(旧实现会漏判,前端 findSetCalls 语义要求吸收)
        let ops2 = parse_set_statements("_.set('x', 0, 1); // 好感度+1");
        assert_eq!(ops2.len(), 1);
        assert_eq!(ops2[0].reason(), Some("好感度+1"));
        // 块注释作为 reason,且不截断语句(前端 findBlockReason 不截断代码)
        let ops3 = parse_set_statements("_.set('x', /* 块原因 */ 0, 1)");
        assert_eq!(ops3.len(), 1);
        assert_eq!(ops3[0].reason(), Some("块原因"));
        // 语句内块注释参与参数切分,reason 取块注释内容
        let ops3b = parse_set_statements("_.set('x', /* 内嵌 */ 0, 1)");
        assert_eq!(ops3b.len(), 1);
        assert_eq!(ops3b[0].reason(), Some("内嵌"));
        // 括号外块注释:与前端 parseSetStatement 一致,语句整体丢弃
        // (块注释不在括号内则无法剥离最外层括号,splitTopLevel 视为单参数)
        let ops4 = parse_set_statements("_.set('x', 0, 1) /* 尾注 */;");
        assert!(ops4.is_empty(), "括号外块注释:命令应被丢弃(与前端一致)");
        // 无注释 → None
        let ops5 = parse_set_statements("_.set('x', 0, 1);");
        assert_eq!(ops5.len(), 1);
        assert_eq!(ops5[0].reason(), None);
        // 空注释 → None
        let ops6 = parse_set_statements("_.set('x', 0, 1);//");
        assert_eq!(ops6.len(), 1);
        assert_eq!(ops6[0].reason(), None);
    }

    /// 任务2:块注释提取(前端 findBlockReason 语义)。
    #[test]
    fn block_reason_extraction() {
        assert_eq!(find_block_reason("/* 原因 */"), Some("原因".to_string()));
        assert_eq!(find_block_reason("a /* b */ c"), Some("b".to_string()));
        assert_eq!(
            find_block_reason("/* 多行\n注释 */"),
            Some("多行\n注释".to_string())
        );
        assert_eq!(find_block_reason("没有块注释"), None);
        assert_eq!(find_block_reason("/* */"), None, "空块注释无 reason");
    }

    /// 任务3:delta 无效值跳过整条(as_f64 失败即跳过该条,锁定现有行为)。
    #[test]
    fn delta_invalid_value_skips_entry() {
        // 'abc' 非数字 → delta 条目被跳过,其余条目保留
        let arr = json!([
            { "op": "delta", "path": "/好感度", "value": "abc" },
            { "op": "replace", "path": "/x", "value": 1 }
        ]);
        let ops = parse_patch_array(&arr).expect("应解析");
        assert_eq!(ops.len(), 1, "非数字 delta 应被跳过");
        assert_eq!(
            ops[0],
            PatchOp::Replace {
                path: "/x".into(),
                value: json!(1),
                reason: None
            }
        );
        // 缺 value → 跳过
        let arr2 = json!([{ "op": "delta", "path": "/好感度" }]);
        assert!(
            parse_patch_array(&arr2).is_none(),
            "delta 缺 value 整批视为无效"
        );
        // 数字 delta 正常
        let arr3 = json!([{ "op": "delta", "path": "/好感度", "value": 2 }]);
        let ops3 = parse_patch_array(&arr3).expect("应解析");
        assert_eq!(
            ops3[0],
            PatchOp::Delta {
                path: "/好感度".into(),
                value: 2.0,
                reason: None
            }
        );
    }

    /// 任务4:insert op 并入 Replace(reason 恒 None)。
    #[test]
    fn insert_op_maps_to_replace() {
        let arr = json!([{ "op": "insert", "path": "/芽衣/行动", "value": "看书" }]);
        let ops = parse_patch_array(&arr).expect("应解析");
        assert_eq!(
            ops[0],
            PatchOp::Replace {
                path: "/芽衣/行动".into(),
                value: json!("看书"),
                reason: None
            }
        );
    }

    /// 任务1+2:_.set 值内的转义字符串经语句解析同样还原(前后端结果一致)。
    #[test]
    fn set_statement_value_with_escapes_and_reason() {
        let ops = parse_set_statements("_.set('x', old, 'a\\nb');//原因");
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            PatchOp::Replace {
                path,
                value,
                reason,
            } => {
                assert_eq!(path, "x");
                assert_eq!(value, &json!("a\nb"), "语句值内 \\n 应还原为真实换行");
                assert_eq!(reason, &Some("原因".into()));
            }
            other => panic!("应为 Replace,实际: {other:?}"),
        }
    }
}
