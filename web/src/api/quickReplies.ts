// 快速回复 API:管理 CRUD(list / create / update / delete)
// 后端契约:GET /api/quick-replies[?all=true] → { quick_replies };POST / 返回 { ok, quick_reply };
// PUT/DELETE /api/quick-replies/{id}。缺省 list 只返回启用项(仅启用项参与 getqr 渲染)。
import { request } from './client';
import type { QuickReplyRecord } from './types';

/** 快速回复输入(创建/更新;id 与时间戳由后端维护) */
export type QuickReplyInput = Omit<QuickReplyRecord, 'id' | 'created_at' | 'updated_at'>;

/** 读取快速回复列表;includeDisabled=true 时返回全部(管理 UI 用),缺省仅启用项 */
export async function listQuickReplies(includeDisabled = false): Promise<QuickReplyRecord[]> {
  const qs = includeDisabled ? '?all=true' : '';
  const data = await request<{ quick_replies: QuickReplyRecord[] }>(`/quick-replies${qs}`);
  return data.quick_replies;
}

/** 新建一条快速回复 */
export async function createQuickReply(input: QuickReplyInput): Promise<QuickReplyRecord> {
  const data = await request<{ ok: boolean; quick_reply: QuickReplyRecord }>('/quick-replies', {
    method: 'POST',
    body: JSON.stringify(input),
  });
  return data.quick_reply;
}

/** 更新一条快速回复 */
export async function updateQuickReply(id: number, input: QuickReplyInput): Promise<QuickReplyRecord> {
  const data = await request<{ ok: boolean; quick_reply: QuickReplyRecord }>(`/quick-replies/${id}`, {
    method: 'PUT',
    body: JSON.stringify(input),
  });
  return data.quick_reply;
}

/** 删除一条快速回复 */
export async function deleteQuickReply(id: number): Promise<void> {
  await request<void>(`/quick-replies/${id}`, { method: 'DELETE' });
}
