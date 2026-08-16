// 提示词注入服务:简单模式(字数/转述/对话/视角)+ 复杂模式楼层系统
// 持久化到 data/prompt_floors.json;全局作用域,所有会话生效。
use crate::utils::logger;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// 简单模式「字数要求」→ 输出预算下限(纯函数,供 chat/send 协调 max_tokens):
/// 中文约 1 字 ≈ 1-2 token,思考模型推理先占输出预算,故按 字数×2 + 512 余量估算,
/// 封顶 8192;请求方显式给的值更大时尊重请求值。
pub fn output_budget_for_word_count(word_count: u32, requested: u32) -> u32 {
    // 0 字数 = 无字数要求,保持请求值不变
    if word_count == 0 {
        return requested;
    }
    let needed = word_count.saturating_mul(2).saturating_add(512).min(8192);
    requested.max(needed)
}

/// 提示词注入服务:持有 data_dir,内存缓存配置,读写 data/prompt_floors.json
pub struct PromptInjectService {
    data_dir: PathBuf,
    config: PromptInjectConfig,
}

impl PromptInjectService {
    pub fn new(data_dir: PathBuf) -> Self {
        let config = PromptInjectConfig::load(&data_dir);
        PromptInjectService { data_dir, config }
    }

    pub fn get(&self) -> &PromptInjectConfig {
        &self.config
    }

    /// 全量替换配置并持久化;校验失败返回 Err(不落盘)
    pub fn set(&mut self, config: PromptInjectConfig) -> Result<(), String> {
        validate_config(&config)?;
        config.save(&self.data_dir)?;
        self.config = config;
        Ok(())
    }

    /// 持久化当前配置(供引擎侧直接改内存后调用)
    pub fn save(&self) -> Result<(), String> {
        self.config.save(&self.data_dir)
    }
}

/// 校验楼层配置:role/position 枚举由 serde 保证;深度/排序任意非负;名称非空可选
fn validate_config(_cfg: &PromptInjectConfig) -> Result<(), String> {
    // 枚举字段由 serde(rename_all) 反序列化时校验,非法值会直接反序列化失败;
    // 这里只做轻量兜底(如内容超长防滥用)。
    Ok(())
}

/// 楼层注入位置(与 SillyTavern Prompt Manager 语义对齐)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FloorPosition {
    /// 拼入系统提示词末尾
    #[default]
    System,
    /// 对话历史最前(开场白之前)
    Before,
    /// 对话历史最后(最新消息之后)
    After,
    /// 深度:从历史末尾往前数第 N 条之后插入(0 = 最新消息后)
    Depth,
}

/// 楼层消息角色(自由选择)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FloorRole {
    #[default]
    System,
    User,
    Assistant,
}

impl FloorRole {
    /// LLM 消息角色字符串(system/user/assistant)
    pub fn as_str(&self) -> &'static str {
        match self {
            FloorRole::System => "system",
            FloorRole::User => "user",
            FloorRole::Assistant => "assistant",
        }
    }
}

/// 单条提示词楼层
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptFloor {
    pub id: String,
    pub name: String,
    pub content: String,
    #[serde(default)]
    pub role: FloorRole,
    #[serde(default)]
    pub position: FloorPosition,
    /// position = depth 时的深度(0 = 最新消息后)
    #[serde(default)]
    pub depth: usize,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 拖拽排序序号(同级内按 order 升序)
    #[serde(default)]
    pub order: usize,
}

fn default_true() -> bool {
    true
}

/// 禁词条目:输出中出现 word 时,注入自省提示词要求换表达(所有模式),
/// deep/agent/custom 模式另由引擎收尾调用 censor_text 工具替换为 replacement(同义改写)。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BannedWordEntry {
    /// 禁词(精确子串匹配)
    #[serde(default)]
    pub word: String,
    /// 替换词(含义相近、更得体的表达;空串 = 直接删除该词)
    #[serde(default)]
    pub replacement: String,
}

/// 简单模式四项配置(+ 禁词库)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimplePromptConfig {
    #[serde(default)]
    pub word_count_enabled: bool,
    /// 字数上限(>0 且启用时生效)
    #[serde(default)]
    pub word_count: u32,
    #[serde(default)]
    pub paraphrase_enabled: bool,
    #[serde(default)]
    pub dialogue_enabled: bool,
    #[serde(default)]
    pub perspective_enabled: bool,
    /// 视角:第一人称 / 第二人称 / 第三人称
    #[serde(default)]
    pub perspective: String,
    /// 注入顺序(合成文本按此顺序拼接;空 = 默认 字数→转述→对话→视角)
    #[serde(default)]
    pub order: Vec<String>,
    /// 禁词库总开关
    #[serde(default)]
    pub banned_words_enabled: bool,
    /// 禁词提示词(纯文本,发给 AI 的自省提示;新格式)
    /// 非空时优先于 banned_words 旧表,直接作为提示词注入
    #[serde(default)]
    pub banned_prompt: String,
    /// 禁词 → 替换词 映射表(旧格式兼容,仅保留字段不再用于注入;
    /// load() 迁移到 banned_prompt 后清空,避免双重注入)
    #[serde(default)]
    pub banned_words: Vec<BannedWordEntry>,
}

impl Default for SimplePromptConfig {
    fn default() -> Self {
        SimplePromptConfig {
            word_count_enabled: false,
            word_count: 200,
            paraphrase_enabled: false,
            dialogue_enabled: false,
            perspective_enabled: false,
            perspective: "第三人称".to_string(),
            order: vec![
                "word_count".to_string(),
                "paraphrase".to_string(),
                "dialogue".to_string(),
                "perspective".to_string(),
            ],
            banned_words_enabled: false,
            banned_prompt: String::new(),
            banned_words: Vec::new(),
        }
    }
}

impl SimplePromptConfig {
    /// 禁词库自省提示(纯函数,简单/复杂模式共用):
    /// 启用且有内容时返回提示词,否则返回空串。
    /// 优先级:banned_prompt(新格式纯文本)> banned_words(旧格式兼容拼接)。
    pub fn banned_words_hint(&self) -> String {
        if !self.banned_words_enabled {
            return String::new();
        }
        // 新格式:纯文本提示词直接返回(用户已写好完整提示词)
        let prompt = self.banned_prompt.trim();
        if !prompt.is_empty() {
            return prompt.to_string();
        }
        // 旧格式兼容:从词条表拼接提示词(仅用于未迁移的旧数据)
        let words: Vec<String> = self
            .banned_words
            .iter()
            .map(|b| b.word.trim().to_string())
            .filter(|w| !w.is_empty())
            .collect();
        if words.is_empty() {
            return String::new();
        }
        format!(
            "输出中禁止出现以下词语,若涉及请用含义相近、更得体的表达替换:{}",
            words.join("、")
        )
    }

    /// 从禁词提示词中提取禁用词列表(供引擎 censor_text 工具兜底替换用)
    /// 规则:按 逗号/顿号/换行 分割,取首个冒号/冒号前的词
    pub fn banned_words_extract(&self) -> Vec<String> {
        let text = if !self.banned_prompt.trim().is_empty() {
            self.banned_prompt.clone()
        } else {
            // 旧格式:直接从词条表取词
            return self
                .banned_words
                .iter()
                .map(|b| b.word.trim().to_string())
                .filter(|w| !w.is_empty())
                .collect();
        };
        // 从纯文本提示词中提取:取「禁止出现:...」或「禁止:...」后的词列表
        let lower = text.to_lowercase();
        for marker in ["禁止出现:", "禁止:", "禁用:"] {
            if let Some(pos) = lower.find(marker) {
                let tail = &text[pos + marker.len()..];
                let words: Vec<String> = tail
                    .split([',', '、', '\n', '，', '。'])
                    .map(|w| w.trim().to_string())
                    .filter(|w| !w.is_empty())
                    .collect();
                if !words.is_empty() {
                    return words;
                }
            }
        }
        // 无标记词时,按分隔符整体切分
        text.split([',', '、', '\n', '，', '。'])
            .map(|w| w.trim().to_string())
            .filter(|w| !w.is_empty())
            .collect()
    }
}

/// 注入模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum InjectMode {
    #[default]
    Simple,
    Complex,
}

/// 提示词注入整体配置(全局)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptInjectConfig {
    #[serde(default)]
    pub mode: InjectMode,
    #[serde(default)]
    pub simple: SimplePromptConfig,
    #[serde(default)]
    pub floors: Vec<PromptFloor>,
}

impl Default for PromptInjectConfig {
    fn default() -> Self {
        PromptInjectConfig {
            mode: InjectMode::Simple,
            simple: SimplePromptConfig::default(),
            floors: Vec::new(),
        }
    }
}

impl PromptInjectConfig {
    /// 从 data/prompt_floors.json 加载;缺失回退默认,损坏记录日志后回退默认(不静默归零)
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join("prompt_floors.json");
        let mut cfg = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<PromptInjectConfig>(&text) {
                Ok(cfg) => cfg,
                Err(e) => {
                    logger::error(
                        "提示词注入配置解析失败,已回退默认配置",
                        &[("error", Value::String(e.to_string()))],
                    );
                    PromptInjectConfig::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => PromptInjectConfig::default(),
            Err(e) => {
                logger::error(
                    "提示词注入配置读取失败,已回退默认配置",
                    &[("error", Value::String(e.to_string()))],
                );
                PromptInjectConfig::default()
            }
        };
        // 旧数据迁移:有 banned_words 词条且 banned_prompt 为空 → 迁移为纯文本提示词
        cfg.migrate_banned_words();
        cfg
    }

    /// 旧数据迁移:把 banned_words 词条表迁移为 banned_prompt 纯文本提示词
    /// 迁移后清空 banned_words,避免后续双重注入
    pub fn migrate_banned_words(&mut self) {
        if self.simple.banned_prompt.trim().is_empty() && !self.simple.banned_words.is_empty() {
            let words: Vec<String> = self
                .simple
                .banned_words
                .iter()
                .map(|b| b.word.trim().to_string())
                .filter(|w| !w.is_empty())
                .collect();
            if !words.is_empty() {
                self.simple.banned_prompt = format!(
                    "输出中禁止出现以下词语,若涉及请用含义相近、更得体的表达替换:{}",
                    words.join("、")
                );
            }
            // 清空旧词条表,防止双重注入
            self.simple.banned_words.clear();
        }
    }

    /// 持久化到 data/prompt_floors.json
    pub fn save(&self, data_dir: &Path) -> Result<(), String> {
        let _ = std::fs::create_dir_all(data_dir);
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(data_dir.join("prompt_floors.json"), text).map_err(|e| e.to_string())
    }

    /// 简单模式:合成注入提示词文本(按 order 顺序拼接启用项;空 = 无注入)
    /// 与楼层系统互斥(由 mode 决定),文本在此层只负责组装,宏展开在引擎层做。
    pub fn simple_inject_text(&self) -> String {
        let s = &self.simple;
        // 启用项内容表(键 = order 中的标识)
        let mut items: Vec<(&str, String)> = Vec::new();
        if s.word_count_enabled && s.word_count > 0 {
            items.push((
                "word_count",
                format!(
                    "请输出篇幅约为 {} 字的正文(明显少于该字数视为不达标,不得大幅缩水)。",
                    s.word_count
                ),
            ));
        }
        if s.paraphrase_enabled {
            items.push((
                "paraphrase",
                "请以转述的方式复述内容,用自己的话表达,不要直接照抄原文。".to_string(),
            ));
        }
        if s.dialogue_enabled {
            items.push((
                "dialogue",
                "请只以对话形式回复,输出角色台词与必要动作描写,不输出旁白叙述段落。".to_string(),
            ));
        }
        if s.perspective_enabled {
            let p = s.perspective.trim();
            if !p.is_empty() {
                items.push(("perspective", format!("请以{}视角进行叙述。", p)));
            }
        }
        // 按 order 排序:order 覆盖全部启用项;未出现的键按默认顺序补尾(兼容旧配置)
        let order = if s.order.is_empty() {
            vec!["word_count", "paraphrase", "dialogue", "perspective"]
        } else {
            s.order.iter().map(|k| k.as_str()).collect::<Vec<_>>()
        };
        let mut sorted: Vec<String> = Vec::new();
        for key in &order {
            if let Some(idx) = items.iter().position(|(k, _)| k == key) {
                sorted.push(items.remove(idx).1);
            }
        }
        // 兜底:默认顺序中未被 order 覆盖的启用项补到末尾(保证不丢)
        for (_, text) in items {
            sorted.push(text);
        }
        // 禁词库自省提示(不进 order 系统,固定追加末尾):所有模式生效——
        // deep/agent/custom 模式另由引擎收尾工具替换兜底。
        let hint = s.banned_words_hint();
        if !hint.is_empty() {
            sorted.push(hint);
        }
        sorted.join("\n")
    }

    /// 启用且按 order 排序的楼层(简单模式返回空)
    pub fn enabled_floors_sorted(&self) -> Vec<&PromptFloor> {
        if self.mode != InjectMode::Complex {
            return Vec::new();
        }
        let mut floors: Vec<&PromptFloor> = self.floors.iter().filter(|f| f.enabled).collect();
        floors.sort_by_key(|f| f.order);
        floors
    }
}

/// 楼层 ID 生成(uuid v4,与 skill_service 一致)
pub fn new_floor_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_inject_text_combines_enabled_items() {
        let mut cfg = PromptInjectConfig::default();
        cfg.simple.word_count_enabled = true;
        cfg.simple.word_count = 150;
        cfg.simple.dialogue_enabled = true;
        let text = cfg.simple_inject_text();
        assert!(text.contains("150 字"), "text: {text}");
        assert!(text.contains("只以对话形式"), "text: {text}");
        assert!(!text.contains("转述"), "text: {text}");
        assert!(!text.contains("视角"), "text: {text}");
    }

    #[test]
    fn simple_inject_text_empty_when_nothing_enabled() {
        let cfg = PromptInjectConfig::default();
        assert_eq!(cfg.simple_inject_text(), "");
        // 字数启用但为 0 → 不注入
        let mut cfg2 = PromptInjectConfig::default();
        cfg2.simple.word_count_enabled = true;
        cfg2.simple.word_count = 0;
        assert_eq!(cfg2.simple_inject_text(), "");
    }

    #[test]
    fn perspective_text_uses_configured_view() {
        let mut cfg = PromptInjectConfig::default();
        cfg.simple.perspective_enabled = true;
        cfg.simple.perspective = "第一人称".to_string();
        let text = cfg.simple_inject_text();
        assert!(text.contains("第一人称视角"), "text: {text}");
    }

    #[test]
    fn banned_words_append_self_check_hint() {
        // 启用禁词库 + 有词条 → 末尾追加自省提示
        let mut cfg = PromptInjectConfig::default();
        cfg.simple.banned_words_enabled = true;
        cfg.simple.banned_words = vec![
            BannedWordEntry {
                word: "笨蛋".into(),
                replacement: "傻瓜".into(),
            },
            BannedWordEntry {
                word: "脏话".into(),
                replacement: String::new(),
            },
        ];
        let text = cfg.simple_inject_text();
        assert!(text.contains("禁止出现以下词语"), "text: {text}");
        assert!(text.contains("笨蛋、脏话"), "应列出全部禁词: {text}");
    }

    #[test]
    fn banned_words_disabled_or_empty_no_hint() {
        // 未启用 → 不追加
        let cfg = PromptInjectConfig::default();
        assert!(!cfg.simple_inject_text().contains("禁止出现以下词语"));
        // 启用但词表为空/仅空白词 → 不追加
        let mut cfg2 = PromptInjectConfig::default();
        cfg2.simple.banned_words_enabled = true;
        assert!(!cfg2.simple_inject_text().contains("禁止出现以下词语"));
        cfg2.simple.banned_words = vec![BannedWordEntry {
            word: "   ".into(),
            replacement: "x".into(),
        }];
        assert!(!cfg2.simple_inject_text().contains("禁止出现以下词语"));
    }

    #[test]
    fn banned_words_hint_shared_all_modes() {
        // 共用方法(简单/复杂模式均追加):未启用 → 空;启用但词表空/空白 → 空;
        // 启用且词表非空 → 拼接提示;替换词不进入提示文本(仅引擎工具兜底使用)
        let mut cfg = PromptInjectConfig::default();
        assert_eq!(cfg.simple.banned_words_hint(), "");

        cfg.simple.banned_words_enabled = true;
        assert_eq!(cfg.simple.banned_words_hint(), "");

        cfg.simple.banned_words = vec![BannedWordEntry {
            word: "   ".into(),
            replacement: "x".into(),
        }];
        assert_eq!(cfg.simple.banned_words_hint(), "");

        cfg.simple.banned_words = vec![
            BannedWordEntry {
                word: "笨蛋".into(),
                replacement: "傻瓜".into(),
            },
            BannedWordEntry {
                word: "脏话".into(),
                replacement: String::new(),
            },
        ];
        let hint = cfg.simple.banned_words_hint();
        assert!(hint.contains("禁止出现以下词语"), "hint: {hint}");
        assert!(hint.contains("笨蛋、脏话"), "应列出全部禁词: {hint}");
        assert!(!hint.contains("傻瓜"), "替换词不应进提示文本: {hint}");
    }

    #[test]
    fn banned_words_roundtrip_persists() {
        // serde 往返:新字段透传(旧 JSON 缺字段 → 默认值)
        let mut cfg = PromptInjectConfig::default();
        cfg.simple.banned_words_enabled = true;
        cfg.simple.banned_words = vec![BannedWordEntry {
            word: "笨蛋".into(),
            replacement: "傻瓜".into(),
        }];
        let json = serde_json::to_string(&cfg).unwrap();
        let back: PromptInjectConfig = serde_json::from_str(&json).unwrap();
        assert!(back.simple.banned_words_enabled);
        assert_eq!(back.simple.banned_words.len(), 1);
        assert_eq!(back.simple.banned_words[0].word, "笨蛋");
        // 旧配置缺禁词字段 → 默认关闭 + 空表,不报错
        let old = r#"{"mode":"simple","simple":{"word_count_enabled":true}}"#;
        let old_cfg: PromptInjectConfig = serde_json::from_str(old).unwrap();
        assert!(!old_cfg.simple.banned_words_enabled);
        assert!(old_cfg.simple.banned_words.is_empty());
    }

    // ---- 纯文本禁词提示词(banned_prompt)----

    #[test]
    fn banned_prompt_priority_over_legacy_words() {
        // 新格式 banned_prompt 非空时优先,直接返回原文
        let mut cfg = PromptInjectConfig::default();
        cfg.simple.banned_words_enabled = true;
        cfg.simple.banned_prompt = "输出中禁止出现:笨蛋、脏话".to_string();
        cfg.simple.banned_words = vec![BannedWordEntry {
            word: "旧词".into(),
            replacement: String::new(),
        }];
        let hint = cfg.simple.banned_words_hint();
        assert_eq!(hint, "输出中禁止出现:笨蛋、脏话");
        assert!(!hint.contains("旧词"), "旧格式应被忽略: {hint}");
    }

    #[test]
    fn banned_prompt_empty_falls_back_to_legacy() {
        // banned_prompt 为空时回退旧格式词条表
        let mut cfg = PromptInjectConfig::default();
        cfg.simple.banned_words_enabled = true;
        cfg.simple.banned_prompt = "   ".to_string(); // 空白 = 无效
        cfg.simple.banned_words = vec![BannedWordEntry {
            word: "笨蛋".into(),
            replacement: "傻瓜".into(),
        }];
        let hint = cfg.simple.banned_words_hint();
        assert!(hint.contains("笨蛋"), "应回退旧格式: {hint}");
    }

    #[test]
    fn banned_prompt_in_simple_inject_text() {
        // simple_inject_text 集成:纯文本禁词提示注入到简单模式末尾
        let mut cfg = PromptInjectConfig::default();
        cfg.simple.banned_words_enabled = true;
        cfg.simple.banned_prompt = "禁止出现:笨蛋".to_string();
        let text = cfg.simple_inject_text();
        assert!(text.contains("禁止出现:笨蛋"), "text: {text}");
    }

    #[test]
    fn migrate_legacy_banned_words_to_prompt() {
        // load() 迁移:旧 banned_words → banned_prompt,并清空旧表
        let mut cfg = PromptInjectConfig::default();
        cfg.simple.banned_words_enabled = true;
        cfg.simple.banned_words = vec![
            BannedWordEntry {
                word: "笨蛋".into(),
                replacement: "傻瓜".into(),
            },
            BannedWordEntry {
                word: "脏话".into(),
                replacement: String::new(),
            },
        ];
        cfg.migrate_banned_words();
        assert!(
            cfg.simple.banned_prompt.contains("笨蛋、脏话"),
            "迁移后提示词: {}",
            cfg.simple.banned_prompt
        );
        assert!(cfg.simple.banned_words.is_empty(), "迁移后应清空旧词条表");
    }

    #[test]
    fn migrate_skipped_when_prompt_exists() {
        // banned_prompt 已有内容时不迁移(保护用户手动写的提示词)
        let mut cfg = PromptInjectConfig::default();
        cfg.simple.banned_prompt = "自定义提示词".to_string();
        cfg.simple.banned_words = vec![BannedWordEntry {
            word: "笨蛋".into(),
            replacement: "傻瓜".into(),
        }];
        cfg.migrate_banned_words();
        assert_eq!(cfg.simple.banned_prompt, "自定义提示词");
        assert!(!cfg.simple.banned_words.is_empty(), "有提示词时不清空旧表");
    }

    #[test]
    fn banned_words_extract_from_prompt() {
        // 从纯文本提示词提取禁用词(供 censor_text 工具)
        let mut cfg = PromptInjectConfig::default();
        cfg.simple.banned_prompt = "输出中禁止出现:笨蛋、脏话、滚".to_string();
        let words = cfg.simple.banned_words_extract();
        assert_eq!(words, vec!["笨蛋", "脏话", "滚"]);

        // 无标记词时按分隔符整体切分
        cfg.simple.banned_prompt = "笨蛋、脏话".to_string();
        let words2 = cfg.simple.banned_words_extract();
        assert_eq!(words2, vec!["笨蛋", "脏话"]);

        // 旧格式:直接从词条表取词
        cfg.simple.banned_prompt = String::new();
        cfg.simple.banned_words = vec![BannedWordEntry {
            word: "笨蛋".into(),
            replacement: "傻瓜".into(),
        }];
        let words3 = cfg.simple.banned_words_extract();
        assert_eq!(words3, vec!["笨蛋"]);
    }

    #[test]
    fn banned_prompt_serde_roundtrip() {
        // serde 往返:banned_prompt 字段透传
        let mut cfg = PromptInjectConfig::default();
        cfg.simple.banned_words_enabled = true;
        cfg.simple.banned_prompt = "禁止出现:笨蛋".to_string();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: PromptInjectConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.simple.banned_prompt, "禁止出现:笨蛋");
        // 旧配置缺 banned_prompt 字段 → 默认空串,不报错
        let old = r#"{"mode":"simple","simple":{"word_count_enabled":true}}"#;
        let old_cfg: PromptInjectConfig = serde_json::from_str(old).unwrap();
        assert!(old_cfg.simple.banned_prompt.is_empty());
    }

    #[test]
    fn floors_sorted_by_order_and_filtered_by_enabled() {
        let cfg = PromptInjectConfig {
            mode: InjectMode::Complex,
            floors: vec![
                PromptFloor {
                    id: "b".into(),
                    name: "B".into(),
                    content: "".into(),
                    role: FloorRole::System,
                    position: FloorPosition::System,
                    depth: 0,
                    enabled: true,
                    order: 2,
                },
                PromptFloor {
                    id: "a".into(),
                    name: "A".into(),
                    content: "".into(),
                    role: FloorRole::System,
                    position: FloorPosition::System,
                    depth: 0,
                    enabled: true,
                    order: 1,
                },
                PromptFloor {
                    id: "c".into(),
                    name: "C".into(),
                    content: "".into(),
                    role: FloorRole::System,
                    position: FloorPosition::System,
                    depth: 0,
                    enabled: false,
                    order: 0,
                },
            ],
            ..PromptInjectConfig::default()
        };
        let sorted = cfg.enabled_floors_sorted();
        let names: Vec<&str> = sorted.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["A", "B"]);
    }

    #[test]
    fn simple_mode_returns_no_floors() {
        let cfg = PromptInjectConfig {
            mode: InjectMode::Simple,
            floors: vec![PromptFloor {
                id: "a".into(),
                name: "A".into(),
                content: "x".into(),
                role: FloorRole::System,
                position: FloorPosition::System,
                depth: 0,
                enabled: true,
                order: 0,
            }],
            ..PromptInjectConfig::default()
        };
        assert!(cfg.enabled_floors_sorted().is_empty());
    }

    #[test]
    fn load_save_roundtrip() {
        let dir = std::env::temp_dir().join(format!("kedai-pi-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let cfg = PromptInjectConfig {
            mode: InjectMode::Complex,
            floors: vec![PromptFloor {
                id: "f1".into(),
                name: "测试楼层".into(),
                content: "内容 {{char}}".into(),
                role: FloorRole::User,
                position: FloorPosition::Depth,
                depth: 2,
                enabled: true,
                order: 0,
            }],
            ..PromptInjectConfig::default()
        };
        cfg.save(&dir).unwrap();
        let loaded = PromptInjectConfig::load(&dir);
        assert_eq!(loaded.mode, InjectMode::Complex);
        assert_eq!(loaded.floors.len(), 1);
        assert_eq!(loaded.floors[0].role, FloorRole::User);
        assert_eq!(loaded.floors[0].position, FloorPosition::Depth);
        assert_eq!(loaded.floors[0].depth, 2);
        assert_eq!(loaded.floors[0].content, "内容 {{char}}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_file_returns_default() {
        let dir = std::env::temp_dir().join(format!("kedai-pi-missing-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let cfg = PromptInjectConfig::load(&dir);
        assert_eq!(cfg.mode, InjectMode::Simple);
        assert!(cfg.floors.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_corrupted_file_falls_back_to_default() {
        let dir = std::env::temp_dir().join(format!("kedai-pi-bad-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("prompt_floors.json"), "{{{ not json").unwrap();
        let cfg = PromptInjectConfig::load(&dir);
        assert_eq!(cfg.mode, InjectMode::Simple);
        assert!(!cfg.simple.word_count_enabled);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- output_budget_for_word_count 边界 ----
    #[test]
    fn budget_zero_word_count_keeps_requested() {
        assert_eq!(output_budget_for_word_count(0, 1024), 1024);
        assert_eq!(output_budget_for_word_count(0, 0), 0);
    }

    #[test]
    fn budget_small_word_count_lifts_requested() {
        // 1200 字 → 1200*2+512 = 2912 > 1024
        assert_eq!(output_budget_for_word_count(1200, 1024), 2912);
        assert_eq!(output_budget_for_word_count(200, 1024), 1024);
    }

    #[test]
    fn budget_large_word_count_capped_at_8192() {
        assert_eq!(output_budget_for_word_count(10000, 1024), 8192);
        assert_eq!(output_budget_for_word_count(u32::MAX, 0), 8192);
    }

    #[test]
    fn budget_respects_larger_requested() {
        assert_eq!(output_budget_for_word_count(1200, 4096), 4096);
        assert_eq!(output_budget_for_word_count(10000, 16384), 16384);
    }
}
