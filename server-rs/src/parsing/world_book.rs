// 世界书(World Info / Lorebook)解析
// 兼容两种来源:
//   1. 独立世界书 JSON(SillyTavern 导出格式:顶层 {"entries": {...对象|数组}} 或 {"originalData": {...}})
//   2. 角色卡 data_raw.character_book.entries(数组,snake_case 字段)
// 条目字段同时兼容 camelCase(ST 新格式:keys/uid/disable)与 snake_case(角色卡:keys/id/enabled)。
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 规范化后的世界书条目(可序列化供 API 返回)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorldEntry {
    /// 数值 id:优先 uid/id,缺失时用序号
    pub id: i64,
    /// 注释/标题(如 "地点"、"世界观")
    pub comment: String,
    /// 触发关键字(子串匹配)
    pub keys: Vec<String>,
    /// 副关键词(ST keysecondary / secondary_keys,命中条件与 keys 并列)
    #[serde(default)]
    pub keys_secondary: Vec<String>,
    /// 可选正则表达式(SillyTavern World Info 的 regex 字段);use_regex=true 时优先于 keys 匹配
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
    /// 是否启用正则匹配(兼容 useRegex / use_regex)
    #[serde(default)]
    pub use_regex: bool,
    pub content: String,
    /// constant=true 时始终注入,不受 key 匹配限制
    pub constant: bool,
    /// 是否启用(兼容 enabled / disable 两个字段;缺省视为启用)
    pub enabled: bool,
    /// 注入位置权重(0=before_char ... 4=after_char;本项目统一并入 system,仅用于排序)
    pub position: i64,
    /// 扫描深度(ST depth):从最新消息往前扫最近 N 条(0 = 全部历史)
    #[serde(default = "default_depth")]
    pub depth: i64,
    /// 注入顺序(ST order / insertion_order):同 position 内的条目先后
    #[serde(default = "default_order")]
    pub order: i64,
    /// 关键词大小写敏感(ST caseSensitive / case_sensitive)
    #[serde(default)]
    pub case_sensitive: bool,
    /// 粘性(ST sticky):命中后接下来 N 条消息持续注入
    #[serde(default)]
    pub sticky: i64,
    /// 冷却(ST cooldown):命中后 N 条消息内不再触发
    #[serde(default)]
    pub cooldown: i64,
    /// 命中概率%(ST probability + useProbability)
    #[serde(default = "default_probability")]
    pub probability: i64,
    /// 是否启用概率(useProbability / use_probability)
    #[serde(default)]
    pub use_probability: bool,
    /// 注入消息角色(system / user / assistant;本项目扩展字段):
    /// None = 缺省按注入位置决定 —— 常驻条目默认 system(并入系统提示词),
    /// 激发条目默认 user(追加最新用户消息尾部)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// ST-Prompt-Template 兼容:内容开头解析出的 `@@` 装饰器行原文
    /// (如 "@@if variables.affection > 50"),对应 content 已剥离装饰器行;
    /// 未识别的装饰器也保留在此(兼容层不丢信息)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decorators: Vec<String>,
}

fn default_depth() -> i64 {
    4
}
fn default_order() -> i64 {
    100
}
fn default_probability() -> i64 {
    100
}

/// 解析内容开头的 `@@` 装饰器行(ST-Prompt-Template 兼容)。
///
/// 规则:
///   - 装饰器行必须在内容开头、每行一个、相邻行之间不能有空行;
///     遇第一个非装饰器行(不以 `@@` 开头)或空行即停止
///   - 装饰器名称与参数以第一个空格分隔,如 `@@if variables.affection > 50`
///     → decorators 存整行 `"@@if variables.affection > 50"`
///   - `@@@` 前缀转义:`@@@activate` 表示字面 `@@activate`(不生效,保留为内容)
///   - 未识别的 `@@xxx` 仍保留在 decorators(兼容层不丢信息)
///
/// 返回 (装饰器行集合, 剥离装饰器行后的 clean_content)。
pub fn parse_decorators(content: &str) -> (Vec<String>, String) {
    let mut decorators: Vec<String> = Vec::new();
    let mut clean: Vec<String> = Vec::new();
    let mut in_decorator_section = true;
    for line in content.split('\n') {
        // 装饰器行比较前先剥掉行尾空白(兼容 \r\n),内容行保持原文
        let t = line.trim_end();
        if in_decorator_section {
            if t.is_empty() {
                // 空行:装饰器区结束,空行本身保留在内容里
                in_decorator_section = false;
                clean.push(line.to_string());
                continue;
            }
            if let Some(rest) = t.strip_prefix("@@@") {
                // 转义:@@@ 表示字面 @@(剥掉一个 @),保留为内容;
                // 转义只影响本行,装饰器区继续扫描后续 @@ 行
                clean.push(format!("@@{rest}"));
                continue;
            }
            if t.starts_with("@@") {
                decorators.push(t.to_string());
                continue;
            }
            // 非装饰器行:装饰器区结束,该行计入内容
            in_decorator_section = false;
        }
        clean.push(line.to_string());
    }
    (decorators, clean.join("\n"))
}

/// 条目装饰器判定辅助(engine 层直接调用,判定语义集中在此层)。
/// 装饰器名 = 整行的第一个空格前部分(含 `@@` 前缀),如 "@@if"。
#[derive(Debug, Clone, Default)]
pub struct EntryDecorators {
    /// 装饰器整行原文(与 args 一一对应)
    pub all: Vec<String>,
    /// 每个装饰器的参数部分(第一个空格后的内容,无参数为空串)
    pub args: Vec<String>,
}

impl EntryDecorators {
    /// 装饰器名精确匹配(如 "@@if")
    pub fn has(&self, name: &str) -> bool {
        self.all.iter().any(|l| decorator_name(l) == name)
    }

    /// 命中任意一个装饰器名
    pub fn has_any(&self, names: &[&str]) -> bool {
        names.iter().any(|n| self.has(n))
    }

    /// 取该装饰器的参数(第一个空格后的内容;无参数返回空串;装饰器不存在返回 None)
    pub fn arg(&self, name: &str) -> Option<&str> {
        self.all
            .iter()
            .position(|l| decorator_name(l) == name)
            .map(|i| self.args[i].as_str())
    }
}

/// 装饰器名:整行第一个空格前的部分(无空格则整行)
fn decorator_name(line: &str) -> &str {
    let t = line.trim();
    match t.find(' ') {
        Some(i) => &t[..i],
        None => t,
    }
}

/// 从条目提取装饰器判定辅助(engine 层直接调用)
pub fn parse_entry_decorators(e: &WorldEntry) -> EntryDecorators {
    let mut all = Vec::new();
    let mut args = Vec::new();
    for line in &e.decorators {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        // 参数 = 第一个空格后的内容;无空格 → 无参数
        let arg = match t.find(' ') {
            Some(i) => t[i + 1..].trim().to_string(),
            None => String::new(),
        };
        all.push(t.to_string());
        args.push(arg);
    }
    EntryDecorators { all, args }
}

/// 解析结果(独立世界书文件)
pub struct ParsedWorldBook {
    /// 世界书名称:data.name || character_book.name || 文件名去扩展名
    pub name: String,
    /// 规范化条目(按 position,id 稳定排序)
    pub entries: Vec<WorldEntry>,
    /// 原始 JSON 完整保留(无损)
    pub data_raw: Value,
}

/// 从文件字节解析独立世界书(UTF-8 JSON)
pub fn parse_world_book_file(
    buffer: &[u8],
    original_name: &str,
) -> Result<ParsedWorldBook, String> {
    let text = String::from_utf8_lossy(buffer);
    let data: Value = serde_json::from_str(&text)
        .map_err(|e| format!("无法解析世界书 \"{original_name}\":不是有效的 JSON({e})"))?;
    if !data.is_object() {
        return Err(format!(
            "无法解析世界书 \"{original_name}\":JSON 顶层须为对象"
        ));
    }
    let entries = collect_entries(&data);
    if entries.is_empty() {
        return Err(format!(
            "无法解析世界书 \"{original_name}\":未找到任何有效条目(entries)"
        ));
    }
    let name = pick_name(&data, original_name);
    Ok(ParsedWorldBook {
        name,
        entries,
        data_raw: data,
    })
}

/// 从任意 JSON 收集规范化条目:
///   - value["entries"] 为对象(ST 用数字 uid 作键)或数组
///   - value 本身为数组
///   - value 本身为单条目对象(含 content 或 keys)
pub fn collect_entries(value: &Value) -> Vec<WorldEntry> {
    let mut out: Vec<WorldEntry> = Vec::new();
    if let Some(entries) = value.get("entries") {
        if let Some(arr) = entries.as_array() {
            for v in arr {
                if let Some(e) = normalize_entry(v) {
                    out.push(e);
                }
            }
        } else if let Some(map) = entries.as_object() {
            // 对象形式:遍历 value(uid 作键),顺序不保证,靠 position+id 排序兜底
            for v in map.values() {
                if let Some(e) = normalize_entry(v) {
                    out.push(e);
                }
            }
        }
    } else if let Some(arr) = value.as_array() {
        for v in arr {
            if let Some(e) = normalize_entry(v) {
                out.push(e);
            }
        }
    } else if value.is_object()
        && (value.get("content").is_some()
            || value.get("keys").is_some()
            || value.get("key").is_some())
    {
        if let Some(e) = normalize_entry(value) {
            out.push(e);
        }
    }
    // 稳定排序:position 升序 → order(注入顺序)升序 → id 升序(与酒馆 displayIndex/insertion_order 一致)
    out.sort_by(|a, b| {
        a.position
            .cmp(&b.position)
            .then(a.order.cmp(&b.order))
            .then(a.id.cmp(&b.id))
    });
    out
}

/// 从角色卡 data_raw 提取内嵌世界书条目(character_book.entries)
/// 兼容 V2(顶层 character_book)与 V3(data.character_book)两种布局。
pub fn character_book_entries(data_raw: &Value) -> Vec<WorldEntry> {
    let candidates = [
        data_raw.get("character_book"),
        data_raw.get("data").and_then(|d| d.get("character_book")),
    ];
    for cb in candidates.into_iter().flatten() {
        let entries = collect_entries(cb);
        if !entries.is_empty() {
            return entries;
        }
    }
    Vec::new()
}

/// 单条规范化;无 content 且无 keys 且无 regex 的条目视为无效,返回 None
fn normalize_entry(v: &Value) -> Option<WorldEntry> {
    let content = v
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    let keys = read_keys(v);
    let regex = v
        .get("regex")
        .and_then(|r| r.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    // ST-Prompt-Template 装饰器:内容开头的 @@ 行被剥离存 decorators,剩余为实际内容
    let (decorators, content) = parse_decorators(&content);
    if content.is_empty() && keys.is_empty() && regex.is_none() {
        return None;
    }
    let id = v
        .get("uid")
        .or_else(|| v.get("id"))
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let comment = v
        .get("comment")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    let constant = v.get("constant").and_then(|c| c.as_bool()).unwrap_or(false);
    // 正则:regex 字符串 + use_regex(兼容 useRegex);正则无效时视为不使用正则匹配
    let use_regex = v
        .get("use_regex")
        .or_else(|| v.get("useRegex"))
        .and_then(|u| u.as_bool())
        .unwrap_or(false)
        && regex.is_some();
    // enabled 字段优先;其次 disable(ST 导出用 disable 表示停用);缺省视为启用
    let enabled = match v.get("enabled") {
        Some(e) => e.as_bool().unwrap_or(true),
        None => match v.get("disable") {
            Some(d) => !d.as_bool().unwrap_or(false),
            None => true,
        },
    };
    // 副关键词:keysecondary / secondary_keys(与 keys 同样兼容 string/array)
    let keys_secondary = read_field_list(v, &["keysecondary", "secondary_keys"]);
    // 位置:数字 0-4,或 ST 导出字符串 before_char/1/2/3/after_char
    let position = read_position(v);
    // 扫描深度(ST depth;数字)
    let depth = v
        .get("depth")
        .and_then(|d| d.as_i64())
        .unwrap_or(default_depth());
    // 注入顺序:order(ST) / insertion_order(originalData);缺省 100
    let order = v
        .get("order")
        .or_else(|| v.get("insertion_order"))
        .and_then(|o| o.as_i64())
        .unwrap_or(default_order());
    // 大小写敏感:caseSensitive / case_sensitive
    let case_sensitive = v
        .get("case_sensitive")
        .or_else(|| v.get("caseSensitive"))
        .and_then(|c| c.as_bool())
        .unwrap_or(false);
    // 粘性/冷却:sticky / cooldown
    let sticky = v.get("sticky").and_then(|s| s.as_i64()).unwrap_or(0);
    let cooldown = v.get("cooldown").and_then(|c| c.as_i64()).unwrap_or(0);
    // 概率:probability + useProbability(extensions 里也有一份,优先条目顶层)
    let probability = v
        .get("probability")
        .or_else(|| v.get("extensions").and_then(|e| e.get("probability")))
        .and_then(|p| p.as_i64())
        .unwrap_or(default_probability());
    let use_probability = v
        .get("use_probability")
        .or_else(|| v.get("useProbability"))
        .or_else(|| v.get("extensions").and_then(|e| e.get("useProbability")))
        .and_then(|u| u.as_bool())
        .unwrap_or(false);
    // 注入角色(system / user / assistant;缺省 None = 按位置语义决定)
    let role = v
        .get("role")
        .and_then(|r| r.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| s == "system" || s == "user" || s == "assistant");
    Some(WorldEntry {
        id,
        comment,
        keys,
        keys_secondary,
        regex,
        use_regex,
        content,
        constant,
        enabled,
        position,
        depth,
        order,
        case_sensitive,
        sticky,
        cooldown,
        probability,
        use_probability,
        role,
        decorators,
    })
}

/// 读取 position:数字 0-4,或 ST originalData 导出的字符串
/// (0=before_char, 1=top, 2=normal, 3=bottom, 4=after_char;数字字符串直接转)
fn read_position(v: &Value) -> i64 {
    if let Some(n) = v.get("position").and_then(|p| p.as_i64()) {
        return n;
    }
    if let Some(s) = v.get("position").and_then(|p| p.as_str()) {
        return match s.trim() {
            "before_char" | "0" => 0,
            "top" | "1" => 1,
            "normal" | "2" => 2,
            "bottom" | "3" => 3,
            "after_char" | "4" => 4,
            other => other.parse().unwrap_or(0),
        };
    }
    0
}

/// 读取字符串/数组字段(keysecondary / secondary_keys 等)
fn read_field_list(v: &Value, fields: &[&str]) -> Vec<String> {
    for field in fields {
        if let Some(k) = v.get(field) {
            if let Some(s) = k.as_str() {
                if !s.is_empty() {
                    return vec![s.to_string()];
                }
            }
            if let Some(arr) = k.as_array() {
                let mut out = Vec::new();
                for item in arr {
                    if let Some(s) = item.as_str() {
                        if !s.is_empty() {
                            out.push(s.to_string());
                        }
                    }
                }
                return out;
            }
        }
    }
    Vec::new()
}

/// WorldEntry → 编辑视图(含 position/depth/order 等,供前端展示/编辑)
pub fn entry_to_view(e: &WorldEntry) -> crate::models::types::WorldBookEntryView {
    crate::models::types::WorldBookEntryView {
        id: e.id,
        comment: e.comment.clone(),
        keys: e.keys.clone(),
        keys_secondary: e.keys_secondary.clone(),
        regex: e.regex.clone(),
        use_regex: e.use_regex,
        constant: e.constant,
        enabled: e.enabled,
        content: e.content.clone(),
        position: e.position,
        depth: e.depth,
        order: e.order,
        case_sensitive: e.case_sensitive,
        sticky: e.sticky,
        cooldown: e.cooldown,
        probability: e.probability,
        use_probability: e.use_probability,
        role: e.role.clone(),
    }
}

/// 编辑视图 → JSON 对象(写回 data_raw.entries 用)
/// 统一输出 ST/角色卡兼容字段:uid/id、keys(数组)、use_regex、disable(ST 惯用,enabled 反转)、
/// position、depth、order、keysecondary、case_sensitive、sticky、cooldown、probability。
/// 注意:未改动的 extensions 保留(data_raw 全量持久化,此处仅补写条目顶层字段)。
pub fn view_to_value(v: &crate::models::types::WorldBookEntryView) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("id".into(), serde_json::json!(v.id));
    m.insert("uid".into(), serde_json::json!(v.id));
    m.insert("comment".into(), serde_json::json!(v.comment));
    m.insert("keys".into(), serde_json::json!(v.keys));
    m.insert("keysecondary".into(), serde_json::json!(v.keys_secondary));
    if let Some(r) = &v.regex {
        m.insert("regex".into(), serde_json::json!(r));
    }
    m.insert("use_regex".into(), serde_json::json!(v.use_regex));
    m.insert("constant".into(), serde_json::json!(v.constant));
    // 用 disable 字段承载启用状态(与 ST/角色卡 V2 惯用一致),enabled 也一并写保证兼容
    m.insert("disable".into(), serde_json::json!(!v.enabled));
    m.insert("enabled".into(), serde_json::json!(v.enabled));
    m.insert("content".into(), serde_json::json!(v.content));
    m.insert("position".into(), serde_json::json!(v.position));
    m.insert("depth".into(), serde_json::json!(v.depth));
    m.insert("order".into(), serde_json::json!(v.order));
    m.insert("case_sensitive".into(), serde_json::json!(v.case_sensitive));
    m.insert("sticky".into(), serde_json::json!(v.sticky));
    m.insert("cooldown".into(), serde_json::json!(v.cooldown));
    m.insert("probability".into(), serde_json::json!(v.probability));
    m.insert(
        "use_probability".into(),
        serde_json::json!(v.use_probability),
    );
    if let Some(r) = &v.role {
        m.insert("role".into(), serde_json::json!(r));
    }
    Value::Object(m)
}

/// 写回条目数组:entries 为对象 map 时按 uid 替换/追加并**删除已移除的条目**,
/// 数组时整组替换(维持 id 稳定排序)。
pub fn merge_entries_into(
    entries_value: &mut Value,
    views: &[crate::models::types::WorldBookEntryView],
) {
    let arr: Vec<Value> = views.iter().map(view_to_value).collect();
    if let Some(map) = entries_value.as_object_mut() {
        // 视图中的 uid 集合:仅这些条目保留/更新,其余条目(已在前端删除)一律移除
        let view_uids: std::collections::HashSet<String> = arr
            .iter()
            .filter_map(|v| v.get("uid").and_then(|u| u.as_i64()).map(|u| u.to_string()))
            .collect();
        let keep: serde_json::Map<String, Value> = map
            .iter()
            .filter(|(k, _)| {
                // 非条目字段(如 name)保留;数字键仅在仍属于视图集合时保留
                !k.parse::<i64>().is_ok() || view_uids.contains(k.as_str())
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut new_map = keep;
        for v in arr {
            let key = v
                .get("uid")
                .and_then(|u| u.as_i64())
                .map(|u| u.to_string())
                .unwrap_or_default();
            new_map.insert(key, v);
        }
        *map = new_map;
    } else {
        *entries_value = Value::Array(arr);
    }
}

fn pick_name(data: &Value, original_name: &str) -> String {
    if let Some(n) = data.get("name").and_then(|n| n.as_str()) {
        if !n.is_empty() {
            return n.to_string();
        }
    }
    if let Some(cb) = data
        .get("character_book")
        .and_then(|cb| cb.get("name"))
        .and_then(|n| n.as_str())
    {
        if !cb.is_empty() {
            return cb.to_string();
        }
    }
    strip_image_ext(original_name)
}

/// keys 兼容 string 与 array 两种形态(字段名 keys / key)
fn read_keys(v: &Value) -> Vec<String> {
    for field in ["keys", "key"] {
        if let Some(k) = v.get(field) {
            if let Some(s) = k.as_str() {
                if !s.is_empty() {
                    return vec![s.to_string()];
                }
            }
            if let Some(arr) = k.as_array() {
                let mut out = Vec::new();
                for item in arr {
                    if let Some(s) = item.as_str() {
                        if !s.is_empty() {
                            out.push(s.to_string());
                        }
                    }
                }
                return out;
            }
        }
    }
    Vec::new()
}

fn strip_image_ext(name: &str) -> String {
    let lower = name.to_lowercase();
    if lower.ends_with(".json") {
        name[..name.len() - 5].to_string()
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 独立世界书 ST 导出格式(对象 entries + originalData)
    #[test]
    fn parses_st_object_entries_format() {
        let raw = json!({
            "entries": {
                "0": { "uid": 0, "comment": "地点", "keys": ["图书馆"], "content": "图书馆安静。", "constant": false, "disable": false, "position": 0 },
                "1": { "uid": 1, "comment": "人物", "keys": ["芽衣"], "content": "芽衣是兔族少女。", "constant": true, "disable": false, "position": 0 }
            },
            "originalData": { "entries": [] }
        });
        let entries = collect_entries(&raw);
        assert_eq!(entries.len(), 2);
        let mei = entries.iter().find(|e| e.comment == "人物").unwrap();
        assert!(mei.constant);
        assert!(mei.enabled);
        assert_eq!(mei.keys, vec!["芽衣"]);
        let loc = entries.iter().find(|e| e.comment == "地点").unwrap();
        assert_eq!(loc.id, 0);
        assert!(!loc.constant);
    }

    /// 独立世界书数组 entries 格式
    #[test]
    fn parses_st_array_entries_format() {
        let raw = json!({
            "entries": [
                { "uid": 3, "comment": "A", "keys": ["a"], "content": "内容A", "constant": false, "position": 0 },
                { "uid": 1, "comment": "B", "keys": [], "content": "内容B", "constant": true, "position": 0 }
            ]
        });
        let entries = collect_entries(&raw);
        assert_eq!(entries.len(), 2);
        // 按 id 稳定排序:A(3) 在 B(1) 之后
        assert_eq!(entries[0].comment, "B");
        assert_eq!(entries[1].comment, "A");
    }

    /// 角色卡 character_book.entries(snake_case:enabled/id/key)
    #[test]
    fn parses_character_book_entries() {
        let raw = json!({
            "character_book": {
                "name": "小兔子",
                "entries": [
                    { "id": 0, "comment": "[InitVar]", "content": "世界:\n 时间: 14:30", "enabled": false, "constant": false },
                    { "id": 1, "comment": "分阶段人设", "keys": [], "content": "<emily_staged_performance>…", "enabled": true, "constant": true }
                ]
            }
        });
        let entries = character_book_entries(&raw);
        assert_eq!(entries.len(), 2);
        assert!(!entries[0].enabled);
        assert!(entries[1].enabled);
        assert!(entries[1].constant);
    }

    /// 角色卡 V3 布局:character_book 位于 data 子对象内
    #[test]
    fn parses_character_book_v3_layout() {
        let raw = json!({
            "spec": "chara_card_v3",
            "data": {
                "character_book": {
                    "name": "小兔子",
                    "entries": [
                        { "id": 0, "comment": "世界观", "content": "兽人社会。", "enabled": true, "constant": true }
                    ]
                }
            }
        });
        let entries = character_book_entries(&raw);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].comment, "世界观");
        assert!(entries[0].constant);
    }

    /// 单条对象也可识别
    #[test]
    fn parses_single_entry_object() {
        let raw = json!({ "uid": 9, "comment": "世界观", "keys": ["兽人"], "content": "兽人社会。", "constant": false });
        let entries = collect_entries(&raw);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "兽人社会。");
    }

    /// key 字段为字符串时归一为数组
    #[test]
    fn keys_accepts_single_string() {
        let raw = json!({ "uid": 1, "key": "火车站", "content": "车站", "constant": false });
        let entries = collect_entries(&raw);
        assert_eq!(entries[0].keys, vec!["火车站"]);
    }

    /// regex + use_regex 解析(兼容 useRegex camelCase)
    #[test]
    fn parses_regex_field() {
        let raw = json!({ "entries": [
            { "uid": 0, "comment": "正则条目", "regex": "\\d{4}-\\d{2}", "use_regex": true, "content": "匹配日期", "constant": false, "disable": false },
            { "uid": 1, "comment": "camel", "regex": "abc", "useRegex": true, "content": "camel 条目", "constant": false },
            { "uid": 2, "comment": "未启用正则", "regex": "xyz", "use_regex": false, "content": "走 keys", "constant": false },
            { "uid": 3, "comment": "只有正则", "regex": "solo", "use_regex": true, "content": "", "constant": false }
        ] });
        let entries = collect_entries(&raw);
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].regex.as_deref(), Some("\\d{4}-\\d{2}"));
        assert!(entries[0].use_regex);
        assert!(entries[1].use_regex);
        assert!(!entries[2].use_regex);
        // 只有正则无 content 的条目仍保留(有效)
        assert_eq!(entries[3].regex.as_deref(), Some("solo"));
        // 空 content + 无 keys + 无 regex 仍被过滤
        let empty =
            json!({ "uid": 9, "comment": "空", "content": "", "keys": [], "constant": false });
        assert!(collect_entries(&empty).is_empty());
    }

    /// 无效条目(无 content 无 keys)被过滤
    #[test]
    fn filters_invalid_entries() {
        let raw = json!({ "entries": [
            { "uid": 0, "comment": "empty", "content": "", "keys": [], "constant": false },
            { "uid": 1, "comment": "ok", "content": "有效", "keys": ["k"], "constant": false }
        ] });
        let entries = collect_entries(&raw);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].comment, "ok");
    }

    /// 文件解析:名称回退到文件名
    #[test]
    fn parses_file_with_name_fallback() {
        let raw = br#"{"entries":{"0":{"uid":0,"keys":["a"],"content":"x","constant":true,"disable":false}}}"#;
        let parsed = parse_world_book_file(raw, "我的世界书.json").unwrap();
        assert_eq!(parsed.name, "我的世界书");
        assert_eq!(parsed.entries.len(), 1);
        // data_raw 无损保留
        assert!(parsed.data_raw.get("entries").is_some());
    }

    /// 非 JSON / 空条目 → 报错
    #[test]
    fn rejects_invalid_files() {
        assert!(parse_world_book_file(b"not json", "a.json").is_err());
        assert!(parse_world_book_file(br#"{"entries":{}}"#, "a.json").is_err());
    }

    /// ST originalData 导出:position 为字符串(before_char 等)、insertion_order、secondary_keys
    #[test]
    fn parses_st_original_data_layout() {
        let raw = json!({
            "entries": [
                { "comment": "主线", "keys": [], "constant": true, "content": "主线设定。", "position": "before_char", "insertion_order": 100, "secondary_keys": ["副1", "副2"], "case_sensitive": null, "use_regex": true }
            ]
        });
        let entries = collect_entries(&raw);
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.position, 0, "before_char → 0");
        assert_eq!(e.order, 100, "insertion_order → order");
        assert_eq!(
            e.keys_secondary,
            vec!["副1", "副2"],
            "secondary_keys → 副关键词"
        );
        // 大小写敏感 null → false
        assert!(!e.case_sensitive);
        // use_regex=true 但无 regex 字段 → 视为未启用正则
        assert!(!e.use_regex);
    }

    /// 顶层导出格式:depth/order/case_sensitive/sticky/cooldown/probability 全部解析
    #[test]
    fn parses_full_st_fields() {
        let raw = json!({
            "entries": {
                "0": { "uid": 0, "comment": "深度条目", "keys": ["关键"], "content": "内容", "constant": false, "disable": false, "depth": 3, "order": 50, "caseSensitive": true, "sticky": 2, "cooldown": 5, "probability": 80, "useProbability": true, "position": 1 }
            }
        });
        let entries = collect_entries(&raw);
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.depth, 3);
        assert_eq!(e.order, 50);
        assert!(e.case_sensitive);
        assert_eq!(e.sticky, 2);
        assert_eq!(e.cooldown, 5);
        assert_eq!(e.probability, 80);
        assert!(e.use_probability);
        assert_eq!(e.position, 1);
    }

    /// view → value 往返:写回字段与 ST/角色卡兼容(disable 反转、uid/id、depth/order 等)
    #[test]
    fn view_value_roundtrip() {
        let v = crate::models::types::WorldBookEntryView {
            id: 3,
            comment: "测试".into(),
            keys: vec!["a".into()],
            keys_secondary: vec!["b".into()],
            regex: None,
            use_regex: false,
            constant: true,
            enabled: false,
            content: "内容".into(),
            position: 2,
            depth: 1,
            order: 100,
            case_sensitive: true,
            sticky: 3,
            cooldown: 0,
            probability: 60,
            use_probability: true,
            role: Some("user".into()),
        };
        let val = view_to_value(&v);
        assert_eq!(val["id"], json!(3));
        assert_eq!(val["uid"], json!(3));
        assert_eq!(val["disable"], json!(true), "enabled=false → disable=true");
        assert_eq!(val["enabled"], json!(false));
        assert_eq!(
            val["keysecondary"],
            json!(["b"]),
            "副关键词写回 ST 字段名 keysecondary"
        );
        assert_eq!(val["position"], json!(2));
        assert_eq!(val["depth"], json!(1));
        assert_eq!(val["order"], json!(100));
        assert_eq!(val["case_sensitive"], json!(true));
        assert_eq!(val["sticky"], json!(3));
        assert_eq!(val["probability"], json!(60));
        assert_eq!(val["use_probability"], json!(true));
        assert_eq!(val["role"], json!("user"), "注入角色写回");
        // 往返:写回后重新解析,新字段保留
        let re = collect_entries(&val);
        assert_eq!(re.len(), 1);
        let re = &re[0];
        assert_eq!(re.depth, 1);
        assert_eq!(re.order, 100);
        assert!(re.case_sensitive);
        assert_eq!(re.sticky, 3);
        assert_eq!(re.probability, 60);
        assert!(re.use_probability);
    }

    /// 回归:对象 map 格式合并后必须删除已移除的条目(前端删除条目后 PUT 保存,
    /// 原实现只插入/覆盖,删除的条目留在 data_raw 里,下次 GET 复活)
    #[test]
    fn merge_entries_into_removes_deleted_entries() {
        let mut entries_value = json!({
            "name": "小兔子",
            "0": { "uid": 0, "comment": "保留", "keys": ["a"], "content": "A", "constant": false },
            "1": { "uid": 1, "comment": "删除", "keys": ["b"], "content": "B", "constant": false },
            "2": { "uid": 2, "comment": "替换", "keys": ["c"], "content": "旧C", "constant": false }
        });
        let views = vec![
            crate::models::types::WorldBookEntryView {
                id: 0,
                comment: "保留".into(),
                keys: vec!["a".into()],
                keys_secondary: vec![],
                regex: None,
                use_regex: false,
                constant: false,
                enabled: true,
                content: "A".into(),
                position: 0,
                depth: 4,
                order: 100,
                case_sensitive: false,
                sticky: 0,
                cooldown: 0,
                probability: 100,
                use_probability: false,
                role: None,
            },
            crate::models::types::WorldBookEntryView {
                id: 2,
                comment: "替换".into(),
                keys: vec!["c".into()],
                keys_secondary: vec![],
                regex: None,
                use_regex: false,
                constant: false,
                enabled: true,
                content: "新C".into(),
                position: 0,
                depth: 4,
                order: 100,
                case_sensitive: false,
                sticky: 0,
                cooldown: 0,
                probability: 100,
                use_probability: false,
                role: None,
            },
        ];
        merge_entries_into(&mut entries_value, &views);
        let map = entries_value.as_object().unwrap();
        assert!(map.contains_key("name"), "非条目字段 name 必须保留");
        assert!(map.contains_key("0"), "仍在视图中的条目必须保留");
        assert!(!map.contains_key("1"), "已移除的条目必须删除");
        assert!(map.contains_key("2"), "仍在视图中的条目必须保留");
        assert_eq!(map["2"]["content"], json!("新C"), "条目内容必须更新");
        assert_eq!(map.len(), 3, "仅剩 name + 2 条条目");
    }

    // ============ ST-Prompt-Template 装饰器解析 ============

    /// 开头连续 @@ 行被剥离为 decorators,clean_content 不含装饰器行
    #[test]
    fn parses_leading_decorators() {
        let content = "@@if variables.affection > 50\n@@var emotion = warm\n这是正文第一行。\n这是第二行。";
        let (decorators, clean) = parse_decorators(content);
        assert_eq!(
            decorators,
            vec!["@@if variables.affection > 50", "@@var emotion = warm"]
        );
        assert_eq!(clean, "这是正文第一行。\n这是第二行。");
        // 无参数装饰器
        let (d2, _) = parse_decorators("@@activate\n正文");
        assert_eq!(d2, vec!["@@activate"]);
    }

    /// 装饰器区在首个非装饰器行或空行处停止;后续 @@ 行保留为内容
    #[test]
    fn stops_decorator_section_at_blank_or_plain_line() {
        // 空行打断
        let (d, c) = parse_decorators("@@if x > 1\n\n@@if y > 2\n正文");
        assert_eq!(d, vec!["@@if x > 1"]);
        assert_eq!(c, "\n@@if y > 2\n正文");
        // 非装饰器行打断
        let (d2, c2) = parse_decorators("@@activate\n正文\n@@if x > 1");
        assert_eq!(d2, vec!["@@activate"]);
        assert_eq!(c2, "正文\n@@if x > 1");
    }

    /// @@@ 转义:字面 @@ 保留为内容,不再当作装饰器
    #[test]
    fn triple_at_escapes_decorator() {
        let (d, c) = parse_decorators("@@@activate\n@@if x > 1\n正文");
        assert_eq!(d, vec!["@@if x > 1"]);
        assert_eq!(c, "@@activate\n正文");
        // 无 @@ 前缀的普通内容 → 无装饰器,内容原样
        let (d2, c2) = parse_decorators("纯文本\n@@if x > 1");
        assert!(d2.is_empty());
        assert_eq!(c2, "纯文本\n@@if x > 1");
    }

    /// collect_entries 解析装饰器:存入条目、content 为 clean_content
    #[test]
    fn collect_entries_stores_decorators() {
        let raw = json!({ "entries": [
            { "uid": 0, "comment": "条件人设", "content": "@@if variables.affection > 50\n这是正文", "constant": true, "disable": false }
        ] });
        let entries = collect_entries(&raw);
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.decorators, vec!["@@if variables.affection > 50"]);
        assert_eq!(e.content, "这是正文");
    }

    /// 装饰器为空的内容仍保持可解析(无脏数据)
    #[test]
    fn parse_decorators_empty_and_plain() {
        let (d, c) = parse_decorators("");
        assert!(d.is_empty());
        assert_eq!(c, "");
        let (d2, c2) = parse_decorators("@@if x > 1");
        assert_eq!(d2, vec!["@@if x > 1"]);
        assert_eq!(c2, "");
    }

    // ============ 条目装饰器判定辅助(EntryDecorators) ============

    /// has/arg 按装饰器名判定,参数取第一个空格后内容
    #[test]
    fn entry_decorators_has_and_arg() {
        let e = WorldEntry {
            id: 0,
            comment: "条件".into(),
            keys: vec![],
            keys_secondary: vec![],
            regex: None,
            use_regex: false,
            content: "正文".into(),
            constant: true,
            enabled: true,
            position: 0,
            depth: 4,
            order: 100,
            case_sensitive: false,
            sticky: 0,
            cooldown: 0,
            probability: 100,
            use_probability: false,
            role: None,
            decorators: vec!["@@if variables.affection > 50".into(), "@@var emotion".into()],
        };
        let d = parse_entry_decorators(&e);
        assert!(d.has("@@if"));
        assert!(d.has("@@var"));
        assert!(!d.has("@@unless"));
        assert!(d.has_any(&["@@when", "@@if"]));
        assert!(!d.has_any(&["@@when", "@@unless"]));
        assert_eq!(d.arg("@@if"), Some("variables.affection > 50"));
        assert_eq!(d.arg("@@var"), Some("emotion"));
        assert_eq!(d.arg("@@missing"), None);
        // all / args 一一对应
        assert_eq!(d.all.len(), 2);
        assert_eq!(d.args, vec!["variables.affection > 50".to_string(), "emotion".to_string()]);
    }

    /// 无装饰器条目 → 空判定;无参数装饰器 → 参数为空串
    #[test]
    fn entry_decorators_empty_and_no_arg() {
        let e = WorldEntry {
            id: 0,
            comment: "普通".into(),
            keys: vec![],
            keys_secondary: vec![],
            regex: None,
            use_regex: false,
            content: "正文".into(),
            constant: true,
            enabled: true,
            position: 0,
            depth: 4,
            order: 100,
            case_sensitive: false,
            sticky: 0,
            cooldown: 0,
            probability: 100,
            use_probability: false,
            role: None,
            decorators: vec![],
        };
        let d = parse_entry_decorators(&e);
        assert!(!d.has("@@if"));
        assert!(!d.has_any(&["@@if"]));
        assert!(d.arg("@@if").is_none());
        assert!(d.all.is_empty());
        assert!(d.args.is_empty());
        // 无参数装饰器
        let e2 = WorldEntry {
            decorators: vec!["@@activate".into()],
            ..e
        };
        let d2 = parse_entry_decorators(&e2);
        assert_eq!(d2.arg("@@activate"), Some(""));
    }
}
