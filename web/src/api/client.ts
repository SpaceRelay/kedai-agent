// API 客户端:同源 bootstrap 获取仅驻留内存的 bearer token,统一附加到 REST/SSE 请求。
export const BASE = '/api';

let tokenPromise: Promise<string> | undefined;

export function resetApiTokenForTest(): void {
  tokenPromise = undefined;
}

export async function apiToken(): Promise<string> {
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

export async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  const res = await authorizedFetch(`${BASE}${path}`, init);
  if (!res.ok) {
    let detail = '';
    try {
      const body = (await res.json()) as { error?: string };
      detail = body.error ?? '';
    } catch {
      /* 忽略 */
    }
    throw new Error(detail || `请求失败 ${res.status}`);
  }
  if (res.status === 204) return undefined as T;
  const text = await res.text();
  if (!text) return undefined as T;
  return JSON.parse(text) as T;
}
