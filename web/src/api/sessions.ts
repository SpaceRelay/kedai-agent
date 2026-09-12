// 会话 / 消息 API(含 history / clear / export / import)
import { request } from './client';
import type {
  ChatMessage,
  MessageRecord,
  MvuSnapshot,
  SessionInfo,
  SessionWithCharacter,
  StMessage,
} from './types';

export async function listSessions(characterId: string): Promise<SessionInfo[]> {
  const data = await request<{ sessions: SessionInfo[] }>(`/chat/sessions?character_id=${encodeURIComponent(characterId)}`);
  return data.sessions;
}

/** 全部会话(联表角色名 + 消息数)——聊天记录面板 */
export async function listAllSessions(): Promise<SessionWithCharacter[]> {
  const data = await request<{ sessions: SessionWithCharacter[] }>('/chat/sessions');
  return data.sessions;
}

/** 新建会话;greetingIndex 指定开场序号(0=主开场,1..=备用开场;缺省 0) */
export async function createSession(characterId: string, title?: string, greetingIndex?: number): Promise<SessionInfo> {
  return request('/chat/sessions', {
    method: 'POST',
    body: JSON.stringify({
      character_id: characterId,
      title,
      ...(greetingIndex !== undefined ? { greeting_index: greetingIndex } : {}),
    }),
  });
}

/** 会话内切换开场:清空会话消息后按 greetingIndex 重新播种开场 */
export async function regreetSession(sessionId: string, greetingIndex: number): Promise<{ ok: boolean }> {
  return request<{ ok: boolean }>(`/chat/sessions/${encodeURIComponent(sessionId)}/regreet`, {
    method: 'POST',
    body: JSON.stringify({ greeting_index: greetingIndex }),
  });
}

export async function deleteSession(id: string): Promise<void> {
  await request<void>(`/chat/sessions/${id}`, { method: 'DELETE' });
}

export async function fetchHistory(sessionId: string): Promise<ChatMessage[]> {
  const data = await request<{ messages: ChatMessage[] }>(`/chat/history?session_id=${encodeURIComponent(sessionId)}`);
  return data.messages;
}

/** 角色扮演 Agent 记录(只读):agent_sessions.state/plan/step_index + tool_calls。
 *  无记录时后端返回 trace: null,前端保持空闲态(实跑问题 4)。 */
export interface AgentTrace {
  state: string;
  plan: string[];
  step_index: number;
  tool_calls: Array<{
    id: string;
    name: string;
    input: unknown;
    output: unknown;
    duration_ms: number;
    created_at: string;
  }>;
}

/** GET /api/chat/sessions/{id}/agent/trace:切会话/重启后恢复右侧 Agent 面板记录 */
export async function fetchAgentTrace(sessionId: string): Promise<AgentTrace | null> {
  const data = await request<{ trace: AgentTrace | null }>(
    `/chat/sessions/${encodeURIComponent(sessionId)}/agent/trace`,
  );
  return data.trace;
}

export async function updateMessage(sessionId: string, id: number, content: string): Promise<ChatMessage> {
  return request(`/chat/messages/${id}?session_id=${encodeURIComponent(sessionId)}`, {
    method: 'PUT',
    body: JSON.stringify({ content }),
  });
}

export async function deleteMessage(sessionId: string, id: number): Promise<void> {
  await request(`/chat/messages/${id}?session_id=${encodeURIComponent(sessionId)}`, { method: 'DELETE' });
}

/** 切换消息 swipe 版本(阶段六 6f):返回激活版本内容与版本索引 */
export async function swipeMessage(
  sessionId: string,
  id: number,
  swipeId: number,
): Promise<{ content: string; swipe_id: number; swipes_count: number }> {
  return request(`/chat/messages/${id}/swipe?session_id=${encodeURIComponent(sessionId)}`, {
    method: 'POST',
    body: JSON.stringify({ swipe_id: swipeId }),
  });
}

/** 截断:保留 anchorId 消息,删除其后所有消息(供「编辑用户消息后重发」) */
export async function truncateMessages(
  sessionId: string,
  anchorId: number,
): Promise<{ ok: boolean; deleted: number }> {
  return request(`/chat/sessions/${encodeURIComponent(sessionId)}/truncate`, {
    method: 'POST',
    body: JSON.stringify({ anchor_id: anchorId }),
  });
}

/** 保存消息 mvu 变量快照到 extra.mvu */
export async function saveMessageVariables(
  sessionId: string,
  id: number,
  snap: MvuSnapshot,
): Promise<MessageRecord> {
  return request<MessageRecord>(`/chat/messages/${id}/variables?session_id=${encodeURIComponent(sessionId)}`, {
    method: 'PATCH',
    body: JSON.stringify(snap),
  });
}

/** 覆盖会话级 mvu 变量树(整树覆写;用于编辑/重发截断后的变量回滚) */
export async function saveAssistantVars(
  sessionId: string,
  statData: Record<string, unknown>,
): Promise<{ ok: boolean }> {
  return request<{ ok: boolean }>(
    `/chat/sessions/${encodeURIComponent(sessionId)}/assistant-vars`,
    {
      method: 'PUT',
      body: JSON.stringify({ stat_data: statData }),
    },
  );
}

/** 获取角色 [InitVar] 初始变量条目 */
export async function fetchInitVars(characterId: string): Promise<Record<string, string>> {
  const data = await request<{ entries: Record<string, string> }>(
    `/chat/init-vars?character_id=${encodeURIComponent(characterId)}`,
  );
  return data.entries ?? {};
}

export async function clearMessages(sessionId: string): Promise<void> {
  await request<{ ok: boolean }>('/chat/clear', { method: 'POST', body: JSON.stringify({ session_id: sessionId }) });
}

export async function exportChat(sessionId: string): Promise<{ session_id: string; messages: StMessage[] }> {
  return request(`/export/chat?session_id=${encodeURIComponent(sessionId)}`);
}

export async function importChat(sessionId: string, messages: StMessage[]): Promise<{ ok: boolean; imported: number }> {
  return request('/import/chat', { method: 'POST', body: JSON.stringify({ session_id: sessionId, messages }) });
}
