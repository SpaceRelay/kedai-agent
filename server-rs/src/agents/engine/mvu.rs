// 酒馆助手变量(MVU)状态管理:补丁应用、首楼状态栏槽位、两步生成与 diff 状态栏
use super::*;

/// 应用 mvu <UpdateVariable> 补丁:应用到变量树 → 持久化 → SSE Vars 事件推送最新树。
/// 无补丁或应用失败返回 None;成功返回应用后的变量树快照(供消息 extra.mvu 落库)。
#[allow(clippy::too_many_arguments)]
pub(super) async fn apply_mvu_patches(
    engine: &AgentEngine,
    session_id: &str,
    assistant_vars: &mut AssistantVars,
    patches: &[PatchOp],
    tx: &mpsc::Sender<SseEvent>,
    abort: &watch::Receiver<bool>,
    flag: &AbortFlag,
) -> Option<Value> {
    if patches.is_empty() {
        return None;
    }
    match assistant_vars.apply_patches(patches) {
        Ok(()) => {
            if let Err(e) = engine.sessions.save_assistant_vars(session_id, assistant_vars) {
                logger::warn(
                    "酒馆助手变量树落库失败",
                    &[
                        ("session_id", Value::String(session_id.to_string())),
                        ("error", Value::String(e)),
                    ],
                );
            }
            let tree = assistant_vars.tree().clone();
            let _ = send_event(
                SseEvent::Vars {
                    stat_data: tree.clone(),
                },
                tx,
                abort,
                flag,
            )
            .await;
            Some(tree)
        }
        Err(e) => {
            logger::warn(
                "酒馆助手变量补丁应用失败",
                &[
                    ("session_id", Value::String(session_id.to_string())),
                    ("error", Value::String(e)),
                ],
            );
            None
        }
    }
}

/// 首楼状态栏槽位:定位会话首条 assistant 消息(开场白),识别其中的
/// <StatusPlaceHolderImpl/> 状态栏占位符与上次写入的状态文本(extra.status_slot.last)。
struct FirstMessageStatusSlot {
    id: i64,
    content: String,
    has_placeholder: bool,
    /// 上一轮写入首楼的状态文本(占位符已被替换后的再次定位锚点)
    last: Option<String>,
}

/// 自动识别并查找首楼里的状态栏:优先带 first_mes 标记的 assistant 消息,否则取第一条
/// assistant 消息。占位符匹配大小写不敏感、容忍空白与自闭合变体(<StatusPlaceHolderImpl/> 等)。
fn find_first_message_status_slot(history: &[MessageRecord]) -> Option<FirstMessageStatusSlot> {
    let first = history
        .iter()
        .find(|m| {
            m.role == "assistant"
                && m.extra.get("first_mes").and_then(|v| v.as_bool()) == Some(true)
        })
        .or_else(|| history.iter().find(|m| m.role == "assistant"))?;
    let has_placeholder = regex::Regex::new(r"(?i)<\s*StatusPlaceHolderImpl\s*\/?\s*>")
        .unwrap()
        .is_match(&first.content);
    let last = first
        .extra
        .get("status_slot")
        .and_then(|s| s.get("last"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Some(FirstMessageStatusSlot {
        id: first.id,
        content: first.content.clone(),
        has_placeholder,
        last,
    })
}

/// 计算首楼状态栏更新后的内容(纯函数,便于单测):
/// 1. 首楼含 <StatusPlaceHolderImpl/> 占位符 → None(占位符由前端脚本渲染为动态 HTML
///    状态栏,服务端替换会毁掉渲染挂载点,导致 HTML 状态栏失效;状态文本仅走气泡);
/// 2. 无占位符但有上次写入的文本(extra.status_slot.last)→ 替换首处上次文本
///    (兼容占位符替换方案的历史会话与纯文本首楼);
/// 3. 均不满足 → None(状态栏仅走气泡 extra.status_bar 路径)。
fn compute_first_message_update(slot: &FirstMessageStatusSlot, text: &str) -> Option<String> {
    if slot.has_placeholder {
        return None;
    }
    let last = slot.last.as_deref()?;
    if last.is_empty() || !slot.content.contains(last) {
        return None;
    }
    Some(slot.content.replacen(last, text, 1))
}

/// 本轮状态栏文本返回决策(纯函数):只要存在首楼槽位(含占位符 / 无锚点首楼 /
/// 有锚点可替换),状态文本就作为本轮气泡显示(extra.status_bar);仅当找不到首楼
/// 槽位时返回 None。防止「无锚点首轮」把状态文本吞掉(碧蓝卡开场无占位符场景)。
fn round_status_bar_text(slot: Option<&FirstMessageStatusSlot>, text: &str) -> Option<String> {
    slot?;
    Some(text.to_string())
}

/// 更新首楼状态栏:返回状态栏文本作为本轮气泡显示(extra.status_bar)。
/// 首楼含 <StatusPlaceHolderImpl/> 占位符时,**不修改首楼**——该占位符由前端脚本
/// 渲染为动态 HTML 状态栏(角色卡「状态栏」脚本匹配占位符构建 scoped 容器),
/// 服务端替换会毁掉挂载点导致 HTML 渲染失效;文本仅作为本轮气泡状态栏。
/// 首楼无占位符(旧方案历史会话 / 纯文本首楼)时按 status_slot.last 锚点替换首处
/// 旧状态文本并落库(内容 + extra.status_slot.last 锚点);无锚点(首轮)则仅作为
/// 本轮气泡状态栏,不修改首楼。
/// 首楼不存在/无槽位时返回 None(仅走气泡 extra.status_bar 路径)。
fn apply_status_bar_update(
    engine: &AgentEngine,
    session_id: &str,
    history: &[MessageRecord],
    text: &str,
) -> Option<String> {
    let slot = find_first_message_status_slot(history);
    // 占位符保留:状态文本仅走本轮气泡,不写入首楼
    let has_placeholder = slot.as_ref().map(|s| s.has_placeholder).unwrap_or(false);
    if !has_placeholder {
        // 首楼无占位符:有 last 锚点则就地替换首处旧状态文本并落库;无锚点(首轮)不修改首楼
        if let Some(s) = slot.as_ref() {
            if let Some(new_content) = compute_first_message_update(s, text) {
                if new_content != s.content {
                    engine
                        .sessions
                        .update_message_content(session_id, s.id, &new_content)?;
                    engine.sessions.merge_message_extra(
                        session_id,
                        s.id,
                        json!({ "status_slot": { "last": text } }),
                    )?;
                }
            }
        }
    }
    // 无论首楼是否可更新,状态文本始终作为本轮气泡显示(extra.status_bar)
    round_status_bar_text(slot.as_ref(), text)
}

/// 第二次输出的工具定义:更新 mvu 变量树(update_variables)+ 更新首楼/气泡状态栏
/// (update_status_bar)。OpenAI 标准格式由连接器统一包装({"type":"function",...})。
fn mvu_status_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "update_variables".into(),
            description: "更新会话的 mvu 变量树:传入 JSON Patch 数组,支持 replace/delta/insert/remove/move 操作;路径对应当前状态树的键(如 /心之所向/好感度,stat_data 前缀可省略)。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "patches": {
                        "type": "array",
                        "description": "JSON Patch 操作数组",
                        "items": {
                            "type": "object",
                            "properties": {
                                "op": { "type": "string", "enum": ["replace", "delta", "insert", "remove", "move"] },
                                "path": { "type": "string", "description": "变量路径,如 /心之所向/好感度" },
                                "value": { "description": "新值(delta 时为数值增量)" },
                                "from": { "type": "string", "description": "move 操作的来源路径" }
                            },
                            "required": ["op", "path"]
                        }
                    }
                },
                "required": ["patches"]
            }),
        },
        ToolDefinition {
            name: "update_status_bar".into(),
            description: "更新本轮回复的状态栏文本(仅显示在本轮气泡;若开场白含 <StatusPlaceHolderImpl/> 占位符,该占位符由前端脚本渲染为动态状态栏,本工具不修改首楼)。文本用中文简要描述关键状态变化,条目间用「·」分隔,如「好感度 0→3 · 情绪:恐慌→稍安」;状态无变化时不要调用本工具。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "状态栏文本" }
                },
                "required": ["text"]
            }),
        },
    ]
}

/// 第二次输出(静默)生成结果:正文文本 + 工具调用(不向客户端流式转发协议内容)
struct StatusCallOutput {
    text: String,
    tool_calls: Vec<ToolCallArgs>,
}

/// 调连接器生成状态更新(拼接 Token 分块 + 收集工具调用;协议内容不流式转发给客户端)
async fn generate_status_call(
    connector: &Connector,
    messages: &[LlmMessage],
    params: &GenerationParams,
    abort: &watch::Receiver<bool>,
) -> Result<StatusCallOutput, String> {
    let chunks = connector
        .generate(messages, params.clone(), abort.clone())
        .await
        .map_err(|e| format!("状态更新生成失败:{e}"))?;
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for c in chunks {
        match c {
            LlmStreamChunk::Token(t) => text.push_str(&t),
            LlmStreamChunk::ToolCall(call) if !call.id.is_empty() && !call.name.is_empty() => {
                tool_calls.push(call);
            }
            _ => {}
        }
    }
    Ok(StatusCallOutput { text, tool_calls })
}

/// 两步生成:「变量更新 + 状态栏」专用调用(角色卡有变量树时,正文之外总是追加一次)。
/// 本次为 function calling 调用:模型通过工具在正文后更新 mvu 变量与首楼状态栏——
///   - update_variables:JSON Patch 应用到变量树并推送 Vars 事件(复用 apply_mvu_patches);
///   - update_status_bar:替换首楼(开场白)里的 <StatusPlaceHolderImpl/> 状态栏并落库,
///     同时作为本轮气泡 extra.status_bar 显示(前端插入对话气泡)。
///
/// 输入仅含精简当前状态、输出协议、首楼状态栏说明与正文(不含用户输入原文,避免测试
/// 钩子误触发且正文本身体现状态变化;独立小请求 + 低温度提高结构化遵循度)。
/// P5:传入契约时,当前状态按 dueFields + observe 裁剪(只暴露到期字段与依赖,
/// 替代整树 to_json —— G4),且输出补丁经契约校验门控(unknown_field/not_owner 拒绝)。
/// 模型未调用工具时回退旧文本协议(<UpdateVariable> + <StatusBar>,兼容无 function calling
/// 的模型);仍未产出状态栏时用 diff_status_bar 对比「本轮初始树 initial_vars_tree」与
/// 「当前树」兜底生成——diff 基准必须用本轮初始树,custom 模式每步已应用补丁,
/// 若用 generate 调用前的树会恒空。返回 (变量树快照, 状态栏文本, 生效变更 entries,
/// pending ops):后两项为 P5 运行态接线产物(无契约时 entries 恒空),供收尾统一
/// commit 到 kaleido_state/kaleido_changelog;调用失败返回 Err(降级为仅正文,不影响主流程)。
#[allow(clippy::too_many_arguments)]
pub(super) async fn generate_mvu_status(
    engine: &AgentEngine,
    session_id: &str,
    history: &[MessageRecord],
    content: &str,
    initial_tree: &Value,
    assistant_vars: &mut AssistantVars,
    params: &GenerationParams,
    tx: &mpsc::Sender<SseEvent>,
    abort: &watch::Receiver<bool>,
    flag: &AbortFlag,
    contract: Option<&crate::contracts::Contract>,
    turn_id: u64,
) -> Result<
    (
        Option<Value>,
        Option<String>,
        Vec<crate::contracts::ChangelogEntry>,
        Vec<crate::contracts::PatchOp>,
    ),
    String,
> {
    if assistant_vars.is_empty() {
        return Ok((None, None, Vec::new(), Vec::new()));
    }
    // P5:契约驱动裁剪的当前状态(无契约走整树兼容层)
    let (state_text, _due) =
        crate::contracts::build_state_prompt(contract, assistant_vars.tree(), turn_id);
    // 自动识别首楼里的状态栏占位符:含占位符时提示模型占位符由前端渲染,状态文本仅走本轮气泡
    let slot = find_first_message_status_slot(history);
    let slot_hint = match &slot {
        Some(s) if s.has_placeholder => "\n\n[首楼状态栏]\n开场白包含状态栏占位符 <StatusPlaceHolderImpl/>,由前端脚本渲染为动态状态栏,\
             无需也不可写入首楼。状态变化时调用 update_status_bar 工具输出本轮状态栏文本(仅显示在本轮回复气泡)。"
            .to_string(),
        Some(_) => "\n\n[首楼状态栏]\n开场白无 <StatusPlaceHolderImpl/> 占位符:状态栏仅显示在本轮回复气泡。".to_string(),
        None => "\n\n[首楼状态栏]\n会话无开场白:状态栏仅显示在本轮回复气泡。".to_string(),
    };
    // 专用 prompt:仅含当前状态、更新方式(工具)、首楼状态栏说明与正文;低温度提高遵循度
    let system = format!(
        "你是「变量状态更新分析器」。根据角色本轮回复,判断变量树是否还有未反映的状态变化,并更新状态栏。\n\n[当前状态](可能已包含本轮部分更新)\n{}\n\n[更新方式]\n\
         通过调用工具完成更新,不要输出任何正文或标签:\n\
         - update_variables:传入 JSON Patch 数组(支持 replace / delta / insert / remove / move,路径对应当前状态的键)\n\
         - update_status_bar:传入状态栏文本(中文简要描述关键状态变化,条目间用「·」分隔,如「好感度 0→3 · 情绪:恐慌→稍安」)\n\
         状态无变化且状态栏无需更新时,不调用任何工具。{}",
        state_text,
        slot_hint
    );
    let user = format!("角色的回复:\n{content}\n\n请分析并输出。");
    let messages = vec![
        LlmMessage::plain("system", &system),
        LlmMessage::plain("user", &user),
    ];
    let mut p = params.clone();
    // G3:变量两步生成独立温度档。设置 mvu_temperature 非空时覆盖内置 0.3,
    // 允许与正文 default_temperature 解耦(变量调用单独降温度提高结构化遵循度)。
    let mvu_temperature = engine
        .settings
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .mvu_temperature
        .unwrap_or(0.3);
    p.temperature = mvu_temperature;
    p.tools = mvu_status_tools();
    let connector = engine.connector.read().await;
    let mut out = generate_status_call(&connector, &messages, &p, abort).await?;
    // deepseek 偶发空输出:提高温度重试一次(重试兜底固定 0.7,与独立温度档语义分离)
    if out.text.trim().is_empty() && out.tool_calls.is_empty() {
        p.temperature = 0.7;
        out = generate_status_call(&connector, &messages, &p, abort).await?;
    }
    drop(connector);
    let mut vars_snapshot: Option<Value> = None;
    let mut status_bar: Option<String> = None;
    // P5 运行态:生效变更 entries(source=agent)与 pending ops,收尾统一 commit
    let mut changelog_entries: Vec<crate::contracts::ChangelogEntry> = Vec::new();
    let mut pending_ops: Vec<crate::contracts::PatchOp> = Vec::new();
    if !out.tool_calls.is_empty() {
        // 工具路径:逐个执行(单轮工具调用即为最终结果,不追加第三轮生成)
        for call in &out.tool_calls {
            let args: Value = serde_json::from_str(&call.arguments)
                .unwrap_or_else(|_| Value::String(call.arguments.clone()));
            match call.name.as_str() {
                "update_variables" => {
                    let patches = args
                        .get("patches")
                        .and_then(parse_patch_array)
                        .unwrap_or_default();
                    // P5:契约门控(无契约原样放行)——未声明字段/越权 op 拒绝,低置信入 pending
                    let gated = crate::contracts::gate_assistant_patches_detailed(
                        contract, &patches, "agent",
                    );
                    if !gated.rejected.is_empty() || !gated.pending.is_empty() {
                        logger::warn(
                            "契约门控过滤了部分变量补丁",
                            &[
                                ("rejected", json!(gated.rejected.len())),
                                ("pending", json!(gated.pending.len())),
                            ],
                        );
                    }
                    if contract.is_some() && !gated.applied.is_empty() {
                        let tree_before = assistant_vars.tree().clone();
                        if let Some(tree) = apply_mvu_patches(
                            engine,
                            session_id,
                            assistant_vars,
                            &gated.applied,
                            tx,
                            abort,
                            flag,
                        )
                        .await
                        {
                            vars_snapshot = Some(tree.clone());
                            changelog_entries.extend(crate::contracts::entries_from_applied(
                                &tree_before,
                                assistant_vars.tree(),
                                turn_id,
                                &gated.applied_ops,
                                crate::contracts::ChangelogSource::Agent,
                            ));
                        }
                    } else if contract.is_none() {
                        if let Some(tree) = apply_mvu_patches(
                            engine,
                            session_id,
                            assistant_vars,
                            &gated.applied,
                            tx,
                            abort,
                            flag,
                        )
                        .await
                        {
                            vars_snapshot = Some(tree);
                        }
                    }
                    pending_ops.extend(gated.pending);
                }
                "update_status_bar" => {
                    if let Some(text) = args
                        .get("text")
                        .and_then(|t| t.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                    {
                        status_bar = apply_status_bar_update(engine, session_id, history, &text);
                    }
                }
                other => logger::warn(
                    "未知的 mvu 状态工具调用",
                    &[("name", Value::String(other.to_string()))],
                ),
            }
        }
    } else {
        // 回退:旧文本协议(<UpdateVariable> + <StatusBar>),兼容无 function calling 的模型
        let (_clean, raw_patches) = parse_update_variable(&out.text);
        // P5:契约门控(无契约原样放行)
        let gated = crate::contracts::gate_assistant_patches_detailed(
            contract, &raw_patches, "agent",
        );
        if !gated.rejected.is_empty() || !gated.pending.is_empty() {
            logger::warn(
                "契约门控过滤了部分文本协议补丁",
                &[
                    ("rejected", json!(gated.rejected.len())),
                    ("pending", json!(gated.pending.len())),
                ],
            );
        }
        if contract.is_some() && !gated.applied.is_empty() {
            let tree_before = assistant_vars.tree().clone();
            vars_snapshot = apply_mvu_patches(
                engine,
                session_id,
                assistant_vars,
                &gated.applied,
                tx,
                abort,
                flag,
            )
            .await;
            if vars_snapshot.is_some() {
                changelog_entries.extend(crate::contracts::entries_from_applied(
                    &tree_before,
                    assistant_vars.tree(),
                    turn_id,
                    &gated.applied_ops,
                    crate::contracts::ChangelogSource::Agent,
                ));
            }
        } else if contract.is_none() {
            vars_snapshot = apply_mvu_patches(
                engine,
                session_id,
                assistant_vars,
                &gated.applied,
                tx,
                abort,
                flag,
            )
            .await;
        }
        pending_ops.extend(gated.pending);
        status_bar = extract_status_bar(&out.text);
    }
    // 模型未产出状态栏时,diff 兜底(基准必须用本轮初始树,终点取 assistant_vars 当前值)
    if status_bar.is_none() {
        status_bar = diff_status_bar(initial_tree, assistant_vars.tree());
    }
    Ok((vars_snapshot, status_bar, changelog_entries, pending_ops))
}

/// 提取 <StatusBar>...</StatusBar> 中的状态栏文本(容错:无匹配返回 None)
/// 剥离正文中的 <StatusBar>…</StatusBar> 协议标签(状态栏文本由两步生成单独落库)。
pub(super) fn strip_status_bar_tag(text: &str) -> String {
    let re = regex::Regex::new(r"(?is)<StatusBar\b[^>]*>[\s\S]*?</StatusBar\s*>").unwrap();
    re.replace_all(text, "").to_string()
}

fn extract_status_bar(text: &str) -> Option<String> {
    let start = text.find("<StatusBar>")?;
    let rest = &text[start + "<StatusBar>".len()..];
    let end = rest.find("</StatusBar>")?;
    let bar = rest[..end].trim();
    if bar.is_empty() {
        None
    } else {
        Some(bar.to_string())
    }
}

/// 变量树前后 diff → 状态栏文本(「叶子: 旧→新 · …」;无变化返回 None)
fn diff_status_bar(old: &Value, new: &Value) -> Option<String> {
    let mut parts = Vec::new();
    collect_diff(old, new, "", &mut parts);
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

fn collect_diff(old: &Value, new: &Value, prefix: &str, out: &mut Vec<String>) {
    if old == new {
        return;
    }
    match (old, new) {
        (Value::Object(o), Value::Object(n)) => {
            for (k, nv) in n {
                let p = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                match o.get(k) {
                    Some(ov) => collect_diff(ov, nv, &p, out),
                    None => out.push(format!("{p}: 新增 {}", fmt_status_val(nv))),
                }
            }
            for k in o.keys() {
                if !n.contains_key(k) {
                    let p = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    out.push(format!("{p}: 移除"));
                }
            }
        }
        _ => {
            let leaf = prefix.rsplit('.').next().unwrap_or(prefix);
            out.push(format!(
                "{leaf}: {}→{}",
                fmt_status_val(old),
                fmt_status_val(new)
            ));
        }
    }
}

/// 状态栏值格式化(字符串截断;数字/布尔原样)
fn fmt_status_val(v: &Value) -> String {
    match v {
        Value::String(s) => s.chars().take(16).collect(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: &str, content: &str) -> MessageRecord {
        message_with(role, content, json!({}))
    }
    /// 带自定义 extra 的消息(供首楼状态栏槽位测试)
    fn message_with(role: &str, content: &str, extra: Value) -> MessageRecord {
        MessageRecord {
            id: 0,
            session_id: "s".into(),
            role: role.into(),
            content: content.into(),
            extra,
            created_at: "".into(),
        }
    }
    /// extract_status_bar:提取 <StatusBar> 文本;无匹配/空返回 None
    #[test]
    fn extract_status_bar_basic() {
        assert_eq!(
            extract_status_bar("正文\n<StatusBar>好感度 0→3 · 情绪:恐慌→稍安</StatusBar>"),
            Some("好感度 0→3 · 情绪:恐慌→稍安".to_string())
        );
        assert_eq!(extract_status_bar("无状态栏"), None);
        assert_eq!(extract_status_bar("<StatusBar>  </StatusBar>"), None);
        assert_eq!(
            extract_status_bar("<StatusBar>状态无变化</StatusBar>"),
            Some("状态无变化".to_string())
        );
    }
    /// diff_status_bar:变量树前后 diff 生成状态栏;无变化返回 None
    #[test]
    fn diff_status_bar_basic() {
        let old = json!({ "心之所向": { "好感度": 0 }, "芽衣": { "心声": "害怕" } });
        let new = json!({ "心之所向": { "好感度": 2 }, "芽衣": { "心声": "安心" } });
        let bar = diff_status_bar(&old, &new).expect("有变化应返回状态栏");
        assert!(bar.contains("好感度: 0→2"), "应含数值变化:{bar}");
        assert!(bar.contains("心声: 害怕→安心"), "应含字符串变化:{bar}");
        assert_eq!(diff_status_bar(&old, &old), None, "无变化应 None");
        // 新增叶子
        let new2 = json!({ "心之所向": { "好感度": 0 }, "芽衣": { "心声": "害怕" }, "身体资讯": { "情绪": "紧张" } });
        let bar2 = diff_status_bar(&old, &new2).expect("新增应返回状态栏");
        assert!(bar2.contains("新增"), "应含新增:{bar2}");
    }
    // ===== 首楼状态栏槽位(两步生成第二轮)=====

    /// find_first_message_status_slot:first_mes 标记优先;占位符大小写/空白容忍;last 锚点提取
    #[test]
    fn first_message_slot_detection() {
        // 无 assistant 消息 → None
        assert!(find_first_message_status_slot(&[message("user", "你好")]).is_none());
        // first_mes 标记优先于更早的普通 assistant 消息
        let h = vec![
            message_with("assistant", "早期楼层", json!({})),
            message_with(
                "assistant",
                "正文 <StatusPlaceHolderImpl/>",
                json!({ "first_mes": true }),
            ),
        ];
        let slot = find_first_message_status_slot(&h).expect("应找到首楼");
        assert!(slot.has_placeholder);
        assert_eq!(slot.last, None);
        // 占位符大小写 + 空白容忍
        let h2 = vec![message_with(
            "assistant",
            "x < statusplaceholderimpl > y",
            json!({}),
        )];
        assert!(
            find_first_message_status_slot(&h2)
                .expect("应找到")
                .has_placeholder
        );
        // last 锚点提取(占位符已被首轮替换后的再次定位)
        let h3 = vec![message_with(
            "assistant",
            "旧状态文本",
            json!({ "status_slot": { "last": "旧状态文本" } }),
        )];
        let s3 = find_first_message_status_slot(&h3).expect("应找到");
        assert!(!s3.has_placeholder);
        assert_eq!(s3.last.as_deref(), Some("旧状态文本"));
    }

    /// compute_first_message_update:含占位符 → None(占位符保留给前端脚本渲染);
    /// 无占位符 + 有 last → 首处替换;无槽位 → None
    #[test]
    fn compute_first_message_update_variants() {        // 含占位符 → None:占位符由前端脚本渲染动态状态栏,服务端不替换
        let slot = FirstMessageStatusSlot {
            id: 1,
            content: "你好 <StatusPlaceHolderImpl/> 再见".into(),
            has_placeholder: true,
            last: None,
        };
        assert!(
            compute_first_message_update(&slot, "新状态").is_none(),
            "含占位符不应替换"
        );
        let slot2 = FirstMessageStatusSlot {
            id: 1,
            content: "a<StatusPlaceHolderImpl/>b<StatusPlaceHolderImpl/>c".into(),
            has_placeholder: true,
            last: Some("旧".into()),
        };
        assert!(
            compute_first_message_update(&slot2, "x").is_none(),
            "含占位符不应替换(即使有 last)"
        );
        // 无占位符 + 有 last → 替换首处 last(旧方案历史会话/纯文本首楼的续轮定位)
        let slot3 = FirstMessageStatusSlot {
            id: 1,
            content: "旧状态文本 你好".into(),
            has_placeholder: false,
            last: Some("旧状态文本".into()),
        };
        assert_eq!(
            compute_first_message_update(&slot3, "新状态").as_deref(),
            Some("新状态 你好")
        );
        // 无占位符 + 无 last → None
        let slot4 = FirstMessageStatusSlot {
            id: 1,
            content: "你好".into(),
            has_placeholder: false,
            last: None,
        };
        assert!(compute_first_message_update(&slot4, "x").is_none());
        // last 不在内容中 → None
        let slot5 = FirstMessageStatusSlot {
            id: 1,
            content: "你好".into(),
            has_placeholder: false,
            last: Some("旧状态文本".into()),
        };
        assert!(compute_first_message_update(&slot5, "x").is_none());
    }

    /// round_status_bar_text:存在首楼槽位(含无锚点首轮)即返回状态文本;
    /// 无首楼槽位返回 None(防「无锚点首轮把状态文本吞掉」——碧蓝卡开场无占位符场景)
    #[test]
    fn round_status_bar_text_without_anchor() {
        // 首楼含占位符 → 返回状态文本(占位符由前端渲染,文本走气泡)
        let slot_ph = FirstMessageStatusSlot {
            id: 1,
            content: "a <StatusPlaceHolderImpl/> b".into(),
            has_placeholder: true,
            last: None,
        };
        assert_eq!(
            round_status_bar_text(Some(&slot_ph), "状态").as_deref(),
            Some("状态")
        );
        // 首楼无占位符且无 last 锚点(首轮)→ 仍返回状态文本(不落库,仅本轮气泡)
        let slot_no_anchor = FirstMessageStatusSlot {
            id: 1,
            content: "开场".into(),
            has_placeholder: false,
            last: None,
        };
        assert_eq!(
            round_status_bar_text(Some(&slot_no_anchor), "好感度 0→3").as_deref(),
            Some("好感度 0→3")
        );
        // 无首楼槽位 → None
        assert_eq!(round_status_bar_text(None, "状态"), None);
    }

    /// mvu_status_tools:两个工具、必填参数形状
    #[test]
    fn mvu_status_tools_shape() {
        let tools = mvu_status_tools();
        assert_eq!(tools.len(), 2);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"update_variables"));
        assert!(names.contains(&"update_status_bar"));
        let uv = tools.iter().find(|t| t.name == "update_variables").unwrap();
        let required = uv.parameters["required"].as_array().expect("应有 required");
        assert!(
            required.iter().any(|x| x == "patches"),
            "update_variables 应必填 patches"
        );
        assert!(uv.description.contains("replace"), "描述应说明支持的操作");
        let usb = tools
            .iter()
            .find(|t| t.name == "update_status_bar")
            .unwrap();
        let required2 = usb.parameters["required"]
            .as_array()
            .expect("应有 required");
        assert!(
            required2.iter().any(|x| x == "text"),
            "update_status_bar 应必填 text"
        );
    }
    /// strip_status_bar_tag:剥离 <StatusBar> 协议标签,不进入正文存储/渲染
    #[test]
    fn strip_status_bar_tag_basic() {
        assert_eq!(
            strip_status_bar_tag("正文<StatusBar>状态</StatusBar>完"),
            "正文完"
        );
        assert_eq!(
            strip_status_bar_tag("a<StatusBar>1</StatusBar>b<StatusBar>2</StatusBar>c"),
            "abc"
        );
        // 大小写容忍
        assert_eq!(strip_status_bar_tag("x<statusbar>s</statusbar>y"), "xy");
        // 无标签原样返回
        assert_eq!(strip_status_bar_tag("普通正文"), "普通正文");
    }
}
