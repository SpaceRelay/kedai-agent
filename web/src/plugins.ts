// 角色卡内嵌插件:前端兜底检测。
// 后端详情接口返回 card_plugins 时以它为准;旧后端无此字段时,
// 从 data_raw 的 character_book 条目自行检测(与后端 detect_card_plugins 逻辑一致)。
import type { CardPluginInfo, PluginFeature } from './api';

/** 从角色卡 data_raw 检测内嵌插件(酒馆助手 SillyTavern-Assistant) */
export function detectCardPluginsClient(dataRaw: Record<string, unknown> | undefined): CardPluginInfo[] {
  if (!dataRaw) return [];
  // 兼容 V2(data.character_book)与 V3/扁平(character_book)布局
  const dataObj = dataRaw.data as Record<string, unknown> | undefined;
  const book = (dataRaw.character_book ?? dataObj?.character_book) as
    | { entries?: Array<{ comment?: string; content?: string }> }
    | undefined;
  const entries = book?.entries ?? [];
  if (entries.length === 0) return [];
  const hay = entries.map((e) => `${e.comment ?? ''}\n${e.content ?? ''}`).join('\n');
  const has = (n: string): boolean => hay.includes(n);
  const features: PluginFeature[] = [
    { id: 'initvar', label: '初始变量 [InitVar]', detected: has('[InitVar]') },
    { id: 'ejs', label: 'EJS 模板渲染(分阶段人设等)', detected: hay.includes('<%') },
    { id: 'variable', label: '变量系统 getvar / stat_data', detected: has('getvar(') || has('{{getvar::') },
    {
      id: 'status',
      label: '状态注入 {{format_message_variable}}',
      detected: has('{{format_message_variable') || has('<StatusPlaceHolderImpl/>'),
    },
    { id: 'protocol', label: '输出协议 <UpdateVariable>', detected: has('<UpdateVariable') || has('JSONPatch') },
  ];
  if (!features.some((f) => f.detected)) return [];
  return [
    {
      id: 'sillytavern-assistant',
      name: '酒馆助手插件',
      name_en: 'SillyTavern-Assistant',
      enabled: true,
      source: 'character_card',
      description:
        '角色卡内嵌的酒馆助手插件:分阶段人设等条目按 stat_data 变量实时渲染(EJS 模板),模型回复中的 <UpdateVariable>/<JSONPatch> 自动剥离并应用,会话状态随对话推进。kedai 已内置完整兼容实现,无需额外安装。',
      features,
    },
  ];
}
