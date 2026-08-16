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
  isAuthorized: ScriptRunContext['isAuthorized'];
  getScrollArea: () => HTMLElement | null;
  afterRender: () => Promise<unknown>;
  renderText: (message: TMessage) => string;
  renderScripts: (text: string, scripts: TScript[], scopeId: string) => { blocks: ScriptBlock[] } | null;
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

  for (const message of options.messages) {
    if (message.role !== 'assistant' || message.streaming || message.id < 0) continue;
    const scoped = options.renderScripts(options.renderText(message), options.scripts, `msg-${message.id}`);
    if (!scoped || scoped.blocks.length === 0) continue;
    try {
      await options.runMessageScripts(root, message.id, scoped.blocks, {
        characterId: options.characterId,
        scriptHash: options.scriptHash,
        isAuthorized: options.isAuthorized,
        initVarEntries: options.initVarEntries,
        mvuVariables: { ...options.mvuVariables },
      });
    } catch (err) {
      console.warn('[kedai-mvu] 脚本执行异常', err);
    }
  }
}
