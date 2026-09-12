import { describe, expect, it, vi } from 'vitest';
import { createSSRApp, h } from 'vue';
import { renderToString } from 'vue/server-renderer';
import { createPinia, setActivePinia, type Pinia } from 'pinia';
import { useAppStore } from '../store';
import CallTracePanel from './CallTracePanel.vue';
import type { TaskLlmCall } from '../api';

// CallTracePanel 冒烟测试:项目无 jsdom / @vue/test-utils,沿用 settingsSections.test.ts 的
// SSR 模式(createSSRApp + renderToString)。SSR 不触发 onMounted/watch,因此面板不会真正
// 发请求;此处只验证「渲染不炸 + 空态文案 + 任务模式调用行/聊天模式推理链渲染」。

// node 环境无 localStorage,而 store 初始化即访问(appMode / callTraceOpen / 脚本授权),补内存桩
const memStorage = new Map<string, string>();
vi.stubGlobal('localStorage', {
  getItem: (k: string) => memStorage.get(k) ?? null,
  setItem: (k: string, v: string) => void memStorage.set(k, String(v)),
  removeItem: (k: string) => void memStorage.delete(k),
  clear: () => memStorage.clear(),
  key: (i: number) => [...memStorage.keys()][i] ?? null,
  get length() { return memStorage.size; },
});

/** 以 pinia 上下文 SSR 渲染面板为 HTML 字符串;setup 可在渲染前预置 store 状态 */
async function render(setup?: (store: ReturnType<typeof useAppStore>) => void): Promise<string> {
  const pinia: Pinia = createPinia();
  setActivePinia(pinia);
  if (setup) setup(useAppStore(pinia));
  const app = createSSRApp({ render: () => h(CallTracePanel) });
  app.use(pinia);
  return renderToString(app);
}

function makeCall(over: Partial<TaskLlmCall> = {}): TaskLlmCall {
  return {
    id: 'c1',
    task_id: 't1',
    phase: 'step',
    step_index: 2,
    model: 'deepseek-chat',
    prompt_summary: '提示词摘要',
    response_summary: '响应摘要',
    prompt_tokens: 10,
    completion_tokens: 20,
    reasoning_tokens: 0,
    elapsed_ms: 1234,
    status: 'ok',
    created_at: '2026-08-28T00:00:00.000Z',
    ...over,
  };
}

describe('CallTracePanel(调用追踪内容,面板合并后为 AgentPanel「调用情况」tab 组件)', () => {
  it('聊天模式空态:渲染不炸,显示「暂无调用记录」', async () => {
    const html = await render((store) => {
      store.appMode = 'roleplay';
    });
    // 合并后不再自带面板头(「调用情况」标题由 AgentPanel tab 条提供)
    expect(html).not.toContain('sv-agent-panel-head');
    expect(html).toContain('推理链');
    expect(html).toContain('工具调用');
    expect(html).toContain('暂无调用记录');
  });

  it('任务模式空态:显示「暂无调用记录」', async () => {
    const html = await render((store) => {
      store.appMode = 'task';
      store.currentTaskId = 't1';
      store.taskCalls = [];
    });
    expect(html).toContain('LLM 调用');
    expect(html).toContain('暂无调用记录');
  });

  it('任务模式有记录:渲染阶段中文标签/模型/token/耗时/状态徽标', async () => {
    const html = await render((store) => {
      store.appMode = 'task';
      store.currentTaskId = 't1';
      store.taskCalls = [
        makeCall(),
        makeCall({ id: 'c2', phase: 'planner', step_index: null, status: 'error', elapsed_ms: 500 }),
      ];
    });
    expect(html).toContain('LLM 调用');
    // 步骤序号契约:后端落库 step_index 为 0 起,面板展示 +1(下标 2 → 步骤 #3)
    expect(html).toContain('步骤 #3');
    expect(html).toContain('规划');
    expect(html).toContain('deepseek-chat');
    expect(html).toContain('30'); // tokens 合计 10 + 20(SSR 插值锚点隔断「30 tokens」连续文本,分开断言)
    expect(html).toContain('tokens');
    expect(html).toContain('1.2s');
    expect(html).toContain('500ms');
    expect(html).toContain('正常');
    expect(html).toContain('错误');
    // 摘要默认收起,不渲染 <pre>
    expect(html).not.toContain('提示词摘要');
  });

  it('聊天模式有推理链与工具调用时渲染对应内容', async () => {
    const html = await render((store) => {
      store.appMode = 'roleplay';
      store.agent = {
        phase: 'executing',
        stepText: '正在调用工具',
        detail: '',
        chain: [{ at: 1, text: '规划完成', detail: '3 步' }],
        pendingTool: null,
        toolCalls: [{ name: 'web_search', input: {}, output: '结果', status: 'done', risk: 'safe' }],
        flowProgress: null,
      };
    });
    expect(html).toContain('规划完成');
    expect(html).toContain('web_search');
    expect(html).toContain('safe');
    expect(html).toContain('完成');
  });

  it('截断标记:finish_reason=length 渲染醒目「截断」徽标,stop/缺省不渲染', async () => {
    const html = await render((store) => {
      store.appMode = 'task';
      store.currentTaskId = 't1';
      store.taskCalls = [
        makeCall({ id: 'c1', finish_reason: 'length' }),
        makeCall({ id: 'c2', phase: 'planner', step_index: null, finish_reason: 'stop' }),
        makeCall({ id: 'c3', phase: 'summarize', step_index: null }),
      ];
    });
    // 仅 length 行带截断徽标(SSR 摘要默认收起,仅行内一处)
    expect(html.match(/sv-badge trunc/g)).toHaveLength(1);
    expect(html).toContain('截断');
  });

  it('步骤序号契约:step_index 0 起、面板展示 +1(首步行显示 步骤 #1,绝不出现 #0)', async () => {
    const html = await render((store) => {
      store.appMode = 'task';
      store.currentTaskId = 't1';
      store.taskCalls = [makeCall({ id: 'c1', step_index: 0 })];
    });
    expect(html).toContain('步骤 #1');
    expect(html).not.toContain('步骤 #0');
  });

  it('阶段标签:team 汇总行 phase=summary 显示「汇总」,不落原始字符串', async () => {
    const html = await render((store) => {
      store.appMode = 'task';
      store.currentTaskId = 't1';
      store.taskCalls = [
        makeCall({ id: 'c1', phase: 'summary', step_index: null }),
        makeCall({ id: 'c2', phase: 'final_audit', step_index: null }),
      ];
    });
    expect(html).toContain('汇总');
    expect(html).toContain('终审');
    // 未命中分支会原样渲染 phase 字符串;summary 已有专属分支,不应裸露
    expect(html).not.toContain('>summary<');
  });

  it('进行中伪行(批次 R4):liveBuffers 非空时列表顶部出现伪行,阶段标签与步骤序号正确', async () => {
    const html = await render((store) => {
      store.appMode = 'task';
      store.currentTaskId = 't1';
      store.taskCalls = [makeCall({ id: 'c1', step_index: 0 })];
      // key 契约:`${phase}:${step_index ?? ''}`(store.appendLiveDelta 同款)
      store.liveBuffers = new Map([
        ['step:1', '半截流式文本'],
        ['planner:', '规划增量'],
      ]);
    });
    expect(html).toContain('正在生成');
    expect(html).toContain('流式中');
    // 伪行复用阶段标签契约(step_index 0 起展示 +1);SSR 插值锚点会隔断连续文本,分段断言
    expect(html).toContain('步骤 #2');
    expect(html.match(/正在生成/g), '两条缓冲应各出一条伪行').toHaveLength(2);
    // 伪行在正式行之前(列表顶部)
    expect(html.indexOf('正在生成')).toBeLessThan(html.indexOf('步骤 #1'));
    // 展开区默认收起:流式文本不渲染
    expect(html).not.toContain('半截流式文本');
  });

  it('进行中伪行:无缓冲时不渲染;伪行消失后正式行仍在(llm_call 落库对齐)', async () => {
    const html = await render((store) => {
      store.appMode = 'task';
      store.currentTaskId = 't1';
      store.taskCalls = [makeCall({ id: 'c1', step_index: 0 })];
      store.liveBuffers = new Map();
    });
    expect(html).not.toContain('正在生成');
    expect(html).toContain('步骤 #1');
  });
});
