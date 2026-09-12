// 脚本树装载(阶段三 3b-1):从 ScriptTree JSON 提取「启用」的脚本执行体。
// 数据源由调用方经 UserScriptService.get_tree(global/character)提供;
// 本模块只做纯函数提取(顶层 script + 文件夹内 script,按 enabled 过滤),便于引擎接线。
use serde_json::Value;

/// 已装载的脚本执行体(供 3b-3 引擎触发)
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedScript {
    pub id: String,
    pub name: String,
    pub content: String,
    /// script 作用域变量(data)
    pub data: Value,
}

/// 按 ScriptTree 提取启用脚本:顶层 script 直接计入;folder 取其 scripts 内启用 script。
pub fn collect_enabled_scripts(trees: &Value) -> Vec<LoadedScript> {
    let Some(arr) = trees.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for node in arr {
        let Some(obj) = node.as_object() else {
            continue;
        };
        match obj.get("type").and_then(|t| t.as_str()) {
            Some("script") => {
                if let Some(s) = script_from_obj(obj) {
                    out.push(s);
                }
            }
            Some("folder") => {
                if let Some(scripts) = obj.get("scripts").and_then(|s| s.as_array()) {
                    for sub in scripts {
                        if let Some(sub_obj) = sub.as_object() {
                            if sub_obj.get("type").and_then(|t| t.as_str()) == Some("script") {
                                if let Some(s) = script_from_obj(sub_obj) {
                                    out.push(s);
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn script_from_obj(obj: &serde_json::Map<String, Value>) -> Option<LoadedScript> {
    // enabled 缺省视为启用(与酒馆助手行为一致)
    let enabled = obj.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true);
    if !enabled {
        return None;
    }
    let content = obj.get("content").and_then(|c| c.as_str()).unwrap_or("");
    if content.trim().is_empty() {
        return None;
    }
    Some(LoadedScript {
        id: obj
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or("")
            .to_string(),
        name: obj
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string(),
        content: content.to_string(),
        data: obj
            .get("data")
            .cloned()
            .unwrap_or(Value::Object(Default::default())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn collects_enabled_top_level_and_folder() {
        let trees = json!([
            { "type": "script", "enabled": true, "id": "a", "name": "A", "content": "1" },
            { "type": "script", "enabled": false, "id": "b", "name": "B", "content": "2" },
            { "type": "folder", "enabled": true, "id": "f", "scripts": [
                { "type": "script", "enabled": true, "id": "c", "name": "C", "content": "3", "data": { "k": 1 } }
            ]}
        ]);
        let got = collect_enabled_scripts(&trees);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id, "a");
        assert_eq!(got[1].id, "c");
        assert_eq!(got[1].data["k"], json!(1));
    }

    #[test]
    fn skips_empty_content_and_missing_enabled_defaults_true() {
        let trees = json!([
            { "type": "script", "id": "a", "content": "" },
            { "type": "script", "id": "b", "content": "hi" }
        ]);
        let got = collect_enabled_scripts(&trees);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "b");
    }

    #[test]
    fn non_array_returns_empty() {
        assert!(collect_enabled_scripts(&json!({ "x": 1 })).is_empty());
    }
}
