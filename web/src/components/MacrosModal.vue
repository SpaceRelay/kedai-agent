<script setup lang="ts">
// 宏调试面板(阶段六 6b):模板输入 + 「展开」按钮手动触发(避免击键防抖复杂度),
// 可选上下文区(角色名/用户名/变量 key::value 行,供 {{getvar::}} 调试),
// 展开结果只读展示;未知宏保留原样提示。
import { ref } from 'vue';
import { useAppStore } from '../store';
import * as api from '../api';
import { MACRO_HINTS } from '../composables/usePromptInject';
import { parseVarsLines } from '../api/macros';

const store = useAppStore();

const text = ref('');
const characterName = ref('');
const userName = ref('');
/** 变量文本区(每行 key::value) */
const varsLines = ref('');
const expanded = ref('');
const expanding = ref(false);
const msg = ref<{ kind: 'ok' | 'err'; text: string } | null>(null);

const close = (): void => {
  store.macrosOpen = false;
};

async function expandNow(): Promise<void> {
  if (expanding.value) return;
  if (!text.value.trim()) {
    expanded.value = '';
    msg.value = { kind: 'ok', text: '输入为空,请在模板里填写 {{...}} 宏' };
    return;
  }
  expanding.value = true;
  msg.value = null;
  try {
    const ctx: api.MacroExpandCtx = {
      character_name: characterName.value || undefined,
      user_name: userName.value || undefined,
      vars: Object.keys(parseVarsLines(varsLines.value)).length ? parseVarsLines(varsLines.value) : undefined,
    };
    expanded.value = await api.expandMacros(text.value, ctx);
  } catch (err) {
    msg.value = { kind: 'err', text: `展开失败:${(err as Error).message}` };
  } finally {
    expanding.value = false;
  }
}
</script>

<template>
  <div class="sv-modal-mask" @click.self="close">
    <div class="sv-modal macros-modal">
      <!-- 头部 -->
      <div class="sv-modal-head">
        <h2 class="flex items-center gap-2">
          <span class="logo" /> 宏调试
        </h2>
        <button class="sv-btn ghost sv-btn-square" @click="close">✕</button>
      </div>

      <div class="sv-modal-body">
        <!-- 说明 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme blue" /> 说明</div>
          <p class="sv-note" style="margin: 0; line-height: 1.8">
            宏调试面板:粘贴含 <code v-pre>{{...}}</code> 的模板,点「展开」查看服务端实际展开结果
            (与提示词注入/世界书/快速回复共用的同一宏引擎)。<br />
            <b>未知宏保留原样</b>(不报错);<b>变量</b> 区供
            <code v-pre>{{getvar::键}}</code> / <code v-pre>{{var::键}}</code> 调试(每行
            <code>键::值</code>,支持 true/false 与整数)。
          </p>
        </div>

        <!-- 模板输入 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme yellow" /> 模板</div>
          <textarea
            v-model="text"
            rows="6"
            class="sv-input"
            style="resize: vertical; line-height: 1.7"
            placeholder="如:{{char}} 对 {{user}} 说 {{getvar::好感}}"
            spellcheck="false"
          ></textarea>
          <div class="sv-wb-edit-foot">
            <button class="sv-btn primary sv-btn-sm" :disabled="expanding" @click="expandNow">
              {{ expanding ? '展开中…' : '展开' }}
            </button>
          </div>
        </div>

        <!-- 可选上下文 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme pink" /> 上下文(可选)</div>
          <div class="flex items-center gap-2">
            <input v-model="characterName" type="text" class="sv-input sv-wb-comment" placeholder="角色名({{char}})" spellcheck="false" />
            <input v-model="userName" type="text" class="sv-input sv-wb-comment" placeholder="用户名({{user}},默认「用户」)" spellcheck="false" />
          </div>
          <textarea
            v-model="varsLines"
            rows="4"
            class="sv-input"
            style="margin-top: 6px; resize: vertical; line-height: 1.7"
            placeholder="变量(每行 键::值,如:好感::88&#10;开关::true)"
            spellcheck="false"
          ></textarea>
        </div>

        <!-- 展开结果 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme green" /> 展开结果</div>
          <div v-if="!expanded && !msg" class="sv-empty" style="padding: 16px 12px">
            <p style="font-size: 12px">点击「展开」查看结果;未知宏会原样保留在输出中。</p>
          </div>
          <pre v-else class="sv-input" style="white-space: pre-wrap; line-height: 1.7; margin: 0; min-height: 60px">{{ expanded }}</pre>
          <div v-if="msg" class="sv-feedback" :class="msg.kind" style="margin-top: 6px">{{ msg.text }}</div>
        </div>

        <!-- 常用宏速查 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme purple" /> 常用宏速查</div>
          <p class="sv-note" style="margin: 0; line-height: 2">{{ MACRO_HINTS.join(' · ') }}</p>
        </div>
      </div>

      <div class="sv-modal-foot">
        <button class="sv-btn primary" @click="close">完成</button>
      </div>
    </div>
  </div>
</template>
