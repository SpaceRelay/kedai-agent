// 角色卡 API
import { BASE, authorizedFetch, request } from './client';
import type { CharacterRecord } from './types';

export async function listCharacters(): Promise<CharacterRecord[]> {
  const data = await request<{ characters: CharacterRecord[] }>('/characters');
  return data.characters;
}

export async function getCharacter(id: string): Promise<CharacterRecord> {
  return request(`/characters/${id}`);
}

export async function uploadCharacter(file: File): Promise<CharacterRecord> {
  const form = new FormData();
  form.append('file', file);
  const res = await authorizedFetch(`${BASE}/characters/upload`, { method: 'POST', body: form }, false);
  if (!res.ok) {
    const body = (await res.json().catch(() => ({}))) as { error?: string };
    throw new Error(body.error ?? '上传失败');
  }
  return (await res.json()) as CharacterRecord;
}

export async function updateCharacter(
  id: string,
  patch: { chara_name?: string; description?: string; first_mes?: string; alternate_greetings?: string[] },
): Promise<CharacterRecord> {
  return request(`/characters/${id}`, { method: 'PUT', body: JSON.stringify(patch) });
}

export async function deleteCharacter(id: string): Promise<void> {
  await request<void>(`/characters/${id}`, { method: 'DELETE' });
}
