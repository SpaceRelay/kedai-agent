import type { MvuVariables } from './mvu/variables';
import type { ScriptBlock, ScriptRunContext } from './scriptRunner';

export interface SchedulableMessage {
  id: number;
  role: 'user' | 'assistant' | 'system';
  streaming?: boolean;
}

export interface MessageScriptSchedulerOptions<TMessage extends SchedulableMessage, TScript> {
  renderHtml: boolean;
  authorized: boolean;
  scripts: TScript[];
  messages: TMessage[];
  characterId: string;
  scriptHash: string;
  initVarEntries: Record<string, string>;
  mvuVariables: MvuVariables;
  /** 角色名(initvar 内容 {{char}} 宏展开用,透传 ScriptRunContext) */
  charName?: string;
  /** 消息级沙箱只读 RPC 扩展点(世界书/聊天消息读取;实跑问题 7) */
  rpcExtensions?: ScriptRunContext['rpcExtensions'];
  isAuthorized: ScriptRunContext['isAuthorized'];
  getScrollArea: () => HTMLElement | null;
  afterRender: () => Promise<unknown>;
  renderText: (message: TMessage) => string;
  /** 该消息的楼层深度(0=最新);渲染与调度必须同口径,否则带 min_depth 的脚本
   *  会「渲染了却不执行」(实跑问题 7 R2)。 */
  depthOf: (message: TMessage, index: number, total: number) => number;
  renderScripts: (
    text: string,
    scripts: TScript[],
    scopeId: string,
    depth: number,
  ) => { blocks: ScriptBlock[] } | null;
  runMessageScripts: (
    root: HTMLElement,
    messageId: number,
    blocks: ScriptBlock[],
    context: ScriptRunContext,
  ) => Promise<void>;
}

/** 执行当前已渲染消息的角色脚本,供首次挂载与后续响应式更新共用。 */
export async function executeCurrentMessageScripts<TMessage extends SchedulableMessage, TScript>(
  options: MessageScriptSchedulerOptions<TMessage, TScript>,
): Promise<void> {
  if (!options.renderHtml || !options.authorized || options.scripts.length === 0) return;
  await options.afterRender();
  const root = options.getScrollArea();
  if (!root) return;

  const total = options.messages.length;
  for (let index = 0; index < total; index += 1) {
    const message = options.messages[index];
    if (message.role !== 'assistant' || message.streaming || message.id < 0) continue;
    // 真实楼层深度:渲染侧 ChatMessageItem 用 messages.length-1-i,这里同口径
    // (实跑问题 7 R2:旧实现写死 0,带 min_depth 的脚本在首楼不执行)
    const depth = options.depthOf(message, index, total);
    const scoped = options.renderScripts(
      options.renderText(message),
      options.scripts,
      `msg-${message.id}`,
      depth,
    );
    if (!scoped || scoped.blocks.length === 0) continue;
    try {
      await options.runMessageScripts(root, message.id, scoped.blocks, {
        characterId: options.characterId,
        scriptHash: options.scriptHash,
        isAuthorized: options.isAuthorized,
        initVarEntries: options.initVarEntries,
        mvuVariables: { ...options.mvuVariables },
        charName: options.charName,
        rpcExtensions: options.rpcExtensions,
      });
    } catch (err) {
      console.warn('[kedai-mvu] 脚本执行异常', err);
    }
  }
}
