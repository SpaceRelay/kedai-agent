// 命令执行审计与执行器状态 API(阶段 E)。
// 后端端点见 server-rs/src/api/exec.rs(tier / audit 列表 / 清空)。
import { request } from './client';

/** 执行器等级(与 Rust ShellTier serde snake_case 对齐) */
export type ShellTier = 'root' | 'shizuku' | 'sandbox' | 'disabled';

/** 命令风险级别(与 Rust CommandRisk serde snake_case 对齐) */
export type CommandRiskLevel = 'safe' | 'sensitive' | 'destructive' | 'admin';

/** GET /api/exec/tier:当前可用执行器等级 */
export async function getExecTier(): Promise<{ tier: ShellTier; label: string }> {
  return request<{ tier: ShellTier; label: string }>('/exec/tier');
}

/** 审计行(与 Rust services::exec::audit::ExecAuditEntry 对齐) */
export interface ExecAuditEntry {
  id: number;
  ts: string;
  source: string;
  task_id: string | null;
  session_id: string | null;
  command: string;
  shell: string;
  tier: string;
  risk: string;
  decision: string;
  exit_code: number | null;
  stdout_summary: string;
  stderr_summary: string;
}

/** GET /api/exec/audit:审计列表(时间倒序,limit 上限 500) */
export async function listExecAudit(
  opts: { limit?: number; source?: string; risk?: string } = {},
): Promise<ExecAuditEntry[]> {
  const params = new URLSearchParams();
  if (opts.limit) params.set('limit', String(opts.limit));
  if (opts.source) params.set('source', opts.source);
  if (opts.risk) params.set('risk', opts.risk);
  const qs = params.toString();
  const data = await request<{ entries: ExecAuditEntry[] }>(
    `/exec/audit${qs ? `?${qs}` : ''}`,
  );
  return data.entries;
}

/** DELETE /api/exec/audit:清空审计 */
export async function clearExecAudit(): Promise<number> {
  const data = await request<{ ok: boolean; deleted: number }>('/exec/audit', {
    method: 'DELETE',
  });
  return data.deleted;
}
