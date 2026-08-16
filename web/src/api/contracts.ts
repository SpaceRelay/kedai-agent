// 契约编辑 API(P6 面板):角色卡内嵌契约(extensions.nlkaleido)读写
import { request } from './client';

/** GET 响应:契约原样 JSON + 服务端校验状态 */
export interface CharacterContractStatus {
  contract: Record<string, unknown> | null;
  /** 服务端 parse_contract 是否通过(已存但解析失败的存量契约显示警示) */
  valid: boolean;
  /** character_card=卡内嵌 / none=无内嵌 */
  source: 'character_card' | 'none';
}

export async function getCharacterContract(characterId: string): Promise<CharacterContractStatus> {
  return request<CharacterContractStatus>(`/characters/${encodeURIComponent(characterId)}/contract`);
}

/** 写入/回滚的降级警告:契约本体已落库,仅变更历史写入失败时服务端返回 */
export interface ContractMutationResult {
  ok: boolean;
  /** 非空表示部分成功(契约已更新,但 changelog 留痕失败),应提示用户 */
  warning?: string | null;
}

export async function putCharacterContract(
  characterId: string,
  contract: Record<string, unknown>,
): Promise<{ ok: boolean; id: string; version: number } & ContractMutationResult> {
  return request(`/characters/${encodeURIComponent(characterId)}/contract`, {
    method: 'PUT',
    body: JSON.stringify(contract),
  });
}

/** 移除成功返回 200 带 {ok, warning};无契约时的幂等短路返回 204(undefined) */
export async function deleteCharacterContract(
  characterId: string,
): Promise<ContractMutationResult | undefined> {
  return request(`/characters/${encodeURIComponent(characterId)}/contract`, { method: 'DELETE' });
}

/** 契约变更历史中的一条记录(后端 Changelog,列表倒序:最新在前) */
export interface ContractChangeEntry {
  seq: number;
  /** 变更来源(后端 ChangelogSource snake_case) */
  source: 'agent' | 'manual' | 'contract_init' | 'rollback' | 'import' | 'repair';
  op: 'replace' | 'remove';
  path: string;
  before: Record<string, unknown> | null;
  after: Record<string, unknown> | null;
  rationale: string | null;
  createdAt: string;
}

export interface ContractHistoryResponse {
  entries: ContractChangeEntry[];
}

/** GET /characters/{id}/contract/history:拉取契约变更历史(默认最近 50 条) */
export async function getContractHistory(characterId: string, limit = 50): Promise<ContractHistoryResponse> {
  return request<ContractHistoryResponse>(
    `/characters/${encodeURIComponent(characterId)}/contract/history?limit=${limit}`,
  );
}

/** POST /characters/{id}/contract/rollback:把指定 seq 记录的 after 快照恢复为当前契约 */
export async function rollbackContract(
  characterId: string,
  seq: number,
): Promise<{ ok: boolean; id: string; seq: number | null; version: number } & ContractMutationResult> {
  return request(`/characters/${encodeURIComponent(characterId)}/contract/rollback`, {
    method: 'POST',
    body: JSON.stringify({ seq }),
  });
}
