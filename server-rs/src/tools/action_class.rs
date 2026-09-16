// 工具调用分类器(三档授权模式的基础):把一次工具调用分解为「操作类型 × 路径区域」,
// 供权限裁决按 严格/宽松/放行 三档矩阵判定,并附带一句面向用户的中文授权理由。
//
// 设计要点:
// - 内置文件工具(read/write/create/replace)的目标路径全经 safe_rel_path 收口在
//   角色文件区内(agent_tools_shared.rs),故其区域恒为 Sandbox;真正的系统路径(盘符/
//   UNC/绝对路径/上级目录逃逸)只可能来自插件与 MCP 工具,它们以 Opaque/SystemPath
//   表示。classify 不重复做路径白名单校验,只做「这次调用属于哪类操作」的语义标注。
// - 非文件目标(如 write target=bubble 写对话气泡)归为 Other,走原有风险裁决,
//   否则严格模式下 Agent 无法正常产出正文。
use serde_json::Value;

use super::command_risk::CommandRisk;

/// 工具来源**已下沉到 L1**（`crate::models::tool_policy::ToolOrigin`，2026-09-14）。
///
/// 理由：`plugins/` 与 `mcp/`（均为 L3）都需要标注工具来源。若该枚举留在 `tools/`（L2），
/// 两个 L3 模块都会构成 `L3→L2` 越代依赖。此处仅重导出，服务 `tools/` 内部既有 `use`。
pub use crate::models::tool_policy::ToolOrigin;

/// 操作类型。派生的 Ord 顺序即危险度序
/// (Other < ReadFile < WriteFile < DeleteFile < Exec),
/// 混合操作调用取最高危项作为整体归类。
/// `Exec` 排在末位:命令执行可造成与「删除文件」不可比的后果(提权/系统级改动),
/// 且其授权判定独立于文件矩阵(见 permissions::decide_with_policy 的 Exec 分支)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ToolOp {
    Other,
    ReadFile,
    WriteFile,
    DeleteFile,
    Exec,
}

/// 路径区域。Sandbox = 内置角色文件区(受 safe_rel_path 约束);
/// SystemPath = 检测到沙箱外系统路径样式;Opaque = 外部工具但未识别出路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathZone {
    Sandbox,
    SystemPath,
    Opaque,
}

/// 一次工具调用的分类结果。reason 为面向用户的简短授权理由(非文件操作为空串)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolAction {
    pub op: ToolOp,
    pub zone: PathZone,
    /// 文件操作的目标(相对路径或检测到的系统路径);用于展示与理由文案
    pub target: Option<String>,
    /// 授权理由(简体中文,简短);非文件操作为空
    pub reason: String,
    /// 命令执行类操作的风险级别(仅 op=Exec 时有值)。
    /// 放在这里而非在上游单独判一次,是为了让权限裁决能直接据其决定
    /// 「是否需要逐条确认」,避免分级口径在两处漂移。
    pub exec_risk: Option<CommandRisk>,
}

impl ToolAction {
    fn other() -> Self {
        Self {
            op: ToolOp::Other,
            zone: PathZone::Opaque,
            target: None,
            reason: String::new(),
            exec_risk: None,
        }
    }

    /// 是否为文件类操作(读/写/删);三档模式的矩阵只作用于这类操作。
    pub fn is_file_op(&self) -> bool {
        matches!(
            self.op,
            ToolOp::ReadFile | ToolOp::WriteFile | ToolOp::DeleteFile
        )
    }

    /// 是否为命令执行类操作。其授权不受三档文件矩阵管辖,由
    /// 「命令级风险分级 + 强制确认」单独把关(见 permissions::decide_with_policy)。
    pub fn is_exec_op(&self) -> bool {
        matches!(self.op, ToolOp::Exec)
    }
}

/// 分类一次工具调用。
/// `name` 为注册名,`args_json` 为模型给出的原始参数 JSON 字符串,`origin` 为工具来源。
pub fn classify(name: &str, args_json: &str, origin: ToolOrigin) -> ToolAction {
    let args: Value = serde_json::from_str(args_json).unwrap_or(Value::Null);
    let mut action = match name {
        "read" => classify_read(&args),
        "write" => classify_write(&args),
        "create" => classify_create(&args),
        "replace" => classify_replace(&args),
        // 命令执行:归 Exec;命令原文进 reason 供确认卡与审计展示。
        // 注意:此处**不**做命令风险分级——分级在 tools::command_risk 里,
        // 由 bash 工具与权限裁决共同消费(避免两处判定口径漂移)。
        "bash" => classify_bash(&args),
        _ => ToolAction::other(),
    };

    // 区域判定:内置文件工具受沙箱约束;外部工具扫描参数中的系统路径样式。
    if action.is_file_op() {
        match origin {
            ToolOrigin::Builtin => action.zone = PathZone::Sandbox,
            ToolOrigin::Plugin | ToolOrigin::Mcp => {
                if let Some(found) = find_system_path(&args) {
                    action.zone = PathZone::SystemPath;
                    action.target = Some(found.path);
                    action.reason = format!(
                        "{}:外部工具参数含系统路径 {}",
                        op_label(action.op),
                        action.target.as_deref().unwrap_or("")
                    );
                } else {
                    action.zone = PathZone::Opaque;
                }
            }
        }
    } else if matches!(origin, ToolOrigin::Plugin | ToolOrigin::Mcp) {
        // 非文件工具但来自外部来源:仅用于风险提示,不影响矩阵(矩阵只作用于文件操作)。
        if find_system_path(&args).is_some() {
            action.zone = PathZone::SystemPath;
        }
    }

    if action.reason.is_empty() {
        action.reason = build_reason(&action);
    }
    action
}

fn op_label(op: ToolOp) -> &'static str {
    match op {
        ToolOp::ReadFile => "读取",
        ToolOp::WriteFile => "写入",
        ToolOp::DeleteFile => "删除",
        ToolOp::Exec => "执行命令",
        ToolOp::Other => "操作",
    }
}

/// bash:命令执行归类。target 取命令首行(截断展示),zone 恒 Opaque
/// (命令可达任意路径,无法用「沙箱/系统路径」二值刻画)。
/// 同时计算命令风险级(exec_risk):权限裁决据其决定是否强制逐条确认。
fn classify_bash(args: &Value) -> ToolAction {
    let cmd = args
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if cmd.is_empty() {
        return ToolAction {
            op: ToolOp::Exec,
            zone: PathZone::Opaque,
            target: None,
            reason: "命令为空".into(),
            // 空命令按最高危处理:宁可多确认,不给「空命令绕过」留缝
            exec_risk: Some(CommandRisk::Admin),
        };
    }
    let risk = super::command_risk::classify_command(cmd, "");
    // 展示用摘要:截断到 200 字符,避免确认卡/审计被超长命令撑爆
    let brief: String = cmd.chars().take(200).collect();
    let brief = if cmd.chars().count() > 200 {
        format!("{brief}…")
    } else {
        brief
    };
    ToolAction {
        op: ToolOp::Exec,
        zone: PathZone::Opaque,
        target: Some(brief.clone()),
        reason: format!("执行命令({}):{brief}", risk.label()),
        exec_risk: Some(risk),
    }
}

/// read: queries[] 中任一 type=file → ReadFile;纯世界书/技能/子任务 → Other
/// (严格模式不应拦截查阅世界书与技能库)。
fn classify_read(args: &Value) -> ToolAction {
    let queries = args.get("queries").and_then(|v| v.as_array());
    let Some(queries) = queries else {
        return ToolAction::other();
    };
    let mut targets: Vec<String> = Vec::new();
    for q in queries {
        if q.get("type").and_then(|v| v.as_str()) == Some("file") {
            if let Some(n) = q.get("name").and_then(|v| v.as_str()) {
                if !n.trim().is_empty() {
                    targets.push(n.to_string());
                }
            }
        }
    }
    if targets.is_empty() {
        return ToolAction::other();
    }
    ToolAction {
        op: ToolOp::ReadFile,
        zone: PathZone::Sandbox,
        target: Some(targets.join(", ")),
        reason: String::new(),
        exec_risk: None,
    }
}

/// write: target=file → WriteFile;target=bubble → Other(写对话正文不纳入文件规则)。
fn classify_write(args: &Value) -> ToolAction {
    if args.get("target").and_then(|v| v.as_str()) != Some("file") {
        return ToolAction::other();
    }
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let target = (!path.trim().is_empty()).then(|| path.clone());
    ToolAction {
        op: ToolOp::WriteFile,
        zone: PathZone::Sandbox,
        target,
        reason: String::new(),
        exec_risk: None,
    }
}

/// create: items[] 创建文件/目录 → WriteFile。
fn classify_create(args: &Value) -> ToolAction {
    let items = args.get("items").and_then(|v| v.as_array());
    let Some(items) = items else {
        return ToolAction::other();
    };
    let paths: Vec<String> = items
        .iter()
        .filter_map(|it| it.get("path").and_then(|v| v.as_str()))
        .filter(|p| !p.trim().is_empty())
        .map(|p| p.to_string())
        .collect();
    if paths.is_empty() {
        return ToolAction::other();
    }
    ToolAction {
        op: ToolOp::WriteFile,
        zone: PathZone::Sandbox,
        target: Some(paths.join(", ")),
        reason: String::new(),
        exec_risk: None,
    }
}

/// replace: operations[] 中 target=file 的操作按 action 归类
/// (delete/remove → DeleteFile,其余 → WriteFile);纯 bubble 操作 → Other。
/// 多个文件操作混合时取最高危项(Delete > Write)。
fn classify_replace(args: &Value) -> ToolAction {
    let ops = args.get("operations").and_then(|v| v.as_array());
    let Some(ops) = ops else {
        return ToolAction::other();
    };
    let mut op = ToolOp::Other;
    let mut targets: Vec<String> = Vec::new();
    for o in ops {
        if o.get("target").and_then(|v| v.as_str()) != Some("file") {
            continue;
        }
        let action = o.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let this_op = if matches!(action, "delete" | "remove") {
            ToolOp::DeleteFile
        } else {
            ToolOp::WriteFile
        };
        if this_op > op {
            op = this_op;
        }
        if let Some(p) = o.get("path").and_then(|v| v.as_str()) {
            if !p.trim().is_empty() {
                targets.push(p.to_string());
            }
        }
    }
    if op == ToolOp::Other {
        return ToolAction::other();
    }
    ToolAction {
        op,
        zone: PathZone::Sandbox,
        target: (!targets.is_empty()).then(|| targets.join(", ")),
        reason: String::new(),
        exec_risk: None,
    }
}

fn build_reason(action: &ToolAction) -> String {
    if !action.is_file_op() {
        return String::new();
    }
    let what = op_label(action.op);
    let where_ = action.target.as_deref().unwrap_or("文件");
    match action.zone {
        PathZone::SystemPath => format!("{what}系统路径 {where_}"),
        _ => format!("{what}角色文件 {where_}"),
    }
}

struct FoundPath {
    path: String,
}

/// 在参数中查找沙箱外系统路径样式:盘符(C:\ / D:/)、UNC(\\server)、Unix 绝对路径(/...)、
/// 含上级目录段(..)。递归遍历所有字符串值(JSON 已解码,故反斜杠为单字符)。
fn find_system_path(value: &Value) -> Option<FoundPath> {
    match value {
        Value::String(s) => looks_like_system_path(s).then(|| FoundPath { path: s.clone() }),
        Value::Array(items) => items.iter().find_map(find_system_path),
        Value::Object(map) => {
            // 优先看路径语义的键,再看其余值;顺序稳定,便于测试断言
            const PATH_KEYS: &[&str] = &["path", "file", "dir", "filename", "target", "name"];
            for (k, v) in map {
                if PATH_KEYS.contains(&k.as_str()) {
                    if let Some(f) = find_system_path(v) {
                        return Some(f);
                    }
                }
            }
            map.values().find_map(find_system_path)
        }
        _ => None,
    }
}

/// 路径样式判定(UTF-8 安全:逐字节判断 ASCII 前缀,不依赖字符索引)
fn looks_like_system_path(s: &str) -> bool {
    if s.trim().is_empty() {
        return false;
    }
    let b = s.as_bytes();
    // 盘符:C:\ 或 D:/ (首字节 ASCII 字母 + 第二字节冒号)
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return true;
    }
    // UNC:\\server\share
    if s.starts_with("\\\\") {
        return true;
    }
    // Unix 绝对路径;注意排除宏/变量等以 / 开头的非路径文本(如 /命令)
    if s.starts_with('/') && !s.starts_with("//") {
        return true;
    }
    // 上级目录逃逸:任一路径段为 ..
    contains_parent_escape(s)
}

fn contains_parent_escape(s: &str) -> bool {
    s.split(['/', '\\']).any(|seg| seg.trim() == "..")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(v: Value) -> String {
        v.to_string()
    }

    #[test]
    fn write_to_bubble_is_not_file_op() {
        let a = classify(
            "write",
            &args(json!({ "target": "bubble", "content": "正文" })),
            ToolOrigin::Builtin,
        );
        assert_eq!(a.op, ToolOp::Other);
        assert!(!a.is_file_op());
        assert!(a.reason.is_empty());
    }

    #[test]
    fn write_to_file_is_sandbox_write() {
        let a = classify(
            "write",
            &args(json!({ "target": "file", "path": "笔记/a.md", "content": "x" })),
            ToolOrigin::Builtin,
        );
        assert_eq!(a.op, ToolOp::WriteFile);
        assert_eq!(a.zone, PathZone::Sandbox);
        assert_eq!(a.target.as_deref(), Some("笔记/a.md"));
        assert_eq!(a.reason, "写入角色文件 笔记/a.md");
    }

    #[test]
    fn read_world_book_is_not_file_op() {
        let a = classify(
            "read",
            &args(json!({ "queries": [{ "type": "world_book", "name": "设定" }] })),
            ToolOrigin::Builtin,
        );
        assert_eq!(a.op, ToolOp::Other);
    }

    #[test]
    fn read_file_is_read_op() {
        let a = classify(
            "read",
            &args(json!({ "queries": [
                { "type": "skill", "name": "写作" },
                { "type": "file", "name": "笔记/a.md" }
            ] })),
            ToolOrigin::Builtin,
        );
        assert_eq!(a.op, ToolOp::ReadFile);
        assert_eq!(a.zone, PathZone::Sandbox);
        assert_eq!(a.target.as_deref(), Some("笔记/a.md"));
    }

    #[test]
    fn create_is_write_op() {
        let a = classify(
            "create",
            &args(json!({ "items": [{ "type": "file", "path": "笔记/b.md" }] })),
            ToolOrigin::Builtin,
        );
        assert_eq!(a.op, ToolOp::WriteFile);
        assert_eq!(a.zone, PathZone::Sandbox);
    }

    #[test]
    fn replace_delete_counts_as_delete() {
        let a = classify(
            "replace",
            &args(json!({ "operations": [
                { "target": "file", "path": "a.md", "action": "delete", "search": "x" }
            ] })),
            ToolOrigin::Builtin,
        );
        assert_eq!(a.op, ToolOp::DeleteFile);
        assert_eq!(a.reason, "删除角色文件 a.md");
    }

    #[test]
    fn replace_remove_counts_as_delete() {
        let a = classify(
            "replace",
            &args(json!({ "operations": [
                { "target": "file", "path": "a.md", "action": "remove" }
            ] })),
            ToolOrigin::Builtin,
        );
        assert_eq!(a.op, ToolOp::DeleteFile);
    }

    #[test]
    fn replace_append_is_write() {
        let a = classify(
            "replace",
            &args(json!({ "operations": [
                { "target": "file", "path": "a.md", "action": "append", "content": "x" }
            ] })),
            ToolOrigin::Builtin,
        );
        assert_eq!(a.op, ToolOp::WriteFile);
    }

    #[test]
    fn replace_mixed_takes_most_dangerous() {
        let a = classify(
            "replace",
            &args(json!({ "operations": [
                { "target": "file", "path": "a.md", "action": "append", "content": "x" },
                { "target": "file", "path": "b.md", "action": "delete" },
                { "target": "bubble", "id": 3, "action": "delete" }
            ] })),
            ToolOrigin::Builtin,
        );
        assert_eq!(a.op, ToolOp::DeleteFile);
    }

    #[test]
    fn replace_bubble_only_is_other() {
        let a = classify(
            "replace",
            &args(json!({ "operations": [
                { "target": "bubble", "id": 3, "action": "delete" }
            ] })),
            ToolOrigin::Builtin,
        );
        assert_eq!(a.op, ToolOp::Other);
    }

    #[test]
    fn plugin_with_drive_letter_is_system_path() {
        let a = classify(
            "my_plugin_tool",
            &args(json!({ "path": "C:\\Windows\\System32\\drivers\\etc\\hosts" })),
            ToolOrigin::Plugin,
        );
        assert_eq!(a.zone, PathZone::SystemPath);
    }

    #[test]
    fn mcp_with_unix_abs_is_system_path() {
        let a = classify(
            "mcp_fs_read",
            &args(json!({ "file": "/etc/passwd" })),
            ToolOrigin::Mcp,
        );
        assert_eq!(a.zone, PathZone::SystemPath);
    }

    #[test]
    fn plugin_without_path_is_opaque() {
        let a = classify(
            "my_plugin_tool",
            &args(json!({ "query": "天气" })),
            ToolOrigin::Plugin,
        );
        assert_eq!(a.op, ToolOp::Other);
        assert_ne!(a.zone, PathZone::SystemPath);
    }

    #[test]
    fn parent_escape_detected() {
        assert!(looks_like_system_path("..\\..\\Windows\\win.ini"));
        assert!(looks_like_system_path("../../etc/passwd"));
        assert!(looks_like_system_path("a/../../b"));
    }

    #[test]
    fn relative_sandbox_path_not_system() {
        assert!(!looks_like_system_path("笔记/a.md"));
        assert!(!looks_like_system_path("a.md"));
        // 中文与普通文本不应误判
        assert!(!looks_like_system_path("这是一段正文"));
    }
}
