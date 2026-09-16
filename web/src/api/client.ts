// API 客户端:同源 bootstrap 获取仅驻留内存的 bearer token,统一附加到 REST/SSE 请求。
export const BASE = '/api';

let tokenPromise: Promise<string> | undefined;

/** 丢弃缓存 token(bootstrap 失败 / 401 重试前调用;测试经 resetApiTokenForTest 复用) */
function forgetApiToken(): void {
  tokenPromise = undefined;
}

export function resetApiTokenForTest(): void {
  forgetApiToken();
}

export async function apiToken(): Promise<string> {
  // 失败不缓存:后端未就绪等瞬态失败若缓存 rejected promise,之后所有 API 调用
  // 会永久连带失败(整个应用空数据,只能手动刷新)。失败即丢弃,下次调用重新引导。
  tokenPromise ??= fetch(`${BASE}/bootstrap`, {
    cache: 'no-store',
    credentials: 'same-origin',
    headers: { 'X-Kedai-Boot': '1' }
  })
    .then(async (res) => {
      if (!res.ok) throw new Error(`API 初始化失败 ${res.status}`);
      const body = await res.json() as { token?: string };
      if (!body.token) throw new Error('API 初始化响应缺少 token');
      return body.token;
    })
    .catch((e: unknown) => {
      forgetApiToken();
      throw e;
    });
  return tokenPromise;
}

export async function authorizedHeaders(init?: HeadersInit, json = true): Promise<Headers> {
  const headers = new Headers(init);
  if (json && !headers.has('Content-Type')) headers.set('Content-Type', 'application/json');
  headers.set('Authorization', `Bearer ${await apiToken()}`);
  // 同源标记头:配合后端 KEDAI_STRICT_CLIENT_HEADER=1 时的写请求强化校验(默认关,加了无害)。
  headers.set('X-Kedai-Client', 'kedai-web');
  return headers;
}

export async function authorizedFetch(input: RequestInfo | URL, init: RequestInit = {}, json = true): Promise<Response> {
  return fetch(input, { ...init, headers: await authorizedHeaders(init.headers, json) });
}

/** 服务端错误响应体(2026-08 起带结构化 code,见 server-rs/src/api/errors.rs)。 */
interface ApiErrorBody {
  error?: string;
  code?: string;
}

/**
 * API 错误:在 Error.message(用户可读提示)之上附 code/status/detail 供程序化分支。
 * 调用方接口不变:仍是 catch 到 Error,读 message 即可。
 */
export class ApiError extends Error {
  /** 服务端结构化错误码(如 NOT_FOUND);未接入错误码的旧端点可能缺失 */
  readonly code?: string;
  /** HTTP 状态码 */
  readonly status: number;
  /** 服务端原始 error 文案(调试/日志用;用户提示以 message 为准) */
  readonly detail?: string;

  constructor(status: number, code: string | undefined, detail: string | undefined, message: string) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
    this.detail = detail;
  }
}

/**
 * 按错误码生成用户提示(错误对象 message 的统一口径):
 * - NOT_FOUND / CONFLICT / VALIDATION:透传服务端原文(用户可据此直接纠正操作)
 * - DB / INTERNAL / UPSTREAM:统一「服务端错误,请查看日志」(原文保留在 detail 供排查)
 * - UNAUTHORIZED:提示重新加载页面(bearer token 失效)
 * - 无 code(旧端点):兜底原文或 `请求失败 {status}`
 */
export function apiErrorMessage(status: number, code?: string, detail?: string): string {
  const fallback = detail?.trim() || `请求失败 ${status}`;
  switch (code) {
    case 'NOT_FOUND':
    case 'CONFLICT':
    case 'VALIDATION':
      return fallback;
    case 'DB':
    case 'INTERNAL':
    case 'UPSTREAM':
      return '服务端错误,请查看日志';
    case 'UNAUTHORIZED':
      return '登录状态已失效,请重新加载页面';
    default:
      return fallback;
  }
}

/**
 * 单次 401 重引导重试:首次响应为 401 时丢弃缓存 token、重新 bootstrap,原样重试一次。
 *
 * 服务端重启后 bearer token 轮换,这是恢复路径。**重试安全性**:401 由鉴权中间件在
 * handler 之前返回,**首请求未被执行**,故非幂等的 POST/上传重试也不会产生重复写入。
 * 重新引导失败(后端未就绪)或重试仍 401 时,**原样返回**最后一次响应,由调用方按
 * 401 报错——不在这里抛异常,以便调用方统一走各自的错误出口。
 */
export async function retryOnUnauthorized(doFetch: () => Promise<Response>): Promise<Response> {
  const res = await doFetch();
  if (res.status !== 401) return res;
  forgetApiToken();
  let refreshed = false;
  try {
    await apiToken();
    refreshed = true;
  } catch {
    /* 重新引导失败:按原响应的 401 处理 */
  }
  return refreshed ? doFetch() : res;
}

export async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  const res = await retryOnUnauthorized(() => authorizedFetch(`${BASE}${path}`, init));
  if (!res.ok) {
    let body: ApiErrorBody = {};
    try {
      body = (await res.json()) as ApiErrorBody;
    } catch {
      /* 忽略非 JSON 错误体 */
    }
    // 401 即使未带 code 也按 token 失效处理(如代理/旧端点路径)
    const code = body.code ?? (res.status === 401 ? 'UNAUTHORIZED' : undefined);
    throw new ApiError(res.status, code, body.error, apiErrorMessage(res.status, code, body.error));
  }
  if (res.status === 204) return undefined as T;
  const text = await res.text();
  if (!text) return undefined as T;
  return JSON.parse(text) as T;
}
