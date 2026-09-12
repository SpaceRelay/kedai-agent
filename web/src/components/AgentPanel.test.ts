import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createSSRApp, h } from 'vue';
import { renderToString } from 'vue/server-renderer';
import { createPinia, setActivePinia, type Pinia } from 'pinia';
import { useAppStore } from '../store';
import { idleAgent } from '../sseReducer';
import AgentPanel from './AgentPanel.vue';

// AgentPanel 冒烟测试(批次 6.1b 「回退到此处」入口):SSR renderToString 模式
// (无 jsdom,同 settingsSections.test.ts)。只验证按钮渲染条件:
// 写工具成功调用项有按钮 / 读工具没有 / undo_enabled 关时全不渲染。

// node 环境无 localStorage(子 store 初始化即访问),补内存桩
const memStorage = new Map<string, string>();
vi.stubGlobal('localStorage', {
  getItem: (k: string) => memStorage.get(k) ?? null,
  setItem: (k: string, v: string) => void memStorage.set(k, String(v)),
  removeItem: (k: string) => void memStorage.delete(k),
  clear: () => memStorage.clear(),
  key: (i: number) => [...memStorage.keys()][i] ?? null,
  get length() { return memStorage.size; },
});

/** 渲染 AgentPanel(渲染前可经 seed 准备 store 状态) */
async function renderPanel(seed: (store: ReturnType<typeof useAppStore>) => void): Promise<string> {
  const pinia: Pinia = createPinia();
  setActivePinia(pinia);
  seed(useAppStore());
  const app = createSSRApp({ render: () => h(AgentPanel) });
  app.use(pinia);
  return renderToString(app);
}

beforeEach(() => {
  memStorage.clear();
});

describe('AgentPanel 回退到此处按钮(批次 6.1b)', () => {
  it('写工具成功调用项渲染按钮;读工具调用项不渲染', async () => {
    const html = await renderPanel((store) => {
      store.currentSessionId = 's1';
      store.undoEnabled = true;
      store.agent = {
        ...idleAgent(),
        toolCalls: [
          { name: 'write', input: { path: 'notes.md' }, status: 'done', callId: 'c1' },
          { name: 'read', input: { path: 'notes.md' }, status: 'done', callId: 'c2' },
        ],
      };
    });
    expect(html).toContain('工具调用(2)');
    expect(html.match(/回退到此处/g), '仅写工具调用项有「回退到此处」').toHaveLength(1);
  });

  it('undo_enabled 关闭或无会话时不渲染按钮', async () => {
    const htmlOff = await renderPanel((store) => {
      store.currentSessionId = 's1';
      store.undoEnabled = false;
      store.agent = {
        ...idleAgent(),
        toolCalls: [{ name: 'write', input: {}, status: 'done', callId: 'c1' }],
      };
    });
    expect(htmlOff).not.toContain('回退到此处');

    const htmlNoSession = await renderPanel((store) => {
      store.currentSessionId = null;
      store.undoEnabled = true;
      store.agent = {
        ...idleAgent(),
        toolCalls: [{ name: 'write', input: {}, status: 'done', callId: 'c1' }],
      };
    });
    expect(htmlNoSession).not.toContain('回退到此处');
  });
});

describe('AgentPanel 合并结构(调用情况收编为内部 tab)', () => {
  it('渲染分区标签页;callTraceOpen 记忆决定默认落 tab', async () => {
    // 默认(false):落在「Agent 状态」tab
    const htmlAgent = await renderPanel((store) => {
      store.currentSessionId = 's1';
    });
    expect(htmlAgent).toContain('Agent 状态');
    expect(htmlAgent).toContain('调用情况');
    expect(htmlAgent).toContain('role="tablist"');

    // callTraceOpen = true:落在「调用情况」tab(aria-selected 标识)
    const htmlTrace = await renderPanel((store) => {
      store.currentSessionId = 's1';
      store.callTraceOpen = true;
    });
    expect(htmlTrace).toMatch(/role="tab"[^>]*aria-selected="false"[^>]*>\s*Agent 状态/);
    expect(htmlTrace).toMatch(/role="tab"[^>]*aria-selected="true"[^>]*>\s*调用情况/);
  });

  it('「调用情况」tab 内容(内嵌 CallTracePanel)常驻挂载:任务模式渲染 LLM 调用区', async () => {
    const html = await renderPanel((store) => {
      store.appMode = 'task';
      store.currentTaskId = 't1';
      store.taskCalls = [];
    });
    // v-show 双 tab 常驻:Agent 状态(任务执行状态)与调用情况(LLM 调用)内容同输出
    expect(html).toContain('计划步骤');
    expect(html).toContain('LLM 调用');
  });
});

// 回归:面板开合只由 agentPanelOpen 决定,生成状态不再强行锁死展开。
// 历史 bug:showPanel = generating || agentPanelOpen,生成期间关闭按钮写 false 无效。
describe('AgentPanel 面板开合不再被生成状态锁死', () => {
  it('generating=true 但 agentPanelOpen=false 时面板不展开', async () => {
    const html = await renderPanel((store) => {
      store.currentSessionId = 's1';
      store.generating = true;
      store.agentPanelOpen = false;
    });
    expect(html).toContain('sv-agent-panel');
    expect(html).not.toMatch(/class="sv-agent-panel open"/);
  });

  it('agentPanelOpen=true 时展开(与 generating 无关)', async () => {
    const html = await renderPanel((store) => {
      store.currentSessionId = 's1';
      store.generating = false;
      store.agentPanelOpen = true;
    });
    expect(html).toMatch(/class="sv-agent-panel open"/);
  });

  it('任务执行态但 agentPanelOpen=false 时面板不展开', async () => {
    const html = await renderPanel((store) => {
      store.appMode = 'task';
      store.agentPanelOpen = false;
    });
    expect(html).not.toMatch(/class="sv-agent-panel open"/);
  });
});
