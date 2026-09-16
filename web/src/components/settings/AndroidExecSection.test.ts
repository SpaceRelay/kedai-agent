// @vitest-environment jsdom
// AndroidExecSection 组件测试(阶段 E):命令执行授权面板。
// 覆盖:总开关渲染、Android 档位按平台条件显示、保存走 store action、
// 审计列表渲染与被拒绝态样式、等级展示。
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { mount } from '@vue/test-utils';
import { createPinia, setActivePinia } from 'pinia';

const memStorage = new Map<string, string>();
vi.stubGlobal('localStorage', {
  getItem: (k: string) => memStorage.get(k) ?? null,
  setItem: (k: string, v: string) => void memStorage.set(k, String(v)),
  removeItem: (k: string) => void memStorage.delete(k),
  clear: () => memStorage.clear(),
  key: (i: number) => [...memStorage.keys()][i] ?? null,
  get length() { return memStorage.size; },
});

vi.mock('../../api/exec', () => ({
  getExecTier: vi.fn().mockResolvedValue({ tier: 'sandbox', label: '沙箱(应用自身权限)' }),
  listExecAudit: vi.fn().mockResolvedValue([]),
  clearExecAudit: vi.fn().mockResolvedValue(0),
}));

// 平台判定可按用例改写:默认非 Android
const platform = { isAndroidTauri: false };
vi.mock('../../platform', () => ({
  get isAndroidTauri() {
    return platform.isAndroidTauri;
  },
  inTauri: true,
  isAndroid: false,
}));

import * as execApi from '../../api/exec';
import { useAppStore } from '../../store';
import AndroidExecSection from './AndroidExecSection.vue';

async function mountSection() {
  setActivePinia(createPinia());
  const store = useAppStore();
  const wrapper = mount(AndroidExecSection);
  await Promise.resolve();
  await Promise.resolve();
  return { store, wrapper };
}

describe('AndroidExecSection 组件(阶段 E)', () => {
  beforeEach(() => {
    memStorage.clear();
    platform.isAndroidTauri = false;
    vi.mocked(execApi.getExecTier).mockResolvedValue({ tier: 'sandbox', label: '沙箱(应用自身权限)' });
    vi.mocked(execApi.listExecAudit).mockResolvedValue([]);
  });

  it('渲染命令执行总开关(默认关闭)', async () => {
    const { wrapper } = await mountSection();
    expect(wrapper.text()).toContain('命令执行');
    expect(wrapper.text()).toContain('启用命令执行');
    expect(wrapper.text()).toContain('不受授权模式豁免');
  });

  it('非 Android 平台不显示 ROOT/Shizuku 档位', async () => {
    const { wrapper } = await mountSection();
    expect(wrapper.text()).not.toContain('ROOT 提权');
    expect(wrapper.text()).not.toContain('请求 Shizuku 授权');
  });

  it('Android 平台显示三档与 Shizuku 授权入口', async () => {
    platform.isAndroidTauri = true;
    const { wrapper } = await mountSection();
    expect(wrapper.text()).toContain('ROOT 提权');
    expect(wrapper.text()).toContain('请求 Shizuku 授权');
    expect(wrapper.text()).toContain('沙箱档');
  });

  it('点击保存走 queueSettingsSave 并携带 4 个开关', async () => {
    const { store, wrapper } = await mountSection();
    const spy = vi.spyOn(store, 'queueSettingsSave').mockResolvedValue(undefined);

    // 打开总开关与沙箱档
    const boxes = wrapper.findAll('input[type="checkbox"]');
    await boxes[0].setValue(true);
    await wrapper.find('.sv-btn-fill').trigger('click');
    await Promise.resolve();

    expect(spy).toHaveBeenCalledWith(
      expect.objectContaining({
        exec_enabled: true,
        exec_allow_root: false,
        exec_allow_shizuku: false,
        exec_allow_sandbox: false,
      }),
    );
  });

  it('审计为空时显示空态', async () => {
    const { wrapper } = await mountSection();
    expect(wrapper.text()).toContain('暂无执行记录');
  });

  it('有审计行时渲染命令与风险标签,被拒绝行带 denied 样式', async () => {
    vi.mocked(execApi.listExecAudit).mockResolvedValue([
      {
        id: 2, ts: '2026-09-13T01:00:00Z', source: 'chat', task_id: null, session_id: 's1',
        command: 'rm -rf /tmp/x', shell: 'sh', tier: 'sandbox', risk: 'destructive',
        decision: 'denied', exit_code: null,
        stdout_summary: '', stderr_summary: '高危命令需逐条确认',
      },
      {
        id: 1, ts: '2026-09-13T00:59:00Z', source: 'chat', task_id: null, session_id: 's1',
        command: 'ls -la', shell: 'sh', tier: 'sandbox', risk: 'safe',
        decision: 'allowed', exit_code: 0, stdout_summary: 'total 0', stderr_summary: '',
      },
    ]);
    const { wrapper } = await mountSection();

    expect(wrapper.findAll('.sv-audit-row').length).toBe(2);
    expect(wrapper.text()).toContain('rm -rf /tmp/x');
    expect(wrapper.text()).toContain('破坏性');
    expect(wrapper.findAll('.sv-audit-row.denied').length).toBe(1);
    expect(wrapper.text()).toContain('已拒绝');
  });

  it('Android 平台展示当前执行器等级(等级可见)', async () => {
    platform.isAndroidTauri = true;
    vi.mocked(execApi.getExecTier).mockResolvedValue({
      tier: 'shizuku',
      label: 'Shizuku(ADB 权限)',
    });
    const { wrapper } = await mountSection();
    // 等级区仅在 Android 渲染(桌面无 su/Shizuku 概念)
    expect(wrapper.text()).toContain('当前等级');
    expect(wrapper.text()).toContain('Shizuku');
  });

  it('非 Android 平台不请求/不展示等级(避免无意义探测)', async () => {
    const { wrapper } = await mountSection();
    expect(wrapper.text()).not.toContain('当前等级');
  });
});
