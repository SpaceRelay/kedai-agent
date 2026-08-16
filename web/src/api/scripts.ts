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
