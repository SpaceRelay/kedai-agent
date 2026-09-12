// 设置 / 连接 / 模型 / Token 与提示词注入 API
import { BASE, authorizedFetch, request } from './client';
import type {
  ConnectorInfo,
  PromptInjectConfig,
  PromptPreview,
  PromptPresetImportResult,
  RuntimeSettings,
  RuntimeSettingsPatch,
} from './types';

export async function testConnect(): Promise<{ ok: boolean; message: string; models: string[] }> {
  return request('/settings/connect', { method: 'POST', body: '{}' });
}

/** POST /api/settings/embedding/test:测试向量化连接(嵌入固定文本,回传实际维度与耗时) */
export async function testEmbedding(): Promise<{
  ok: boolean;
  dim?: number;
  latency_ms?: number;
  message: string;
}> {
  return request('/settings/embedding/test', { method: 'POST', body: '{}' });
}

export async function settingsInfo(): Promise<ConnectorInfo> {
  return request('/settings/info');
}

export async function getModel(): Promise<string> {
  const data = await request<{ model: string }>('/settings/model');
  return data.model;
}

export async function switchModel(model: string): Promise<{ model: string; changed: boolean }> {
  return request('/settings/model', { method: 'PUT', body: JSON.stringify({ model }) });
}

export async function listModels(): Promise<string[]> {
  const data = await request<{ models: string[] }>('/settings/models');
  return data.models;
}

/** GET /api/settings:读取运行期设置(API Key 脱敏);mode 指定读取哪个模式的合并值 */
export async function getSettings(mode?: 'roleplay' | 'task'): Promise<RuntimeSettings> {
  const q = mode ? `?mode=${mode}` : '';
  return request(`/settings${q}`);
}

/** POST /api/settings/refresh-models:向已保存的 API 请求可用模型列表 */
export async function refreshModels(): Promise<{ models: string[]; message: string | null }> {
  const data = await request<{ ok: boolean; models: string[]; message?: string | null }>(
    '/settings/refresh-models',
    { method: 'POST', body: '{}' },
  );
  return { models: data.models, message: data.message ?? null };
}

/** PUT /api/settings:保存运行期设置(部分字段;Base URL/Key 变更立即重建连接器);mode 指定写入哪个模式 */
export async function saveSettings(patch: RuntimeSettingsPatch, mode?: 'roleplay' | 'task'): Promise<{ ok: boolean; settings: RuntimeSettings }> {
  const q = mode ? `?mode=${mode}` : '';
  return request(`/settings${q}`, { method: 'PUT', body: JSON.stringify(patch) });
}

/** GET /api/settings/agent-prompt:读取项目级运行时主 Agent 提示词(项目根 AGENTS_RUNTIME.md,注入模型) */
export async function getAgentPromptMd(): Promise<{ path: string; content: string }> {
  return request('/settings/agent-prompt');
}

/** PUT /api/settings/agent-prompt:保存运行时主 Agent 提示词(写回项目根 AGENTS_RUNTIME.md) */
export async function saveAgentPromptMd(content: string): Promise<{ ok: boolean; path: string }> {
  return request('/settings/agent-prompt', { method: 'PUT', body: JSON.stringify({ content }) });
}

/** GET /api/settings/prompt-preview：按来源、角色、层级与顺序查看最终提示词；历史正文已脱敏。
 *  mode 指定按哪个模式的合并设置预览(task 走 for_mode 覆盖层并追加三层固定提示词)。 */
export async function getPromptPreview(sessionId?: string, characterId?: string, mode?: 'roleplay' | 'task'): Promise<PromptPreview> {
  const query = new URLSearchParams();
  if (sessionId) query.set('session_id', sessionId);
  if (characterId) query.set('character_id', characterId);
  if (mode) query.set('mode', mode);
  return request(`/settings/prompt-preview${query.size ? `?${query}` : ''}`);
}

// ===== 提示词注入(简单模式 + 楼层系统) =====

/** GET /api/prompt-inject:读取注入配置 */
export async function getPromptInject(): Promise<PromptInjectConfig> {
  const data = await request<{ ok: boolean; config: PromptInjectConfig }>('/prompt-inject');
  return data.config;
}

/** PUT /api/prompt-inject:全量保存注入配置 */
export async function savePromptInject(config: PromptInjectConfig): Promise<PromptInjectConfig> {
  const data = await request<{ ok: boolean; config: PromptInjectConfig }>('/prompt-inject', {
    method: 'PUT',
    body: JSON.stringify({ config }),
  });
  return data.config;
}

/** POST /api/prompt-inject/import:导入酒馆(SillyTavern)预设 JSON,替换现有楼层 */
export async function importPromptPreset(file: File): Promise<PromptPresetImportResult> {
  const form = new FormData();
  form.append('file', file);
  const res = await authorizedFetch(`${BASE}/prompt-inject/import`, { method: 'POST', body: form }, false);
  if (!res.ok) {
    const body = (await res.json().catch(() => ({}))) as { error?: string };
    throw new Error(body.error ?? '导入失败');
  }
  return (await res.json()) as PromptPresetImportResult;
}

// ===== Token 计数 =====

export async function countTokens(messages: Array<{ role: string; content: string }>, model?: string): Promise<number> {
  const data = await request<{ total: number }>('/token/count', {
    method: 'POST',
    body: JSON.stringify({ messages, model }),
  });
  return data.total;
}

/** GET /api/token/session-total:当前会话累计 token */
export async function getSessionTotalTokens(sessionId: string): Promise<number> {
  const data = await request<{ total_tokens: number }>(`/token/session-total?session_id=${encodeURIComponent(sessionId)}`);
  return data.total_tokens;
}

/** GET /api/token/global-total:全局累计 token */
export async function getGlobalTotalTokens(): Promise<number> {
  const data = await request<{ total_tokens: number }>('/token/global-total');
  return data.total_tokens;
}
