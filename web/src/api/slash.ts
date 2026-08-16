// slash 命令 API:GET /api/slash/commands(前端输入框联想数据源)
import { request } from './client';
import type { SlashCommandMeta } from './types';

/** 读取内置 slash 命令清单(name/description/params) */
export async function listSlashCommands(): Promise<SlashCommandMeta[]> {
  const data = await request<{ commands: SlashCommandMeta[] }>('/slash/commands');
  return data.commands;
}
