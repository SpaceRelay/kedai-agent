// 角色卡 API
import { request } from './client';
import { uploadForm } from './stream';
import type { CharacterRecord } from './types';

export async function listCharacters(): Promise<CharacterRecord[]> {
  const data = await request<{ characters: CharacterRecord[] }>('/characters');
  return data.characters;
}

export async function getCharacter(id: string): Promise<CharacterRecord> {
  return request(`/characters/${id}`);
}

export async function uploadCharacter(file: File): Promise<CharacterRecord> {
  return uploadForm<CharacterRecord>('/characters/upload', file);
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
