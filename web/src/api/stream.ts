// 流式与上传请求底座(SSE 读循环 + multipart 上传)。
//
// **收口背景(2026-09-16)**:`/api/chat/send` 与 `/api/tasks/events` 各自逐行复制
// 「非 2xx 错误体解析 + getReader 读循环 + finish」;4 处 multipart 上传
// (characters / plugins / settings / world-books)各自手写
// `throw new Error(body.error ?? '上传失败')`,错误口径与主链路 `client.ts` 的
// `ApiError` + `apiErrorMessage` 不一致(前端拿到的是无 code 的裸 Error)。
//
// 本模块只收敛**共通机制**,流各自的语义保留在调用方:
//   - 聊天流「必发终态」:非 2xx 时补合成 step + finish 事件复位 generating;
//   - 任务流「长连接」:非 2xx 时抛 ApiError,由调用方退避重连(重连算法不动)。
import { ApiError, BASE, apiErrorMessage, authorizedFetch, retryOnUnauthorized } from './client';
import { createSseFrameParser } from './sseParser';

/** 服务端错误响应的解出结果(供调用方自行决定合成终态还是抛错) */
export interface ApiErrorParts {
  status: number;
  code?: string;
  detail?: string;
}

/**
 * 解析非 2xx 响应体为错误三元组。
 * 非 JSON 错误体(代理返回 HTML、413 空体等)不抛异常,code/detail 留空后由
 * `apiErrorMessage` 兜底为 `请求失败 {status}`。
 */
export async function readErrorParts(res: Response): Promise<ApiErrorParts> {
  let body: { error?: string; code?: string } = {};
  try {
    body = (await res.json()) as { error?: string; code?: string };
  } catch {
    /* 忽略非 JSON 错误体 */
  }
  // 401 即使未带 code 也按 token 失效处理(如代理/旧端点路径),与 client.ts 同口径
  const code = body.code ?? (res.status === 401 ? 'UNAUTHORIZED' : undefined);
  return { status: res.status, code, detail: body.error };
}

/** 解析非 2xx 响应为 ApiError(任务流等「抛错交给调用方重连」的场景用) */
export async function toApiError(res: Response): Promise<ApiError> {
  const { status, code, detail } = await readErrorParts(res);
  return new ApiError(status, code, detail, apiErrorMessage(status, code, detail));
}

/**
 * 消费 SSE 响应体:`getReader()` 读循环 + 共享帧解析 + EOF 冲刷。
 *
 * 只负责「把字节流喂给帧解析器」,不含任何终态/重连语义——终态判定归调用方
 * (聊天流看是否收到 finish/interrupted/error,任务流看是否自然结束)。
 * 响应无 body 时直接返回(调用方按错误分支处理)。
 */
export async function pumpSseFrames(
  res: Response,
  onData: (data: string) => void,
): Promise<void> {
  if (!res.body) return;
  const reader = res.body.getReader();
  const frames = createSseFrameParser(onData);
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    frames.push(value);
  }
  frames.finish();
}

/**
 * multipart 上传(四个上传端点共用)。
 *
 * - 表单字段:`file` 为文件,`extra` 为可选附加字段(如 character_id);
 * - 不设 Content-Type:必须留给浏览器自动补上 multipart boundary;
 * - 401 自动重引导重试一次(见 `retryOnUnauthorized`):上传虽是 POST,但 401 由
 *   鉴权中间件在 handler 之前返回,**首请求未被执行**,重试不会产生重复写入。
 */
export async function uploadForm<T>(
  path: string,
  file: File,
  extra: Record<string, string> = {},
): Promise<T> {
  const form = new FormData();
  form.append('file', file);
  for (const [k, v] of Object.entries(extra)) form.append(k, v);

  const res = await retryOnUnauthorized(() =>
    authorizedFetch(`${BASE}${path}`, { method: 'POST', body: form }, false),
  );
  if (!res.ok) throw await toApiError(res);
  return (await res.json()) as T;
}
