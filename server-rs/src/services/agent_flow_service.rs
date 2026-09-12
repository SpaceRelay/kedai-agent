// 自定义 Agent 执行流程服务(custom 模式):用户可编辑步骤序列(名称/目标/生成开关/
// 反思/系统提示词/温度/输出上限/工具白名单),持久化到 data/agent_flows.json。
// 支持多流程库:可新建/复制/选择/删除/导入/导出流程,当前选中流程决定 custom 模式行为。
// 与提示词注入服务同构:全局作用域、内存缓存配置、全量读写。
use crate::models::types::PlanStep;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// 自定义执行流程服务:持有 data_dir,内存缓存流程库,读写 data/agent_flows.json
pub struct AgentFlowService {
    data_dir: PathBuf,
    library: AgentFlowLibrary,
    registered_tools: BTreeSet<String>,
}

impl AgentFlowService {
    pub fn new(data_dir: PathBuf, registered_tools: Vec<String>) -> Self {
        let library = AgentFlowLibrary::load(&data_dir);
        AgentFlowService {
            data_dir,
            library,
            registered_tools: registered_tools.into_iter().collect(),
        }
    }

    /// 当前选中的流程(未选择时 None → custom 模式不生效)
    pub fn get(&self) -> Option<&AgentFlowConfig> {
        let id = self.library.current_flow_id.as_ref()?;
        self.library.flows.iter().find(|f| &f.id == id)
    }

    pub fn get_library(&self) -> &AgentFlowLibrary {
        &self.library
    }

    /// 使用启动时已注册工具集合校验流程,供保存与执行前复用。
    pub fn validate(&self, config: &AgentFlowConfig) -> Result<(), String> {
        validate_flow(config, &self.registered_tools)
    }

    /// 保存(创建或更新)流程并设为当前选中;id 为空时自动生成新 id(新建)。
    /// 校验失败返回 Err(不落盘)。
    pub fn set(&mut self, mut config: AgentFlowConfig) -> Result<(), String> {
        validate_flow(&config, &self.registered_tools)?;
        if config.id.trim().is_empty() {
            config.id = Uuid::new_v4().to_string();
        }
        if config.name.trim().is_empty() {
            config.name = "未命名流程".into();
        }
        match self.library.flows.iter_mut().find(|f| f.id == config.id) {
            Some(existing) => *existing = config.clone(),
            None => self.library.flows.push(config.clone()),
        }
        self.library.current_flow_id = Some(config.id);
        self.save_library()
    }

    /// 选择某个流程为当前流程(id 不存在返回 Err)
    pub fn select(&mut self, id: &str) -> Result<(), String> {
        if !self.library.flows.iter().any(|f| f.id == id) {
            return Err(format!("流程不存在:{}", id));
        }
        self.library.current_flow_id = Some(id.to_string());
        self.save_library()
    }

    /// 删除流程;删除当前选中时回退到第一个流程(库空则 None)。至少保留一个流程时不允许删空。
    pub fn remove(&mut self, id: &str) -> Result<(), String> {
        let before = self.library.flows.len();
        self.library.flows.retain(|f| f.id != id);
        if self.library.flows.len() == before {
            return Err(format!("流程不存在:{}", id));
        }
        if self.library.current_flow_id.as_deref() == Some(id) {
            self.library.current_flow_id = self.library.flows.first().map(|f| f.id.clone());
        }
        self.save_library()
    }

    /// 持久化流程库到 data/agent_flows.json(原子写:崩溃不留半截 JSON)
    fn save_library(&self) -> Result<(), String> {
        let text = serde_json::to_string_pretty(&self.library).map_err(|e| e.to_string())?;
        crate::utils::fs_atomic::write_atomic(
            &self.data_dir.join("agent_flows.json"),
            text.as_bytes(),
        )
        .map_err(|e| e.to_string())
    }
}

/// 流程库(全局):当前选中的流程 + 全部流程
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentFlowLibrary {
    /// 当前选中的流程 id(None = 未选择,custom 模式不生效)
    #[serde(default)]
    pub current_flow_id: Option<String>,
    /// 全部流程(按数组顺序展示)
    #[serde(default)]
    pub flows: Vec<AgentFlowConfig>,
}

/// 自定义执行流程配置(单个流程)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentFlowConfig {
    /// 流程 id(库内唯一;旧格式迁移时自动生成)
    #[serde(default)]
    pub id: String,
    /// 流程名称(选择器展示)
    #[serde(default)]
    pub name: String,
    /// 流程说明(可选)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 是否启用自定义流程(chat/send 的 agent_mode=custom 需要 enabled=true 才生效)
    #[serde(default)]
    pub enabled: bool,
    /// 步骤序列(按数组顺序执行;disabled 步骤跳过)
    #[serde(default)]
    pub steps: Vec<PlanStep>,
}

impl AgentFlowLibrary {
    /// 从 data/agent_flows.json 加载;缺失/损坏记录日志后回退默认(不静默)。
    /// 兼容旧单流程格式 `{enabled, steps}`:自动迁移为库(旧步骤保留)。
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join("agent_flows.json");
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                // 按顶层字段预判格式:含 flows/current_flow_id 为库格式,否则为旧单流程格式
                // (库结构字段全部带 default,直接按库解析会误吞旧格式 → 必须先预判)
                let value: Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::error!(
                            error = e.to_string(),
                            "自定义流程配置解析失败,已回退默认配置"
                        );
                        return AgentFlowLibrary::default();
                    }
                };
                if value.get("flows").is_some() || value.get("current_flow_id").is_some() {
                    match serde_json::from_value::<AgentFlowLibrary>(value) {
                        Ok(lib) => finalize_library(lib),
                        Err(e) => {
                            tracing::error!(
                                error = e.to_string(),
                                "自定义流程配置解析失败,已回退默认配置"
                            );
                            AgentFlowLibrary::default()
                        }
                    }
                } else {
                    // 旧版单流程格式 → 迁移为流程库
                    match serde_json::from_value::<AgentFlowConfig>(value) {
                        Ok(mut old) => {
                            tracing::info!("检测到旧版单流程配置,已迁移为流程库");
                            if old.id.trim().is_empty() {
                                old.id = Uuid::new_v4().to_string();
                            }
                            if old.name.trim().is_empty() {
                                old.name = "默认流程".into();
                            }
                            // 旧版空配置(从未编辑过)→ 替换为内置协调流程,开箱即用
                            if old.steps.is_empty() {
                                old = builtin_flow();
                            }
                            let lib = AgentFlowLibrary {
                                current_flow_id: Some(old.id.clone()),
                                flows: vec![old],
                            };
                            if let Err(e) = lib.save(data_dir) {
                                tracing::error!(error = e, "旧配置迁移写入失败");
                            }
                            lib
                        }
                        Err(e) => {
                            tracing::error!(
                                error = e.to_string(),
                                "自定义流程配置解析失败,已回退默认配置"
                            );
                            AgentFlowLibrary::default()
                        }
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // 首次运行:注入内置协调流程,开箱即用
                let lib = AgentFlowLibrary {
                    current_flow_id: Some(builtin_flow().id.clone()),
                    flows: vec![builtin_flow()],
                };
                let _ = lib.save(data_dir);
                lib
            }
            Err(e) => {
                tracing::error!(
                    error = e.to_string(),
                    "自定义流程配置读取失败,已回退默认配置"
                );
                AgentFlowLibrary::default()
            }
        }
    }

    /// 持久化流程库到 data/agent_flows.json(原子写:崩溃不留半截 JSON)
    pub fn save(&self, data_dir: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        crate::utils::fs_atomic::write_atomic(&data_dir.join("agent_flows.json"), text.as_bytes())
            .map_err(|e| e.to_string())
    }
}

/// 库加载收尾:未选择流程时默认指向第一个(升级/迁移兜底)
fn finalize_library(mut lib: AgentFlowLibrary) -> AgentFlowLibrary {
    if lib.current_flow_id.is_none() && !lib.flows.is_empty() {
        lib.current_flow_id = lib.flows.first().map(|f| f.id.clone());
    }
    lib
}

/// 内置默认流程:与 Kedai harness 协调的文学创作/角色扮演流程。
/// 三步:起草正文(先写 ≤200 字计划再输出正文)→ 反思(判定 PASS/FAIL)→ 修订(生成)。
/// 步骤级提示词使用 Kedai 酒馆宏与 mvu 变量协议,与 custom 模式引擎语义对齐。
pub fn builtin_flow() -> AgentFlowConfig {
    AgentFlowConfig {
        id: "builtin-coordination".into(),
        name: "文学创作协调流程".into(),
        description: Some(
            "Kedai 内置协调流程:起草正文(先计划后正文)→ 反思质量 → 修订输出;与酒馆宏、世界书、mvu 变量协议(function calling 优先)对齐。"
                .into(),
        ),
        enabled: true,
        steps: vec![
            PlanStep {
                id: "draft".into(),
                name: "起草正文".into(),
                enabled: true,
                goal: "先撰写 ≤200 字写作计划,再按计划生成正文,服从用户字数/视角要求".into(),
                action: "direct".into(),
                generates: Some(true),
                system_prompt: Some(
                    "【本步指令·计划并起草】开始输出正文前,先在草稿区撰写一份 200 字以内的写作计划\
                     (段落结构、核心要点、情节走向、节奏安排),随后严格按该计划输出正文;\
                     正文中不得包含计划本身,不得出现「计划」「草稿」等字样。\
                     以 {{char}} 的视角与口吻输出:视角遵循系统提示词与用户要求\
                     (用户未指定时以角色自身视角叙述);不要出现旁白标题、「以上是回复」等元文本。\
                     服从用户的字数与风格要求;用户提问必须正面回答。\
                     变量更新优先通过 function calling 工具(update_variables)完成,模型未走工具时\
                     才回退 <UpdateVariable> 文本协议(JSONPatch 数组,支持 replace / delta / insert / remove / move,\
                     路径对应当前状态的键);无变化则不输出;纯状态更新时正文可以为空。除该块外不得使用任何自定义标签。"
                        .into(),
                ),
                tools: Some(vec!["update_variables".into()]),
                tool_choice: Some("auto".into()),
                ..Default::default()
            },
            PlanStep {
                id: "reflect".into(),
                name: "反思质量".into(),
                enabled: true,
                goal: "检查草稿:字数是否达标、疑问是否回答、视角是否一致、是否被截断;判定输出 PASS 或 FAIL".into(),
                action: "reflect".into(),
                generates: None,
                ..Default::default()
            },
            PlanStep {
                id: "revise".into(),
                name: "修订输出".into(),
                enabled: true,
                goal: "按反思结论修订并输出最终正文;未发现问题时直接输出原草稿".into(),
                action: "direct".into(),
                generates: Some(true),
                system_prompt: Some(
                    "【本步指令·修订输出】基于反思结论修订上一版草稿:修复缺陷后再次以 {{char}} 视角完整输出最终正文;\
                     若反思结论为通过,原样输出草稿。任何情况下都不得输出反思过程本身。\
                     变量更新同样优先经 function calling 工具(update_variables),未走工具时回退 <UpdateVariable> 文本协议,\
                     格式与起草步骤相同。"
                        .into(),
                ),
                tools: Some(vec!["update_variables".into()]),
                tool_choice: Some("auto".into()),
                ..Default::default()
            },
        ],
    }
}

/// 校验自定义流程(失败返回中文错误,PUT 时 400 给用户):
///  - 启用的步骤非空
///  - 至少一个「生成正文」的 direct 步骤(反思回退/输出依赖它)
///  - action 仅 direct / reflect;reflect 步骤不得携带 generates=Some(true)
///  - 温度 0.0-2.0、输出上限 1-32768(仅校验显式提供的值)
pub fn validate_flow(
    cfg: &AgentFlowConfig,
    registered_tools: &BTreeSet<String>,
) -> Result<(), String> {
    // 未启用(开关关闭):允许保存任意状态(用户可先编辑、后启用);启用时才校验
    if !cfg.enabled {
        return Ok(());
    }
    let active: Vec<&PlanStep> = cfg.steps.iter().filter(|s| s.enabled).collect();
    if active.is_empty() {
        return Err("自定义流程为空:请启用至少一个步骤".into());
    }
    if !active
        .iter()
        .any(|s| s.action == "direct" && s.generates == Some(true))
    {
        return Err("自定义流程缺少生成步骤:至少需要一个「生成正文」的 direct 步骤".into());
    }
    for s in &active {
        if s.action != "direct" && s.action != "reflect" {
            return Err(format!(
                "步骤「{}」动作非法(仅支持 direct / reflect)",
                s.name
            ));
        }
        if s.action == "reflect" && s.generates == Some(true) {
            return Err(format!("步骤「{}」为反思步骤,不应开启生成正文", s.name));
        }
        if s.action == "reflect" && s.system_prompt.is_some() {
            return Err(format!("步骤「{}」为反思步骤,不支持系统提示词", s.name));
        }
        if let Some(t) = s.temperature {
            if !(0.0..=2.0).contains(&t) {
                return Err(format!("步骤「{}」温度需在 0.0-2.0 之间", s.name));
            }
        }
        if let Some(m) = s.max_tokens {
            if !(1..=32768).contains(&m) {
                return Err(format!("步骤「{}」输出上限需在 1-32768 之间", s.name));
            }
        }
        if s.goal.trim().is_empty() {
            return Err(format!("步骤「{}」缺少目标说明", s.name));
        }
        if let Some(names) = &s.tools {
            for name in names {
                if name.trim().is_empty() || !registered_tools.contains(name) {
                    return Err(format!("步骤「{}」引用了未注册工具: {}", s.name, name));
                }
            }
        }
        let choice = s.tool_choice.as_deref().unwrap_or("auto");
        if !matches!(choice, "auto" | "none" | "required" | "function") {
            return Err(format!("步骤「{}」tool_choice 非法", s.name));
        }
        if choice == "required" && s.tools.is_none() {
            return Err(format!(
                "步骤「{}」tool_choice=required 时必须配置有效工具",
                s.name
            ));
        }
        if choice == "function" {
            let function = s
                .tool_choice_function
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| format!("步骤「{}」tool_choice=function 时必须指定工具", s.name))?;
            let effective = match &s.tools {
                None => false,
                Some(names) if names.is_empty() => registered_tools.contains(function),
                Some(names) => names.iter().any(|name| name == function),
            };
            if !effective {
                return Err(format!(
                    "步骤「{}」指定的 function 工具不在有效工具内: {}",
                    s.name, function
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools() -> BTreeSet<String> {
        ["read", "search", "update_variables", "calculator"]
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    fn step(name: &str, action: &str, generates: Option<bool>) -> PlanStep {
        PlanStep {
            id: format!("s-{name}"),
            name: name.into(),
            enabled: true,
            goal: format!("{name} 目标"),
            action: action.into(),
            generates,
            ..Default::default()
        }
    }

    fn flow(name: &str, steps: Vec<PlanStep>) -> AgentFlowConfig {
        AgentFlowConfig {
            id: format!("flow-{name}"),
            name: name.into(),
            description: None,
            enabled: true,
            steps,
        }
    }

    #[test]
    fn builtin_flow_step_tools_and_mvu_wording() {
        let f = builtin_flow();
        // 内置流程:起草(先计划后正文)→ 反思 → 修订(理解意图步已废弃)
        assert_eq!(f.steps.len(), 3);
        // 起草步:配 update_variables,提示词含「先写计划再输出正文」且点名工具
        let draft = &f.steps[0];
        assert_eq!(
            draft.tools.as_deref(),
            Some(&["update_variables".to_string()][..]),
            "起草步应实际下发 update_variables"
        );
        let dsp = draft.system_prompt.as_deref().unwrap_or("");
        assert!(
            dsp.contains("update_variables"),
            "起草步应点名 update_variables 工具: {dsp}"
        );
        assert!(
            dsp.contains("回退") && dsp.contains("<UpdateVariable>"),
            "起草步应说明文本协议是回退: {dsp}"
        );
        assert!(
            dsp.contains("200 字") && dsp.contains("计划") && dsp.contains("正文"),
            "起草步应先写 ≤200 字计划再输出正文: {dsp}"
        );
        // 反思步:无工具、无 system_prompt
        assert!(f.steps[1].tools.is_none());
        assert!(f.steps[1].system_prompt.is_none());
        // 修订步:update_variables + 回退协议
        let revise = &f.steps[2];
        assert_eq!(
            revise.tools.as_deref(),
            Some(&["update_variables".to_string()][..])
        );
        let rsp = revise.system_prompt.as_deref().unwrap_or("");
        assert!(
            rsp.contains("update_variables") && rsp.contains("回退"),
            "修订步应点名 update_variables 与回退协议: {rsp}"
        );
    }

    #[test]
    fn valid_flow_passes() {
        let cfg = flow(
            "ok",
            vec![
                step("理解", "direct", Some(false)),
                step("生成", "direct", Some(true)),
                step("反思", "reflect", None),
            ],
        );
        assert!(validate_flow(&cfg, &tools()).is_ok());
    }

    #[test]
    fn empty_flow_rejected() {
        // 未启用(开关关闭):允许保存空流程(用户可先编辑后启用)
        assert!(validate_flow(&AgentFlowConfig::default(), &tools()).is_ok());
        // 启用但无步骤 → 拒绝
        let mut cfg = flow("x", Vec::new());
        cfg.enabled = true;
        assert!(validate_flow(&cfg, &tools()).is_err());
        // 启用但全部步骤 disabled → 拒绝
        let mut cfg = flow("x", vec![step("禁用", "direct", Some(true))]);
        cfg.steps[0].enabled = false;
        assert!(validate_flow(&cfg, &tools()).is_err());
    }

    #[test]
    fn no_generating_step_rejected() {
        let cfg = flow(
            "x",
            vec![
                step("理解", "direct", Some(false)),
                step("反思", "reflect", None),
            ],
        );
        assert!(validate_flow(&cfg, &tools()).is_err());
    }

    #[test]
    fn unregistered_whitelist_tool_is_rejected() {
        let mut generation = step("生成", "direct", Some(true));
        generation.tools = Some(vec!["missing_tool".into()]);
        let error = validate_flow(&flow("x", vec![generation]), &tools()).unwrap_err();
        assert!(error.contains("未注册工具"), "实际错误: {error}");
    }

    #[test]
    fn function_choice_must_reference_effective_tool() {
        let mut generation = step("生成", "direct", Some(true));
        generation.tools = Some(vec!["read".into()]);
        generation.tool_choice = Some("function".into());
        generation.tool_choice_function = Some("calculator".into());
        let error = validate_flow(&flow("x", vec![generation]), &tools()).unwrap_err();
        assert!(error.contains("不在有效工具内"), "实际错误: {error}");
    }

    #[test]
    fn bad_action_rejected() {
        let cfg = flow("x", vec![step("怪步", "teleport", Some(true))]);
        assert!(validate_flow(&cfg, &tools()).is_err());
    }

    #[test]
    fn reflect_with_prompt_rejected() {
        let cfg = flow(
            "x",
            vec![
                step("生成", "direct", Some(true)),
                PlanStep {
                    system_prompt: Some("不应支持".into()),
                    ..step("反思", "reflect", None)
                },
            ],
        );
        assert!(validate_flow(&cfg, &tools()).is_err());
    }

    #[test]
    fn load_corrupted_file_falls_back_to_default() {
        let dir = std::env::temp_dir().join(format!("kedai-flow-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("agent_flows.json"), "{not json").unwrap();
        let lib = AgentFlowLibrary::load(&dir);
        assert!(lib.flows.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_file_creates_builtin() {
        let dir = std::env::temp_dir().join(format!("kedai-flow-missing-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::remove_file(dir.join("agent_flows.json"));
        let lib = AgentFlowLibrary::load(&dir);
        assert_eq!(lib.flows.len(), 1);
        assert_eq!(lib.flows[0].id, "builtin-coordination");
        assert_eq!(lib.current_flow_id.as_deref(), Some("builtin-coordination"));
        assert!(validate_flow(&lib.flows[0], &tools()).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_legacy_single_flow_migrates() {
        let dir = std::env::temp_dir().join(format!("kedai-flow-legacy-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(
            dir.join("agent_flows.json"),
            r#"{"enabled":true,"steps":[{"id":"s1","name":"生成","enabled":true,"goal":"生成正文","action":"direct","generates":true}]}"#,
        )
        .unwrap();
        let lib = AgentFlowLibrary::load(&dir);
        assert_eq!(lib.flows.len(), 1);
        assert_eq!(lib.flows[0].steps.len(), 1);
        assert_eq!(lib.flows[0].name, "默认流程");
        assert_eq!(
            lib.current_flow_id.as_deref(),
            Some(lib.flows[0].id.as_str())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn service_set_select_remove_roundtrip() {
        let dir = std::env::temp_dir().join(format!("kedai-flow-svc-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let mut svc = AgentFlowService::new(dir.clone(), tools().into_iter().collect());
        // 内置默认流程已在首次加载时注入
        assert_eq!(svc.get_library().flows.len(), 1);

        // 新建流程(set 空 id → 自动分配)
        let cfg = flow("my", vec![step("生成", "direct", Some(true))]);
        let mut new_cfg = cfg.clone();
        new_cfg.id = String::new();
        svc.set(new_cfg).unwrap();
        let lib = svc.get_library();
        assert_eq!(lib.flows.len(), 2);
        let mine = lib.flows.iter().find(|f| f.name == "my").unwrap();
        assert!(!mine.id.is_empty());
        assert_eq!(lib.current_flow_id.as_deref(), Some(mine.id.as_str()));

        // 选择内置流程
        svc.select("builtin-coordination").unwrap();
        assert_eq!(svc.get().map(|f| f.name.as_str()), Some("文学创作协调流程"));

        // 删除当前流程 → 回退到第一个
        svc.remove("builtin-coordination").unwrap();
        assert_eq!(svc.get_library().flows.len(), 1);
        assert_eq!(
            svc.get_library().current_flow_id.as_deref(),
            Some(svc.get_library().flows[0].id.as_str())
        );

        // 重新加载(持久化验证)
        let svc2 = AgentFlowService::new(dir.clone(), tools().into_iter().collect());
        assert_eq!(svc2.get_library().flows.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
