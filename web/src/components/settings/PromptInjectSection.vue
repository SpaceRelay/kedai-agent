<script setup lang="ts">
// 设置区:提示词注入(简单模式 + 楼层系统)。
// 从 SettingsModal.vue 双模板合并而来:取 embedded 版本——禁词库统一为 banned_prompt
// 纯文本编辑(注入配置即以该文本承载);standalone 分支的「禁词词条表」是旧版可视化
// 编辑入口(编辑结果同样写回 banned_prompt),合并后废弃,能力由纯文本编辑保留。
// 状态由壳(SettingsModal)创建一次后经 prop 传入,与 PresetImportExportSection 共享。
import { onMounted } from 'vue';
import type { usePromptInject } from '../../composables/usePromptInject';
import { FLOOR_ROLE_LABELS, FLOOR_POS_LABELS, MACRO_HINTS } from '../../composables/usePromptInject';

const props = withDefaults(defineProps<{
  /** usePromptInject 的返回对象(壳共享实例) */
  state: ReturnType<typeof usePromptInject>;
  /** 是否显示(embedded 模式按 activeSection 切换;standalone 恒 true) */
  show?: boolean;
}>(), {
  show: true,
});

const {
  injectDraft, injectSaving, injectMsg, editingFloorId, dragFloorId, injectImportInput, injectImporting,
  loadInjectConfig, saveInjectNow, addFloor, removeFloor, moveFloor, onImportPreset, exportInjectConfig,
  onDragStart, onDragOver, onDrop, onDragEnd,
} = props.state;

onMounted(async () => {
  // 加载提示词注入配置(简单模式 + 楼层)
  await loadInjectConfig();
});
</script>

<template>
  <div v-show="props.show" class="sv-field">
    <div class="sv-field-label"><span class="sv-supreme orange" /> 提示词注入</div>
    <div class="sv-stack">
      <div class="sv-inp-row">
        <label class="sv-inp-tag">模式</label>
        <div v-if="injectDraft" class="sv-mode">
          <button :class="{ active: injectDraft.mode === 'simple' }" @click="injectDraft.mode = 'simple'">
            简单
          </button>
          <button :class="{ active: injectDraft.mode === 'complex' }" @click="injectDraft.mode = 'complex'">
            复杂
          </button>
        </div>
        <span v-else class="sv-note">加载中...</span>
      </div>

      <!-- 禁词库:输出含禁用词时注入此提示词(所有模式);deep/agent/custom 另由引擎收尾工具同义替换 -->
      <div v-if="injectDraft" class="sv-inp-row" style="align-items: flex-start">
        <label class="sv-inp-tag">禁词库</label>
        <button
          class="sv-btn ghost"
          :class="{ 'sv-btn-on': injectDraft.simple.banned_words_enabled }"
          @click="injectDraft.simple.banned_words_enabled = !injectDraft.simple.banned_words_enabled"
        >
          {{ injectDraft.simple.banned_words_enabled ? '已启用' : '已关闭' }}
        </button>
        <textarea
          v-model="injectDraft.simple.banned_prompt"
          rows="2"
          class="sv-input"
          placeholder="输出中禁止出现以下词语,如:笨蛋、脏话、滚"
          :disabled="!injectDraft.simple.banned_words_enabled"
          spellcheck="false"
        />
      </div>
      <p v-if="injectDraft" class="sv-note">输出含禁用词时注入此提示词,对所有模式生效;deep/agent/custom 模式另由引擎收尾做同义替换。</p>

      <!-- 简单模式:字数/转述/对话/视角 -->
      <template v-if="injectDraft?.mode === 'simple'">
        <div class="sv-inp-row">
          <label class="sv-inp-tag">字数</label>
          <button
            class="sv-btn ghost"
            :class="{ 'sv-btn-on': injectDraft.simple.word_count_enabled }"
            @click="injectDraft.simple.word_count_enabled = !injectDraft.simple.word_count_enabled"
          >
            {{ injectDraft.simple.word_count_enabled ? '已启用' : '已关闭' }}
          </button>
          <input
            v-model.number="injectDraft.simple.word_count"
            type="number"
            min="1"
            max="100000"
            class="sv-input inject-num"
            :disabled="!injectDraft.simple.word_count_enabled"
          />
        </div>
        <div class="sv-inp-row">
          <label class="sv-inp-tag">转述</label>
          <button
            class="sv-btn ghost"
            :class="{ 'sv-btn-on': injectDraft.simple.paraphrase_enabled }"
            @click="injectDraft.simple.paraphrase_enabled = !injectDraft.simple.paraphrase_enabled"
          >
            {{ injectDraft.simple.paraphrase_enabled ? '已启用' : '已关闭' }}
          </button>
          <span class="sv-note">用自己的话复述内容,不直接照抄原文</span>
        </div>
        <div class="sv-inp-row">
          <label class="sv-inp-tag">对话</label>
          <button
            class="sv-btn ghost"
            :class="{ 'sv-btn-on': injectDraft.simple.dialogue_enabled }"
            @click="injectDraft.simple.dialogue_enabled = !injectDraft.simple.dialogue_enabled"
          >
            {{ injectDraft.simple.dialogue_enabled ? '已启用' : '已关闭' }}
          </button>
          <span class="sv-note">只输出台词与必要动作描写,不输出旁白段落</span>
        </div>
        <div class="sv-inp-row">
          <label class="sv-inp-tag">视角</label>
          <button
            class="sv-btn ghost"
            :class="{ 'sv-btn-on': injectDraft.simple.perspective_enabled }"
            @click="injectDraft.simple.perspective_enabled = !injectDraft.simple.perspective_enabled"
          >
            {{ injectDraft.simple.perspective_enabled ? '已启用' : '已关闭' }}
          </button>
          <select
            v-model="injectDraft.simple.perspective"
            class="sv-select"
            :disabled="!injectDraft.simple.perspective_enabled"
          >
            <option>第一人称</option>
            <option>第二人称</option>
            <option>第三人称</option>
          </select>
        </div>
        <p class="sv-note">
          启用的项目会合成一条注入提示词拼入系统提示词末尾,对所有会话生效;支持宏(如
          <code v-pre>{{time}}</code>、<code v-pre>{{random:1,100}}</code>)。
        </p>
      </template>

      <!-- 复杂模式:楼层系统(仿 SillyTavern Prompt Manager) -->
      <template v-else-if="injectDraft">
        <p class="sv-note">
          楼层按从上到下的顺序注入:可拖拽排序,自由选择消息角色与位置,内容支持完整酒馆宏。
          <code v-pre>{{setvar::键::值}}</code> 会持久化到会话变量,后续楼层可
          <code v-pre>{{getvar::键}}</code> 读取。
        </p>
        <div v-if="!injectDraft.floors.length" class="sv-note inject-empty">尚未添加楼层,点击下方「新增楼层」。</div>
        <div
          v-for="floor in injectDraft.floors"
          :key="floor.id"
          class="floor-row"
          :class="{ dragging: dragFloorId === floor.id }"
          draggable="true"
          @dragstart="onDragStart($event, floor.id)"
          @dragover="onDragOver($event, floor.id)"
          @drop="onDrop($event, floor.id)"
          @dragend="onDragEnd"
        >
          <span class="floor-grip" title="拖拽排序">⋮⋮</span>
          <button
            class="sv-btn ghost"
            :class="{ 'sv-btn-on': floor.enabled }"
            :title="floor.enabled ? '点击停用' : '点击启用'"
            @click="floor.enabled = !floor.enabled"
          >
            {{ floor.enabled ? '开' : '关' }}
          </button>
          <input v-model="floor.name" type="text" class="sv-input floor-name" placeholder="楼层名称" spellcheck="false" />
          <select v-model="floor.role" class="sv-select floor-select" title="消息角色">
            <option v-for="(label, val) in FLOOR_ROLE_LABELS" :key="val" :value="val">{{ label }}</option>
          </select>
          <select v-model="floor.position" class="sv-select floor-select" title="注入位置(before/after/depth 已废弃,引擎统一归位系统提示词;仅兼容导入的酒馆预设)">
            <option v-for="(label, val) in FLOOR_POS_LABELS" :key="val" :value="val">{{ label }}</option>
          </select>
          <input
            v-if="floor.position === 'depth'"
            v-model.number="floor.depth"
            type="number"
            min="0"
            class="sv-input floor-depth"
            title="从最新消息往前数第 N 条之后插入(0 = 最新消息后)"
          />
          <div class="floor-actions">
            <button class="sv-btn ghost sv-btn-square" title="上移" @click="moveFloor(floor.id, -1)">↑</button>
            <button class="sv-btn ghost sv-btn-square" title="下移" @click="moveFloor(floor.id, 1)">↓</button>
            <button
              class="sv-btn ghost sv-btn-square"
              :title="editingFloorId === floor.id ? '收起编辑' : '编辑内容'"
              @click="editingFloorId = editingFloorId === floor.id ? null : floor.id"
            >
              ✎
            </button>
            <button class="sv-btn danger sv-btn-square" title="删除楼层" @click="removeFloor(floor.id)">✕</button>
          </div>
          <textarea
            v-if="editingFloorId === floor.id"
            v-model="floor.content"
            rows="5"
            class="sv-input floor-content"
            placeholder="楼层内容,支持酒馆宏。例如:{{char}}内心提醒:{{setvar::场景::酒馆}}今晚在{{getvar::场景}}见面。"
            spellcheck="false"
          />
        </div>
        <div class="sv-btn-row">
          <button class="sv-btn ghost sv-btn-fill" @click="addFloor">+ 新增楼层</button>
        </div>
        <details class="macro-hint">
          <summary>酒馆宏速查(点击展开)</summary>
          <code class="sv-code">{{ MACRO_HINTS.join('  ·  ') }}</code>
        </details>
      </template>

      <div class="sv-btn-row">
        <button
          class="sv-btn ghost sv-btn-fill"
          :disabled="injectImporting"
          title="导入 SillyTavern 预设 JSON,替换当前全部楼层(保留各条目启用状态)"
          @click="injectImportInput?.click()"
        >
          {{ injectImporting ? '导入中...' : '导入酒馆预设(替换楼层)' }}
        </button>
        <button class="sv-btn ghost sv-btn-fill" :disabled="!injectDraft" @click="exportInjectConfig">
          导出注入配置(JSON)
        </button>
        <input
          ref="injectImportInput"
          type="file"
          accept=".json,application/json"
          class="hidden"
          @change="onImportPreset"
        />
      </div>
      <div class="sv-btn-row">
        <button class="sv-btn primary sv-btn-fill" :disabled="injectSaving || !injectDraft" @click="saveInjectNow">
          {{ injectSaving ? '保存中...' : '保存注入设置' }}
        </button>
        <div v-if="injectMsg" class="sv-feedback ok sv-feedback-flex">{{ injectMsg }}</div>
      </div>
    </div>
  </div>
</template>
