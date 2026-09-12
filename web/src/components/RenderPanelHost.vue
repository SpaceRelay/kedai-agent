<script setup lang="ts">
// 渲染面板宿主(消息画布容器,阶段五 5b,TH-render 等价物):
// 承载 v-html 注入的渲染面板所需的宿主侧行为——
//  1) 水合:面板 iframe 加载后,宿主经 postMessage 投递面板 HTML(ready/boot 双向认证 +
//     超时兜底);面板内 parent.postMessage 上报 {type:'kd-panel-resize', height} 时调整
//     iframe 高度;{type:'kd-panel-event'} 走 console 记录(扩展位)。
//  2) 折叠/展开:收起 = 隐藏 iframe 并展示代码块原文,展开 = 恢复 iframe(已注入的宿主
//     文档保留,无需重新投递)。
// 面板 DOM 由 ChatMessageItem 以 v-html 字符串注入,无法逐按钮模板绑定,故点击处理
// 以 @click 绑定在本组件根元素上(closest 委托):随组件卸载自动移除,重渲染不丢。
// (自 ChatWindow 拆出:原为 scrollArea 上手动 addEventListener 的全局委托,语义不变。)
import { ref } from 'vue';
import { buildRenderDocument, RENDER_PANEL_CHANNEL } from '../renderPanel';

/** 消息画布根元素(ChatWindow 经 expose 取用:滚动容器、脚本容器与资源卡片扫描的根) */
const el = ref<HTMLElement | null>(null);

/**
 * 渲染面板水合:面板占位 HTML 随消息渲染注入(渲染缓存见 ChatMessageItem),
 * 本函数扫描画布内未投递的面板 iframe 并投递面板 HTML(仿 hydrateRemoteResources
 * 的 ready/boot 双向认证 + 超时兜底;面板 HTML 直接内嵌于 data-kd-render-code,
 * 无后端代理)。重复调用幂等(kdLoaded 闩锁)。
 */
async function hydrate(): Promise<void> {
  if (!el.value) return;
  const panels = el.value.querySelectorAll<HTMLElement>('[data-kd-render-panel="1"]');
  for (const panel of panels) {
    const frame = panel.querySelector<HTMLIFrameElement>('[data-kd-render-frame="1"]');
    if (!frame || frame.dataset.kdLoaded) continue;
    const nonce = panel.dataset.kdRenderNonce;
    const code = panel.dataset.kdRenderCode;
    if (!nonce || code === undefined) continue;
    const html = buildRenderDocument(code, RENDER_PANEL_CHANNEL, nonce);
    const win = frame.contentWindow;
    if (!win) {
      // 宿主文档尚未加载完成(contentWindow 为 null):延迟重试(仿资源卡片);
      // 此时 kdLoaded 未标记,重扫会再次进入本分支直至就绪
      const retry = (attempt: number): void => {
        if (frame.dataset.kdLoaded || !frame.isConnected) return;
        if (frame.contentWindow) void hydrate();
        else if (attempt < 50) setTimeout(() => retry(attempt + 1), 300);
      };
      setTimeout(() => retry(1), 300);
      continue;
    }
    frame.dataset.kdLoaded = '1';
    // 高度上报监听(宿主文档面板脚本触发;按 nonce 认证,防伪造面板)
    const onSize = (ev: MessageEvent): void => {
      const m = ev.data;
      if (!m || ev.source !== win || m.channel !== RENDER_PANEL_CHANNEL || m.nonce !== nonce) return;
      if (m.type === 'kd-panel-resize' && typeof m.height === 'number') {
        const h = Math.min(Math.max(Math.round(m.height), 80), 3000);
        if (frame.style.height !== `${h}px`) frame.style.height = `${h}px`;
      } else if (m.type === 'kd-panel-event') {
        console.info('[kedai-render-panel]', m.message ?? '');
      }
    };
    window.addEventListener('message', onSize);
    // ready 后投递;ready 先于监听发出时用超时兜底直接投递(闩锁只接受第一条 boot)
    let ready = false;
    let injected = false;
    const inject = (): void => {
      if (injected) return;
      injected = true;
      win.postMessage({ channel: RENDER_PANEL_CHANNEL, nonce, type: 'boot', html }, '*');
    };
    const onReady = (ev: MessageEvent): void => {
      const m = ev.data;
      if (ev.source !== win || !m) return;
      if (m.channel === RENDER_PANEL_CHANNEL && m.nonce === nonce && m.type === 'ready' && !ready) {
        ready = true;
        window.removeEventListener('message', onReady);
        inject();
      }
    };
    window.addEventListener('message', onReady);
    setTimeout(() => {
      if (!ready) {
        window.removeEventListener('message', onReady);
        inject();
      }
    }, 12000);
  }
}

/** 渲染面板折叠切换:绑定在画布根元素上(面板由 v-html 注入,closest 委托命中按钮) */
function onToggle(e: MouseEvent): void {
  const btn = (e.target as HTMLElement).closest<HTMLElement>('.sv-render-panel-toggle');
  if (!btn) return;
  const panel = btn.closest<HTMLElement>('[data-kd-render-panel="1"]');
  const code = panel?.dataset.kdRenderCode;
  if (!panel || code === undefined) return;
  const frame = panel.querySelector<HTMLElement>('[data-kd-render-frame="1"]');
  const note = panel.querySelector<HTMLElement>('.sv-render-panel-note');
  if (panel.dataset.kdFolded === '1') {
    // 展开回面板
    panel.dataset.kdFolded = '';
    if (frame) frame.style.display = '';
    if (note) note.style.display = '';
    panel.querySelector<HTMLElement>('[data-kd-render-code-text]')?.remove();
    btn.textContent = '收起 ▾';
    void hydrate();
  } else {
    // 收起为代码块原文
    panel.dataset.kdFolded = '1';
    if (frame) frame.style.display = 'none';
    if (note) note.style.display = 'none';
    const codeEl = document.createElement('pre');
    codeEl.dataset.kdRenderCodeText = '1';
    codeEl.textContent = code;
    panel.appendChild(codeEl);
    btn.textContent = '展开 ▸';
  }
}

defineExpose({ el, hydrate });
</script>

<template>
  <!-- 消息画布(原 ChatWindow 的 scrollArea 容器;DOM 类名与结构语义不变) -->
  <div ref="el" class="sv-canvas" @click="onToggle">
    <slot />
  </div>
</template>
