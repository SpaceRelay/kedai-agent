// 响应形状闸门(批次 1 · 错误面收口)。
//
// 动机(实测教训,源自 diagnostics.ts 的 assertDiagnostics):`request()` **只按 HTTP
// 状态判成败**。历史上后端曾在失败时返回 200 + `{"error": ...}`,或字段改名后仍回
// 200;此时 `const data = await request<T>(...)` 的泛型是**编译期谎言**,调用方
// `data.memories` / `data.model` 会解出 undefined,并在下游(组件渲染、`v-for`、
// 长度计算)以随机 TypeError 崩溃——报错点离真正原因很远,难查。
//
// 本模块把「载荷形状不对」收敛为**可捕获的 Error**,并优先透出服务端 error 原文
// (旧版 200 错误体可直接显示给用户)。纪律:只做最小存在性校验,不做全字段类型
// 穷举——过度校验会把「后端加字段」误判为故障。
//
// 使用边界:仅用于 `request<T>()` 后**直接解构**的封装。已是简单透传、或调用方本
// 就按可选字段处理的接口不必套用(套了是无谓的耦合)。

/** 从疑似错误载荷中取出服务端 error 原文(旧版 200 错误体的兼容读取) */
function serverError(value: unknown): string | undefined {
  if (typeof value === 'object' && value !== null) {
    const err = (value as { error?: unknown }).error;
    if (typeof err === 'string' && err.trim()) return err;
  }
  return undefined;
}

/** 统一失败出口:有服务端文案则透出,否则给出「哪个接口的响应格式异常」 */
function shapeFail(what: string, value: unknown): never {
  throw new Error(serverError(value) ?? `${what}响应格式异常`);
}

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

/**
 * 整个响应应是对象。返回原对象(带调用方声明的类型)。
 * 用于「响应本身就是实体对象」的接口(无包装字段)。
 */
export function requireObject<T extends object = Record<string, unknown>>(
  value: unknown,
  what: string,
): T {
  if (!isPlainObject(value)) shapeFail(what, value);
  return value as T;
}

/**
 * 取对象的**对象类型**字段(如 `{ config: {...} }` 的 config)。
 * 缺失或非对象(含 null / 数组)即抛错。
 */
export function requireObjectField<T extends object>(
  value: unknown,
  field: string,
  what: string,
): T {
  const obj = isPlainObject(value) ? value : shapeFail(what, value);
  const got = obj[field];
  if (!isPlainObject(got)) shapeFail(what, value);
  return got as T;
}

/**
 * 取对象的**数组类型**字段(如 `{ memories: [...] }`)。
 * 缺失或非数组即抛错——空数组是合法值(业务上「没有数据」),不在此拦截。
 */
export function requireArrayField<T>(value: unknown, field: string, what: string): T[] {
  const obj = isPlainObject(value) ? value : shapeFail(what, value);
  const got = obj[field];
  if (!Array.isArray(got)) shapeFail(what, value);
  return got as T[];
}

/** 取对象的**字符串**字段(如 `{ model: "gpt-..." }`) */
export function requireStringField(value: unknown, field: string, what: string): string {
  const obj = isPlainObject(value) ? value : shapeFail(what, value);
  const got = obj[field];
  if (typeof got !== 'string') shapeFail(what, value);
  return got;
}

/**
 * 取对象的**数字**字段(如 token 计数)。
 * 注意:NaN 视为非法(JSON 里本不该出现,出现即为上游异常)。
 */
export function requireNumberField(value: unknown, field: string, what: string): number {
  const obj = isPlainObject(value) ? value : shapeFail(what, value);
  const got = obj[field];
  if (typeof got !== 'number' || !Number.isFinite(got)) shapeFail(what, value);
  return got;
}

/**
 * 取对象的**可空对象**字段(如 `{ trace: AgentTrace | null }`)。
 * 区分「字段缺失」(异常,抛错)与「显式 null」(合法:业务上无记录)。
 */
export function requireNullableObjectField<T extends object>(
  value: unknown,
  field: string,
  what: string,
): T | null {
  const obj = isPlainObject(value) ? value : shapeFail(what, value);
  if (!(field in obj)) shapeFail(what, value);
  const got = obj[field];
  if (got === null) return null;
  if (!isPlainObject(got)) shapeFail(what, value);
  return got as T;
}

/** 取对象的**字符串字典**字段(如 `{ entries: { [k]: v } }`);值一律取字符串 */
export function requireStringMapField(
  value: unknown,
  field: string,
  what: string,
): Record<string, string> {
  const obj = isPlainObject(value) ? value : shapeFail(what, value);
  const got = obj[field];
  if (!isPlainObject(got)) shapeFail(what, value);
  return got as Record<string, string>;
}
