// 健康检查 API
import { request } from './client';

export async function health(): Promise<{ ok: boolean; ts: number }> {
  return request('/health');
}
