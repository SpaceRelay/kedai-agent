// 注入标签解析(ST-Prompt-Template 兼容, L1 老层 · parsing/assistant):
// 世界书条目 comment(标题)中的 [GENERATE:...]/[RENDER:...] 注入标签解析,
// 以及带注入标签条目的收集(engine 层注入用)。
use crate::parsing::world_book::WorldEntry;

use super::render::render_assistant_content_with;
use super::vars::AssistantVars;
use super::RenderCtx;

// ===================== 注入标签解析(ST-Prompt-Template) =====================

/// 世界书条目 comment(标题)中的内容注入标签(ST-Prompt-Template 兼容)。
/// 从 comment 解析(大小写不敏感、容忍注释前后空白):
///   [GENERATE:BEFORE] / [GENERATE:AFTER] / [GENERATE:{idx}:BEFORE|AFTER]
///   [GENERATE:REGEX:pattern] / [RENDER:BEFORE] / [RENDER:AFTER]
///   [InitialVariables](与 [InitVar] 等价,初始变量用)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InjectTag {
    /// [GENERATE:BEFORE]:模型生成前注入
    GenerateBefore,
    /// [GENERATE:AFTER]:模型生成后注入
    GenerateAfter,
    /// [GENERATE:{idx}:BEFORE|AFTER]:按索引位次注入(idx 从 0 开始)
    GenerateIndex { idx: usize, before: bool },
    /// [GENERATE:REGEX:pattern]:按正则匹配注入(pattern 原样保留,默认 before)
    GenerateRegex { pattern: String, before: bool },
    /// [RENDER:BEFORE]:渲染阶段注入(内容含 EJS,需渲染后再注入)
    RenderBefore,
    /// [RENDER:AFTER]:渲染阶段注入
    RenderAfter,
    /// [InitialVariables] / [InitVar]:初始变量(不进注入收集)
    InitialVariables,
    /// 无匹配
    None,
}

/// 从世界书条目 comment(标题)解析内容注入标签;无匹配返回 InjectTag::None。
/// 解析大小写不敏感、容忍注释前后空白;REGEX 的 pattern 原样保留(不转大小写)。
pub fn parse_inject_tag(comment: &str) -> InjectTag {
    // 初始变量标签(独立匹配,防止被 GENERATE/RENDER 分支吞掉)
    let upper = comment.to_uppercase();
    if upper.contains("[INITIALVARIABLES]") || upper.contains("[INITVAR]") {
        return InjectTag::InitialVariables;
    }
    let Some(caps) = inject_tag_regex().captures(comment) else {
        return InjectTag::None;
    };
    let kind = caps.get(1).map(|m| m.as_str()).unwrap_or("");
    let body = caps.get(2).map(|m| m.as_str()).unwrap_or("").trim();
    match kind.to_uppercase().as_str() {
        "GENERATE" => parse_generate_body(body),
        "RENDER" => match body.to_uppercase().as_str() {
            "BEFORE" => InjectTag::RenderBefore,
            "AFTER" => InjectTag::RenderAfter,
            _ => InjectTag::None,
        },
        _ => InjectTag::None,
    }
}

/// 解析 [GENERATE:...] 的标签体:BEFORE / AFTER / {idx}:BEFORE|AFTER / REGEX:pattern
fn parse_generate_body(body: &str) -> InjectTag {
    let up = body.to_uppercase();
    match up.as_str() {
        "BEFORE" => return InjectTag::GenerateBefore,
        "AFTER" => return InjectTag::GenerateAfter,
        _ => {}
    }
    // REGEX 变体:REGEX:pattern(pattern 原样保留,默认 before)
    if up.starts_with("REGEX:") {
        return InjectTag::GenerateRegex {
            pattern: body[6..].trim().to_string(),
            before: true,
        };
    }
    // 索引变体:{idx}:BEFORE|AFTER
    if let Some(colon) = body.find(':') {
        let idx = body[..colon].trim().parse::<usize>();
        let side = body[colon + 1..].trim().to_uppercase();
        if let Ok(idx) = idx {
            match side.as_str() {
                "BEFORE" => return InjectTag::GenerateIndex { idx, before: true },
                "AFTER" => return InjectTag::GenerateIndex { idx, before: false },
                _ => {}
            }
        }
    }
    InjectTag::None
}

/// 注入标签正则(常量,一次性编译):匹配 [GENERATE:...] / [RENDER:...](大小写不敏感)
fn inject_tag_regex() -> &'static regex::Regex {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // 常量模式编译(一次性);模式为写死字面量,编译必然成功
        regex::Regex::new(r"(?i)\[(generate|render)\s*:\s*([^\]]*)\]").unwrap()
    })
}

// ===================== 注入标签条目收集(ST-Prompt-Template) =====================

/// 带注入标签(GENERATE/RENDER)的世界书条目收集结果(engine 层注入用)
#[derive(Debug, Clone)]
pub struct GenerateEntry {
    /// 条目注入标签(决定注入时机/位次)
    pub tag: InjectTag,
    /// 原始条目(decorators/keys 等仍可查)
    pub entry: WorldEntry,
    /// 渲染后的条目内容(render_assistant_content 结果)
    pub rendered: String,
}

/// 收集带注入标签的世界书条目:
///   - comment 含注入标签(parse_inject_tag != None)且 enabled 的条目
///     → 先 render_assistant_content 渲染内容,作为 GenerateEntry 收集
///   - [InitialVariables] / [InitVar] 标签条目不进收集(保持原 [InitVar] 语义)
///   - 其余条目(普通世界书注入 / 未启用 / 无标签)原样返回,走原注入链路
/// 排序:GenerateIndex 按 idx 升序(engine 层再按 before/after 分组);其余保持 entries 顺序。
pub fn collect_generate_entries(
    entries: &[WorldEntry],
    vars: &mut AssistantVars,
) -> (Vec<GenerateEntry>, Vec<WorldEntry>) {
    let mut ctx = RenderCtx::new(vars);
    collect_generate_entries_with(&entries, &mut ctx)
}

/// 带渲染上下文的收集变体(engine 层注入链路用):
/// 带标签条目经 ctx 渲染(内建 getwi/getqr/getChatMessage/injectPrompt 等真实读取),
/// 渲染登记的注入清单留在 ctx.data.injected 供 engine 消费。
pub fn collect_generate_entries_with(
    entries: &[WorldEntry],
    ctx: &mut RenderCtx<'_>,
) -> (Vec<GenerateEntry>, Vec<WorldEntry>) {
    let mut generated: Vec<GenerateEntry> = Vec::new();
    let mut rest: Vec<WorldEntry> = Vec::new();
    for e in entries {
        if !e.enabled {
            rest.push(e.clone());
            continue;
        }
        match parse_inject_tag(&e.comment) {
            // 无标签 / 初始变量条目不进生成收集
            InjectTag::None | InjectTag::InitialVariables => rest.push(e.clone()),
            tag => {
                let rendered = render_assistant_content_with(&e.content, ctx);
                generated.push(GenerateEntry {
                    tag,
                    entry: e.clone(),
                    rendered,
                });
            }
        }
    }
    // GenerateIndex 按 idx 升序;稳定排序保证其余标签与同 idx 条目保持原相对顺序
    generated.sort_by_key(|g| index_key(&g.tag));
    (generated, rest)
}

/// GenerateIndex 条目的排序键(非索引标签用 usize::MAX 保持原顺序)
fn index_key(tag: &InjectTag) -> usize {
    match tag {
        InjectTag::GenerateIndex { idx, .. } => *idx,
        _ => usize::MAX,
    }
}
