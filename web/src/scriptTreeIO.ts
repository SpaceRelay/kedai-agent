// 脚本树导入导出纯函数(阶段六 6e):
//  - parseScriptTreeImport:JSON 解析 + 结构校验 + 递归重分配 id(避免覆盖既有脚本);
//  - stripByExportWith:按酒馆 export_with 语义剥离 content/info(仅 data/button 时脚本不可执行但结构保留)。
import type { ScriptTree, ScriptTreeNode, ScriptNode, ScriptFolder } from './api';

/** 生成新 id(uuid 优先,回退时间戳+随机) */
function uid(): string {
  return crypto?.randomUUID?.() ?? `s-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

function parseScriptNode(raw: unknown, path: string): ScriptNode {
  if (!isRecord(raw)) throw new Error(`「${path}」应为对象`);
  if (raw.type !== 'script') throw new Error(`「${path}」缺少脚本类型(type: script)`);
  if (typeof raw.name !== 'string' || typeof raw.content !== 'string') {
    throw new Error(`「${path}」缺少名称或内容字段`);
  }
  const node: ScriptNode = {
    type: 'script',
    enabled: raw.enabled !== false,
    name: raw.name,
    id: uid(),
    content: raw.content,
  };
  if (typeof raw.info === 'string') node.info = raw.info;
  if (isRecord(raw.button)) {
    node.button = {
      enabled: raw.button.enabled !== false,
      buttons: Array.isArray(raw.button.buttons)
        ? raw.button.buttons.filter(isRecord).map((b) => ({
            name: typeof b.name === 'string' ? b.name : '按钮',
            visible: b.visible !== false,
          }))
        : [],
    };
  }
  if (isRecord(raw.data)) node.data = raw.data;
  if (isRecord(raw.export_with)) {
    node.export_with = {
      data: raw.export_with.data !== false,
      button: raw.export_with.button !== false,
    };
  }
  return node;
}

function parseScriptFolder(raw: unknown, path: string): ScriptFolder {
  if (!isRecord(raw)) throw new Error(`「${path}」应为对象`);
  if (raw.type !== 'folder') throw new Error(`「${path}」缺少文件夹类型(type: folder)`);
  if (typeof raw.name !== 'string') throw new Error(`「${path}」缺少名称字段`);
  const folder: ScriptFolder = {
    type: 'folder',
    enabled: raw.enabled !== false,
    name: raw.name,
    id: uid(),
    scripts: [],
  };
  if (typeof raw.icon === 'string') folder.icon = raw.icon;
  if (typeof raw.color === 'string') folder.color = raw.color;
  if (Array.isArray(raw.scripts)) {
    folder.scripts = raw.scripts.map((sub, i) => parseScriptNode(sub, `${path}.scripts[${i}]`));
  }
  return folder;
}

/**
 * 解析导入的脚本树 JSON:顶层必须为数组,元素为 script / folder(文件夹内再含一层 script)。
 * 校验失败抛错(含中文消息);id 一律重分配(仿 onFlowImport 恒分配新 id,避免与既有脚本撞车)。
 */
export function parseScriptTreeImport(raw: unknown): ScriptTree {
  if (!Array.isArray(raw)) throw new Error('脚本文件格式错误:顶层应为数组');
  const tree: ScriptTree = [];
  for (let i = 0; i < raw.length; i++) {
    const node = raw[i];
    const path = `[${i}]`;
    if (!isRecord(node)) throw new Error(`「${path}」应为对象`);
    if (node.type === 'folder') {
      tree.push(parseScriptFolder(node, path));
    } else if (node.type === 'script') {
      tree.push(parseScriptNode(node, path));
    } else {
      throw new Error(`「${path}」存在未知类型:${String(node.type)}(仅支持 script/folder)`);
    }
  }
  return tree;
}

/**
 * 按酒馆 export_with 语义剥离脚本树:节点 export_with.data=false 时清空 data、
 * export_with.button=false 时清空 button;同时剥离 content/info(仅保留结构)。
 * 缺省(无 export_with)视为全导出,原样保留结构。
 */
export function stripByExportWith(tree: ScriptTree): ScriptTree {
  return tree.map(stripNode);
}

function stripNode(node: ScriptTreeNode): ScriptTreeNode {
  if (node.type === 'folder') {
    return { ...node, scripts: node.scripts.map(stripNode) };
  }
  // export_with 语义下 content/info 被有意剥离,剥离后结构与 ScriptNode(content 必填)不再一致:
  // 局部按 Partial 操作(delete 运算符要求可选字段),返回处断言回 ScriptNode(消费侧兼容缺 content 的不可执行节点)。
  const stripped: Partial<ScriptNode> = { ...node };
  const ew = node.export_with;
  if (ew && ew.data === false) delete stripped.data;
  if (ew && ew.button === false) delete stripped.button;
  // 仅 data/button 导出:剥离可执行体与说明,脚本结构保留(导入后不可执行,对齐酒馆 export_with 语义)
  if (ew && (ew.data || ew.button)) {
    delete stripped.content;
    delete stripped.info;
  }
  return stripped as ScriptNode;
}
