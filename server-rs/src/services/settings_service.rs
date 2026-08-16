// 运行期设置服务:API 连接(Base URL / Key / 模型)与生成参数(temperature/top_p/max_tokens/上下文窗口)
// 持久化到 data/settings.json;存在则优先于 .env,支持前端直接编辑(PUT /api/settings)。
//
// 凭据安全:openai_api_key 字段在内存中始终是明文(供连接器直接使用),但落盘前经
// secret_store::protect 加密(Windows DPAPI,绑定当前用户),load 时解密。旧版明文
// settings.json 可直接读取,并在下次保存时自动升级为密文。
use crate::config::AppConfig;
use crate::services::secret_store;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeSettings {
    pub openai_base_url: String,
    /// API Key:内存中为明文;持久化时由 save() 加密、load() 解密(见 secret_store)
    pub openai_api_key: String,
    pub model: String,
    pub default_temperature: f64,
    pub default_top_p: f64,
    pub default_max_tokens: u32,
    /// 上下文窗口上限(token):历史超出后按时间裁剪
    pub max_context_tokens: u32,
    /// Agent 系统提示词(空 = 使用内置默认模板;支持 {{character_name}} {{character_description}} {{world_info}} 占位符)
    #[serde(default)]
    pub agent_system_prompt: String,
    /// 联网搜索端点(search 工具;默认 DuckDuckGo HTML 接口,可换成自建 SearXNG 等)
    #[serde(default)]
    pub search_endpoint: String,
    /// mvu 变量状态注入位置:system(默认,世界书并入 system 提示词)/
    /// user_tail(追加到最新用户消息尾部,system+早期历史前缀稳定,前缀缓存友好)
    #[serde(default)]
    pub mvu_vars_position: String,
    /// 变量两步生成独立温度档(默认 None = 沿用内置 0.3;范围 0..=2.0)。
    /// 与正文 default_temperature 解耦,允许变量调用单独降温度以提高结构化遵循度(G3)。
    #[serde(default)]
    pub mvu_temperature: Option<f64>,
    /// 变量两步生成独立模型(默认 None = 与正文共用同一连接器/模型)。
    /// P2 仅预留字段,暂不接双模型(文档 D-1:先独立温度档,独立模型按需)。
    #[serde(default)]
    pub mvu_model: Option<String>,
    /// 反思提示词(空 = 使用机械规则检查;非空 = 反思步骤改为调用 LLM 按本提示词判定)
    #[serde(default)]
    pub reflect_prompt: String,
    /// 复杂模式预设尾部提示词(空 = 禁用):拼入最新用户消息尾部,位于位置1 世界书激发之后
    #[serde(default)]
    pub preset_tail_prompt: String,
    /// 预设尾部提示词注入角色(user / assistant;system 钳制为 user)
    #[serde(default = "default_user_role")]
    pub preset_tail_role: String,
    /// 反思失败建议的可选补充说明(空 = 仅自动建议):主体建议由引擎自动生成(≤200 token),
    /// 反思未通过放弃重试时注入位置0(位置1 激发之后、预设尾部之前);本字段附加在自动建议之后
    #[serde(default)]
    pub reflect_advice_prompt: String,
    /// 反思失败建议注入角色(user / assistant;system 钳制为 user)
    #[serde(default = "default_user_role")]
    pub reflect_advice_role: String,
    /// 放行模式:true = 除黑名单工具外自动放行,不弹授权框
    #[serde(default)]
    pub bypass_mode: bool,
    /// 放行模式黑名单(即使放行模式开启,这些工具仍需要授权)
    #[serde(default = "default_bypass_blacklist")]
    pub bypass_blacklist: Vec<String>,
    /// AGENT/CUSTOM 模式工具循环轮次上限(默认 32;每轮可执行多个工具调用,
    /// 达到上限后停止调用工具并输出当前结果)
    #[serde(default = "default_max_tool_rounds")]
    pub max_tool_rounds: u32,
    /// HTML 渲染开关(状态栏脚本执行前置条件):true = 开启(需用户主动授权脚本后再开启)
    #[serde(default)]
    pub render_html: bool,
    /// 上下文压缩模式:off(默认,不压缩)/ manual(手动触发)/ auto(token 超阈值自动压缩)
    #[serde(default = "default_compaction_mode")]
    pub compaction_mode: String,
    /// 上下文压缩触发阈值(0.5..=0.95,默认 0.8):auto 模式下历史 token 占比达到该值即压缩
    #[serde(default = "default_compaction_threshold")]
    pub compaction_threshold: f32,
    /// LLM 请求快照开关(第四点·主题 A):true = 每次下发前把完整消息数组落 llm_requests 表
    /// (含 system/注入/工具消息,可回放调试;默认关闭避免占用磁盘)
    #[serde(default)]
    pub llm_request_log: bool,
}

/// 默认工具循环轮次上限
fn default_max_tool_rounds() -> u32 {
    32
}

/// 默认放行模式黑名单
fn default_bypass_blacklist() -> Vec<String> {
    vec![
        "delete_file".to_string(),
        "format_disk".to_string(),
        "modify_system".to_string(),
        "registry_write".to_string(),
    ]
}

/// 默认尾部注入角色(user;旧配置缺省保持 user 行为)
fn default_user_role() -> String {
    "user".to_string()
}

/// 默认上下文压缩模式(off = 不压缩,保持既有行为)
fn default_compaction_mode() -> String {
    "off".to_string()
}

/// 默认上下文压缩触发阈值(0.8 = 历史 token 达上下文窗口 80% 时自动压缩)
fn default_compaction_threshold() -> f32 {
    0.8
}

/// 默认搜索端点(DuckDuckGo HTML 免费接口,无需 API Key)
pub const DEFAULT_SEARCH_ENDPOINT: &str = "https://html.duckduckgo.com/html/";

impl RuntimeSettings {
    /// 从环境配置构建默认设置
    pub fn from_config(cfg: &AppConfig) -> Self {
        RuntimeSettings {
            openai_base_url: cfg.openai_base_url.clone(),
            openai_api_key: cfg.openai_api_key.clone(),
            model: cfg.openai_model.clone(),
            default_temperature: cfg.default_temperature,
            default_top_p: cfg.default_top_p,
            default_max_tokens: cfg.default_max_tokens,
            max_context_tokens: cfg.default_max_context_tokens,
            agent_system_prompt: String::new(),
            search_endpoint: DEFAULT_SEARCH_ENDPOINT.to_string(),
            mvu_vars_position: "system".to_string(),
            mvu_temperature: None,
            mvu_model: None,
            reflect_prompt: String::new(),
            preset_tail_prompt: String::new(),
            preset_tail_role: "user".to_string(),
            reflect_advice_prompt: String::new(),
            reflect_advice_role: "user".to_string(),
            bypass_mode: false,
            bypass_blacklist: default_bypass_blacklist(),
            max_tool_rounds: default_max_tool_rounds(),
            render_html: false,
            compaction_mode: default_compaction_mode(),
            compaction_threshold: default_compaction_threshold(),
            llm_request_log: false,
        }
    }

    /// 从 data/settings.json 加载;缺失或损坏则回退环境配置。
    /// API Key 解密为明文(旧版无前缀明文原样读取);解密失败视为未配置,需在设置页重填。
    pub fn load(data_dir: &Path, cfg: &AppConfig) -> Self {
        let path = data_dir.join("settings.json");
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(mut s) = serde_json::from_str::<RuntimeSettings>(&text) {
                // 旧版 settings.json 无 search_endpoint:回退默认搜索端点
                if s.search_endpoint.trim().is_empty() {
                    s.search_endpoint = DEFAULT_SEARCH_ENDPOINT.to_string();
                }
                // 旧版 settings.json 无 mvu_vars_position:回退 system
                if s.mvu_vars_position.trim().is_empty() {
                    s.mvu_vars_position = "system".to_string();
                }
                // 变量独立温度钳制到 0..=2.0;非法值(旧配置/越界)回退 None(沿用内置默认)
                if let Some(t) = s.mvu_temperature {
                    if !(0.0..=2.0).contains(&t) {
                        s.mvu_temperature = None;
                    }
                }
                // 上下文窗口下限对齐:低于 64K(旧默认 8192 等)钳制到 64K,保证与
                // 前端滑块最小位一致;上限 1M 由 API 校验兜底
                if s.max_context_tokens < 65_536 {
                    s.max_context_tokens = 65_536;
                }
                // 压缩模式仅接受 off / manual / auto,旧配置或非法值回退 off
                if !matches!(s.compaction_mode.as_str(), "off" | "manual" | "auto") {
                    s.compaction_mode = default_compaction_mode();
                }
                // 压缩阈值钳制到 0.5..=0.95,旧配置缺省已由 serde default 填 0.8
                if !(0.5..=0.95).contains(&s.compaction_threshold) {
                    s.compaction_threshold = default_compaction_threshold();
                }
                let was_plaintext =
                    !s.openai_api_key.is_empty() && !secret_store::is_protected(&s.openai_api_key);
                s.openai_api_key = secret_store::unprotect(&s.openai_api_key);
                // 旧版明文配置:立即重写为密文(一次性迁移,失败仅告警不影响启动)
                if was_plaintext && !s.openai_api_key.is_empty() {
                    if let Err(e) = s.save(data_dir) {
                        eprintln!("[settings] API Key 加密迁移写回失败(下次保存设置时重试):{e}");
                    } else {
                        eprintln!("[settings] 已将 settings.json 中的明文 API Key 迁移为加密存储");
                    }
                }
                return s;
            }
        }
        Self::from_config(cfg)
    }

    /// 持久化到 data/settings.json;API Key 加密后写入,不落明文。
    /// 先写同目录临时文件再替换,避免进程中断留下半截 JSON。
    pub fn save(&self, data_dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(data_dir).map_err(|e| e.to_string())?;
        // 仅持久化副本加密,不改动内存中的明文 Key(连接器仍需直接使用)
        let mut persisted = self.clone();
        persisted.openai_api_key = secret_store::protect(&self.openai_api_key)?;
        let text = serde_json::to_string_pretty(&persisted).map_err(|e| e.to_string())?;
        let target = data_dir.join("settings.json");
        let temporary = data_dir.join(format!("settings.json.tmp-{}", uuid::Uuid::new_v4()));
        std::fs::write(&temporary, text).map_err(|e| e.to_string())?;
        if target.exists() {
            std::fs::remove_file(&target).map_err(|e| {
                let _ = std::fs::remove_file(&temporary);
                e.to_string()
            })?;
        }
        std::fs::rename(&temporary, &target).map_err(|e| {
            let _ = std::fs::remove_file(&temporary);
            e.to_string()
        })
    }

    /// API Key 脱敏展示(仅保留后 4 位)
    pub fn masked_api_key(&self) -> String {
        mask_key(&self.openai_api_key)
    }
}

/// 脱敏:非空时返回 `****xxxx`(保留后 4 位)
pub fn mask_key(key: &str) -> String {
    if key.is_empty() {
        return String::new();
    }
    let last = key.chars().rev().take(4).collect::<Vec<_>>();
    let last: String = last.into_iter().rev().collect();
    format!("****{last}")
}

/// 规范化 OpenAI 兼容 API 地址(自动补全格式):
/// - 去空白与尾部斜杠
/// - 无协议时补协议:本机地址(localhost/127.x/0.0.0.0/[::1])补 http://,其余补 https://
/// - 无路径时补 `/v1`(OpenAI 兼容服务普遍要求 /v1,缺它请求 /models 会 404)
pub fn normalize_base_url(input: &str) -> String {
    let mut s = input.trim().to_string();
    if s.is_empty() {
        return s;
    }
    // 去尾部斜杠
    while s.ends_with('/') {
        s.pop();
    }
    // 补协议
    if !s.starts_with("http://") && !s.starts_with("https://") {
        let lower = s.to_lowercase();
        let is_local = lower.starts_with("localhost")
            || lower.starts_with("127.")
            || lower.starts_with("0.0.0.0")
            || lower.starts_with("[::1]");
        s = if is_local {
            format!("http://{s}")
        } else {
            format!("https://{s}")
        };
    }
    // 补 /v1(仅当 host 后无任何路径时)
    let (_, rest) = s.split_once("://").unwrap_or(("", s.as_str()));
    let has_path = match rest.find('/') {
        None => false,
        Some(i) => !rest[i..].trim_matches('/').is_empty(),
    };
    if !has_path {
        s.push_str("/v1");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_base_url() {
        // 无协议 → 补 https
        assert_eq!(
            normalize_base_url("api.openai.com"),
            "https://api.openai.com/v1"
        );
        // 有协议无路径 → 补 /v1
        assert_eq!(
            normalize_base_url("https://api.openai.com"),
            "https://api.openai.com/v1"
        );
        // 已带 /v1 → 保持不变(去尾部斜杠)
        assert_eq!(
            normalize_base_url("https://api.deepseek.com/v1/"),
            "https://api.deepseek.com/v1"
        );
        assert_eq!(
            normalize_base_url("https://api.deepseek.com/v1"),
            "https://api.deepseek.com/v1"
        );
        // 已有其他路径 → 不补 /v1
        assert_eq!(
            normalize_base_url("https://openrouter.ai/api/v1"),
            "https://openrouter.ai/api/v1"
        );
        // 本机地址 → 补 http + /v1
        assert_eq!(
            normalize_base_url("localhost:11434"),
            "http://localhost:11434/v1"
        );
        assert_eq!(
            normalize_base_url("http://localhost:11434"),
            "http://localhost:11434/v1"
        );
        assert_eq!(
            normalize_base_url("127.0.0.1:1234"),
            "http://127.0.0.1:1234/v1"
        );
        // 空输入
        assert_eq!(normalize_base_url("   "), "");
    }

    /// 临时数据目录(测试隔离用)
    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "kedai-settings-test-{tag}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 最小 AppConfig(仅供设置加载/保存测试;不读环境变量,避免受本机 .env 影响)
    fn test_cfg() -> AppConfig {
        AppConfig {
            host: "127.0.0.1".into(),
            port: 0,
            data_dir: std::env::temp_dir(),
            log_dir: std::env::temp_dir(),
            web_dist: None,
            connector: "mock".into(),
            openai_base_url: "https://example.com/v1".into(),
            openai_api_key: String::new(),
            openai_model: "test-model".into(),
            default_temperature: 0.8,
            default_top_p: 0.9,
            default_max_tokens: 1024,
            default_max_context_tokens: 65_536,
            log_level: "info".into(),
            api_token: "test-token".into(),
            auth_required: false,
            api_token_injected: true,
            allow_remote: false,
            bootstrap_enabled: true,
            strict_client_header: false,
        }
    }

    /// API Key 落盘应为密文(文件不含明文),load 后还原为明文
    #[test]
    fn api_key_is_encrypted_on_disk_and_restored_on_load() {
        let dir = tmp_dir("enc");
        let mut s = RuntimeSettings::from_config(&test_cfg());
        s.openai_api_key = "sk-secret-value-9999".to_string();
        s.save(&dir).unwrap();

        let raw = std::fs::read_to_string(dir.join("settings.json")).unwrap();
        assert!(
            !raw.contains("sk-secret-value-9999"),
            "settings.json 不应包含明文 API Key"
        );

        let loaded = RuntimeSettings::load(&dir, &test_cfg());
        assert_eq!(
            loaded.openai_api_key, "sk-secret-value-9999",
            "load 应还原明文"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 旧版明文 settings.json:可正常读取,且 load 时自动就地迁移为密文
    #[test]
    fn legacy_plaintext_settings_are_migrated_on_load() {
        let dir = tmp_dir("migrate");
        let mut s = RuntimeSettings::from_config(&test_cfg());
        s.openai_api_key = "sk-legacy-plain-1234".to_string();
        // 绕过 save 的加密,直接写旧版明文文件
        std::fs::write(
            dir.join("settings.json"),
            serde_json::to_string_pretty(&s).unwrap(),
        )
        .unwrap();

        let loaded = RuntimeSettings::load(&dir, &test_cfg());
        assert_eq!(
            loaded.openai_api_key, "sk-legacy-plain-1234",
            "旧版明文应能正常读取"
        );
        let raw = std::fs::read_to_string(dir.join("settings.json")).unwrap();
        assert!(
            !raw.contains("sk-legacy-plain-1234"),
            "load 后应已就地迁移为密文: {raw}"
        );
        // 迁移后再次 load 仍应得到同一明文
        let again = RuntimeSettings::load(&dir, &test_cfg());
        assert_eq!(again.openai_api_key, "sk-legacy-plain-1234");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 未配置 Key:落盘保持空串,不产生密文噪声
    #[test]
    fn empty_api_key_stays_empty() {
        let dir = tmp_dir("empty");
        let mut s = RuntimeSettings::from_config(&test_cfg());
        s.openai_api_key = String::new();
        s.save(&dir).unwrap();
        let loaded = RuntimeSettings::load(&dir, &test_cfg());
        assert!(loaded.openai_api_key.is_empty(), "空 Key 应保持空");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 旧版 settings.json 缺少 render_html 时应兼容加载并默认关闭。
    #[test]
    fn legacy_settings_without_render_html_default_to_false() {
        let dir = tmp_dir("legacy-render-html");
        let s = RuntimeSettings::from_config(&test_cfg());
        let mut json = serde_json::to_value(&s).unwrap();
        json.as_object_mut().unwrap().remove("render_html");
        std::fs::write(
            dir.join("settings.json"),
            serde_json::to_string_pretty(&json).unwrap(),
        )
        .unwrap();

        let loaded = RuntimeSettings::load(&dir, &test_cfg());
        assert!(!loaded.render_html, "旧配置缺省时 HTML 渲染必须关闭");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
