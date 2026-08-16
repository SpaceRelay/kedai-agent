// scriptRunner.ts — 消息脚本受控执行器(从 ChatWindow 抽离)
// 职责:对 v-html 渲染出的 scoped 容器执行角色卡脚本(先 setMvuHostContext,再逐块执行)。
// 设计要点:
// - 每个容器(scopeId)绑定一份独立变量上下文,执行完 flushJqReady 后清空,避免串扰
// - [InitVar] 初始变量解析带缓存(角色 + 条目指纹失效),避免每消息重复解析
// - 跳过负 id 临时消息:流式结束(负 id)与 loadHistory 载入真实 id 各触发一次渲染,
//   若对临时消息也执行脚本,同一内容会执行两遍(非幂等脚本产生重复副作用);
//   临时消息不执行,等真实消息落库后统一执行一遍。

import type { MvuVariables } from './mvu/variables';
import { collectInitVars, deepMerge } from './mvu/initvar';
import { setMvuHostContext, flushJqReady } from './mvu/host';
import { executeSandboxedCharacterScript, type SandboxCleanup } from './characterScriptSandbox';

export interface ScriptBlock {
  scopeId: string;
  scripts: string[];
}

/** 脚本执行所需外部状态(由调用方每次传入当前值) */
export interface ScriptRunContext {
  characterId: string;
  /** 当前角色全部脚本内容的稳定 SHA-256 */
  scriptHash: string;
  /** 执行入口必须调用的授权检查，不能只依赖 UI 状态 */
  isAuthorized: (characterId: string, scriptHash: string) => boolean;
  /** [InitVar] 初始变量条目(comment → content) */
  initVarEntries: Record<string, string>;
  /** 当前会话变量树(消息快照回放值) */
  mvuVariables: MvuVariables;
}

/** 深拷贝(avoid structuredClone on Vue reactive Proxy) */
function deepCopy<T>(v: T): T {
  return JSON.parse(JSON.stringify(v)) as T;
}

export type CharacterScriptExecutor = (
  code: string,
  context: { container: HTMLElement; variables: MvuVariables },
) => Promise<unknown>;

interface ActiveExecution {
  promise: Promise<void>;
  containers: HTMLElement[];
}

export class ScriptRunner {
  private initVarCacheKey = '';
  private initVarCache: MvuVariables | null = null;
  /** 已成功执行的渲染容器;WeakSet 不阻止 Vue 卸载后的 DOM 被回收。 */
  private readonly executedContainers = new WeakSet<HTMLElement>();
  /** 每个渲染容器关联的长期沙箱清理器,供事件回调在初始执行结束后继续工作。 */
  private readonly cleanups = new Map<HTMLElement, SandboxCleanup[]>();
  /** 同一消息渲染实例的执行中任务,用于合并并发/重复调度。 */
  private readonly inFlight = new Map<string, ActiveExecution>();

  constructor(private readonly execute: CharacterScriptExecutor = executeSandboxedCharacterScript) {}

  /** [InitVar] 解析结果缓存:key = 角色 id + 条目内容指纹;角色切换/条目变化时自动失效 */
  private cachedInitVars(ctx: ScriptRunContext): MvuVariables {
    const key = `${ctx.characterId}:${JSON.stringify(ctx.initVarEntries)}`;
    if (this.initVarCacheKey === key && this.initVarCache) return this.initVarCache;
    const vars = collectInitVars(
      Object.entries(ctx.initVarEntries).map(([comment, content]) => ({ comment, content })),
    );
    this.initVarCache = { stat_data: vars.stat_data, display_data: vars.display_data };
    this.initVarCacheKey = key;
    return this.initVarCache;
  }

  /** 脚本可用变量树:[InitVar] 初始值打底(缓存解析),再叠加消息快照回放值 */
  private scriptVariables(ctx: ScriptRunContext): MvuVariables {
    const init = this.cachedInitVars(ctx);
    const stat = deepCopy(init.stat_data);
    const disp = deepCopy(init.display_data);
    deepMerge(stat, deepCopy(ctx.mvuVariables.stat_data));
    deepMerge(disp, deepCopy(ctx.mvuVariables.display_data));
    return { stat_data: stat, display_data: disp };
  }

  /**
   * 执行某消息的全部脚本块。
   * @param root 消息列表滚动容器(在其中按 scopeId 定位块容器)
   * @param msgId 消息 id;负 id(流式临时消息)跳过执行
   * @param blocks 该消息渲染出的脚本块
   * @param ctx 外部状态快照
   */
  async runMessageScripts(
    root: HTMLElement,
    msgId: number,
    blocks: ScriptBlock[],
    ctx: ScriptRunContext,
  ): Promise<void> {
    if (msgId < 0 || blocks.length === 0) return; // 临时消息不执行,避免真实消息落库后双执行
    // 强制门禁:角色 ID 与当前脚本哈希必须同时匹配持久授权。
    if (!ctx.characterId || !ctx.scriptHash || !ctx.isAuthorized(ctx.characterId, ctx.scriptHash)) return;
    const executionKey = `${ctx.characterId}:${ctx.scriptHash}:${msgId}`;
    const containers = blocks.map((block) => {
      const container = root.querySelector<HTMLElement>(`[data-kd-scope="${block.scopeId}"]`);
      if (!container) throw new Error(`未找到状态栏脚本容器: ${block.scopeId}`);
      return container;
    });
    const current = this.inFlight.get(executionKey);
    if (current && current.containers.length === containers.length
      && current.containers.every((container, index) => container === containers[index])) {
      return current.promise;
    }

    const execution = (async (): Promise<void> => {
      const vars = this.scriptVariables(ctx);
      for (let index = 0; index < blocks.length; index += 1) {
        const block = blocks[index];
        const container = containers[index];
        if (this.executedContainers.has(container)) continue;
        const blockCleanups: SandboxCleanup[] = [];
        setMvuHostContext({ container, variables: vars });
        try {
          for (const code of block.scripts) {
            const result = await this.execute(code, { container, variables: deepCopy(vars) });
            if (typeof result === 'function') blockCleanups.push(result as SandboxCleanup);
          }
          // 保留宿主自身 ready 队列兼容,角色卡代码只通过 iframe RPC 修改白名单 DOM。
          await flushJqReady();
          if (blockCleanups.length > 0) this.cleanups.set(container, blockCleanups);
          this.executedContainers.add(container);
        } catch (error) {
          for (const dispose of blockCleanups) dispose();
          throw error;
        } finally {
          setMvuHostContext(null);
        }
      }
    })();
    this.inFlight.set(executionKey, { promise: execution, containers });
    try {
      await execution;
    } finally {
      if (this.inFlight.get(executionKey)?.promise === execution) this.inFlight.delete(executionKey);
    }
  }

  /** 释放长期沙箱及其宿主 DOM 事件监听器;组件卸载或会话切换时调用。 */
  cleanup(): number {
    let count = 0;
    for (const disposers of this.cleanups.values()) {
      for (const dispose of disposers) {
        dispose();
        count += 1;
      }
    }
    this.cleanups.clear();
    return count;
  }
}
