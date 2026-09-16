// 密钥迁移(DPAPI):settings.json 加载/保存;API Key 落盘前 secret_store::protect 加密、
// load 时 unprotect 解密,旧版明文配置读取后就地重写为密文(一次性迁移,失败仅告警)。
use crate::config::AppConfig;
use crate::services::secret_store;
use std::path::Path;

use super::connection::DEFAULT_SEARCH_ENDPOINT;
use super::params::{
    default_compaction_keep_recent, default_compaction_mode, default_compaction_snip_bytes,
    default_compaction_threshold, default_memory_inject_char_budget, default_memory_inject_limit,
    default_memory_max_entries, default_roleplay_agent_prompt, default_subagent_max_concurrency,
    default_subagent_max_depth, default_subagent_result_max_chars, default_task_tool_policy,
    default_tool_authorization_timeout_secs, default_tool_history_budget_tokens,
    default_tool_history_keep_rounds, migrate_authorization_mode,
};
use super::{RoleplayPromptConfig, RuntimeSettings};

impl RuntimeSettings {
    /// 从 data/settings.json 加载;缺失或损坏则回退环境配置。
    /// API Key 解密为明文(旧版无前缀明文原样读取);解密失败视为未配置,需在设置页重填。
    pub fn load(data_dir: &Path, cfg: &AppConfig) -> Self {
        let path = data_dir.join("settings.json");
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(mut s) = serde_json::from_str::<RuntimeSettings>(&text) {
                // 授权模式迁移(批次授权改造):旧配置只有 bypass_mode 布尔值,没有
                // authorization_mode 键。按旧值映射:true → Bypass(旧「除黑名单外全放行」),
                // false → Strict(旧「非安全工具都需授权」最贴近且更安全)。
                // 判据用原始 JSON 是否含该键,避免覆盖新装默认(loose)与用户已写值。
                if !text.contains("\"authorization_mode\"") {
                    s.authorization_mode = migrate_authorization_mode(s.bypass_mode);
                }
                // 旧「放行模式黑名单」默认值是四个不存在的工具名,迁移为空的
                // 「始终需授权」清单;仅当内容全是旧假名时清空,保留用户自定义项。
                let legacy_fake = [
                    "delete_file",
                    "format_disk",
                    "modify_system",
                    "registry_write",
                ];
                if !s.bypass_blacklist.is_empty()
                    && s.bypass_blacklist
                        .iter()
                        .all(|n| legacy_fake.contains(&n.as_str()))
                {
                    s.bypass_blacklist.clear();
                }
                // 授权等待超时钳制 30..=1800 秒(越界回退默认,防 0 秒立即超时或长挂)
                if !(30..=1800).contains(&s.tool_authorization_timeout_secs) {
                    s.tool_authorization_timeout_secs = default_tool_authorization_timeout_secs();
                }
                // 任务工具策略仅接受三值,非法回退默认(与 API 校验同规则)
                if !matches!(
                    s.task_tool_policy.as_str(),
                    "all" | "deny_dangerous" | "allowlist"
                ) {
                    s.task_tool_policy = default_task_tool_policy();
                }
                // 旧版 settings.json 无 search_endpoint:回退默认搜索端点
                if s.search_endpoint.trim().is_empty() {
                    s.search_endpoint = DEFAULT_SEARCH_ENDPOINT.to_string();
                }
                // 旧版 settings.json 无 mvu_vars_position:回退 system
                if s.mvu_vars_position.trim().is_empty() {
                    s.mvu_vars_position = "system".to_string();
                }
                // 角色扮演 Agent 系统提示词为空时物化内置默认(对齐 search_endpoint 回退模式):
                // 空串语义 = 使用内置默认模板(设置页文案与 docs/契约-协议与配置.md 同口径),
                // 故此前安装(settings.json 已存在且该字段为空)也回退到内置默认,与首装/Android 端一致;
                // 用户已保存的非空文本优先,不会被本回填覆盖。
                if s.agent_system_prompt.0.trim().is_empty() {
                    s.agent_system_prompt = RoleplayPromptConfig(default_roleplay_agent_prompt());
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
                // 压缩保留条数钳制到 >= 2(0/1 会导致压缩后无上下文或永不触发),上限 200
                if !(2..=200).contains(&s.compaction_keep_recent) {
                    s.compaction_keep_recent = default_compaction_keep_recent();
                }
                // snip 阈值钳制到 0..=1MB(0 = 禁用 snip;负数/超大值视为异常回退默认)
                if s.compaction_snip_bytes > 1_048_576 {
                    s.compaction_snip_bytes = default_compaction_snip_bytes();
                }
                // 记忆注入上限钳制到 0..=50(0 = 关闭注入;旧配置缺省由 serde default 填 8)
                if s.memory_inject_limit > 50 {
                    s.memory_inject_limit = default_memory_inject_limit();
                }
                // 记忆槽字符预算钳制到 0..=20000(0 = 不限制;升级工作流 B2)
                if s.memory_inject_char_budget > 20_000 {
                    s.memory_inject_char_budget = default_memory_inject_char_budget();
                }
                // 每角色记忆容量上限钳制到 0..=10000(0 = 不淘汰;升级工作流 B3)
                if s.memory_max_entries > 10_000 {
                    s.memory_max_entries = default_memory_max_entries();
                }
                // embedding 向量维度钳制:0 = 未探测(首次调用回填),否则 16..=8192;
                // 越界视为异常配置回退 0,由下次测试连接重新探测
                if s.embedding_dim != 0 && !(16..=8192).contains(&s.embedding_dim) {
                    s.embedding_dim = 0;
                }
                // 落地项 3:子智能体调度参数钳制(深度 1..=4 / 并发 1..=16 / 结果 500..=8000,
                // 越界回退默认;渐进披露开关为 bool 无需钳制)
                if !(1..=4).contains(&s.subagent_max_depth) {
                    s.subagent_max_depth = default_subagent_max_depth();
                }
                if !(1..=16).contains(&s.subagent_max_concurrency) {
                    s.subagent_max_concurrency = default_subagent_max_concurrency();
                }
                if !(500..=8000).contains(&s.subagent_result_max_chars) {
                    s.subagent_result_max_chars = default_subagent_result_max_chars();
                }
                // R3b:工具历史回灌参数钳制(保留轮数 1..=32;预算 0=禁用,否则 1024..=1M,
                // 越界回退默认;旧配置缺省已由 serde default 填默认值)
                if !(1..=32).contains(&s.tool_history_keep_rounds) {
                    s.tool_history_keep_rounds = default_tool_history_keep_rounds();
                }
                if s.tool_history_budget_tokens != 0
                    && !(1024..=1_048_576).contains(&s.tool_history_budget_tokens)
                {
                    s.tool_history_budget_tokens = default_tool_history_budget_tokens();
                }
                // MCP 服务器列表(批次 6.2):settings.json 可手改,启动装配前做一次卫生清理
                // (trim 名称/命令,丢弃缺名或缺命令的不可用条目;与 PUT 校验同规则)
                s.mcp_servers.retain_mut(|srv| {
                    srv.name = srv.name.trim().to_string();
                    srv.command = srv.command.trim().to_string();
                    !srv.name.is_empty() && !srv.command.is_empty()
                });
                let was_plaintext =
                    !s.openai_api_key.is_empty() && !secret_store::is_protected(&s.openai_api_key);
                s.openai_api_key = secret_store::unprotect(&s.openai_api_key);
                // embedding Key 同策略:解密到内存;旧版明文在下方统一触发一次写回迁移
                let embedding_was_plaintext = !s.embedding_api_key.is_empty()
                    && !secret_store::is_protected(&s.embedding_api_key);
                s.embedding_api_key = secret_store::unprotect(&s.embedding_api_key);
                // 旧版明文配置:立即重写为密文(一次性迁移,失败仅告警不影响启动)
                if (was_plaintext && !s.openai_api_key.is_empty())
                    || (embedding_was_plaintext && !s.embedding_api_key.is_empty())
                {
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
    /// 统一走原子写(utils::fs_atomic:写临时文件 + 原子替换),
    /// 进程中断不会留下半截 JSON;Windows 下 ReplaceFileW 替换,消除了旧实现
    /// 「先删目标再 rename」的非原子窗口。
    ///
    /// 防「密钥静默清空」(2026-09-13 批次 2):`unprotect` 解密失败时返回空串
    /// (视为未配置,避免把密文当 Key 发上游),若直接保存就会用 `protect("")` 的
    /// 空串覆盖磁盘上的密文 —— 用户密钥从此永久丢失且无提示。故保存前读一次磁盘:
    /// 内存为空而磁盘仍是非空 `enc:v1:` 密文时,原样回写该密文(保留原值)。
    /// 该规则不会误伤「用户主动清空」:PUT /api/settings 对空值直接忽略
    /// (`api/settings.rs:300-305`),接口层面无法把已配置的 Key 由非空改为空。
    pub fn save(&self, data_dir: &Path) -> Result<(), String> {
        // 仅持久化副本加密,不改动内存中的明文 Key(连接器仍需直接使用)
        let mut persisted = self.clone();
        let on_disk = read_preserved_ciphertexts(data_dir);
        persisted.openai_api_key =
            encrypt_or_preserve(&self.openai_api_key, on_disk.openai.as_deref())?;
        persisted.embedding_api_key =
            encrypt_or_preserve(&self.embedding_api_key, on_disk.embedding.as_deref())?;
        let text = serde_json::to_string_pretty(&persisted).map_err(|e| e.to_string())?;
        crate::utils::fs_atomic::write_atomic(&data_dir.join("settings.json"), text.as_bytes())
            .map_err(|e| e.to_string())
    }
}

/// 磁盘上仍是非空密文的敏感字段(供「内存为空」时回写保真)
#[derive(Default)]
struct OnDiskCiphertexts {
    openai: Option<String>,
    embedding: Option<String>,
}

/// 读取 settings.json 中仍为 `enc:v1:` 非空密文的 Key 字段。
/// 文件缺失/损坏/字段缺失一律返回 None(按「无旧值」处理,写入新值)。
fn read_preserved_ciphertexts(data_dir: &Path) -> OnDiskCiphertexts {
    let Ok(text) = std::fs::read_to_string(data_dir.join("settings.json")) else {
        return OnDiskCiphertexts::default();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return OnDiskCiphertexts::default();
    };
    let pick = |field: &str| -> Option<String> {
        let v = json.get(field)?.as_str()?.trim();
        if v.is_empty() || !secret_store::is_protected(v) {
            return None;
        }
        Some(v.to_string())
    };
    OnDiskCiphertexts {
        openai: pick("openai_api_key"),
        embedding: pick("embedding_api_key"),
    }
}

/// 加密待落盘值;内存为空但磁盘仍有密文时保留磁盘原密文(防静默清空)。
fn encrypt_or_preserve(
    in_memory: &str,
    on_disk_ciphertext: Option<&str>,
) -> Result<String, String> {
    if in_memory.is_empty() {
        if let Some(ct) = on_disk_ciphertext {
            eprintln!(
                "[settings] API Key 在内存中为空但磁盘仍为密文(此前解密失败?),保留原密文不覆盖;请在设置页重填以恢复"
            );
            return Ok(ct.to_string());
        }
        return secret_store::protect(""); // 空串保持空串
    }
    secret_store::protect(in_memory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_config;
    use crate::utils::test_support::TempDataDir;

    /// 隔离临时数据目录(uuid 唯一 + 作用域结束自动清理)
    fn temp_dir(tag: &str) -> TempDataDir {
        TempDataDir::new(&format!("secret-preserve-{tag}"))
    }

    fn write_settings(dir: &Path, json: &str) {
        std::fs::write(dir.join("settings.json"), json.as_bytes()).unwrap();
    }

    /// 核心契约:内存 Key 为空而磁盘是密文 → 保存必须保留原密文,绝不写成空
    /// (2026-09-13 批次 2:修复「解密失败后一次保存即静默清空密钥」)
    #[test]
    fn save_preserves_ciphertext_when_memory_key_is_empty() {
        let dir = temp_dir("keep");
        write_settings(
            &dir,
            r#"{"openai_api_key":"enc:v1:KEPT-CIPHERTEXT","embedding_api_key":"enc:v1:KEPT-EMB"}"#,
        );
        let mut s = RuntimeSettings::from_config(&test_config());
        s.openai_api_key = String::new(); // 模拟解密失败后的内存态
        s.embedding_api_key = String::new();
        s.save(&dir).expect("保存应成功");

        let text = std::fs::read_to_string(dir.join("settings.json")).unwrap();
        assert!(
            text.contains("enc:v1:KEPT-CIPHERTEXT"),
            "openai_api_key 必须保留原密文:{text}"
        );
        assert!(
            text.contains("enc:v1:KEPT-EMB"),
            "embedding_api_key 必须保留原密文:{text}"
        );
    }

    /// 未配置(磁盘无密文)时保存仍写空串;新的非空 Key 正常加密覆盖
    #[cfg(windows)]
    #[test]
    fn save_writes_empty_or_new_key_normally() {
        let dir = temp_dir("normal");
        let mut s = RuntimeSettings::from_config(&test_config());
        s.openai_api_key = String::new();
        s.save(&dir).unwrap();
        let text = std::fs::read_to_string(dir.join("settings.json")).unwrap();
        assert!(
            text.contains("\"openai_api_key\": \"\""),
            "未配置应写空串:{text}"
        );

        s.openai_api_key = "sk-new-key-1234".to_string();
        s.save(&dir).unwrap();
        let text = std::fs::read_to_string(dir.join("settings.json")).unwrap();
        assert!(
            text.contains("enc:v1:") && !text.contains("sk-new-key-1234"),
            "新 Key 应加密落盘:{text}"
        );
    }

    /// 旧版明文 Key 的迁移路径不受影响:解密(直读)后重新加密
    #[cfg(windows)]
    #[test]
    fn legacy_plaintext_key_still_migrates_to_ciphertext() {
        let dir = temp_dir("legacy");
        write_settings(
            &dir,
            r#"{"openai_api_key":"sk-legacy-plain","embedding_api_key":""}"#,
        );
        let cfg = test_config();
        let mut s = RuntimeSettings::from_config(&cfg);
        s.openai_api_key = secret_store::unprotect("sk-legacy-plain");
        assert_eq!(s.openai_api_key, "sk-legacy-plain");
        s.save(&dir).unwrap();
        let text = std::fs::read_to_string(dir.join("settings.json")).unwrap();
        assert!(
            text.contains("enc:v1:") && !text.contains("sk-legacy-plain"),
            "明文应迁移为密文:{text}"
        );
    }
}
