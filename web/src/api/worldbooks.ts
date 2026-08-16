// 世界书 API
import { BASE, authorizedFetch, request } from './client';
import type { WorldBookEntry, WorldBookRecord } from './types';

export async function listWorldBooks(): Promise<WorldBookRecord[]> {
  const data = await request<{ world_books: WorldBookRecord[] }>('/world-books');
  return data.world_books;
}

/** 上传独立世界书(JSON);characterId 可选绑定到角色 */
export async function uploadWorldBook(file: File, characterId?: string): Promise<WorldBookRecord> {
  const form = new FormData();
  form.append('file', file);
  if (characterId) form.append('character_id', characterId);
  const res = await authorizedFetch(`${BASE}/world-books/upload`, { method: 'POST', body: form }, false);
  if (!res.ok) {
    const body = (await res.json().catch(() => ({}))) as { error?: string };
    throw new Error(body.error ?? '上传失败');
  }
  return (await res.json()) as WorldBookRecord;
}

/** 更新世界书:enabled / character_id(空串=转全局)/ name */
export async function updateWorldBook(
  id: string,
  patch: { enabled?: boolean; character_id?: string; name?: string },
): Promise<WorldBookRecord> {
  return request(`/world-books/${id}`, { method: 'PUT', body: JSON.stringify(patch) });
}

export async function deleteWorldBook(id: string): Promise<void> {
  await request<void>(`/world-books/${id}`, { method: 'DELETE' });
}

/** 条目预览 */
export async function getWorldBookEntries(id: string): Promise<WorldBookEntry[]> {
  const data = await request<{ id: string; entries: WorldBookEntry[] }>(`/world-books/${id}/entries`);
  return data.entries;
}

/** 全量回写独立世界书条目(关键词/正则/常驻/激活/位置) */
export async function saveWorldBookEntries(id: string, entries: WorldBookEntry[]): Promise<WorldBookEntry[]> {
  const data = await request<{ ok: boolean; entries: WorldBookEntry[] }>(`/world-books/${id}/entries`, {
    method: 'PUT',
    body: JSON.stringify({ entries }),
  });
  return data.entries;
}

/** 新增一条独立世界书条目(返回新条目,空内容需编辑后生效) */
export async function addWorldBookEntry(id: string): Promise<WorldBookEntry> {
  const data = await request<{ ok: boolean; entry: WorldBookEntry }>(`/world-books/${id}/entries`, { method: 'POST' });
  return data.entry;
}

/** 当前角色卡内嵌世界书条目(character_book) */
export async function getCharacterWorldEntries(characterId: string): Promise<WorldBookEntry[]> {
  const data = await request<{ character_id: string; entries: WorldBookEntry[] }>(`/characters/${characterId}/world-entries`);
  return data.entries;
}

/** 回写角色卡内嵌世界书条目 */
export async function saveCharacterWorldEntries(characterId: string, entries: WorldBookEntry[]): Promise<WorldBookEntry[]> {
  const data = await request<{ ok: boolean; entries: WorldBookEntry[] }>(`/characters/${characterId}/world-entries`, {
    method: 'PUT',
    body: JSON.stringify({ entries }),
  });
  return data.entries;
}

/** 自检:世界书「属性自动分配机制」是否可用(常驻→system、激发→user) */
export interface AutoAssignCheckItem {
  name: string;
  label: string;
  status: 'ok' | 'error';
  detail: string;
}
export interface AutoAssignCheckResult {
  ok: boolean;
  checks: AutoAssignCheckItem[];
  summary: string;
}

export async function checkWorldBookAutoAssign(characterId?: string): Promise<AutoAssignCheckResult> {
  const qs = characterId ? `?character_id=${encodeURIComponent(characterId)}` : '';
  return request<AutoAssignCheckResult>(`/world-books/auto-assign-check${qs}`);
}
