// 任务模式 API:任务 CRUD + 执行/停止 + 任务事件 SSE 订阅(WP5)。
// 后端契约:GET /api/tasks → { tasks };POST /api/tasks → { ok, task };
// GET /api/tasks/{id} → { task, subtasks };POST /api/tasks/{id}/run | /stop;
// DELETE /api/tasks/{id} → 204;GET /api/tasks/events → SSE(KeepAlive 30s)。
import { BASE, ApiError, apiErrorMessage, authorizedFetch, request } from './client';
import { createSseFrameParser } from './sseParser';
import type { TaskDetail, TaskEvent, TaskLlmCall, TaskRecord, TaskRunMode, TaskStep, TaskUsageTotal } from './types';

/** 读取任务列表(最新在前) */
export async function listTasks(): Promise<TaskRecord[]> {
  const data = await request<{ tasks: TaskRecord[] }>('/tasks');
  return data.tasks;
}

/** 新建任务;character_id 可选(执行者人设角色);task_mode 可选(批次 4 六模式,缺省 legacy) */
export async function createTask(title: string, characterId?: string, taskMode?: TaskRunMode): Promise<TaskRecord> {
  const data = await request<{ ok: boolean; task: TaskRecord }>('/tasks', {
    method: 'POST',
    body: JSON.stringify({ title, character_id: characterId ?? null, task_mode: taskMode ?? 'legacy' }),
  });
  return data.task;
}

/** 读取任务详情(含子任务) */
export async function getTask(id: string): Promise<TaskDetail> {
  return request<TaskDetail>(`/tasks/${id}`);
}

/** 启动任务执行(后台) */
export async function runTask(id: string): Promise<void> {
  await request<{ ok: boolean }>(`/tasks/${id}/run`, { method: 'POST' });
}

/** 停止任务执行 */
export async function stopTask(id: string): Promise<void> {
  await request<{ ok: boolean }>(`/tasks/${id}/stop`, { method: 'POST' });
}

/**
 * 批准计划(plan 模式,批次 4):仅 status='planned' 时合法,否则后端 400;
 * 传入 plan 则替换计划,批准后按 solo 续跑。
 */
export async function approveTask(id: string, plan?: TaskStep[]): Promise<{ ok: boolean }> {
  return request<{ ok: boolean }>(`/tasks/${id}/approve`, {
    method: 'POST',
    body: JSON.stringify(plan ? { plan } : {}),
  });
}

/**
 * 终态追加指令(批次 R2a;R2b+ 扩 mode):仅终态(done/partial/error/ended)可追加,
 * 空指令 400(VALIDATION)/ 非法 mode 400 / 非终态 409(CONFLICT);任务回 running
 * 以「原目标 + 上轮结果 + 追加指令」solo 续跑,产出落 messages。
 * mode=append(缺省)追加进 result;mode=replace 整体替换 result(段标「修订 N」)。
 */
export async function followupTask(
  id: string,
  content: string,
  mode: 'append' | 'replace' = 'append',
): Promise<{ ok: boolean }> {
  return request<{ ok: boolean }>(`/tasks/${id}/followup`, {
    method: 'POST',
    body: JSON.stringify({ content, mode }),
  });
}

/**
 * 批准环节规划对话(批次 R2b):仅 planned 态合法(空反馈 400 / 非 planned 409);
 * 同步等待规划器按反馈修订计划(数秒~数十秒),响应携带修订后计划;
 * 任务保持 planned,需重新批准。
 */
export async function planChatTask(id: string, message: string): Promise<{ ok: boolean; plan: TaskStep[] }> {
  return request<{ ok: boolean; plan: TaskStep[] }>(`/tasks/${id}/plan-chat`, {
    method: 'POST',
    body: JSON.stringify({ message }),
  });
}

/** 删除任务(含子任务) */
export async function deleteTask(id: string): Promise<void> {
  await request<void>(`/tasks/${id}`, { method: 'DELETE' });
}

/** 全部任务 token 累计(侧栏任务模式「全局累计」) */
export async function getTaskUsageTotal(): Promise<TaskUsageTotal> {
  const data = await request<{ usage_total: TaskUsageTotal }>('/tasks/usage-total');
  return data.usage_total;
}

/** 任务 LLM 调用记录(批次 3 L3 调用追踪面板;按 created_at,id 升序) */
export async function getTaskCalls(taskId: string): Promise<TaskLlmCall[]> {
  const data = await request<{ calls: TaskLlmCall[] }>(`/tasks/${encodeURIComponent(taskId)}/calls`);
  return data.calls;
}

/**
 * 订阅任务事件流(GET /api/tasks/events,WP4 后端 SSE,WP5 取代前端 1s 轮询)。
 * 帧解析复用 sseParser.ts 共享层(兼容 CRLF/LF、跨 chunk 分隔、多 data 行拼接),
 * 本层只保留任务流专属语义——长连接,无聊天终态,只有「对端断开 / 网络错误 / 主动关闭」三种结局:
 * - onEvent:每收到一帧 data: {…task 事件…} 触发(坏帧与非 task 类型帧跳过,不阻断后续);
 * - onClose:非 2xx 响应(err 为 ApiError)/ 网络错误 / 流自然结束(err 为空)时触发,由调用方退避重连;
 * - 返回关闭函数:主动关闭走 AbortController,不触发 onClose。
 */
export function streamTaskEvents(
  onEvent: (ev: TaskEvent) => void,
  onClose: (err?: Error) => void,
): () => void {
  const controller = new AbortController();
  /** 主动关闭标记:abort 触发的异常与自然结束都不再回调 onClose */
  let closed = false;
  const close = (): void => {
    if (closed) return;
    closed = true;
    controller.abort();
  };

  void (async () => {
    try {
      const res = await authorizedFetch(`${BASE}/tasks/events`, {
        method: 'GET',
        headers: { Accept: 'text/event-stream' },
        signal: controller.signal,
      });
      if (!res.ok || !res.body) {
        // 错误口径与 client.ts request() 一致:结构化 code 分类 + ApiError
        const body = (await res.json().catch(() => ({}))) as { error?: string; code?: string };
        const code = body.code ?? (res.status === 401 ? 'UNAUTHORIZED' : undefined);
        throw new ApiError(res.status, code, body.error, apiErrorMessage(res.status, code, body.error));
      }

      const reader = res.body.getReader();
      /** 帧解析(共享层):data 文本 → task 事件(非 task 类型/坏帧忽略) */
      const frames = createSseFrameParser((data) => {
        try {
          const ev = JSON.parse(data) as TaskEvent;
          if (ev && ev.type === 'task' && typeof ev.task_id === 'string') onEvent(ev);
        } catch {
          // 单个坏事件不阻断后续事件
        }
      });

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        frames.push(value);
      }
      frames.finish();
      // 流自然结束(对端关闭):非主动关闭,通知调用方重连
      if (!closed) {
        closed = true;
        onClose();
      }
    } catch (e) {
      // 主动关闭的 AbortError 不回调;其余(非 2xx ApiError / 网络错误)通知调用方重连
      if (closed || (e as Error).name === 'AbortError') return;
      closed = true;
      onClose(e as Error);
    }
  })();

  return close;
}
