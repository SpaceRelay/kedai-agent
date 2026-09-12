import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createSSRApp, h, type Component } from 'vue';
import { renderToString } from 'vue/server-renderer';
import { createPinia, setActivePinia, type Pinia } from 'pinia';
import { useTaskStore } from '../stores/task';
import TaskBoard from './TaskBoard.vue';
import TaskModeSelect from './TaskModeSelect.vue';
import type { TaskDetail, TaskRecord, TaskRunMode, TaskStep, TaskStatus } from '../api';

// 批次 4 六模式 UI 冒烟测试:项目无 jsdom / @vue/test-utils,沿用 settingsSections.test.ts 的
// SSR 模式(createSSRApp + renderToString)。SSR 不触发 onMounted,组件不发请求;
// 此处验证「渲染不炸 + 关键文案/结构存在」(批准区、分工卡、审计结论卡、模式选择器)。
// 注:Sidebar 引用的 /logo.png 静态资源在 vitest 下无法解析,故「下达目标」区的
// 模式选择器抽为 TaskModeSelect.vue 独立冒烟(Sidebar 内联渲染该组件)。

// node 环境无 localStorage,而 store 初始化即访问(appMode / taskRunMode / 渲染偏好等),补内存桩
const memStorage = new Map<string, string>();
vi.stubGlobal('localStorage', {
  getItem: (k: string) => memStorage.get(k) ?? null,
  setItem: (k: string, v: string) => void memStorage.set(k, String(v)),
  removeItem: (k: string) => void memStorage.delete(k),
  clear: () => memStorage.clear(),
  key: (i: number) => [...memStorage.keys()][i] ?? null,
  get length() { return memStorage.size; },
});

function makeTask(status: TaskStatus, mode: TaskRunMode, plan: TaskStep[] = [], result = ''): TaskRecord {
  return {
    id: 't1',
    title: '调研并撰写季度报告',
    status,
    plan,
    result,
    error: '',
    task_mode: mode,
    created_at: '2026-08-29T00:00:00.000Z',
    updated_at: '2026-08-29T00:00:00.000Z',
  };
}

function makeDetail(task: TaskRecord): TaskDetail {
  return { task, subtasks: [], usage_total: { prompt_tokens: 0, completion_tokens: 0, reasoning_tokens: 0 } };
}

/** 以 pinia 上下文 SSR 渲染组件为 HTML 字符串;seed 在渲染前对 task store 播种状态 */
async function render(comp: Component, seed?: (pinia: Pinia) => void): Promise<string> {
  const pinia = createPinia();
  setActivePinia(pinia);
  seed?.(pinia);
  const app = createSSRApp({ render: () => h(comp) });
  app.use(pinia);
  return renderToString(app);
}

/** 播种当前任务详情(选中态;供 TaskBoard 渲染) */
function seedCurrentTask(pinia: Pinia, detail: TaskDetail): void {
  void pinia;
  const task = useTaskStore();
  task.currentTask = detail;
  task.currentTaskId = detail.task.id;
}

beforeEach(() => {
  memStorage.clear();
});

describe('TaskBoard 批次 4:plan 模式批准区', () => {
  it('status=planned 时渲染批准区(批准执行 / 修改后批准 / 放弃)与模式徽标', async () => {
    const plan: TaskStep[] = [
      { name: '搜集资料', goal: '收集季度数据', status: 'pending', result: '' },
      { name: '撰写正文', goal: '输出报告初稿', status: 'pending', result: '' },
    ];
    const html = await render(TaskBoard, (p) => seedCurrentTask(p, makeDetail(makeTask('planned', 'plan', plan))));
    expect(html).toContain('计划待批准');
    expect(html).toContain('批准执行');
    expect(html).toContain('修改后批准');
    expect(html).toContain('放弃');
    expect(html).toContain('待批准');
    expect(html).toContain('模式:先规划后批准');
    // 计划步骤仍展示(供批准前审阅)
    expect(html).toContain('搜集资料');
  });

  it('planned 状态完整显示计划清单:步骤名 + 目标(批准前可审阅计划内容)', async () => {
    const plan: TaskStep[] = [
      { name: '搜集资料', goal: '收集季度数据与竞品信息', status: 'pending', result: '' },
      { name: '撰写正文', goal: '输出 2000 字报告初稿', status: 'pending', result: '' },
    ];
    const html = await render(TaskBoard, (p) => seedCurrentTask(p, makeDetail(makeTask('planned', 'plan', plan))));
    expect(html).toContain('计划待批准');
    // 目标文本必须渲染(实测反馈「不显示最终计划」的回归锁定:旧渲染只出步骤名)
    expect(html).toContain('收集季度数据与竞品信息');
    expect(html).toContain('输出 2000 字报告初稿');
    expect(html).toContain('sv-task-step-goal');
  });

  it('非 planned 状态不渲染批准区', async () => {
    const html = await render(TaskBoard, (p) =>
      seedCurrentTask(p, makeDetail(makeTask('running', 'plan'))),
    );
    expect(html).not.toContain('计划待批准');
  });
});

describe('TaskBoard plan 模式:批准后/完成后计划清单持续可见(实测回归)', () => {
  const planBase: TaskStep[] = [
    { name: '搜集资料', goal: '收集季度数据', status: 'pending', result: '' },
    { name: '撰写正文', goal: '输出报告初稿', status: 'pending', result: '' },
  ];

  it('running:步骤状态徽标(done/running)与已完成步骤 result 实时反映', async () => {
    const plan: TaskStep[] = [
      { ...planBase[0], status: 'done', result: '资料摘要:三季度销量环比上升' },
      { ...planBase[1], status: 'running' },
    ];
    const html = await render(TaskBoard, (p) => seedCurrentTask(p, makeDetail(makeTask('running', 'plan', plan))));
    expect(html).toContain('计划步骤');
    expect(html).not.toContain('计划待批准');
    // 每步状态徽标
    expect(html).toContain('已完成');
    expect(html).toContain('执行中');
    // 已完成步骤的 result 与目标同屏
    expect(html).toContain('资料摘要:三季度销量环比上升');
    expect(html).toContain('输出报告初稿');
  });

  it('done:计划清单(全部 done + 各步 result)与成果汇总并存,互不顶掉', async () => {
    const plan: TaskStep[] = [
      { ...planBase[0], status: 'done', result: '资料摘要……' },
      { ...planBase[1], status: 'done', result: '初稿……' },
    ];
    // 实跑问题 1:「成果汇总」卡默认收起,断言其内容前先打开开关(持久化偏好)
    memStorage.set('kedai.task-result-summary.v1', '1');
    const html = await render(TaskBoard, (p) =>
      seedCurrentTask(p, makeDetail(makeTask('done', 'plan', plan, '最终成果:完整季度报告'))),
    );
    expect(html).toContain('计划步骤');
    expect(html).toContain('资料摘要……');
    expect(html).toContain('初稿……');
    expect(html).toContain('成果汇总');
    expect(html).toContain('最终成果:完整季度报告');
  });

  it('partial:失败步骤(error + 错误文本)保留在清单中,部分成果照常显示', async () => {
    const plan: TaskStep[] = [
      { ...planBase[0], status: 'done', result: '资料摘要……' },
      { ...planBase[1], status: 'error', result: '模型超时,该步未完成' },
    ];
    memStorage.set('kedai.task-result-summary.v1', '1');
    const html = await render(TaskBoard, (p) =>
      seedCurrentTask(p, makeDetail(makeTask('partial', 'plan', plan, '部分成果:仅有资料摘要'))),
    );
    expect(html).toContain('计划步骤');
    expect(html).toContain('出错');
    expect(html).toContain('模型超时,该步未完成');
    expect(html).toContain('成果汇总');
    expect(html).toContain('部分成果:仅有资料摘要');
  });

  it('partial:任务级 error 原因在状态行可见(实跑问题 2:不再只显示黄标不说话)', async () => {
    const task = {
      ...makeTask('partial', 'team', [], '成果正文\n\n## 审计结论\n审计未通过'),
      error: '审计/终审未通过,未达交付标准(详见「审计结论」)',
    };
    const html = await render(TaskBoard, (p) => seedCurrentTask(p, makeDetail(task)));
    expect(html).toContain('部分完成');
    expect(html).toContain('审计/终审未通过,未达交付标准(详见「审计结论」)');
  });
});

describe('TaskBoard 批次 4:team 模式分工卡与审计结论', () => {
  it('按「【主Agent-N】」前缀分组为分工卡;result 尾部「## 审计结论」单独成卡', async () => {
    const plan: TaskStep[] = [
      { name: '【主Agent-1】搜集资料', goal: '收集数据', status: 'done', result: '' },
      { name: '【主Agent-2】撰写正文', goal: '输出初稿', status: 'done', result: '' },
      { name: '汇总成果', goal: '合并产出', status: 'done', result: '' },
    ];
    const task = makeTask('done', 'team', plan, '季度报告正文\n\n## 审计结论\n两位主 Agent 产出一致,审计通过。');
    const html = await render(TaskBoard, (p) => seedCurrentTask(p, makeDetail(task)));
    expect(html).toContain('主 Agent 分工');
    expect(html).toContain('【主Agent-1】');
    expect(html).toContain('【主Agent-2】');
    // 无前缀步骤归入未分工组;卡内步骤名不再重复前缀
    expect(html).toContain('未分工');
    expect(html).toContain('汇总成果');
    expect(html).toContain('审计结论');
    expect(html).toContain('模式:团队协作');
  });
});

describe('TaskBoard 批次 4:solo/multi 调用情况入口与 custom 步骤进度', () => {
  it('solo 模式渲染「查看调用情况」入口(面板合并后指向 Agent 面板「调用情况」tab)', async () => {
    const html = await render(TaskBoard, (p) =>
      seedCurrentTask(p, makeDetail(makeTask('running', 'solo'))),
    );
    expect(html).toContain('查看调用情况');
    expect(html).toContain('Agent 面板「调用情况」');
    expect(html).toContain('模式:单 Agent');
  });

  it('custom 模式计划区标题为「流程步骤进度」且带步骤状态徽标', async () => {
    const plan: TaskStep[] = [{ name: '生成草稿', goal: '生成', status: 'done', result: '' }];
    const html = await render(TaskBoard, (p) =>
      seedCurrentTask(p, makeDetail(makeTask('running', 'custom', plan))),
    );
    expect(html).toContain('流程步骤进度');
    expect(html).toContain('已完成');
  });

  it('legacy 模式渲染保持现状(无批准区/分工卡/调用面板入口)', async () => {
    const plan: TaskStep[] = [{ name: '步骤一', goal: '目标一', status: 'pending', result: '' }];
    const html = await render(TaskBoard, (p) =>
      seedCurrentTask(p, makeDetail(makeTask('running', 'legacy', plan))),
    );
    expect(html).toContain('计划步骤');
    expect(html).toContain('模式:三段式');
    expect(html).not.toContain('计划待批准');
    expect(html).not.toContain('主 Agent 分工');
    expect(html).not.toContain('查看调用情况面板');
  });
});

describe('TaskBoard 批次 R1:plan 模式最终计划输出', () => {
  it('planned 态:批准区内渲染 result 计划清单(markdown),下方计划步骤区隐藏(视觉去重)', async () => {
    const plan: TaskStep[] = [
      { name: '搜集资料', goal: '收集季度数据', status: 'pending', result: '' },
      { name: '撰写正文', goal: '输出报告初稿', status: 'pending', result: '' },
    ];
    // 批次 R1:planned 态 result = 待批准的计划清单(server-rs task_engine plan 模式写入)
    const result = '计划已产出,共 2 步:\n1. 搜集资料:收集季度数据\n2. 撰写正文:输出报告初稿';
    const html = await render(TaskBoard, (p) =>
      seedCurrentTask(p, makeDetail(makeTask('planned', 'plan', plan, result))),
    );
    expect(html).toContain('计划待批准');
    expect(html).toContain('计划已产出,共 2 步');
    expect(html).toContain('收集季度数据');
    // result 计划卡与下方步骤区不重复渲染(planned 态步骤全 pending,徽标无信息量)。
    // 精确匹配标题 div 收尾:SSR 输出保留模板注释,纯文本断言会被注释文本污染
    expect(html).not.toContain('计划步骤</div>');
  });

  it('done 态:最终成果卡在上、最终计划卡在下,各自成卡', async () => {
    const plan: TaskStep[] = [
      { name: '搜集资料', goal: '收集季度数据', status: 'done', result: '资料摘要' },
      { name: '撰写正文', goal: '输出报告初稿', status: 'done', result: '初稿内容' },
    ];
    const result =
      '最终成果:完整季度报告\n\n## 最终计划\n1. 搜集资料(done):资料摘要\n2. 撰写正文(done):初稿内容';
    const html = await render(TaskBoard, (p) =>
      seedCurrentTask(p, makeDetail(makeTask('done', 'plan', plan, result))),
    );
    expect(html).toContain('最终成果');
    // 独立卡标题(section-title div),非 markdown 正文里的 h2
    expect(html).toContain('最终计划</div>');
    expect(html).toContain('资料摘要');
    // 顺序:最终成果卡在上、最终计划卡在下
    expect(html.indexOf('最终成果')).toBeLessThan(html.indexOf('最终计划</div>'));
  });

  it('approve 后过渡窗口:running 态 result 仍持计划清单时不渲染成果卡(终态门控)', async () => {
    const plan: TaskStep[] = [
      { name: '搜集资料', goal: '收集季度数据', status: 'running', result: '' },
    ];
    // approve 后到续跑完成前,result 仍是 planned 态写入的计划清单文本
    const result = '计划已产出,共 1 步:\n1. 搜集资料:收集季度数据';
    const html = await render(TaskBoard, (p) =>
      seedCurrentTask(p, makeDetail(makeTask('running', 'plan', plan, result))),
    );
    // 精确匹配卡标题 div 收尾:SSR 输出保留模板注释,纯文本断言会被注释文本污染
    expect(html).not.toContain('最终成果</div>');
    expect(html).not.toContain('最终计划</div>');
    // 步骤区照常显示执行进度
    expect(html).toContain('计划步骤</div>');
  });
});

describe('TaskModeSelect 批次 4:六档模式选择器(Sidebar「下达目标」区)', () => {
  it('渲染六档执行模式选项,v-model 绑定 store.taskRunMode', async () => {
    const html = await render(TaskModeSelect, (p) => {
      void p;
      useTaskStore().taskRunMode = 'team';
    });
    expect(html).toContain('模式:三段式(默认)');
    expect(html).toContain('模式:单 Agent');
    expect(html).toContain('模式:多 Agent');
    expect(html).toContain('模式:先规划后批准');
    expect(html).toContain('模式:团队协作');
    expect(html).toContain('模式:自定义流程');
    // 绑定生效:当前选中值落在 team 选项上
    expect(html).toMatch(/value="team"[^>]*selected|selected[^>]*value="team"/);
  });
});

describe('TaskBoard 批次 R4:「正在生成」流式块', () => {
  it('liveBuffers 非空时渲染流式块(纯文本 + 光标类),缓冲为空不渲染', async () => {
    // 运行中任务 + 流式缓冲 → 块出现,含攒批文本与流式光标类
    let html = await render(TaskBoard, (p) => {
      seedCurrentTask(p, makeDetail(makeTask('running', 'legacy')));
      useTaskStore().liveBuffers = new Map([['step:0', '正在流出的正文第一段']]);
    });
    expect(html).toContain('正在生成');
    expect(html).toContain('正在流出的正文第一段');
    expect(html).toContain('sv-stream-cursor');

    // 流式期间纯文本:markdown 标记原样显示,绝不渲染为 HTML(防每 token 重跑 renderMarkdown)
    html = await render(TaskBoard, (p) => {
      seedCurrentTask(p, makeDetail(makeTask('running', 'legacy')));
      useTaskStore().liveBuffers = new Map([['agent:', '**加粗**标记']]);
    });
    expect(html).toContain('**加粗**标记');
    expect(html).not.toContain('<strong>');

    // 缓冲清空(llm_call 落库对齐后)→ 块消失,权威文本由计划/成果区 markdown 接管
    html = await render(TaskBoard, (p) => {
      seedCurrentTask(p, makeDetail(makeTask('running', 'legacy')));
      useTaskStore().liveBuffers = new Map();
    });
    expect(html).not.toContain('sv-task-live');
  });

  it('多缓冲(team 并行)时带 key 前缀区分各调用', async () => {
    const html = await render(TaskBoard, (p) => {
      seedCurrentTask(p, makeDetail(makeTask('running', 'team')));
      useTaskStore().liveBuffers = new Map([
        ['agent:0', '甲主的产出'],
        ['agent:1', '乙主的产出'],
      ]);
    });
    expect(html).toContain('甲主的产出');
    expect(html).toContain('乙主的产出');
    expect(html).toContain('agent:0');
    expect(html).toContain('agent:1');
  });
});

// ==================== 2026-09-10 六模式实跑修复:F7 子任务区去重 ====================

/** 带子任务的详情(legacy 的 subtasks 与 plan 步骤同源;multi 为子 agent 记录) */
function detailWithSubtasks(task: TaskRecord): TaskDetail {
  return {
    task,
    subtasks: [
      {
        id: 'st1',
        task_id: task.id,
        name: '开场白',
        instruction: '写开场白',
        status: 'done',
        result: '开场白成果',
        error: '',
        created_at: '2026-08-29T00:00:00.000Z',
        updated_at: '2026-08-29T00:00:00.000Z',
      },
    ],
    usage_total: { prompt_tokens: 0, completion_tokens: 0, reasoning_tokens: 0 },
  };
}

describe('TaskBoard F7:legacy 子任务区去重', () => {
  const plan: TaskStep[] = [
    { name: '搜集资料', goal: '收集数据', status: 'done', result: '摘要' },
  ];

  it('legacy:子任务区不渲染(与计划步骤同源重复),计划步骤区保留', async () => {
    const html = await render(TaskBoard, (p) =>
      seedCurrentTask(p, detailWithSubtasks(makeTask('done', 'legacy', plan))),
    );
    expect(html).not.toContain('子任务执行');
    // 计划步骤区仍在(信息以该区为准)
    expect(html).toContain('计划步骤');
    expect(html).toContain('搜集资料');
  });

  it('multi:子任务区照常渲染(子 agent 记录,不与计划步骤重复)', async () => {
    const html = await render(TaskBoard, (p) =>
      seedCurrentTask(p, detailWithSubtasks(makeTask('done', 'multi'))),
    );
    expect(html).toContain('子任务执行');
    expect(html).toContain('开场白');
  });
});
