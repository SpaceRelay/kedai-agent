// 健康检查 API;响应含构建指纹(version/build_id/build_time,由 server-rs/build.rs
// 编译期注入),用于界面展示与「测试版/便携版是否同步」的肉眼核对。
import { request } from './client';

export interface HealthInfo {
  ok: boolean;
  ts: number;
  /** 后端 crate 版本号 */
  version?: string;
  /** 内嵌前端 web/dist 的内容哈希(FNV-1a,16 位十六进制) */
  build_id?: string;
  /** 构建时间(Unix 秒,字符串) */
  build_time?: string;
}

export async function health(): Promise<HealthInfo> {
  return request('/health');
}
