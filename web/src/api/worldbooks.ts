// 世界书 API
import { request } from './client';
import { requireArrayField, requireObjectField } from './shape';
import { uploadForm } from './stream';
import type { WorldBookEntry, WorldBookRecord } from './types';

export async function listWorldBooks(): Promise<WorldBookRecord[]> {
  const data = await request<unknown>('/world-books');
  // 形状闸门:resources store 直接落 worldBooks 渲染列表,解出 undefined 会让面板空白
  return requireArrayField<WorldBookRecord>(data, 'world_books', '世界书列表');
}

/** 上传独立世界书(JSON);characterId 可选绑定到角色 */
export async function uploadWorldBook(file: File, characterId?: string): Promise<WorldBookRecord> {
  return uploadForm<WorldBookRecord>(
    '/world-books/upload',
    file,
    characterId ? { character_id: characterId } : {},
  );
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
  const data = await request<unknown>(`/world-books/${id}/entries`);
  return requireArrayField<WorldBookEntry>(data, 'entries', '世界书条目');
}

/** 全量回写独立世界书条目(关键词/正则/常驻/激活/位置) */
export async function saveWorldBookEntries(id: string, entries: WorldBookEntry[]): Promise<WorldBookEntry[]> {
  const data = await request<unknown>(`/world-books/${id}/entries`, {
    method: 'PUT',
    body: JSON.stringify({ entries }),
  });
  return requireArrayField<WorldBookEntry>(data, 'entries', '世界书条目');
}

/** 新增一条独立世界书条目(返回新条目,空内容需编辑后生效) */
export async function addWorldBookEntry(id: string): Promise<WorldBookEntry> {
  const data = await request<unknown>(`/world-books/${id}/entries`, { method: 'POST' });
  return requireObjectField<WorldBookEntry>(data, 'entry', '世界书条目');
}

/** 当前角色卡内嵌世界书条目(character_book) */
export async function getCharacterWorldEntries(characterId: string): Promise<WorldBookEntry[]> {
  const data = await request<unknown>(`/characters/${characterId}/world-entries`);
  return requireArrayField<WorldBookEntry>(data, 'entries', '角色卡世界书条目');
}

/** 回写角色卡内嵌世界书条目 */
export async function saveCharacterWorldEntries(characterId: string, entries: WorldBookEntry[]): Promise<WorldBookEntry[]> {
  const data = await request<unknown>(`/characters/${characterId}/world-entries`, {
    method: 'PUT',
    body: JSON.stringify({ entries }),
  });
  return requireArrayField<WorldBookEntry>(data, 'entries', '角色卡世界书条目');
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
