// 记忆库 API:跨会话记忆蒸馏(server-rs/src/api/memory.rs)。
//   POST   /api/memory/distill   蒸馏指定会话(需开启 memory_distill_enabled)
//   GET    /api/memory           按角色列出全部记忆(最新在前,含未选中条目与计数)
//   GET    /api/memory/search    FTS5 全文检索(短查询后端回退 LIKE;limit 默认 20 上限 100)
//   POST   /api/memory           手动添加(kind='manual')
//   POST   /api/memory/prune     硬删除该角色已归档(selected=0)条目
//   PATCH  /api/memory/:id       编辑 content / selected / pinned
//   DELETE /api/memory/:id       删除(204 无正文)
import { request } from './client';

/** 记忆条目(memory_entries 表行;kind: distilled | tool | manual) */
export interface MemoryEntry {
  id: number;
  character_id: string;
  /** 来源会话(蒸馏记忆记录来源;手动/工具记忆为 null) */
  source_session_id: string | null;
  kind: string;
  content: string;
  /** 注入次数(每次注入后 +1) */
  usage_count: number;
  /** 最近一次注入时间(null = 从未注入) */
  last_usage: string | null;
  /** 是否参与注入(精选开关) */
  selected: boolean;
  /** 分层注入最高优先级(1 = 常驻置顶,排序时优先于 usage) */
  pinned: boolean;
  created_at: string;
  updated_at: string;
}

/** 蒸馏结果:inserted = 本次落库条数(空历史为 0,不调模型) */
export interface DistillResult {
  ok: boolean;
  inserted: number;
  character_id: string;
}

/** 清理归档结果:deleted = 本次硬删除的已归档条目数 */
export interface PruneResult {
  ok: boolean;
  deleted: number;
}

/** GET /api/memory?character_id=:角色全部记忆(最新在前) */
export async function listMemories(characterId: string): Promise<MemoryEntry[]> {
  const data = await request<{ memories: MemoryEntry[] }>(
    `/memory?character_id=${encodeURIComponent(characterId)}`,
  );
  return data.memories;
}

/** GET /api/memory/search?character_id=&q=&limit=:FTS5 全文检索(limit 默认 20,上限 100) */
export async function searchMemories(
  characterId: string,
  q: string,
  limit = 20,
): Promise<MemoryEntry[]> {
  const params = new URLSearchParams({
    character_id: characterId,
    q,
    limit: String(limit),
  });
  const data = await request<{ memories: MemoryEntry[] }>(`/memory/search?${params}`);
  return data.memories;
}

/** POST /api/memory/prune:硬删除该角色已归档(selected=0)条目,返回删除条数 */
export async function pruneMemories(
  characterId: string,
): Promise<{ ok: boolean; deleted: number }> {
  // 后端字段名为 removed(server-rs/src/api/memory.rs),deleted 兼容未来改名
  const data = await request<{ ok: boolean; removed?: number; deleted?: number }>(
    '/memory/prune',
    { method: 'POST', body: JSON.stringify({ character_id: characterId }) },
  );
  return { ok: data.ok, deleted: data.deleted ?? data.removed ?? 0 };
}

/** POST /api/memory/distill:蒸馏当前会话为角色记忆(未开启蒸馏时 400 带指引文案) */
export async function distillMemory(sessionId: string): Promise<DistillResult> {
  return request('/memory/distill', {
    method: 'POST',
    body: JSON.stringify({ session_id: sessionId }),
  });
}

/** POST /api/memory:手动添加一条记忆(kind='manual'),返回落库条目 */
export async function createMemory(characterId: string, content: string): Promise<MemoryEntry> {
  const data = await request<{ ok: boolean; memory: MemoryEntry }>('/memory', {
    method: 'POST',
    body: JSON.stringify({ character_id: characterId, content }),
  });
  return data.memory;
}

/** PATCH /api/memory/:id:编辑 content / selected / pinned(空白 content 后端拒绝) */
export async function updateMemory(
  id: number,
  patch: { content?: string; selected?: boolean; pinned?: boolean },
): Promise<MemoryEntry> {
  const data = await request<{ ok: boolean; memory: MemoryEntry }>(`/memory/${id}`, {
    method: 'PATCH',
    body: JSON.stringify(patch),
  });
  return data.memory;
}

/** DELETE /api/memory/:id:删除一条记忆(204 无正文) */
export async function deleteMemory(id: number): Promise<void> {
  await request(`/memory/${id}`, { method: 'DELETE' });
}

/** 向量索引状态(Phase 3):embedded/total 表示回填进度;dim_mismatch 非空表示需重建 */
export interface EmbeddingStatus {
  enabled: boolean;
  model: string;
  configured_dim: number;
  status: {
    total: number;
    embedded: number;
    dim: number | null;
    dim_mismatch: string | null;
  };
}

/** GET /api/memory/embedding-status:向量索引状态 */
export async function getEmbeddingStatus(): Promise<EmbeddingStatus> {
  return request('/memory/embedding-status');
}

/** POST /api/memory/rebuild-embeddings:手动重建向量索引(清表 + 全量回填) */
export async function rebuildEmbeddings(): Promise<{
  ok: boolean;
  embedded: number;
  dim: number;
}> {
  return request('/memory/rebuild-embeddings', { method: 'POST', body: '{}' });
}
