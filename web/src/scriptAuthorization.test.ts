import { beforeEach, describe, expect, it } from 'vitest';
import {
  LocalScriptAuthorizationStore,
  hashRegexScripts,
  type StorageLike,
} from './scriptAuthorization';
import type { RegexScript } from './api';

class MemoryStorage implements StorageLike {
  private values = new Map<string, string>();
  getItem(key: string): string | null { return this.values.get(key) ?? null; }
  setItem(key: string, value: string): void { this.values.set(key, value); }
  removeItem(key: string): void { this.values.delete(key); }
}

const scripts: RegexScript[] = [{
  id: 'status',
  script_name: '状态栏',
  find_regex: 'PH',
  replace_string: '<div>状态</div><script>document.body.dataset.ready = "1"</script>',
  enabled: true,
  // 显示渲染类脚本(非隐藏剥除类),必填字段补 false
  markdown_only: false,
}];

describe('角色卡脚本授权', () => {
  let storage: MemoryStorage;
  let auth: LocalScriptAuthorizationStore;

  beforeEach(() => {
    storage = new MemoryStorage();
    auth = new LocalScriptAuthorizationStore(storage);
  });

  it('新角色卡默认未授权，授权 A 不会授权 B', async () => {
    const hash = await hashRegexScripts(scripts);
    expect(auth.isAuthorized('character-a', hash)).toBe(false);
    auth.grant('character-a', hash, new Date('2026-08-09T00:00:00Z'));
    expect(auth.isAuthorized('character-a', hash)).toBe(true);
    expect(auth.isAuthorized('character-b', hash)).toBe(false);
  });

  it('脚本内容变化后原授权自动失效', async () => {
    const oldHash = await hashRegexScripts(scripts);
    auth.grant('character-a', oldHash);
    const changedHash = await hashRegexScripts([
      { ...scripts[0], replace_string: '<script>document.body.dataset.ready = "2"</script>' },
    ]);
    expect(changedHash).not.toBe(oldHash);
    expect(auth.isAuthorized('character-a', changedHash)).toBe(false);
  });

  it('可逐卡撤销并且不会保留损坏 schema', async () => {
    const hash = await hashRegexScripts(scripts);
    auth.grant('character-a', hash);
    auth.revoke('character-a');
    expect(auth.isAuthorized('character-a', hash)).toBe(false);

    storage.setItem('kedai.character-script-authorizations.v1', '{"version":1,"grants":{"x":{"hash":3}}}');
    const reloaded = new LocalScriptAuthorizationStore(storage);
    expect(reloaded.list()).toEqual([]);
  });
});

describe('授权哈希纳入卡级脚本(th-script)', () => {
  const cardScript = { id: 'cs-1', name: '随开场白切换世界书', content: 'eventOn("message_swiped",()=>{});', enabled: true };

  it('含卡级脚本时哈希变化(新执行代码需重新授权)', async () => {
    const regexOnly = await hashRegexScripts(scripts);
    const withCard = await hashRegexScripts(scripts, [cardScript]);
    expect(withCard).not.toBe(regexOnly);
    const auth = new LocalScriptAuthorizationStore(new MemoryStorage());
    auth.grant('character-a', regexOnly);
    expect(auth.isAuthorized('character-a', withCard)).toBe(false);
  });

  it('卡级脚本正文变化哈希随之变化;enabled 翻转也变化', async () => {
    const base = await hashRegexScripts(scripts, [cardScript]);
    const edited = await hashRegexScripts(scripts, [{ ...cardScript, content: 'eventOn("x",()=>{});' }]);
    const toggled = await hashRegexScripts(scripts, [{ ...cardScript, enabled: false }]);
    expect(edited).not.toBe(base);
    expect(toggled).not.toBe(base);
  });

  it('无卡级脚本(缺省/空数组)时哈希与旧行为逐字节一致(老卡不抖动)', async () => {
    const legacy = await hashRegexScripts(scripts);
    const omitted = await hashRegexScripts(scripts);
    const empty = await hashRegexScripts(scripts, []);
    expect(omitted).toBe(legacy);
    expect(empty).toBe(legacy);
  });
});
