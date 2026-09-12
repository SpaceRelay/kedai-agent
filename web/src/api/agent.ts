// Agent 计划与自定义执行流程(agent-flows)API
import { request } from './client';
import type { AgentFlowConfig, AgentFlowLibrary, AgentMode, AgentPlan, ToolPermission } from './types';

export async function getToolPermissions(sessionId: string, characterId: string): Promise<ToolPermission[]> {
  const query = new URLSearchParams({ session_id: sessionId, character_id: characterId });
  const data = await request<{ tools: ToolPermission[] }>(`/agent/tool-permissions?${query}`);
  return data.tools;
}

/** 该会话/角色的显式授权(session_grants / role_grants),供授权管理区展示与撤销 */
export async function getGrants(
  sessionId: string,
  characterId: string,
): Promise<{ session: string[]; role: string[] }> {
  const query = new URLSearchParams({ session_id: sessionId, character_id: characterId });
  const data = await request<{ grants?: { session?: string[]; role?: string[] } }>(
    `/agent/tool-permissions?${query}`,
  );
  return { session: data.grants?.session ?? [], role: data.grants?.role ?? [] };
}

export async function authorizeTool(tool: string, scope: 'session' | 'role', sessionId: string): Promise<void> {
  // 10s 超时:防止请求挂起导致前端 authorizing 永久占用、授权按钮全部禁用
  await request('/agent/tool-permissions', {
    method: 'POST',
    body: JSON.stringify({ session_id: sessionId, tool, scope }),
    signal: AbortSignal.timeout(10000),
  });
}

export async function resolveToolAuthorization(
  tool: string,
  decision: 'once' | 'session' | 'role' | 'deny',
  sessionId: string,
  runId: string,
  callId: string,
): Promise<void> {
  await request('/agent/tool-permissions/resolve', {
    method: 'POST',
    body: JSON.stringify({ session_id: sessionId, tool, scope: decision, run_id: runId, call_id: callId }),
    signal: AbortSignal.timeout(10000),
  });
}

export async function revokeTool(tool: string, scope: 'session' | 'role', sessionId: string): Promise<void> {
  await request('/agent/tool-permissions', {
    method: 'DELETE',
    body: JSON.stringify({ session_id: sessionId, tool, scope }),
  });
}

export async function agentPlan(
  message: string,
  agentMode: AgentMode,
  sessionId?: string,
): Promise<AgentPlan> {
  return request('/agent/plan', {
    method: 'POST',
    body: JSON.stringify({ message, agent_mode: agentMode, session_id: sessionId }),
  });
}

/** GET /api/agent-flows:读取流程库 + 当前选中流程 */
export async function getAgentFlow(): Promise<{ library: AgentFlowLibrary; config: AgentFlowConfig | null }> {
  const data = await request<{ ok: boolean; library: AgentFlowLibrary; config: AgentFlowConfig | null }>(
    '/agent-flows',
  );
  return { library: data.library, config: data.config };
}

/** PUT /api/agent-flows:保存(创建或更新)流程并设为当前选中;id 为空 = 新建(校验失败 400) */
export async function saveAgentFlow(config: AgentFlowConfig): Promise<AgentFlowLibrary> {
  const data = await request<{ ok: boolean; library: AgentFlowLibrary }>('/agent-flows', {
    method: 'PUT',
    body: JSON.stringify({ config }),
  });
  return data.library;
}

/** POST /api/agent-flows/select:切换当前选中流程 */
export async function selectAgentFlow(id: string): Promise<AgentFlowLibrary> {
  const data = await request<{ ok: boolean; library: AgentFlowLibrary }>('/agent-flows/select', {
    method: 'POST',
    body: JSON.stringify({ id }),
  });
  return data.library;
}

/** DELETE /api/agent-flows/{id}:删除流程(删除当前流程时回退到第一个) */
export async function deleteAgentFlow(id: string): Promise<AgentFlowLibrary> {
  const data = await request<{ ok: boolean; library: AgentFlowLibrary }>(`/agent-flows/${encodeURIComponent(id)}`, {
    method: 'DELETE',
  });
  return data.library;
}
