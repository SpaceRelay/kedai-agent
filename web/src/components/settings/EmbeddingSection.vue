<script setup lang="ts">
// 设置区:向量化模型(记忆库语义召回用的 embedding 服务)。
// 独立于聊天 API 配置:可指向不同厂商;Key 走与聊天 Key 同一套加密与脱敏回显。
// 提供「测试连接」(验证配置并回填实际维度)与「重建向量索引」(为存量记忆补向量)。
import { onMounted } from 'vue';
import { useEmbeddingSettings } from '../../composables/useEmbeddingSettings';

const props = withDefaults(defineProps<{ show?: boolean }>(), { show: true });

const panel = useEmbeddingSettings();
// 解构后模板中 ref 自动解包(经 `s.xxx` 访问 Ref 不会解包,故此处展开)
const {
  enabled, baseUrl, apiKey, apiKeyMasked, hasApiKey, model, dim,
  loading, saving, testing, rebuilding, status, feedback,
  load, onBaseUrlBlur, fillFromChat, save, test, rebuild,
} = panel;

onMounted(() => {
  void load();
});
</script>

<template>
  <div v-show="props.show" class="sv-field">
    <div class="sv-field-label"><span class="sv-supreme blue" /> 向量化模型</div>
    <p class="sv-note" style="margin: 0 0 10px; line-height: 1.8">
      记忆库的<strong>语义召回</strong>需要 embedding 服务:开启后,记忆写入时生成向量,
      召回时按「向量相似度 70% + 关键词 30%」混合排序。未配置时自动降级为纯关键词召回,
      不影响记忆功能本身。需 OpenAI 兼容的 <code>/embeddings</code> 接口。
    </p>

    <div class="sv-stack">
      <!-- 总开关 -->
      <div class="sv-inp-row">
        <label class="sv-inp-tag">启用向量化</label>
        <label style="display: flex; gap: 6px; align-items: center; cursor: pointer">
          <input v-model="enabled" type="checkbox" style="flex-shrink: 0" />
          <span class="sv-note">开启后新记忆自动生成向量;存量记忆需点下方「重建向量索引」</span>
        </label>
      </div>

      <div class="sv-inp-row">
        <label class="sv-inp-tag">BASE URL</label>
        <input
          v-model="baseUrl"
          type="text"
          class="sv-input"
          placeholder="https://api.openai.com/v1"
          spellcheck="false"
          @blur="onBaseUrlBlur"
        />
      </div>

      <div class="sv-inp-row">
        <label class="sv-inp-tag">API KEY</label>
        <input
          v-model="apiKey"
          type="password"
          class="sv-input"
          :placeholder="hasApiKey ? `已配置 ${apiKeyMasked} · 留空保持现有` : '粘贴 API Key'"
          autocomplete="off"
          spellcheck="false"
        />
      </div>

      <div class="sv-inp-row">
        <label class="sv-inp-tag">模型</label>
        <input
          v-model="model"
          type="text"
          class="sv-input"
          placeholder="text-embedding-3-small / embedding-3 / bge-m3"
          spellcheck="false"
        />
      </div>

      <div class="sv-inp-row">
        <label class="sv-inp-tag">维度</label>
        <input
          v-model.number="dim"
          type="number"
          min="0"
          max="8192"
          step="1"
          class="sv-input inject-num"
          title="0 = 由「测试连接」自动探测;也可手动指定,需与模型实际输出一致"
        />
        <span class="sv-note">0 = 自动探测(保存后点「测试连接」回填)</span>
      </div>

      <div class="sv-btn-row">
        <button class="sv-btn primary sv-btn-fill" :disabled="saving" @click="save">
          {{ saving ? '保存中...' : '保存设置' }}
        </button>
        <button class="sv-btn ghost sv-btn-fill" :disabled="testing" @click="test">
          {{ testing ? '测试中...' : '测试连接' }}
        </button>
      </div>
      <div class="sv-btn-row">
        <button class="sv-btn ghost sv-btn-fill" :disabled="loading" @click="fillFromChat">
          从聊天配置填入地址
        </button>
        <button
          class="sv-btn ghost sv-btn-fill"
          :disabled="rebuilding || !enabled"
          :title="enabled ? '' : '请先启用向量化'"
          @click="rebuild"
        >
          {{ rebuilding ? '重建中...' : '重建向量索引' }}
        </button>
      </div>

      <!-- 索引状态:进度 + 维度漂移提示 -->
      <div v-if="status" class="sv-note emb-status">
        <span>已生成向量 <b>{{ status.status.embedded }}</b> / {{ status.status.total }} 条</span>
        <span v-if="status.status.dim">· 当前维度 {{ status.status.dim }}</span>
        <span v-if="status.status.dim_mismatch" class="emb-warn">
          · 维度已变更({{ status.status.dim_mismatch }}),需点「重建向量索引」
        </span>
      </div>
      <p class="sv-note">
        Key 仅存本地服务端,落盘加密、不回显明文;地址会自动补全协议与 /v1。
        向量化失败不影响记忆写入,可在本页随时手动重建。
      </p>
    </div>

    <div v-if="feedback" class="sv-feedback" :class="feedback.kind">{{ feedback.text }}</div>
  </div>
</template>

<style scoped>
/* 索引状态行:紧凑单行,与 sv-note 同字号;漂移提示用警示色(不引入 !important) */
.emb-status {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
  align-items: center;
  margin-top: 2px;
}
.emb-warn {
  color: var(--sv-red);
}
</style>
