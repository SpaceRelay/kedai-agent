<script setup lang="ts">
// 技能库弹窗:导入提示词技能(JSON 单对象/数组)、每项带 启用/停用 与 删除 按钮。
// 技能通过 Agent 模式的 read 工具读取,可注入上下文。
import { ref, onMounted } from 'vue';
import { useAppStore } from '../store';
import { storeToRefs } from 'pinia';

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

async function onToggle(s: { id: string; enabled: boolean }): Promise<void> {
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

onMounted(() => void store.loadSkills());
</script>

<template>
  <div class="sv-modal-mask" @click.self="close">
    <div class="sv-modal md">
      <div class="sv-modal-head">
        <h2 class="flex items-center gap-2">
          <span class="logo" /> 技能库
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
              <button class="sv-btn danger sv-btn-sm" :disabled="busyId === s.id" @click="onDelete(s.id, s.name)">
                删除
              </button>
            </div>
          </div>
          <div v-if="skills.length === 0" class="sv-note" style="padding: 8px 0">
            技能库为空。点击「导入技能」选择 JSON 文件。
          </div>
        </div>
      </div>

      <div class="sv-modal-foot">
        <button class="sv-btn primary" @click="close">完成</button>
      </div>
    </div>
  </div>
</template>
