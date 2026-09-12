// 回退快照(undo)API(批次 6.1b):写工具执行前的状态快照查询与回退。
// 后端契约:GET /api/chat/sessions/{id}/undo → { snapshots }(新→旧排序);
// POST /api/undo/{id}/restore(空 JSON 体)→ { ok };失败后端 500 带中文错误。
// 快照仅对写工具(write/replace/create/update_variables/memory_write)且 undo_enabled 开时产生;
// 恢复成功后该快照及同会话更新的快照被服务端清除。
import { request } from './client';

/** 回退快照(写工具执行前的状态存档) */
export interface UndoSnapshot {
  id: string;
  /** 产生快照的写工具名(write / replace / create / update_variables / memory_write) */
  tool_name: string;
  /** 中文简述标签(如「写入文件 notes.md」「更新变量树」) */
  label: string;
  /** 锚定消息 id(快照产生时所在的聊天消息;可能为 null) */
  anchor_message_id: number | null;
  created_at: string;
}

/** 读取会话的回退快照列表(新→旧排序) */
export async function listUndoSnapshots(sessionId: string): Promise<UndoSnapshot[]> {
  const data = await request<{ snapshots: UndoSnapshot[] }>(
    `/chat/sessions/${encodeURIComponent(sessionId)}/undo`,
  );
  return data.snapshots;
}

/** 回退到指定快照(成功后该快照及同会话更新的快照被清除) */
export async function restoreUndoSnapshot(id: string): Promise<void> {
  await request<{ ok: boolean }>(`/undo/${encodeURIComponent(id)}/restore`, {
    method: 'POST',
    body: JSON.stringify({}),
  });
}
