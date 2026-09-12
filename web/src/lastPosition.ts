// 记住上次浏览位置(2026-09 修复「重进后聊天记录丢失」的 UX 主因):
// 重进应用时恢复上次选中的角色,以及该角色上次打开的会话;
// 不再无条件落在「最新创建的角色」上(旧行为会让用户以为聊天记录丢了)。
// 仅存 localStorage(轻偏好);角色/会话已删除时自动失效并回退默认。

const LAST_CHARACTER_KEY = 'kedai.last-character.v1';
const CHAR_SESSION_KEY = 'kedai.char-session.v1';

export function readLastCharacterId(): string | null {
  try {
    return localStorage.getItem(LAST_CHARACTER_KEY);
  } catch {
    return null;
  }
}

export function writeLastCharacterId(id: string | null): void {
  try {
    if (id) localStorage.setItem(LAST_CHARACTER_KEY, id);
    else localStorage.removeItem(LAST_CHARACTER_KEY);
  } catch {
    /* 忽略 */
  }
}

function readCharSessionMap(): Record<string, string> {
  try {
    const raw = localStorage.getItem(CHAR_SESSION_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    return parsed && typeof parsed === 'object' ? (parsed as Record<string, string>) : {};
  } catch {
    return {};
  }
}

/** 该角色上次打开的会话;无记录返回 null */
export function readLastSessionId(characterId: string): string | null {
  return readCharSessionMap()[characterId] ?? null;
}

export function writeLastSessionId(characterId: string, sessionId: string): void {
  try {
    const map = readCharSessionMap();
    map[characterId] = sessionId;
    localStorage.setItem(CHAR_SESSION_KEY, JSON.stringify(map));
  } catch {
    /* 忽略 */
  }
}

/** 删除角色时清理其位置记忆(不留孤儿数据) */
export function removeCharacterPosition(characterId: string): void {
  try {
    const map = readCharSessionMap();
    if (characterId in map) {
      delete map[characterId];
      localStorage.setItem(CHAR_SESSION_KEY, JSON.stringify(map));
    }
    if (readLastCharacterId() === characterId) writeLastCharacterId(null);
  } catch {
    /* 忽略 */
  }
}
