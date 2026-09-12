import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createSSRApp, h, type Component } from 'vue';
import { renderToString } from 'vue/server-renderer';
import { createPinia, setActivePinia } from 'pinia';
import { useApiSettings } from '../../composables/useApiSettings';
import { usePromptInject } from '../../composables/usePromptInject';
import ConnectionSection from './ConnectionSection.vue';
import McpSection from './McpSection.vue';
import PresetImportExportSection from './PresetImportExportSection.vue';
import AgentSettingsSection from './AgentSettingsSection.vue';
import GenParamsSection from './GenParamsSection.vue';
import SettingsModal from '../SettingsModal.vue';

// 设置区组件冒烟测试:项目无 jsdom / @vue/test-utils,沿用 CacheHealthPanel.test.ts 的
// SSR 模式(createSSRApp + renderToString)。SSR 不触发 onMounted,因此各 section
// 的数据加载不会真正发请求;此处只验证「渲染不炸 + 关键文案存在 + show 开关生效」。

// node 环境无 localStorage,而 store 初始化即访问(HTML 渲染记忆 / 脚本授权存储),补内存桩
const memStorage = new Map<string, string>();
vi.stubGlobal('localStorage', {
  getItem: (k: string) => memStorage.get(k) ?? null,
  setItem: (k: string, v: string) => void memStorage.set(k, String(v)),
  removeItem: (k: string) => void memStorage.delete(k),
  clear: () => memStorage.clear(),
  key: (i: number) => [...memStorage.keys()][i] ?? null,
  get length() { return memStorage.size; },
});

/** 以 pinia 上下文 SSR 渲染组件为 HTML 字符串 */
async function render(comp: Component, props: Record<string, unknown> = {}): Promise<string> {
  const app = createSSRApp({ render: () => h(comp, props) });
  app.use(createPinia());
  return renderToString(app);
}

beforeEach(() => {
  // composable 在测试直接调用(构造 prop 状态)时需要活动 pinia
  setActivePinia(createPinia());
});

describe('ConnectionSection(后端连接区)', () => {
  it('渲染连接器信息与测试连接按钮(与 ApiSettingsSection 共享壳传入状态)', async () => {
    const state = useApiSettings();
    const html = await render(ConnectionSection, { state });
    expect(html).toContain('后端连接');
    expect(html).toContain('测试连接');
  });

  it('show=false 时根节点 display:none(embedded 模式按 activeSection 切换)', async () => {
    const state = useApiSettings();
    const html = await render(ConnectionSection, { state, show: false });
    expect(html).toMatch(/display:\s*none/);
  });
});

describe('PresetImportExportSection(预设导入/导出区)', () => {
  it('渲染导入/导出两行(合并后 standalone 也包含本区,防回归)', async () => {
    const state = usePromptInject();
    const html = await render(PresetImportExportSection, { state });
    expect(html).toContain('预设导入 / 导出');
    expect(html).toContain('导入酒馆预设');
    expect(html).toContain('导出注入配置');
  });
});

describe('SettingsModal(壳)', () => {
  it('standalone 模式渲染遮罩/头部,且包含预设导入导出区(原 standalone 缺失)', async () => {
    const html = await render(SettingsModal, { embedded: false });
    expect(html).toContain('sv-modal-mask');
    expect(html).toContain('预设导入 / 导出');
    expect(html).toContain('API 设置');
    expect(html).toContain('提示词注入');
  });

  it('embedded 模式仅渲染内容区,且只显示 activeSection 对应分区', async () => {
    const html = await render(SettingsModal, { embedded: true, activeSection: 'data' });
    expect(html).not.toContain('sv-modal-mask');
    expect(html).toContain('sv-settings-embedded');
    // 数据管理区可见;API 设置区应被 v-show 隐藏
    expect(html).toContain('数据管理');
  });

  it('装配:MCP 分区挂进壳(standalone 渲染;embedded 挂载不炸)', async () => {
    const standalone = await render(SettingsModal, { embedded: false });
    expect(standalone).toContain('MCP 服务');
    expect(standalone).toContain('重启后生效');

    const embedded = await render(SettingsModal, { embedded: true, activeSection: 'mcp' });
    expect(embedded).toContain('sv-settings-embedded');
    expect(embedded).toContain('MCP 服务');
  });
});

describe('McpSection(MCP 服务区,批次 6.2)', () => {
  it('渲染总开关/新增表单/「重启后生效」提示(默认关)', async () => {
    const html = await render(McpSection);
    expect(html).toContain('MCP 服务');
    expect(html).toContain('启用 MCP 服务');
    expect(html).toContain('已关闭'); // 默认 mcp_enabled=false
    expect(html).toContain('新增服务器');
    expect(html).toContain('重启后生效');
    expect(html).toContain('保存 MCP 设置');
  });

  it('show=false 时根节点 display:none(embedded 模式按 activeSection 切换)', async () => {
    const html = await render(McpSection, { show: false });
    expect(html).toMatch(/display:\s*none/);
  });
});

describe('AgentSettingsSection(Agent 设置区)', () => {
  it('渲染执行者人设精简/完整选择器(R3a;常显,仅任务模式生效说明)', async () => {
    const html = await render(AgentSettingsSection);
    expect(html).toContain('执行者人设');
    expect(html).toContain('精简(默认)');
    expect(html).toContain('完整');
    expect(html).toContain('仅任务模式生效');
  });

  it('渲染任务继承提示词注入开关(2026-09-10 实跑修复;默认不继承)', async () => {
    const html = await render(AgentSettingsSection);
    expect(html).toContain('任务继承提示词注入');
    expect(html).toContain('不继承(默认)');
    expect(html).toContain('继承');
  });
});

describe('GenParamsSection(生成参数区:记忆槽预算/容量上限)', () => {
  it('渲染记忆字符预算与容量上限两个 number 输入(0 = 不限制说明)', async () => {
    const html = await render(GenParamsSection);
    expect(html).toContain('记忆字符预算');
    expect(html).toContain('记忆容量上限');
    expect(html).toContain('0 = 不限制');
    expect(html).toMatch(/max="20000"/);
    expect(html).toMatch(/max="10000"/);
  });

  it('show=false 时根节点 display:none(embedded 模式按 activeSection 切换)', async () => {
    const html = await render(GenParamsSection, { show: false });
    expect(html).toMatch(/display:\s*none/);
  });
});
