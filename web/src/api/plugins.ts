// 插件(自定义工具)API
import { BASE, authorizedFetch, request } from './client';
import type { PluginToolsStatus } from './types';

/** 列出已注册工具插件 + 磁盘文件 */
export async function listPluginTools(): Promise<PluginToolsStatus> {
  return request<PluginToolsStatus>('/plugins/tools');
}

/** 导入工具插件 JSON 文件(multipart) */
export async function uploadPluginTool(file: File): Promise<{ ok: boolean; name: string; file: string }> {
  const form = new FormData();
  form.append('file', file);
  const res = await authorizedFetch(`${BASE}/plugins/tools/upload`, { method: 'POST', body: form }, false);
  if (!res.ok) {
    const body = (await res.json().catch(() => ({}))) as { error?: string };
    throw new Error(body.error ?? '导入失败');
  }
  return (await res.json()) as { ok: boolean; name: string; file: string };
}

/** 热重载全部工具插件 */
export async function reloadPluginTools(): Promise<{ ok: boolean; loaded: number; errors?: string[] }> {
  return request<{ ok: boolean; loaded: number; errors?: string[] }>('/plugins/tools/reload', { method: 'POST' });
}

/** 删除已导入的工具插件文件 */
export async function deletePluginTool(name: string): Promise<{ ok: boolean; removed: string }> {
  return request<{ ok: boolean; removed: string }>(`/plugins/tools/${encodeURIComponent(name)}`, { method: 'DELETE' });
}
