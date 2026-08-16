// 技能库(提示词技能)API
import { request } from './client';
import type { SkillImportItem, SkillRecord } from './types';

/** 列出全部技能(含已停用) */
export async function listSkills(): Promise<SkillRecord[]> {
  const data = await request<{ skills: SkillRecord[] }>('/skills');
  return data.skills;
}

/** 导入技能:单对象 / 数组均可 */
export async function importSkills(items: SkillImportItem[]): Promise<{ ok: boolean; imported: number }> {
  const body = items.length === 1 ? items[0] : items;
  return request('/skills', { method: 'POST', body: JSON.stringify(body) });
}

/** 更新技能(启用/停用、改名、改内容,仅提供字段生效) */
export async function updateSkill(
  id: string,
  patch: { enabled?: boolean; name?: string; description?: string; content?: string },
): Promise<{ ok: boolean; skill: SkillRecord }> {
  return request(`/skills/${id}`, { method: 'PUT', body: JSON.stringify(patch) });
}

/** 删除技能 */
export async function deleteSkill(id: string): Promise<{ ok: boolean }> {
  return request(`/skills/${id}`, { method: 'DELETE' });
}
