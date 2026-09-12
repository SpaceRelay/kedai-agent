<script setup lang="ts">
// 技能库弹窗:导入提示词技能(JSON 单对象/数组)、每项带 启用/停用 与 删除 按钮。
// 技能通过 Agent 模式的 read 工具读取,可注入上下文。
import { ref, onMounted } from 'vue';
import { useAppStore } from '../store';
import { storeToRefs } from 'pinia';
import type { SkillRecord } from '../api';
import * as api from '../api';

const store = useAppStore();
const { skills, skillsOpen } = storeToRefs(store);

const fileInput = ref<HTMLInputElement | null>(null);
const msg = ref<{ kind: 'ok' | 'err' | 'info'; text: string } | null>(null);
const busyId = ref<string | null>(null);

const close = (): void => {
  store.skillsOpen = false;
};

async function onFilePicked(e: Event): Promise<void> {
  const input = e.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = '';
  if (!file) return;
  msg.value = null;
  try {
    const n = await store.importSkillsFile(file);
    msg.value = { kind: 'ok', text: `已导入 ${n} 个技能` };
  } catch (err) {
    msg.value = { kind: 'err', text: `导入失败:${(err as Error).message}` };
  }
}

// 参数用完整 SkillRecord(store.toggleSkill 需要整条记录持久化,不止 id/enabled)
async function onToggle(s: SkillRecord): Promise<void> {
  busyId.value = s.id;
  msg.value = null;
  try {
    await store.toggleSkill(s);
  } catch (err) {
    msg.value = { kind: 'err', text: `操作失败:${(err as Error).message}` };
  } finally {
    busyId.value = null;
  }
}

async function onDelete(id: string, name: string): Promise<void> {
  if (!confirm(`确定删除技能「${name}」?`)) return;
  busyId.value = id;
  msg.value = null;
  try {
    await store.removeSkill(id);
    msg.value = { kind: 'ok', text: `已删除技能「${name}」` };
  } catch (err) {
    msg.value = { kind: 'err', text: `删除失败:${(err as Error).message}` };
  } finally {
    busyId.value = null;
  }
}

// ===== 技能高级设置(工具白名单 / 子智能体 / 模型覆盖)=====
// 后端 SkillRecord 早已支持这三个字段,前端此前无入口(见 docs/contract-drift-2026-09-09.md #9-#11)。
const editingId = ref<string | null>(null);
/** 编辑草稿:allowedTools 以逗号分隔文本编辑,保存时转 JSON 数组字符串 */
const draft = ref<{ allowedTools: string; runAsSubagent: boolean; model: string }>({
  allowedTools: '', runAsSubagent: false, model: '',
});

function openAdvanced(s: SkillRecord): void {
  editingId.value = editingId.value === s.id ? null : s.id;
  if (editingId.value === s.id) {
    draft.value = {
      allowedTools: (s.allowed_tools ?? []).join(', '),
      runAsSubagent: s.run_as_subagent ?? false,
      model: s.model ?? '',
    };
  }
}

async function saveAdvanced(s: SkillRecord): Promise<void> {
  busyId.value = s.id;
  msg.value = null;
  try {
    // allowed_tools 后端存 JSON 数组字符串;空文本 → "[]" 表示不限制
    const names = draft.value.allowedTools
      .split(',')
      .map((x) => x.trim())
      .filter(Boolean);
    await api.updateSkill(s.id, {
      allowed_tools: JSON.stringify(names),
      run_as_subagent: draft.value.runAsSubagent,
      model: draft.value.model.trim(),
    });
    await store.loadSkills();
    editingId.value = null;
    msg.value = { kind: 'ok', text: `已保存技能「${s.name}」的高级设置` };
  } catch (err) {
    msg.value = { kind: 'err', text: `保存失败:${(err as Error).message}` };
  } finally {
    busyId.value = null;
  }
}

onMounted(() => void store.loadSkills());
</script>

<template>
  <div class="sv-modal-mask" @click.self="close">
    <div class="sv-modal md">
      <div class="sv-modal-head">
        <h2 class="flex items-center gap-2">
          <span class="sv-supreme pink" /> 技能库
        </h2>
        <button class="sv-btn ghost sv-btn-square" @click="close">✕</button>
      </div>

      <div class="sv-modal-body">
        <p class="sv-note" style="margin-bottom: 12px">
          提示词技能库:导入 JSON(<code>[{&quot;name&quot;:&quot;…&quot;,&quot;description&quot;:&quot;…&quot;,&quot;content&quot;:&quot;…&quot;}]</code>
          或单对象 / <code>{&quot;skills&quot;:[…]}</code> 包壳)。同名导入会覆盖。Agent 模式的
          <code>read</code> 工具可按名称或关键词读取技能内容。
        </p>

        <!-- 操作区 -->
        <div class="flex items-center gap-2" style="margin-bottom: 12px">
          <button class="sv-btn primary sv-btn-sm" @click="fileInput?.click()">导入技能</button>
        </div>
        <input
          ref="fileInput"
          type="file"
          accept=".json,application/json"
          class="hidden"
          @change="onFilePicked"
        />

        <div v-if="msg" class="sv-feedback" :class="msg.kind === 'err' ? 'err' : msg.kind === 'ok' ? 'ok' : ''" style="margin-bottom: 10px">
          {{ msg.text }}
        </div>

        <!-- 技能列表 -->
        <div class="sv-plugin-list">
          <div v-for="s in skills" :key="s.id" class="sv-plugin-item">
            <div class="min-w-0 flex-1">
              <div class="sv-char-name">
                {{ s.name }}
                <span v-if="!s.enabled" class="sv-badge-off">已停用</span>
              </div>
              <div class="sv-char-desc">{{ s.description || '无描述' }}</div>
              <div v-if="s.content" class="sv-skill-preview">{{ s.content }}</div>
            </div>
            <div class="flex flex-col gap-1 shrink-0">
              <button
                class="sv-btn ghost sv-btn-sm"
                :disabled="busyId === s.id"
                @click="onToggle(s)"
              >
                {{ s.enabled ? '停用' : '启用' }}
              </button>
              <button class="sv-btn ghost sv-btn-sm" :disabled="busyId === s.id" @click="openAdvanced(s)">
                {{ editingId === s.id ? '收起' : '高级' }}
              </button>
              <button class="sv-btn danger sv-btn-sm" :disabled="busyId === s.id" @click="onDelete(s.id, s.name)">
                删除
              </button>
            </div>

            <!-- 高级设置:工具白名单 / 子智能体派发 / 模型覆盖 -->
            <div v-if="editingId === s.id" class="sv-skill-advanced">
              <label class="sv-inp-row">
                <span class="sv-inp-tag">工具白名单</span>
                <input
                  v-model="draft.allowedTools"
                  class="sv-input"
                  placeholder="留空 = 不限制;逗号分隔,如 read, search"
                  title="该技能经 read(type=skill) 加载时可用的工具集合;留空表示不限制"
                />
              </label>
              <label class="sv-inp-row">
                <span class="sv-inp-tag">可派子智能体</span>
                <input v-model="draft.runAsSubagent" type="checkbox" title="允许该技能作为子智能体技能派发(agentgo 链路)" />
                <span class="sv-note">允许作为子智能体技能派发</span>
              </label>
              <label class="sv-inp-row">
                <span class="sv-inp-tag">模型覆盖</span>
                <input
                  v-model="draft.model"
                  class="sv-input"
                  placeholder="留空 = 沿用当前模型"
                  title="该技能专用模型名;留空表示沿用当前连接器模型"
                />
              </label>
              <button class="sv-btn primary sv-btn-sm" :disabled="busyId === s.id" @click="saveAdvanced(s)">
                {{ busyId === s.id ? '保存中...' : '保存高级设置' }}
              </button>
            </div>
          </div>
          <div v-if="skills.length === 0" class="sv-empty" style="padding: 20px 8px">
            <div class="sv-empty-geo mb8">
              <span class="sq black" />
              <span class="sq pink" />
              <span class="sq deep" />
            <i class="diag" />
            </div>
            <p style="font-size: 12px">技能库为空</p>
            <p style="font-size: 11px">点击「导入技能」选择 JSON 文件</p>
          </div>
        </div>
      </div>

      <div class="sv-modal-foot">
        <button class="sv-btn primary" @click="close">完成</button>
      </div>
    </div>
  </div>
</template>
