// 插件(自定义工具)API
import { request } from './client';
import { uploadForm } from './stream';
import type { PluginToolsStatus } from './types';

/** 列出已注册工具插件 + 磁盘文件 */
export async function listPluginTools(): Promise<PluginToolsStatus> {
  return request<PluginToolsStatus>('/plugins/tools');
}

/** 导入工具插件 JSON 文件(multipart) */
export async function uploadPluginTool(file: File): Promise<{ ok: boolean; name: string; file: string }> {
  return uploadForm<{ ok: boolean; name: string; file: string }>('/plugins/tools/upload', file);
}

/** 热重载全部工具插件 */
export async function reloadPluginTools(): Promise<{ ok: boolean; loaded: number; errors?: string[] }> {
  return request<{ ok: boolean; loaded: number; errors?: string[] }>('/plugins/tools/reload', { method: 'POST' });
}

/** 删除已导入的工具插件文件 */
export async function deletePluginTool(name: string): Promise<{ ok: boolean; removed: string }> {
  return request<{ ok: boolean; removed: string }>(`/plugins/tools/${encodeURIComponent(name)}`, { method: 'DELETE' });
}
