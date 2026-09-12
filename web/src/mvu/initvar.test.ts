// [InitVar] 条目识别与内容解析测试
import { describe, expect, it } from 'vitest';
import { collectInitVars, isInitVarEntry } from './initvar';

describe('isInitVarEntry', () => {
  it('大小写不敏感:小写 [initvar] 标签(碧蓝卡形态)同样识别', () => {
    expect(isInitVarEntry('[InitVar]变量初始化')).toBe(true);
    expect(isInitVarEntry('[initvar]变量初始化')).toBe(true);
    expect(isInitVarEntry('[INITVAR]')).toBe(true);
    expect(isInitVarEntry('[InitialVariables]')).toBe(true);
    expect(isInitVarEntry('[initialvariables]初始')).toBe(true);
    expect(isInitVarEntry('[mvu_update]变量更新规则')).toBe(false);
    expect(isInitVarEntry('普通条目')).toBe(false);
  });
});

describe('collectInitVars(碧蓝卡真实形态)', () => {
  const AZUR_INITVAR = `world_state:
  date: "New Era, Year 1, Month 3"
  time: "08:00"
  weather: 晴朗
  location: 指挥官学院群岛
  phase: Daily

user:
  name: "{{user}}"
  # 初始默认为学院生,若走佣兵线,AI会在开局剧情中通过Update修改
  identity_type: Academy_Student
  career_path: Navy
  rank_title: Cadet
  stats:
    command_level: 1
    mental_state: 100
    stamina: 100

reputation:
  official: 0
  underground: 0

resources:
  gold: 1000
  oil: 500
  cubes: 2
  merit: 0

shipgirls: {}

quest_log:
  main_chapter: "Prologue: The Awakening"
  active_mission: "前往学院报到并进行首次建造"
  flags: {}

inventory: {}
`;

  it('小写 [initvar] 条目参与收集;YAML 结构/注释/引号/数字/空容器全部正确', () => {
    const vars = collectInitVars([{ comment: '[initvar]变量初始化', content: AZUR_INITVAR }]);
    const stat = vars.stat_data as Record<string, Record<string, unknown>>;
    // 三层结构与世界状态
    const world = stat.world_state as Record<string, unknown>;
    expect(world.time).toEqual(['08:00', '初始']);
    expect(world.weather).toEqual(['晴朗', '初始']);
    expect(world.location).toEqual(['指挥官学院群岛', '初始']);
    // 嵌套 stats 数字
    const stats = (stat.user as Record<string, Record<string, unknown>>).stats;
    expect(stats.stamina).toEqual([100, '初始']);
    expect(stats.mental_state).toEqual([100, '初始']);
    // 注释行被忽略,不污染键
    const user = stat.user as Record<string, unknown>;
    expect(user.identity_type).toEqual(['Academy_Student', '初始']);
    expect(Object.keys(user)).not.toContain('# 初始默认为学院生');
    // 空容器标量:{} 解析为空对象(与 wuwa 卡 `物品栏: {}` 用例同约定,裸空对象
    // 不作叶子包裹;字符串 "{}" 会让 _.isEmpty 判定失效、UI 渲染异常)
    expect(stat.shipgirls).toEqual({});
    expect(stat.inventory).toEqual({});
    const quest = stat.quest_log as Record<string, unknown>;
    expect(quest.flags).toEqual({});
    expect(quest.main_chapter).toEqual(['Prologue: The Awakening', '初始']);
  });

  it('非 InitVar 条目不参与收集', () => {
    const vars = collectInitVars([
      { comment: '[mvu_update]变量更新规则', content: 'foo: 1' },
      { comment: '变量列表', content: 'bar: 2' },
    ]);
    expect(Object.keys(vars.stat_data)).toHaveLength(0);
  });
});
