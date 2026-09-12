// 契约引擎统一写入出口(P7 方案 A):多步工具 apply_patch 与 HTTP
// POST /api/variable/update 共用同一门控/留痕/pending 维护管线,
// 存储单库无分叉——外部调用与 Agent 主路径行为完全一致。
//
// 锁纪律:Db 单连接 Mutex,本服务串行调 SessionService/KaleidoStateService,
// 各自短持锁,无事务内跨 service 调用(与 multistep 执行器同款)。
use crate::contracts::changelog::ChangelogEntry;
use crate::contracts::{ContractRegistry, PatchOp, RejectedOp};
use crate::services::kaleido_state_service::KaleidoStateService;
use crate::services::session_service::SessionService;
use serde_json::Value;
use std::sync::Arc;

/// 统一写入出口:门控 → 应用 → 留痕 → pending 维护。
pub struct VariableApplyService {
    sessions: Arc<SessionService>,
    contracts: Arc<ContractRegistry>,
    kaleido: Arc<KaleidoStateService>,
}

/// 应用结果:工具层与 HTTP 层各自序列化(LLM 文本 / JSON)。
pub struct VariableApplyOutcome {
    /// 是否至少一条 op 生效(全部被拦时 false,树保持原样)
    pub ok: bool,
    /// 应用后的完整变量树(未生效时为当前快照)
    pub tree: Value,
    /// 人类可读警告:逐条拒绝原因(path(原因))、低置信条数
    pub warnings: Vec<String>,
    /// 本轮 changelog 留痕(契约生效才有;留痕失败降级时为空)
    pub entries: Vec<ChangelogEntry>,
    /// 被拒 op(带 unknown_field/not_owner 原因)
    pub rejected: Vec<RejectedOp>,
    /// 低置信 op(已入 meta.pending)
    pub pending: Vec<PatchOp>,
    /// 熔断指纹(被拒/低置信 op 的 failure_hash,供多步驱动器熔断)
    pub breaker_hashes: Vec<u64>,
}

/// 从 patch 对象数组提取置信度声明:AI 可在单条 patch 里声明
/// confidence("low"/"medium"/"high",缺省视为确信);解析层协议无该字段,
/// 按归一化点分路径构建覆盖映射。非法值(大小写/拼写)按 Low 处理:
/// LLM/外部输入不可信,宁可错杀进 pending,不可静默升级为 High 绕过门控。
/// path 缺失时与解析层同口径用 from 兜底(move 无显式 path 的形态)。
pub fn extract_confidence_overrides(
    items: &[Value],
) -> std::collections::BTreeMap<String, crate::contracts::Confidence> {
    items
        .iter()
        .filter_map(|it| {
            let p = it
                .get("path")
                .or_else(|| it.get("from"))
                .and_then(Value::as_str)?;
            let cf = serde_json::from_value::<crate::contracts::Confidence>(
                it.get("confidence")?.clone(),
            )
            .unwrap_or(crate::contracts::Confidence::Low);
            let norm = crate::parsing::assistant::split_path(p).join(".");
            Some((norm, cf))
        })
        .collect()
}

impl VariableApplyService {
    pub fn new(
        sessions: Arc<SessionService>,
        contracts: Arc<ContractRegistry>,
        kaleido: Arc<KaleidoStateService>,
    ) -> Self {
        VariableApplyService {
            sessions,
            contracts,
            kaleido,
        }
    }

    /// 应用一批补丁(统一管线)。
    ///
    /// - `items`:JSON Patch 对象数组(原始 Value,confidence 声明在此提取);
    /// - `writer`:写者子系统 id(主路径 "agent";HTTP 外部调用可指定)。
    ///
    /// 全部 op 被拦时返回 Ok(outcome.ok=false):低置信提议已入
    /// meta.pending 等待自纠,树未变;items 结构非法才返回 Err。
    /// 留痕失败仅降级告警——补丁已生效,不能因留痕失败让调用方报错。
    pub fn apply(
        &self,
        session_id: &str,
        character_id: &str,
        items: &[Value],
        writer: &str,
    ) -> Result<VariableApplyOutcome, String> {
        let raw = crate::parsing::assistant::parse_patch_array(&Value::Array(items.to_vec()))
            .ok_or_else(|| "patches 没有有效操作".to_string())?;
        let overrides = extract_confidence_overrides(items);
        // 契约门控(无契约原样放行):unknown_field/not_owner 拒绝,低置信入 pending
        let contract = self.contracts.load(character_id);
        let gated = crate::contracts::gate_assistant_patches_overrides(
            contract.as_ref(),
            &raw,
            &overrides,
            writer,
        );
        let mut warnings: Vec<String> = Vec::new();
        if !gated.rejected.is_empty() {
            // 逐条带原因:调用方需要知道哪条为何被拒才能自纠,而非盲重试到熔断
            let detail = gated
                .rejected
                .iter()
                .map(|r| format!("{}({})", r.op.path, r.reason))
                .collect::<Vec<_>>()
                .join("; ");
            warnings.push(format!("被契约拒绝: {detail}"));
        }
        if !gated.pending.is_empty() {
            warnings.push(format!("{} 条操作低置信未写入", gated.pending.len()));
        }
        let breaker_hashes = crate::contracts::rejection_hashes(&gated.rejected, &gated.pending);
        // turn_id 用消息数推算(与 get_state/引擎收尾 len-as-turn 启发式同源)
        let turn_id = self.sessions.get_messages(session_id).len() as u64;

        // 全部被拦:pending 仍须入队等待自纠/复核(自纠闭环),树保持原样
        if gated.applied.is_empty() {
            let tree = self.sessions.load_assistant_vars(session_id).tree().clone();
            if let Some(contract) = contract.as_ref() {
                if !gated.pending.is_empty() {
                    let mut meta = self
                        .kaleido
                        .load_meta(session_id)
                        .ok()
                        .flatten()
                        .map(|(_, m)| m)
                        .unwrap_or_default();
                    meta.pending = crate::contracts::merge_pending(
                        &meta.pending,
                        &gated.pending,
                        &[],
                        turn_id,
                    );
                    meta.last_turn_id = turn_id;
                    meta.last_contract_version = contract.version;
                    if let Err(e) = self.kaleido.commit_turn(
                        session_id,
                        contract.version,
                        &tree,
                        &meta,
                        &mut Vec::new(),
                    ) {
                        tracing::warn!(
                            session_id = session_id,
                            error = e.as_str(),
                            "低置信提议入队失败(未写入任何变更)"
                        );
                    }
                }
            }
            return Ok(VariableApplyOutcome {
                ok: false,
                tree,
                warnings,
                entries: Vec::new(),
                rejected: gated.rejected,
                pending: gated.pending,
                breaker_hashes,
            });
        }

        // 应用 + 持久化
        let mut vars = self.sessions.load_assistant_vars(session_id);
        let tree_before = vars.tree().clone();
        vars.apply_patches(&gated.applied)?;
        self.sessions
            .save_assistant_vars(session_id, &vars)
            .map_err(|e| format!("变量树落库失败: {e}"))?;

        // 契约生效时留痕:变更进 changelog、pending 并入 meta(消费/去重/上限)。
        // 同 path 的 op 在 validate_ops 已按 merge 规则合并,不可能同时出现在
        // applied 与 pending,故一次 merge 即可:existing 按本轮 applied_paths
        // 消费(跨轮覆盖),incoming 直接入队。
        let mut entries: Vec<ChangelogEntry> = Vec::new();
        if let Some(contract) = contract.as_ref() {
            let mut round_entries = crate::contracts::entries_from_applied(
                &tree_before,
                vars.tree(),
                turn_id,
                &gated.applied_ops,
                crate::contracts::ChangelogSource::Agent,
            );
            let applied_paths: Vec<&str> = round_entries.iter().map(|e| e.path.as_str()).collect();
            // meta 读取失败(库错误/解析失败)从默认起步,与引擎收尾同款取舍
            let mut meta = self
                .kaleido
                .load_meta(session_id)
                .ok()
                .flatten()
                .map(|(_, m)| m)
                .unwrap_or_default();
            meta.pending = crate::contracts::merge_pending(
                &meta.pending,
                &gated.pending,
                &applied_paths,
                turn_id,
            );
            meta.last_turn_id = turn_id;
            meta.last_contract_version = contract.version;
            for e in &round_entries {
                meta.confidence.insert(e.path.clone(), e.confidence);
            }
            match self.kaleido.commit_turn(
                session_id,
                contract.version,
                vars.tree(),
                &meta,
                &mut round_entries,
            ) {
                Ok(()) => entries = round_entries,
                Err(e) => tracing::warn!(
                    session_id = session_id,
                    error = e.as_str(),
                    "补丁已生效但契约留痕失败"
                ),
            }
        }
        Ok(VariableApplyOutcome {
            ok: true,
            tree: vars.tree().clone(),
            warnings,
            entries,
            rejected: gated.rejected,
            pending: gated.pending,
            breaker_hashes,
        })
    }
}
