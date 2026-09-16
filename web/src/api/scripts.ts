// 用户脚本(ScriptTree)API:global / character 两级脚本树全量读写
import { request } from './client';
import type { ScriptTree } from './types';

export type ScriptScope = 'global' | 'character';

/** 读取脚本树;character 级需传入角色 id(global 可省略) */
export async function getScriptTree(
  scope: ScriptScope,
  characterId?: string,
): Promise<ScriptTree> {
  const qs = new URLSearchParams({ scope });
  if (scope === 'character' && characterId) qs.set('character_id', characterId);
  const data = await request<{ scope: string; trees: ScriptTree }>(`/scripts/tree?${qs}`);
  return data.trees;
}

/** 全量覆盖保存脚本树;character 级需传入角色 id */
export async function saveScriptTree(
  trees: ScriptTree,
  scope: ScriptScope,
  characterId?: string,
): Promise<void> {
  const qs = new URLSearchParams({ scope });
  if (scope === 'character' && characterId) qs.set('character_id', characterId);
  await request<{ ok: boolean }>(`/scripts/tree?${qs}`, {
    method: 'PUT',
    body: JSON.stringify({ trees }),
  });
}

// ---------------- 角色卡脚本授权台账(2026-09-14,后端授权门 L12) ----------------
//
// **哈希唯一来源是后端**:`getScriptAuthorization` 返回后端实时计算的 `current_hash`,
// 前端授权时**原样回传**给 `grantScriptAuthorization`。前端**不自行计算哈希**——
// 那样会因跨语言算法/字段裁剪差异导致永久不匹配(设计选择见后端
// services/script_authorization_service.rs 的 compute_hash)。

export interface ScriptAuthorizationStatus {
  /** 当前脚本内容是否已被授权(后端执行门据此放行) */
  authorized: boolean;
  /** 后端实时计算的当前脚本哈希(授权时原样回传) */
  current_hash: string;
  /** 台账中已登记的哈希(与 current_hash 不同 = 脚本已变更、授权失效) */
  stored_hash?: string | null;
  authorized_at?: string | null;
}

/** 查询角色卡脚本的授权态 */
export async function getScriptAuthorization(
  characterId: string,
): Promise<ScriptAuthorizationStatus> {
  const qs = new URLSearchParams({ character_id: characterId });
  return request<ScriptAuthorizationStatus>(`/script-authorizations?${qs}`);
}

/** 授权角色卡脚本:hash 必须来自 getScriptAuthorization 的 current_hash */
export async function grantScriptAuthorization(
  characterId: string,
  scriptHash: string,
): Promise<void> {
  await request<{ ok: boolean }>('/script-authorizations', {
    method: 'PUT',
    body: JSON.stringify({ character_id: characterId, script_hash: scriptHash }),
  });
}

/** 撤销角色卡脚本授权 */
export async function revokeScriptAuthorization(characterId: string): Promise<void> {
  const qs = new URLSearchParams({ character_id: characterId });
  await request<{ ok: boolean }>(`/script-authorizations?${qs}`, { method: 'DELETE' });
}
