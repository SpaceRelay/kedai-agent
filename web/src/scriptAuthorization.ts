import type { RegexScript } from './api';

const STORAGE_KEY = 'kedai.character-script-authorizations.v1';

export interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

export interface ScriptAuthorizationGrant {
  characterId: string;
  scriptHash: string;
  authorizedAt: string;
}

interface StoredSchema {
  version: 1;
  grants: Record<string, { scriptHash: string; authorizedAt: string }>;
}

function emptySchema(): StoredSchema {
  return { version: 1, grants: {} };
}

function parseSchema(raw: string | null): StoredSchema {
  if (!raw) return emptySchema();
  try {
    const value = JSON.parse(raw) as unknown;
    if (!value || typeof value !== 'object') return emptySchema();
    const obj = value as { version?: unknown; grants?: unknown };
    if (obj.version !== 1 || !obj.grants || typeof obj.grants !== 'object' || Array.isArray(obj.grants)) {
      return emptySchema();
    }
    const grants: StoredSchema['grants'] = {};
    for (const [characterId, grant] of Object.entries(obj.grants as Record<string, unknown>)) {
      if (!characterId || !grant || typeof grant !== 'object') continue;
      const item = grant as { scriptHash?: unknown; authorizedAt?: unknown };
      if (typeof item.scriptHash !== 'string' || !/^[a-f0-9]{64}$/.test(item.scriptHash)) continue;
      if (typeof item.authorizedAt !== 'string' || Number.isNaN(Date.parse(item.authorizedAt))) continue;
      grants[characterId] = { scriptHash: item.scriptHash, authorizedAt: item.authorizedAt };
    }
    return { version: 1, grants };
  } catch {
    return emptySchema();
  }
}

/** 仅保存角色 ID、脚本哈希和授权时间，不保存脚本正文。 */
export class LocalScriptAuthorizationStore {
  constructor(private readonly storage: StorageLike) {}

  private read(): StoredSchema {
    return parseSchema(this.storage.getItem(STORAGE_KEY));
  }

  private write(value: StoredSchema): void {
    this.storage.setItem(STORAGE_KEY, JSON.stringify(value));
  }

  isAuthorized(characterId: string, scriptHash: string): boolean {
    if (!characterId || !scriptHash) return false;
    return this.read().grants[characterId]?.scriptHash === scriptHash;
  }

  grant(characterId: string, scriptHash: string, now = new Date()): void {
    if (!characterId || !/^[a-f0-9]{64}$/.test(scriptHash)) throw new Error('角色 ID 或脚本哈希无效');
    const value = this.read();
    value.grants[characterId] = { scriptHash, authorizedAt: now.toISOString() };
    this.write(value);
  }

  revoke(characterId: string): void {
    const value = this.read();
    delete value.grants[characterId];
    this.write(value);
  }

  list(): ScriptAuthorizationGrant[] {
    return Object.entries(this.read().grants).map(([characterId, grant]) => ({
      characterId,
      scriptHash: grant.scriptHash,
      authorizedAt: grant.authorizedAt,
    }));
  }
}

/** 对会影响匹配、HTML 输出或 JavaScript 执行的字段做稳定 SHA-256。
 *  cardScripts(可选):角色卡内嵌酒馆助手卡级脚本(data_raw.extensions.tavern_helper),
 *  其 content 直接构成执行体,必须进哈希——卡更新卡级脚本后旧授权自动失效;
 *  缺省/空数组时哈希与历史行为逐字节一致(无卡级脚本的老卡不抖动)。 */
export async function hashRegexScripts(
  scripts: RegexScript[],
  cardScripts?: Array<{ id: string; name: string; content: string; enabled: boolean }>,
): Promise<string> {
  const canonical: Array<Record<string, unknown>> = scripts.map((script) => ({
    id: script.id ?? '',
    script_name: script.script_name ?? '',
    find_regex: script.find_regex ?? '',
    replace_string: script.replace_string ?? '',
    enabled: script.enabled !== false,
    markdown_only: script.markdown_only === true,
  }));
  for (const s of cardScripts ?? []) {
    canonical.push({
      kind: 'th-script',
      id: s.id,
      name: s.name,
      content: s.content,
      enabled: s.enabled,
    });
  }
  const bytes = new TextEncoder().encode(JSON.stringify(canonical));
  const digest = await crypto.subtle.digest('SHA-256', bytes);
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, '0')).join('');
}
