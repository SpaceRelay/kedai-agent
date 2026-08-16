// 工具调用/结果展示(render intent,借鉴 deepseek-harness):按 renderKind 分派纯函数,
// 未知/缺失回退 JSON 直出。纯函数便于单元测试,调用方只调用不感知工具名。
//
// 输出统一为字符串(便于 pre 展示);后续可扩展为结构化节点(如 diff/terminal 卡片)。

/** 工具调用卡片(输入参数)的展示文本 */
export function renderToolInput(tool: { name: string; input: unknown; renderKind?: string }): string {
  if (tool.renderKind === 'generic') return prettyJson(tool.input);
  // read/search 等其余意图的「调用」阶段仍用 JSON 直出(输入参数无特殊结构)
  return prettyJson(tool.input);
}

/** 工具结果卡片的展示文本:按 renderKind 分派,未知/缺失回退 JSON 直出 */
export function renderToolOutput(tool: { name: string; output: unknown; renderKind?: string }): string {
  if (tool.output === undefined) return '';
  switch (tool.renderKind) {
    case 'read':
      return renderReadOutput(tool.output);
    case 'search':
      return renderSearchOutput(tool.output);
    case 'generic':
      return renderGenericOutput(tool.output);
    default:
      return prettyJson(tool.output);
  }
}

/** read 意图:结果通常是字符串(文件/条目内容),直接原样展示,不套 JSON 引号 */
function renderReadOutput(output: unknown): string {
  if (typeof output === 'string') return output;
  return prettyJson(output);
}

/** search 意图:结果通常是 { query, results: [...], ... } 结构,提取结果列表逐行展示 */
function renderSearchOutput(output: unknown): string {
  if (output && typeof output === 'object') {
    const obj = output as Record<string, unknown>;
    const results = Array.isArray(obj.results)
      ? obj.results
      : Array.isArray(obj.items)
        ? obj.items
        : null;
    if (results) {
      const lines = results.map((r, i) => {
        if (r && typeof r === 'object') {
          const it = r as Record<string, unknown>;
          const title = it.title ?? it.name ?? '';
          const url = it.url ?? it.link ?? '';
          return `${i + 1}. ${String(title)}${url ? `\n   ${url}` : ''}`;
        }
        return `${i + 1}. ${String(r)}`;
      });
      if (lines.length) return lines.join('\n');
    }
  }
  return prettyJson(output);
}

/** generic 意图:单层标量直接取值展示,对象/数组回退 JSON(避免过度包装) */
function renderGenericOutput(output: unknown): string {
  if (output && typeof output === 'object' && !Array.isArray(output)) {
    const obj = output as Record<string, unknown>;
    // 常见「结果」包裹(calculator 的 { result } / memory 的 { value } 等)直接取 result
    if ('result' in obj && obj.result !== undefined) return String(obj.result);
    if ('value' in obj && obj.value !== undefined) return String(obj.value);
  }
  return prettyJson(output);
}

/** 安全 JSON 序列化(对象格式化,标量原样,undefined → 空串) */
function prettyJson(value: unknown): string {
  if (value === undefined) return '';
  if (typeof value === 'string') return value;
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}
