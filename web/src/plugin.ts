// 前端插件框架
// 插件可注册:
//  - markdownExtension(md):扩展 markdown-it 渲染
//  - messageTransform(content, role, ctx):消息内容变换(渲染前)
//  - hooks.onMessageRendered(ctx):消息容器挂载后回调
// 加载源:
//  - 内置插件(代码内,默认启用)
//  - web/public/plugins/*.js(随构建拷贝到 dist/plugins,运行时 fetch 加载)
import type MarkdownIt from 'markdown-it';
import { registerMarkdownExtension } from './markdown';

export interface MessageRenderCtx {
  messageId: number;
  role: string;
  characterId?: string;
  characterName: string;
  container: HTMLElement | null;
}

export interface KedaiPlugin {
  id: string;
  name: string;
  version?: string;
  description?: string;
  enabled?: boolean;
  /** 扩展 markdown-it(html:false 已全局关闭) */
  markdownExtension?: (md: MarkdownIt) => void;
  /** 渲染前变换消息文本(返回新文本) */
  messageTransform?: (content: string, role: string) => string;
  /** 消息容器挂载后回调(DOM 可用) */
  onMessageRendered?: (ctx: MessageRenderCtx) => void;
}

const plugins = new Map<string, KedaiPlugin>();
let externalLoaded = false;

export function registerPlugin(p: KedaiPlugin): void {
  plugins.set(p.id, p);
  if (p.enabled !== false && p.markdownExtension) {
    registerMarkdownExtension(p.markdownExtension);
  }
}

export function getPlugins(): KedaiPlugin[] {
  return Array.from(plugins.values());
}

export function getEnabledPlugins(): KedaiPlugin[] {
  return getPlugins().filter((p) => p.enabled !== false);
}

/** 依次应用插件消息变换 */
export function applyPluginTransforms(content: string, role: string): string {
  let out = content;
  for (const p of getEnabledPlugins()) {
    if (p.messageTransform) out = p.messageTransform(out, role) ?? out;
  }
  return out;
}

/** 消息渲染完成钩子(容器挂载后) */
export function fireMessageRendered(ctx: MessageRenderCtx): void {
  for (const p of getEnabledPlugins()) {
    try {
      p.onMessageRendered?.(ctx);
    } catch (e) {
      console.warn(`[plugin:${p.id}] onMessageRendered 失败`, e);
    }
  }
}

/** 运行第三方插件 JS(来源可配置,默认 dist/plugins 目录) */
export async function loadExternalPlugins(base = '/plugins'): Promise<string[]> {
  if (externalLoaded) return [];
  externalLoaded = true;
  const loaded: string[] = [];
  try {
    const res = await fetch(`${base}/manifest.json`, { cache: 'no-store' });
    if (!res.ok) return loaded;
    const manifest = (await res.json()) as { plugins?: string[] };
    for (const file of manifest.plugins ?? []) {
      try {
        if (!/^[\w.-]+\.js$/.test(file)) throw new Error('插件文件名无效');
        const url = new URL(`${base}/${file}`, window.location.origin);
        if (url.origin !== window.location.origin) throw new Error('插件必须同源');
        const module = await import(/* @vite-ignore */ url.href) as {
          default?: KedaiPlugin | ((register: typeof registerPlugin) => void);
          plugin?: KedaiPlugin;
        };
        if (typeof module.default === 'function') module.default(registerPlugin);
        else if (module.default && typeof module.default === 'object') registerPlugin(module.default);
        else if (module.plugin) registerPlugin(module.plugin);
        else throw new Error('插件模块必须导出 default 或 plugin');
        loaded.push(file);
      } catch (e) {
        console.warn(`[plugin] 加载 ${file} 失败`, e);
      }
    }
  } catch {
    /* manifest 不存在则跳过 */
  }
  return loaded;
}
