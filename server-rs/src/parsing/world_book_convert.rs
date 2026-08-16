// 世界书条目自动转换(酒馆兼容健壮化)
// 解决导入的酒馆世界书/角色卡内嵌世界书常见的兼容缺陷:
//   1. 关键词字段变体(keys/key/keywords/keyword)与逗号分隔字符串不拆分 → 触发不了
//   2. constant 为字符串 "true"/"false" 或数字 1/0 时被误判为激发
//   3. 缺失 constant 的条目没有自动判定(无关键词无正则 → 常驻,其余 → 激发)
//   4. 缺失 role 的条目未显式"自动"——依赖注入引擎按 常驻→system / 激发→user 分配
// 转换原则:仅修正不规范变体,已规范条目原样保留;无关字段(extensions 等)不动。
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// 转换统计报告(上传响应返回,供前端展示)
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ConversionReport {
    /// 处理的条目总数
    #[serde(default)]
    pub total_count: usize,
    /// 实际发生转换的条目数
    #[serde(default)]
    pub converted_count: usize,
    /// 缺失 constant 时自动判定为常态(常驻)的条数
    #[serde(default)]
    pub constant_auto_count: usize,
    /// 关键词经字段变体/逗号拆分归一的条数
    #[serde(default)]
    pub key_normalized_count: usize,
}

/// 单条条目转换结果(内部:转换后的值 + 该条命中的转换类型)
struct EntryConverted {
    value: Value,
    constant_auto: bool,
    key_normalized: bool,
}

/// 顶层转换入口:定位 entries(独立世界书顶层)与 character_book.entries
/// (角色卡 V2 顶层 / V3 data.character_book 两种布局),逐条规范化并返回统计。
/// 三种布局互斥,按优先级取第一个命中的;无法定位到任何条目时返回全零报告。
pub fn convert_world_book(data_raw: &mut Value) -> ConversionReport {
    let mut report = ConversionReport::default();
    if let Some(entries) = data_raw.get_mut("entries") {
        convert_entries_value(entries, &mut report);
    } else if let Some(cb) = data_raw.get_mut("character_book") {
        if let Some(entries) = cb.get_mut("entries") {
            convert_entries_value(entries, &mut report);
        }
    } else if let Some(d) = data_raw.get_mut("data") {
        if let Some(cb) = d.get_mut("character_book") {
            if let Some(entries) = cb.get_mut("entries") {
                convert_entries_value(entries, &mut report);
            }
        }
    }
    report
}

/// 对 entries 容器(对象 map 或数组)逐条转换并累加报告
fn convert_entries_value(entries: &mut Value, report: &mut ConversionReport) {
    if let Some(map) = entries.as_object_mut() {
        for v in map.values_mut() {
            let converted = normalize_entry_impl(v);
            report.total_count += 1;
            if converted.value != *v {
                report.converted_count += 1;
            }
            if converted.constant_auto {
                report.constant_auto_count += 1;
            }
            if converted.key_normalized {
                report.key_normalized_count += 1;
            }
            *v = converted.value;
        }
    } else if let Some(arr) = entries.as_array_mut() {
        for v in arr.iter_mut() {
            let converted = normalize_entry_impl(v);
            report.total_count += 1;
            if converted.value != *v {
                report.converted_count += 1;
            }
            if converted.constant_auto {
                report.constant_auto_count += 1;
            }
            if converted.key_normalized {
                report.key_normalized_count += 1;
            }
            *v = converted.value;
        }
    }
}

/// 单条转换的公开入口(供自检/测试):返回转换后的值,保留全部原始字段
pub fn normalize_entry_value(v: &Value) -> Value {
    normalize_entry_impl(v).value
}

/// 单条转换实现:
///   - 关键词:字段名兼容 keys/key/keywords/keyword;字符串按 [,，、\n] 拆分 trim 去空;
///     归一后写回 keys(数组),删除变体字段
///   - 常态/激发:constant 布尔原样;字符串 "true"/"false"、数字 1/0 转布尔;
///     缺失或无法解析时自动判定:无关键词且无正则 → 常驻(true),否则 → 激发(false)
///   - 激活状态:兼容 enabled/disable/disabled 变体,归一写回 enabled + disable(反转),删除 disabled
///   - 属性默认自动:role 仅保留 system/user/assistant 合法值,其余/缺失一律移除字段
fn normalize_entry_impl(v: &Value) -> EntryConverted {
    let Some(map) = v.as_object() else {
        return EntryConverted {
            value: v.clone(),
            constant_auto: false,
            key_normalized: false,
        };
    };
    let mut out = map.clone();

    // ---- 关键词归一 ----
    let (keys, key_normalized) = normalize_keys(&out);
    let keys_value: Vec<Value> = keys.iter().map(|k| Value::String(k.clone())).collect();
    out.insert("keys".into(), Value::Array(keys_value));
    for f in ["key", "keywords", "keyword"] {
        out.remove(f);
    }

    // ---- 常态/激发 ----
    let (constant, constant_auto) = normalize_constant(&out, &keys);
    out.insert("constant".into(), Value::Bool(constant));

    // ---- 激活状态 ----
    let enabled = read_enabled(&out);
    out.insert("enabled".into(), Value::Bool(enabled));
    out.insert("disable".into(), Value::Bool(!enabled));
    out.remove("disabled");

    // ---- 属性默认自动 ----
    match out.get("role") {
        Some(Value::String(s)) => {
            let t = s.trim().to_lowercase();
            if t == "system" || t == "user" || t == "assistant" {
                out.insert("role".into(), Value::String(t));
            } else {
                out.remove("role");
            }
        }
        _ => {
            out.remove("role");
        }
    }

    EntryConverted {
        value: Value::Object(out),
        constant_auto,
        key_normalized,
    }
}

/// 关键词归一:兼容字段名 keys/key/keywords/keyword(首个非空字段优先);
/// 字符串按逗号/中文逗号/顿号/换行拆分 trim 去空;数组逐项转字符串去空。
/// 返回 (规范 keys 数组, 是否发生了归一)。
fn normalize_keys(map: &Map<String, Value>) -> (Vec<String>, bool) {
    let mut used_field: Option<&str> = None;
    let mut raw: Option<&Value> = None;
    for f in ["keys", "key", "keywords", "keyword"] {
        if let Some(v) = map.get(f) {
            used_field = Some(f);
            raw = Some(v);
            break;
        }
    }
    let Some(raw) = raw else {
        return (Vec::new(), false);
    };
    let keys: Vec<String> = match raw {
        Value::String(s) => s
            .split([',', '，', '、', '\n', '\r'])
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect(),
        Value::Array(arr) => arr
            .iter()
            .map(|x| x.as_str().map(|s| s.trim().to_string()).unwrap_or_default())
            .filter(|s| !s.is_empty())
            .collect(),
        other => {
            // 数字等其它形态 → 单元素
            vec![other.as_str().unwrap_or_default().trim().to_string()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect()
        }
    };
    // 判定是否发生归一:字段名不是 keys、字符串形态、数组含非字符串/空串
    let normalized = used_field != Some("keys")
        || matches!(raw, Value::String(_))
        || matches!(raw, Value::Array(arr) if arr.iter().any(|x| !x.is_string() || x.as_str().unwrap().trim().is_empty()));
    (keys, normalized)
}

/// 常态/激发解析:constant 布尔原样;字符串 "true"/"1"/"yes" → true、"false"/"0"/"no" → false;
/// 数字 1 → true、0 → false;缺失/无法解析时自动判定:
/// 无关键词且无正则 → 常驻(true),其余 → 激发(false)。
/// 返回 (constant, 是否走了自动判定)。
fn normalize_constant(map: &Map<String, Value>, keys: &[String]) -> (bool, bool) {
    if let Some(c) = map.get("constant").and_then(read_bool) {
        return (c, false);
    }
    let has_regex = map
        .get("regex")
        .and_then(|r| r.as_str())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let auto = keys.is_empty() && !has_regex;
    (auto, true)
}

/// 兼容布尔读取:bool 原样;字符串 "true"/"1"/"yes"(trim 忽略大小写)→ true,
/// "false"/"0"/"no" → false;数字 1 → true、0 → false;其余返回 None
fn read_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.trim().to_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Some(true),
            "false" | "0" | "no" | "off" => Some(false),
            _ => None,
        },
        Value::Number(n) => match n.as_i64() {
            Some(1) => Some(true),
            Some(0) => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// 激活状态读取:enabled 优先;否则 disable(反转);否则 disabled(反转);全缺省默认启用
fn read_enabled(map: &Map<String, Value>) -> bool {
    if let Some(e) = map.get("enabled").and_then(read_bool) {
        return e;
    }
    if let Some(d) = map.get("disable").and_then(read_bool) {
        return !d;
    }
    if let Some(d) = map.get("disabled").and_then(read_bool) {
        return !d;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn conv(v: &Value) -> Value {
        normalize_entry_value(v)
    }

    /// 逗号分隔字符串关键词 → 拆分为数组
    #[test]
    fn splits_comma_string_keys() {
        let v = conv(&json!({ "uid": 0, "content": "内容", "keys": "车站, 铁路、码头" }));
        assert_eq!(v["keys"], json!(["车站", "铁路", "码头"]));
    }

    /// keywords / keyword 字段变体 → 归一为 keys
    #[test]
    fn normalizes_keywords_field_variants() {
        let v = conv(&json!({ "uid": 0, "content": "内容", "keywords": ["火车站"] }));
        assert_eq!(v["keys"], json!(["火车站"]));
        assert!(v.get("keywords").is_none(), "变体字段应删除");
        let v = conv(&json!({ "uid": 1, "content": "内容", "keyword": "码头" }));
        assert_eq!(v["keys"], json!(["码头"]));
    }

    /// 字符串/数字 constant → 布尔
    #[test]
    fn converts_constant_variants() {
        assert_eq!(
            conv(&json!({ "keys": ["a"], "constant": "true" }))["constant"],
            json!(true)
        );
        assert_eq!(
            conv(&json!({ "keys": ["a"], "constant": "FALSE" }))["constant"],
            json!(false)
        );
        assert_eq!(
            conv(&json!({ "keys": ["a"], "constant": 1 }))["constant"],
            json!(true)
        );
        assert_eq!(
            conv(&json!({ "keys": ["a"], "constant": 0 }))["constant"],
            json!(false)
        );
    }

    /// 缺失 constant 自动判定:有 key → 激发;无 key 无 regex → 常驻;仅 regex → 激发
    #[test]
    fn auto_detects_constant() {
        let v = conv(&json!({ "uid": 0, "content": "内容", "keys": ["人物"] }));
        assert_eq!(v["constant"], json!(false), "有关键词 → 激发");
        let v = conv(&json!({ "uid": 1, "content": "世界观" }));
        assert_eq!(v["constant"], json!(true), "无关键词无正则 → 常驻");
        let v = conv(&json!({ "uid": 2, "content": "内容", "regex": "\\d{4}" }));
        assert_eq!(v["constant"], json!(false), "仅正则 → 激发");
    }

    /// role 缺失/非法 → 移除(自动);合法值保留
    #[test]
    fn role_defaults_to_auto() {
        let v = conv(&json!({ "keys": ["a"], "content": "内容", "role": "  System " }));
        assert_eq!(v["role"], json!("system"), "合法 role 规范化保留");
        let v = conv(&json!({ "keys": ["a"], "content": "内容", "role": "xxx" }));
        assert!(v.get("role").is_none(), "非法 role 移除 → 自动");
        let v = conv(&json!({ "keys": ["a"], "content": "内容" }));
        assert!(v.get("role").is_none(), "缺失 role → 自动");
    }

    /// 已规范条目原样保留(值不变)
    #[test]
    fn leaves_normalized_entries_untouched() {
        let raw = json!({ "uid": 0, "comment": "地点", "keys": ["图书馆"], "content": "安静。", "constant": false, "enabled": true, "disable": false, "position": 0 });
        assert_eq!(conv(&raw), raw);
    }

    /// 激活状态:disabled 变体 → enabled + disable 反转
    #[test]
    fn normalizes_disabled_variant() {
        let v = conv(&json!({ "keys": ["a"], "content": "内容", "disabled": true }));
        assert_eq!(v["enabled"], json!(false));
        assert_eq!(v["disable"], json!(true));
        assert!(v.get("disabled").is_none());
    }

    /// 顶层入口:独立世界书 entries(对象/数组)与角色卡 character_book(V2/V3)均被转换
    #[test]
    fn convert_world_book_locations() {
        let mut raw = json!({
            "entries": {
                "0": { "uid": 0, "keys": "车站, 铁路", "content": "A", "constant": "true" },
                "1": { "uid": 1, "keys": [], "content": "世界观" }
            }
        });
        let report = convert_world_book(&mut raw);
        assert_eq!(report.total_count, 2);
        assert_eq!(
            report.constant_auto_count, 1,
            "条目1 缺失 constant 自动判定"
        );
        assert_eq!(report.key_normalized_count, 1, "条目0 关键词拆分");
        assert_eq!(raw["entries"]["0"]["keys"], json!(["车站", "铁路"]));
        assert_eq!(raw["entries"]["0"]["constant"], json!(true));
        assert_eq!(
            raw["entries"]["1"]["constant"],
            json!(true),
            "无关键词 → 常驻"
        );

        let mut raw = json!({
            "character_book": {
                "entries": [
                    { "id": 0, "keyword": "码头", "content": "B" },
                    { "id": 1, "content": "C", "constant": 0 }
                ]
            }
        });
        let report = convert_world_book(&mut raw);
        assert_eq!(report.total_count, 2);
        assert_eq!(raw["character_book"]["entries"][0]["keys"], json!(["码头"]));
        assert_eq!(
            raw["character_book"]["entries"][1]["constant"],
            json!(false)
        );

        let mut raw = json!({
            "data": {
                "character_book": {
                    "entries": [
                        { "id": 0, "keys": "甲、乙", "content": "D", "constant": false }
                    ]
                }
            }
        });
        let report = convert_world_book(&mut raw);
        assert_eq!(report.total_count, 1);
        assert_eq!(
            raw["data"]["character_book"]["entries"][0]["keys"],
            json!(["甲", "乙"])
        );
    }

    /// 转换后再 collect_entries 重新解析,字段读取正确
    #[test]
    fn converted_entries_reparse_correctly() {
        let mut raw = json!({
            "entries": [
                { "uid": 0, "keys": "火车站、铁路", "content": "车站设定。", "constant": "true", "disable": false },
                { "uid": 1, "content": "世界观设定。" }
            ]
        });
        let report = convert_world_book(&mut raw);
        assert_eq!(report.converted_count, 2);
        let entries = crate::parsing::world_book::collect_entries(&raw);
        assert_eq!(entries.len(), 2);
        let e0 = entries.iter().find(|e| e.id == 0).unwrap();
        assert_eq!(e0.keys, vec!["火车站", "铁路"]);
        assert!(e0.constant);
        assert!(e0.enabled);
        let e1 = entries.iter().find(|e| e.id == 1).unwrap();
        assert!(e1.constant, "无关键词条目 → 常驻");
        assert!(e1.role.is_none(), "role 缺失 → 自动");
    }
}
