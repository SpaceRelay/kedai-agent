// 远程资源卡片宿主 composable(自 ChatWindow 拆出,逻辑原样迁移):
// 卡片渲染后,父页面(带 token)经 /api/resource/proxy 取作者页面 HTML,投递给独立
// 宿主文档 /resource-frame.html(iframe src 加载,不继承父页面 CSP,作者脚本可执行;
// 宿主文档 DOM 重建触发脚本,详见其模板)。绕开作者服务器 X-Frame-Options 与 CSP 对
// srcdoc/iframe 直嵌的拦截;iframe sandbox 无 same-origin,与主页面完全隔离。
//
// reload 自举(吸血鬼卡「下载资源→进游戏界面」的关键):
// 作者页面下载完成后 location.reload(),iframe 重载回来的是空宿主文档。
// 本模块对每个框架保持常驻消息监听,收到 reload 后的 ready 即以缓存 HTML +
// 最新存储快照(resourceStore:localStorage/sessionStorage/Cache shim 持久化)
// 重新 boot——作者脚本初始化时命中快照,跳过下载页直接进游戏界面,重启 App 后仍有效。
//
// 监听器接线:宽屏按钮点击委托在 composable 挂载时自动绑定到消息画布(卡片由 v-html
// 动态注入,委托重渲染不丢),卸载自动移除;window 的 message 常驻监听由调用方
// (ChatWindow)显式接线,保持与拆出前一致的注册时序。

import { onBeforeUnmount, onMounted } from 'vue';
import type { Ref } from 'vue';
import { authorizedFetch, BASE } from '../api/client';
import { getCharacter } from '../api/characters';
import type { CharacterRecord } from '../api/types';
import { applyResourceSync, loadResourceSnapshot, type ResourceSyncMessage } from '../resourceStore';

export interface UseResourceFramesOptions {
  /** 消息画布滚动容器(RenderPanelHost 根元素;卡片 DOM 扫描与宽屏点击委托的挂载点) */
  scrollArea: Ref<HTMLElement | null>;
  /** 当前角色详情(boot 时随快照下发卡元数据与世界书) */
  currentCharacter: Ref<CharacterRecord | null>;
  /** 当前角色 id(TavernHelper RPC 转发生成时的 character_id) */
  currentCharacterId: Ref<string | null>;
}

interface ResourceEntry {
  url: string;
  nonce: string;
  frame: HTMLIFrameElement;
  /** 当前文档代数是否已成功投递 boot(reload 后 ready 会复位为 false) */
  booted: boolean;
  /** 代理拉取连续失败次数(退避重试用,成功投递后清零) */
  attempts: number;
  /** nonce 丢失修复次数(防重建循环) */
  repairs: number;
}

/** iframe srcdoc 注入用:HTML 属性转义(资源页原文仅作 srcdoc 字符串,不拼接进本页面) */
function escapeAttr(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

export function useResourceFrames(opts: UseResourceFramesOptions) {
  /** 已注册的资源框架:contentWindow(跨 reload 稳定)→ 条目 */
  const resourceFrames = new Map<Window, ResourceEntry>();
  /** 作者页面 HTML 按 URL 缓存(同一作者页多条消息共享;失败不缓存,便于重试) */
  const resourceHtmlCache = new Map<string, Promise<{ ok?: boolean; html?: string; base_url?: string; error?: string }>>();

  function fetchResourceHtml(url: string): Promise<{ ok?: boolean; html?: string; base_url?: string; error?: string }> {
    let p = resourceHtmlCache.get(url);
    if (!p) {
      p = (async () => {
        const res = await authorizedFetch(`${BASE}/resource/proxy?url=${encodeURIComponent(url)}`, undefined, false);
        return (await res.json().catch(() => ({}))) as { ok?: boolean; html?: string; base_url?: string; error?: string };
      })();
      resourceHtmlCache.set(url, p);
      const drop = (): void => {
        if (resourceHtmlCache.get(url) === p) resourceHtmlCache.delete(url);
      };
      p.then((b) => {
        if (!b?.ok) drop();
      }, drop);
    }
    return p;
  }

  /** 向框架投递 boot(HTML + 存储快照);失败退避重试,超过 3 次在卡片说明区给「重试」按钮 */
  async function bootResourceFrame(entry: ResourceEntry): Promise<void> {
    const { frame, nonce, url } = entry;
    if (!frame.isConnected) return;
    const win = frame.contentWindow;
    if (!win) return;
    try {
      const body = await fetchResourceHtml(url);
      if (!body.ok || typeof body.html !== 'string') throw new Error(body.error ?? '未知错误');
      const html = body.base_url ? `<base href="${escapeAttr(body.base_url)}">\n${body.html}` : body.html;
      const snapshot = await loadResourceSnapshot(url);
      // 酒馆助手式资源页需要当前角色卡元数据推导资源包口令(TavernHelper shim)
      const ch = opts.currentCharacter.value;
      const charData = ch
        ? {
            name: ch.chara_name || ch.name,
            creator: ch.creator ?? null,
            character_version: ch.character_version ?? null,
            creator_notes: ch.creator_notes ?? null,
          }
        : null;
      // 世界书随 boot 下发:作者页 getCharWorldbookNames 是同步接口,走不了 postMessage
      // RPC,只能预发数据由 shim 读内存(吸血鬼卡的开局消息/设定在世界书条目里)。
      // 体积保护:超 512KB 不下发(世界书巨大时作者页按无世界书容错,界面仍可运行)。
      // 注意:角色列表接口不带 data_raw(体积大,仅详情返回),worldbook 提取必须先
      // 拉详情兜底——否则开局消息/设定永远缺失(实测卡死在游戏 home)。
      let worldbook: { name: string; entries: unknown[] } | null = null;
      let rawBook = (ch?.data_raw as Record<string, unknown> | undefined)?.character_book as
        | { name?: string; entries?: unknown[] }
        | undefined;
      if (!rawBook && ch?.id) {
        try {
          const detail = await getCharacter(ch.id);
          rawBook = (detail?.data_raw as Record<string, unknown> | undefined)?.character_book as
            | { name?: string; entries?: unknown[] }
            | undefined;
        } catch {
          /* 详情拉取失败:按无世界书继续 */
        }
      }
      if (rawBook?.name && Array.isArray(rawBook.entries) && rawBook.entries.length > 0) {
        const candidate = { name: rawBook.name, entries: rawBook.entries };
        try {
          if (JSON.stringify(candidate).length <= 512 * 1024) worldbook = candidate;
        } catch {
          /* 循环引用等异常:不下发 */
        }
      }
      win.postMessage(
        { channel: 'kedai-resource-frame-v1', nonce, type: 'boot', html, snapshot, charData, worldbook },
        '*',
      );
      entry.booted = true;
      entry.attempts = 0;
      entry.repairs = 0;
    } catch (e) {
      entry.attempts += 1;
      if (entry.attempts <= 3) {
        const delay = 1500 * entry.attempts;
        setTimeout(() => {
          if (!entry.booted && frame.isConnected) void bootResourceFrame(entry);
        }, delay);
      } else if (!entry.booted) {
        showResourceError(entry, String((e as Error).message || '网络错误'));
      }
    }
  }

  /** 加载失败的可见反馈:说明区追加「重试」(iframe 内仍是宿主模板文档,重试即重新 boot,无需重建框架) */
  function showResourceError(entry: ResourceEntry, message: string): void {
    const card = entry.frame.closest('[data-kd-resource-url]');
    const note = card?.querySelector<HTMLElement>('.sv-resource-card-note');
    if (!note) return;
    note.textContent = `资源界面加载失败:${message}。`;
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'sv-btn ghost sv-btn-sm';
    btn.textContent = '重试';
    btn.onclick = () => {
      note.textContent = '资源页面由角色卡作者提供,已隔离加载;若未显示,请使用上方链接在新窗口打开。';
      entry.attempts = 0;
      entry.booted = false;
      void bootResourceFrame(entry);
    };
    note.appendChild(btn);
  }

  /** 常驻消息分发:ready(首载/reload 自举)、store-sync(shim 持久化桥) */
  function handleMessage(ev: MessageEvent): void {
    const entry = resourceFrames.get(ev.source as Window);
    if (!entry) return;
    const m = ev.data as ({ channel?: string; nonce?: string; type?: string } & Partial<ResourceSyncMessage>) | null;
    if (!m || m.channel !== 'kedai-resource-frame-v1') return;
    if (m.type === 'ready') {
      if (m.nonce === entry.nonce) {
        // 首次加载或作者页面 reload 自举:新文档需要重新 boot(携带最新快照)
        entry.booted = false;
        void bootResourceFrame(entry);
      } else if (!m.nonce && entry.repairs < 2 && entry.frame.isConnected) {
        // nonce 丢失的重载(window.name 被作者页面覆写等):重建 src 恢复 hash 里的 nonce
        entry.repairs += 1;
        entry.frame.removeAttribute('srcdoc');
        entry.frame.src = `/resource-frame.html#${encodeURIComponent(entry.nonce)}`;
      }
      return;
    }
    if (m.nonce !== entry.nonce) return;
    if (m.type === 'store-sync') {
      applyResourceSync(entry.url, m as ResourceSyncMessage);
      return;
    }
    if (m.type === 'tavern-call') {
      void handleTavernCall(entry, m as { callId?: string; method?: string; args?: unknown });
    }
  }

  /**
   * TavernHelper RPC 桥:沙箱 iframe 内作者脚本(吸血鬼卡等)调 TavernHelper.generate
   * 生成开场白/总结;iframe 无 token,经宿主转发 /api/chat/generate-raw(一次性非流式)。
   * 作者页参数形态:{user_input, injects:[{role,content,position}], overrides:{chat_history:{prompts}}}
   * —— 组装为扁平 messages:chat_history.prompts + in_chat injects + user_input。
   */
  async function handleTavernCall(
    entry: ResourceEntry,
    m: { callId?: string; method?: string; args?: unknown },
  ): Promise<void> {
    // 调试痕迹:资源页 RPC 调用记录(诊断桥断点;环形保留 20 条)
    try {
      const log = JSON.parse(localStorage.getItem('kedai.tavern-call-log') ?? '[]') as unknown[];
      log.push({ t: Date.now(), method: m.method, callId: m.callId });
      localStorage.setItem('kedai.tavern-call-log', JSON.stringify(log.slice(-20)));
    } catch {
      /* 忽略 */
    }
    const win = entry.frame.contentWindow;
    if (!win || !m.callId) return;
    const reply = (ok: boolean, value?: unknown, error?: string): void => {
      win.postMessage(
        { channel: 'kedai-resource-frame-v1', nonce: entry.nonce, type: 'tavern-result', callId: m.callId, ok, value, error },
        '*',
      );
    };
    if (m.method !== 'generate') {
      reply(false, undefined, `不支持的调用:${m.method}`);
      return;
    }
    try {
      const a = (m.args ?? {}) as {
        user_input?: string;
        injects?: Array<{ role?: string; content?: string; position?: string }>;
        overrides?: { chat_history?: { prompts?: Array<{ role?: string; content?: string }> } };
        /** 模板侧并入的预设提示词(作者页 replacePreset 注入的防掉格式提示等),位于历史之后、注入之前 */
        __preset_prompts?: Array<{ role?: string; content?: string }>;
      };
      const messages: Array<{ role: string; content: string }> = [];
      for (const p of a.overrides?.chat_history?.prompts ?? []) {
        if (p?.role && typeof p.content === 'string') messages.push({ role: p.role, content: p.content });
      }
      for (const p of a.__preset_prompts ?? []) {
        if ((p?.role === 'user' || p?.role === 'system') && typeof p.content === 'string') {
          messages.push({ role: p.role, content: p.content });
        }
      }
      for (const inj of a.injects ?? []) {
        if (inj?.role === 'system' && typeof inj.content === 'string') {
          messages.push({ role: 'system', content: inj.content });
        }
      }
      if (typeof a.user_input === 'string' && a.user_input.trim()) {
        messages.push({ role: 'user', content: a.user_input });
      }
      if (messages.length === 0) {
        reply(false, undefined, '消息列表为空');
        return;
      }
      const res = await authorizedFetch(`${BASE}/chat/generate-raw`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        // character_id 触发后端世界书匹配注入(对齐酒馆 generate 语义):
        // 作者页只自组历史与 user_input,世界书关键字命中(如吸血鬼卡 "system log"
        // 触发的输出格式规范)由后端注入,缺失会让模型自由发挥 → 游戏自检「丢格式」
        body: JSON.stringify({ messages, character_id: opts.currentCharacterId.value ?? undefined }),
      });
      const data = (await res.json().catch(() => ({}))) as { ok?: boolean; text?: string; error?: string };
      if (data.ok && typeof data.text === 'string') reply(true, data.text);
      else reply(false, undefined, data.error ?? `HTTP ${res.status}`);
    } catch (e) {
      reply(false, undefined, String((e as Error).message || '桥接失败'));
    }
  }

  /** 资源卡片扫描与接线:新出现的 iframe 注册条目并预拉取作者页(幂等,kdWired 闩锁) */
  async function hydrate(): Promise<void> {
    if (!opts.scrollArea.value) return;
    // 清理已从 DOM 移除的框架条目(消息删除/虚拟滚动卸载),防 map 泄漏
    for (const [win, entry] of resourceFrames) {
      if (!entry.frame.isConnected) resourceFrames.delete(win);
    }
    const frames = opts.scrollArea.value.querySelectorAll<HTMLIFrameElement>('iframe[data-kd-resource-frame="1"]');
    for (const frame of frames) {
      if (frame.dataset.kdWired) continue;
      const card = frame.closest('[data-kd-resource-url]') as HTMLElement | null;
      const rawUrl = card?.dataset.kdResourceUrl;
      const nonce = card?.dataset.kdResourceNonce;
      if (!rawUrl || !nonce) continue;
      const url = decodeURIComponent(rawUrl);
      const win = frame.contentWindow;
      if (!win) {
        // iframe 宿主文档尚未加载完成(contentWindow 为 null):延迟重试,
        // 不依赖 load 事件(load 可能先于监听触发或 lazy 加载延后,导致永不被投递)。
        // 每 300ms 重试,最多 50 次(15s);期间 kdWired 未标记,超时后放弃(卡片留有"新窗口打开"兜底)。
        const retry = (attempt: number): void => {
          if (frame.dataset.kdWired || !frame.isConnected) return;
          if (frame.contentWindow) {
            void hydrate();
          } else if (attempt < 50) {
            setTimeout(() => retry(attempt + 1), 300);
          }
        };
        setTimeout(() => retry(1), 300);
        continue;
      }
      frame.dataset.kdWired = '1';
      const entry: ResourceEntry = { url, nonce, frame, booted: false, attempts: 0, repairs: 0 };
      resourceFrames.set(win, entry);
      // 预拉取作者页面 HTML(与模板加载并行;失败不缓存,boot 分支会退避重试)
      void fetchResourceHtml(url).catch(() => undefined);
      // ready 可能先于常驻监听注册(模板加载很快):超时兜底直接投递,
      // 宿主文档 booted 闩锁只接受第一条 boot,重复投递无害。
      // 12s 覆盖代理下载大资源页(如吸血鬼卡 1.25MB)+ 宿主文档重建时间。
      setTimeout(() => {
        if (!entry.booted && frame.isConnected) void bootResourceFrame(entry);
      }, 12000);
    }
  }

  /**
   * 资源卡片宽屏切换(吸血鬼卡等的游戏界面为全屏设计):作者页往父文档注入撑满
   * 样式在沙箱下必抛 SecurityError(模板已装 parent shim 让作者页切入宽屏态),
   * 视口撑满由宿主侧把整卡 fixed 撑满补足。走消息画布上的事件委托,重渲染不丢。
   */
  function onWideClick(e: MouseEvent): void {
    const btn = (e.target as HTMLElement).closest<HTMLElement>('[data-kd-resource-wide="1"]');
    if (!btn || !opts.scrollArea.value?.contains(btn)) return;
    const card = btn.closest<HTMLElement>('[data-kd-resource-url]');
    if (!card) return;
    const wide = card.dataset.kdWide !== '1';
    card.dataset.kdWide = wide ? '1' : '';
    btn.textContent = wide ? '退出宽屏' : '宽屏';
  }

  // 宽屏按钮点击委托:挂载时绑定到消息画布(卡片由 v-html 注入),卸载自动移除
  onMounted(() => {
    opts.scrollArea.value?.addEventListener('click', onWideClick);
  });
  onBeforeUnmount(() => {
    opts.scrollArea.value?.removeEventListener('click', onWideClick);
  });

  return {
    /** 扫描画布内资源卡片 iframe 并完成接线/投递(重复调用幂等) */
    hydrate,
    /** 常驻 window message 分发(ready / store-sync / tavern-call),由调用方接线 */
    handleMessage,
  };
}
