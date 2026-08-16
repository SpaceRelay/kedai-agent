// 角色卡正则脚本(regex_scripts)解析与消息处理
// 兼容两种位置:
//   1. extensions.regex_scripts[] (V2 常见)
//   2. 顶层 regex_scripts[] (部分导出格式)
// 用途:AI 输出(或输入)中的占位符/变量标记按正则替换为 display HTML(如状态栏、提示词标记),
//       替换结果用于前端可选 HTML 渲染,不注入 LLM system。
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 规范化后的正则脚本
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegexScript {
    pub id: String,
    /// 脚本名(如 "状态栏"、"去除变量更新")
    pub script_name: String,
    /// 查找模式(JS 风格,如 "/<StatusPlaceHolderImpl/>" 或 "pattern"/"gmi")
    pub find_regex: String,
    /// 替换字符串
    pub replace_string: String,
    /// markdownOnly=true 表示仅作用于用户可见消息(本客户端一律作用于消息渲染,不区分 role)
    pub markdown_only: bool,
    /// 是否启用
    pub enabled: bool,
}

/// 从角色卡 data_raw 提取正则脚本(extensions.regex_scripts 或顶层 regex_scripts)
pub fn extract_regex_scripts(data_raw: &Value) -> Vec<RegexScript> {
    let candidates = [
        data_raw
            .get("extensions")
            .and_then(|e| e.get("regex_scripts"))
            .and_then(|r| r.as_array()),
        data_raw.get("regex_scripts").and_then(|r| r.as_array()),
        // V3 布局:data.extensions.regex_scripts
        data_raw
            .get("data")
            .and_then(|d| d.get("extensions"))
            .and_then(|e| e.get("regex_scripts"))
            .and_then(|r| r.as_array()),
    ];
    for arr in candidates.into_iter().flatten() {
        let mut out: Vec<RegexScript> = Vec::new();
        for v in arr {
            if let Some(s) = normalize_script(v) {
                out.push(s);
            }
        }
        if !out.is_empty() {
            return out;
        }
    }
    Vec::new()
}

fn normalize_script(v: &Value) -> Option<RegexScript> {
    let find_regex = v
        .get("findRegex")
        .and_then(|f| f.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if find_regex.is_empty() {
        return None;
    }
    let enabled = v
        .get("disabled")
        .and_then(|d| d.as_bool())
        .map(|d| !d)
        .unwrap_or(true);
    Some(RegexScript {
        id: v
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or("")
            .to_string(),
        script_name: v
            .get("scriptName")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string(),
        find_regex,
        replace_string: v
            .get("replaceString")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string(),
        markdown_only: v
            .get("markdownOnly")
            .and_then(|m| m.as_bool())
            .unwrap_or(false),
        enabled,
    })
}

/// 将 JS 风格模式(可选 /pattern/flags 包裹)拆分为 (pattern, flags)
fn split_js_regex(find_regex: &str) -> (String, String) {
    let t = find_regex.trim();
    if t.len() >= 2 && t.starts_with('/') {
        // 找到最后一个未转义的 '/'
        let mut i = t.len() - 1;
        while i > 0 {
            let c = t.as_bytes()[i];
            if c == b'/' {
                let count = t.as_bytes()[..i]
                    .iter()
                    .rev()
                    .take_while(|&&b| b == b'\\')
                    .count();
                if count % 2 == 0 {
                    let pattern = t[1..i].to_string();
                    let flags = t[i + 1..].to_string();
                    return (pattern, flags);
                }
            }
            i -= 1;
        }
    }
    (t.to_string(), String::new())
}

/// 从 JS flags 生成 regex 编译配置(regex crate)
fn build_regex(pattern: &str, flags: &str) -> Result<regex::Regex, String> {
    let mut b = regex::RegexBuilder::new(pattern);
    for ch in flags.chars() {
        match ch {
            'i' => {
                b.case_insensitive(true);
            }
            'm' => {
                b.multi_line(true);
            }
            's' => {
                b.dot_matches_new_line(true);
            }
            _ => {}
        }
    }
    b.build().map_err(|e| e.to_string())
}

/// 对文本执行全部启用的正则脚本替换(按顺序;正则无效或替换字符串为空时跳过该脚本)
pub fn apply_regex_scripts(text: &str, scripts: &[RegexScript]) -> String {
    let mut out = text.to_string();
    for s in scripts {
        if !s.enabled {
            continue;
        }
        if s.replace_string.is_empty() {
            continue;
        }
        let (pattern, flags) = split_js_regex(&s.find_regex);
        if pattern.is_empty() {
            continue;
        }
        let Ok(re) = build_regex(pattern.as_str(), &flags) else {
            continue;
        };
        // 用占位替换避免递归处理
        let placeholders: Vec<String> = (0..64).map(|i| format!("\u{0}KDAI_PH_{i}\u{0}")).collect();
        let mut used = 0usize;
        let mut replaced = String::with_capacity(out.len());
        let mut last = 0usize;
        for cap in re.captures_iter(&out) {
            if let Some(m) = cap.get(0) {
                replaced.push_str(&out[last..m.start()]);
                let ph = if used < placeholders.len() {
                    let p = placeholders[used].clone();
                    used += 1;
                    p
                } else {
                    format!("\u{0}KDAI_PH_{}_\u{0}", used + 1000)
                };
                replaced.push_str(&ph);
                last = m.end();
            }
        }
        replaced.push_str(&out[last..]);
        // 还原占位符
        for ph in placeholders.iter().take(used) {
            replaced = replaced.replace(ph, &s.replace_string);
        }
        out = replaced;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn script(find: &str, repl: &str, enabled: bool, markdown_only: bool) -> RegexScript {
        RegexScript {
            id: "x".into(),
            script_name: "t".into(),
            find_regex: find.into(),
            replace_string: repl.into(),
            markdown_only,
            enabled,
        }
    }

    /// 从 extensions.regex_scripts 提取
    #[test]
    fn extracts_from_extensions() {
        let raw = json!({
            "extensions": {
                "regex_scripts": [
                    { "id": "1", "scriptName": "状态栏", "findRegex": "/<StatusPlaceHolderImpl\\/>/g", "replaceString": "<div class=\"status\">状态</div>", "markdownOnly": true, "disabled": false },
                    { "id": "2", "scriptName": "去除标记", "findRegex": "<UpdateVariable>[\\s\\S]*?</UpdateVariable>", "replaceString": "", "markdownOnly": true, "disabled": false }
                ]
            }
        });
        let scripts = extract_regex_scripts(&raw);
        assert_eq!(scripts.len(), 2);
        assert_eq!(scripts[0].script_name, "状态栏");
        assert!(scripts[0].markdown_only);
        assert!(scripts[0].enabled);
    }

    /// V3 data.extensions 布局
    #[test]
    fn extracts_from_v3_data() {
        let raw = json!({
            "data": {
                "extensions": {
                    "regex_scripts": [
                        { "id": "1", "scriptName": "s", "findRegex": "/A/g", "replaceString": "B", "markdownOnly": true, "disabled": false }
                    ]
                }
            }
        });
        let scripts = extract_regex_scripts(&raw);
        assert_eq!(scripts.len(), 1);
    }

    /// JS 风格模式拆分
    #[test]
    fn splits_js_regex() {
        assert_eq!(
            split_js_regex("/<StatusPlaceHolderImpl\\/>/g").0,
            "<StatusPlaceHolderImpl\\/>"
        );
        assert_eq!(split_js_regex("/abc/i").1, "i");
        assert_eq!(split_js_regex("plain").0, "plain");
    }

    /// 占位符替换生效
    #[test]
    fn replaces_placeholder() {
        let s = script(
            r"/<StatusPlaceHolderImpl\s*\/?>/",
            "<b>状态</b>",
            true,
            true,
        );
        let out = apply_regex_scripts("你好 <StatusPlaceHolderImpl/> 再见", &[s]);
        assert_eq!(out, "你好 <b>状态</b> 再见");
    }

    /// 多脚本顺序替换
    #[test]
    fn applies_in_order() {
        let s1 = script("A", "B", true, true);
        let s2 = script("B", "C", true, true);
        // "A B" → s1(A→B) → "B B" → s2(B→C) → "C C"(顺序替换的自然结果)
        let out = apply_regex_scripts("A B", &[s1, s2]);
        assert_eq!(out, "C C");
    }

    /// 禁用 / 空替换串 / 无效正则跳过
    #[test]
    fn skips_invalid_scripts() {
        let disabled = script("A", "B", false, true);
        let empty_repl = script("A", "", true, true);
        let bad_re = script("([", "B", true, true);
        let out = apply_regex_scripts("A", &[disabled, empty_repl, bad_re]);
        assert_eq!(out, "A");
    }
}
