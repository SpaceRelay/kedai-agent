<script setup lang="ts">
// 退出确认弹窗(2026-09-16 批次 5):取代此前的 Tauri 原生 window.dialog()。
//
// 为什么自绘:Tauri 原生对话框是 Windows MessageBox,外观不受前端 CSS 影响,做不出
// 应用统一的至上主义/构成主义语言。现在由壳 prevent_close + 下发 kedai://close-requested
// 打开本弹窗,用户确认才发 kedai://exit-app 真正退出。
//
// 两个平台共用本弹窗:
//   - 桌面:点窗口关闭 → 壳下发事件 → 打开;
//   - Android:返回键无处可退时由 App.vue 打开。
// 取消时额外发 kedai://close-cancelled:桌面壳据此复位「二次关闭兜底」标记,
// 否则取消一次之后,下一次单击关闭会被兜底逻辑直接放行(不再询问)。
//
// 样式纪律:只复用 style.css 既有 .sv-modal/.sv-btn/.sv-note 等类与 :root token,
// 不往全局样式表新增类;本组件专有的排版写在 <style scoped> 内。
import { useAppStore } from '../store';

const store = useAppStore();

/** 取消退出:关弹窗并通知壳复位兜底标记(失败仅记日志,不阻断关闭弹窗) */
async function cancel(): Promise<void> {
  store.exitConfirmOpen = false;
  try {
    const { emit } = await import('@tauri-apps/api/event');
    await emit('kedai://close-cancelled');
  } catch (e) {
    console.debug('[kedai] 取消退出通知未送达(非 Tauri 环境可忽略)', e);
  }
}

/** 确认退出:壳收到后清理端口残留并结束进程(本弹窗随进程一同消失,无需自关) */
async function confirm(): Promise<void> {
  try {
    const { emit } = await import('@tauri-apps/api/event');
    await emit('kedai://exit-app');
  } catch (e) {
    // 事件送不出去时明确告知用户,而不是留一个点了没反应的按钮
    console.warn('[kedai] 退出请求发送失败', e);
    window.alert('退出请求发送失败,请手动关闭窗口');
  }
}
</script>

<template>
  <div class="sv-modal-mask" @click.self="cancel">
    <div class="sv-modal sm">
      <div class="sv-modal-head">
        <h2 class="sv-head-title">
          <span class="sv-supreme red" />
          退出 Kedai
        </h2>
        <button class="sv-btn ghost sv-btn-square" title="取消" @click="cancel">✕</button>
      </div>

      <div class="sv-modal-body">
        <p class="sv-note exit-note">确定要退出 Kedai 吗?</p>
        <ul class="exit-warn">
          <li>正在生成或执行中的任务会被中断。</li>
          <li>未保存的编辑内容将丢失。</li>
        </ul>
        <!-- 构成主义签名:红斜线 + 黑方块,与全站装饰语言一致(纯装饰,不参与交互) -->
        <div class="exit-mark" aria-hidden="true">
          <span class="sv-diagonal lg" />
          <span class="sv-supreme" />
        </div>
      </div>

      <div class="sv-modal-foot">
        <button class="sv-btn primary" @click="cancel">取消</button>
        <button class="sv-btn danger" @click="confirm">退出</button>
      </div>
    </div>
  </div>
</template>

<style scoped>
/* 标题:黑条头部内的白字 + 小号装饰方块对齐(其余 .sv-modal-head 样式来自全局) */
.sv-head-title {
  display: flex;
  align-items: center;
  gap: 8px;
}

.exit-note {
  /* 正文首句是提问本身,比普通说明更重:提亮并放大一档 */
  color: var(--sv-ink);
  font-size: 13px;
  font-weight: 700;
}

/* 后果清单:黑色方括号标记 + 硬排版,替代无序列点 */
.exit-warn {
  margin: 12px 0 0;
  padding: 0;
  list-style: none;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.exit-warn li {
  position: relative;
  padding-left: 16px;
  font-size: 12px;
  line-height: 1.6;
  color: var(--sv-ink-soft);
}

.exit-warn li::before {
  content: '';
  position: absolute;
  left: 0;
  top: 0.5em;
  width: 6px;
  height: 6px;
  background: var(--sv-ink);
}

/* 底部装饰:斜线 + 方块右对齐收尾,呼应至上主义签名 */
.exit-mark {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 10px;
  margin-top: 18px;
}

.exit-mark .sv-supreme {
  width: 8px;
  height: 8px;
}
</style>
