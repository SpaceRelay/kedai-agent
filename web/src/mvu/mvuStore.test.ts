// mvuStore 回放与命令应用:开场白 <initvar> 播种语义
// 背景:wuwa 卡 14 个备用开场尾部携带 <UpdateVariable><initvar>YAML</initvar></UpdateVariable>,
// 开场白是种子消息(不经 applyMvuUpdate、无 extra.mvu 快照),不支持 initvar 时变量树恒空,
// 状态栏 24 处取值全部显示 "--"。
import { describe, expect, it } from 'vitest';
import { applyMvuCommands, replayMvuVariables } from './mvuStore';
import { pathGet, statValue } from './variables';
import { emptyVariables } from './variables';

const GREETING = [
  '守岸人抬眸望向你:「你醒了。」',
  '<UpdateVariable>',
  '<initvar>',
  '当前时间: "第1年 10月13日 周日 14:00"',
  '所在地点: "黑海岸-泰缇斯枢纽-花房"',
  '主角信息:',
  '  是否是漂泊者: true',
  '女性角色:',
  '  守岸人:',
  '    好感度: 80',
  '</initvar>',
  '</UpdateVariable>',
  '<StatusPlaceHolderImpl/>',
].join('\n');

describe('replayMvuVariables + <initvar>', () => {
  it('无快照的开场白携带 initvar 时播种初始变量', () => {
    const vars = replayMvuVariables([{ role: 'assistant', content: GREETING, extra: {} }]);
    expect(statValue(pathGet(vars.stat_data, '当前时间'))).toBe('第1年 10月13日 周日 14:00');
    expect(statValue(pathGet(vars.stat_data, '女性角色.守岸人.好感度'))).toBe(80);
    expect(statValue(pathGet(vars.stat_data, '主角信息.是否是漂泊者'))).toBe(true);
    // display_data 镜像 "v->v(初始)"
    expect(pathGet(vars.display_data, '女性角色.守岸人.好感度')).toBe('80->80(初始)');
  });

  it('无 initvar 的普通消息不播种', () => {
    const vars = replayMvuVariables([{ role: 'assistant', content: '普通回复', extra: {} }]);
    expect(Object.keys(vars.stat_data)).toHaveLength(0);
  });

  it('快照出现后不再播种(快照是真相,避免复活已删除键)', () => {
    const vars = replayMvuVariables([
      { role: 'assistant', content: GREETING, extra: {} },
      {
        role: 'assistant',
        content: 'x',
        extra: { mvu: { stat_data: { 女性角色: { 守岸人: { 好感度: [85, '约会'] } } } } },
      },
      // 第二条无快照但带 initvar 的消息(理论不出现):不应再播种
      { role: 'assistant', content: GREETING, extra: {} },
    ]);
    expect(statValue(pathGet(vars.stat_data, '女性角色.守岸人.好感度'))).toBe(85);
    // 第二条 GREETING 未播种:好感度保持快照的 85 而非初始 80
  });

  it('快照合并覆盖播种底(推进顺序:播种 → 快照)', () => {
    const vars = replayMvuVariables([
      { role: 'assistant', content: GREETING, extra: {} },
      {
        role: 'assistant',
        content: 'x',
        extra: { mvu: { stat_data: { 当前时间: ['第1年 10月14日 周一 08:00', '推进'] } } },
      },
    ]);
    expect(statValue(pathGet(vars.stat_data, '当前时间'))).toBe('第1年 10月14日 周一 08:00');
    // 快照未覆盖的键保留播种值
    expect(statValue(pathGet(vars.stat_data, '所在地点'))).toBe('黑海岸-泰缇斯枢纽-花房');
  });
});

describe('applyMvuCommands + <initvar>', () => {
  it('变量树为空时先播种再应用命令', () => {
    const vars = applyMvuCommands(
      emptyVariables(),
      [{ path: '女性角色.守岸人.好感度', oldValue: 80, newValue: 85, reason: '约会' }],
      '女性角色:\n  守岸人:\n    好感度: 80\n当前时间: "14:00"',
    );
    // 命令覆盖播种值;未触及的播种键保留
    expect(statValue(pathGet(vars.stat_data, '女性角色.守岸人.好感度'))).toBe(85);
    expect(statValue(pathGet(vars.stat_data, '当前时间'))).toBe('14:00');
  });

  it('变量树非空时 initvar 不覆盖进行中进度', () => {
    const cur = emptyVariables();
    cur.stat_data['当前时间'] = ['第2年 01月01日', '推进'];
    const vars = applyMvuCommands(cur, [], '当前时间: "第1年 10月13日"');
    expect(statValue(pathGet(vars.stat_data, '当前时间'))).toBe('第2年 01月01日');
  });

  it('无 initvar 时行为与原先一致', () => {
    const vars = applyMvuCommands(emptyVariables(), [
      { path: 'a.b', oldValue: null, newValue: 1, reason: 'r' },
    ]);
    expect(statValue(pathGet(vars.stat_data, 'a.b'))).toBe(1);
  });
});
