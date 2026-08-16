import { describe, it, expect } from 'vitest';
import { detectCardPluginsClient } from './plugins';

// 酒馆助手卡 data_raw(V2 布局,data.character_book)
const assistantCard = {
  spec: 'chara_card_v2',
  data: {
    name: '雪白芽衣',
    character_book: {
      entries: [
        { id: 0, comment: '[InitVar]', content: '心之所向:\n  好感度: 0', enabled: false },
        {
          id: 1,
          comment: '分阶段人设',
          content: "<%_ if (getvar('stat_data.心之所向.好感度') <= 0) { _%>惊弓之鸟<%_ } _%>",
          constant: true,
        },
        {
          id: 2,
          comment: '变量更新规则',
          content:
            '{{format_message_variable::stat_data}}\n<UpdateVariable><Analysis>…</Analysis><JSONPatch>[…]</JSONPatch></UpdateVariable>',
          constant: true,
        },
      ],
    },
  },
};

describe('detectCardPluginsClient(前端兜底检测)', () => {
  it('酒馆助手卡:检出 1 个插件,5 项特性全命中', () => {
    const plugins = detectCardPluginsClient(assistantCard as unknown as Record<string, unknown>);
    expect(plugins).toHaveLength(1);
    const p = plugins[0];
    expect(p.id).toBe('sillytavern-assistant');
    expect(p.name_en).toBe('SillyTavern-Assistant');
    expect(p.enabled).toBe(true);
    expect(p.source).toBe('character_card');
    const detected = (id: string) => p.features.find((f) => f.id === id)?.detected;
    expect(detected('initvar')).toBe(true);
    expect(detected('ejs')).toBe(true);
    expect(detected('variable')).toBe(true);
    expect(detected('status')).toBe(true);
    expect(detected('protocol')).toBe(true);
  });

  it('扁平布局(character_book 在顶层)同样检出', () => {
    const flat = {
      name: '芽衣',
      character_book: {
        entries: [{ id: 0, comment: '[InitVar]', content: '好感度: 0' }],
      },
    };
    const plugins = detectCardPluginsClient(flat as unknown as Record<string, unknown>);
    expect(plugins).toHaveLength(1);
    expect(plugins[0].features.find((f) => f.id === 'initvar')?.detected).toBe(true);
    expect(plugins[0].features.find((f) => f.id === 'ejs')?.detected).toBe(false);
  });

  it('普通卡(无特征)→ 空', () => {
    const plain = {
      data: {
        name: '普通角色',
        character_book: { entries: [{ id: 0, comment: '地点', content: '图书馆安静。', constant: true }] },
      },
    };
    expect(detectCardPluginsClient(plain as unknown as Record<string, unknown>)).toEqual([]);
  });

  it('无 data_raw / 无 entries → 空', () => {
    expect(detectCardPluginsClient(undefined)).toEqual([]);
    expect(detectCardPluginsClient({ name: 'x' })).toEqual([]);
  });
});
