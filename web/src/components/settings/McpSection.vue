<script setup lang="ts">
// 设置区:MCP 服务(批次 6.2,L3 隔离;默认关)。
// 总开关 mcp_enabled + 服务器列表(name/command/args 空格分隔/启用 toggle/删除)+ 新增行表单。
// 读写走 settings 既有通道(store.saveSettings patch),参照 GenParamsSection 模式:
// 直接编辑 store 草稿,保存时全量提交;v1 仅启动时装配,改动重启后生效(无热重连)。
import { ref } from 'vue';
import { useAppStore } from '../../store';
import { storeToRefs } from 'pinia';
import type { McpServerConfig } from '../../api/types';

withDefaults(defineProps<{
  /** 是否显示(embedded 模式按 activeSection 切换;standalone 恒 true) */
  show?: boolean;
}>(), {
  show: true,
});

const store = useAppStore();
const { mcpEnabled, mcpServers } = storeToRefs(store);

// ===== 新增行表单(本地草稿,添加后清空) =====
const newName = ref('');
const newCommand = ref('');
const newArgs = ref('');

/** args 空格分隔文本 ↔ 数组(列表行内联编辑用) */
function argsText(s: McpServerConfig): string {
  return (s.args ?? []).join(' ');
}
function setArgs(s: McpServerConfig, v: string): void {
  s.args = v.split(/\s+/).filter(Boolean);
}

function addServer(): void {
  const name = newName.value.trim();
  const command = newCommand.value.trim();
  if (!name || !command) return; // 缺名/缺命令不添加(后端同样会清理)
  mcpServers.value.push({
    name,
    command,
    args: newArgs.value.split(/\s+/).filter(Boolean),
    enabled: true,
  });
  newName.value = '';
  newCommand.value = '';
  newArgs.value = '';
}

function removeServer(index: number): void {
  mcpServers.value.splice(index, 1);
}

// ===== 保存(全量替换语义;改动重启后生效) =====
const saving = ref(false);
const saveMsg = ref('');

async function saveNow(): Promise<void> {
  saving.value = true;
  saveMsg.value = '';
  try {
    await store.saveSettings({
      mcp_enabled: mcpEnabled.value,
      mcp_servers: mcpServers.value,
    });
    saveMsg.value = '已保存,重启后生效';
    setTimeout(() => (saveMsg.value = ''), 2500);
  } catch (e) {
    saveMsg.value = `保存失败:${(e as Error).message}`;
  } finally {
    saving.value = false;
  }
}
</script>

<template>
  <div v-show="show" class="sv-field">
    <div class="sv-field-label"><span class="sv-supreme blue" /> MCP 服务</div>
    <div class="sv-datalist">
      <div class="sv-data-row">
        <div class="info">
          <b>启用 MCP 服务</b>
          <span>连接本机 MCP stdio 服务器,其工具以 mcp_ 前缀注入 Agent(未知工具默认按危险级需授权)。改动重启后生效</span>
        </div>
        <button type="button" class="sv-btn ghost" :aria-pressed="mcpEnabled" @click="mcpEnabled = !mcpEnabled">
          {{ mcpEnabled ? '已开启' : '已关闭' }}
        </button>
      </div>

      <div v-for="(s, i) in mcpServers" :key="`${s.name}-${i}`" class="sv-data-row" style="align-items: flex-start">
        <div class="info" style="flex: 1; min-width: 0">
          <div class="sv-inp-row">
            <label class="sv-inp-tag">名称</label>
            <input v-model="s.name" class="sv-input" placeholder="如 filesystem" title="服务器名,注册工具前缀 mcp_{名称}_{工具}" />
          </div>
          <div class="sv-inp-row">
            <label class="sv-inp-tag">命令</label>
            <input v-model="s.command" class="sv-input" placeholder="如 npx" title="可执行命令" />
          </div>
          <div class="sv-inp-row">
            <label class="sv-inp-tag">参数</label>
            <input
              :value="argsText(s)"
              class="sv-input"
              placeholder="空格分隔,如 -y @modelcontextprotocol/server-filesystem"
              title="命令行参数(空格分隔)"
              @input="setArgs(s, ($event.target as HTMLInputElement).value)"
            />
          </div>
        </div>
        <div style="display: grid; gap: 6px; justify-items: end">
          <button type="button" class="sv-btn ghost" :aria-pressed="s.enabled" @click="s.enabled = !s.enabled">
            {{ s.enabled ? '启用中' : '已停用' }}
          </button>
          <button type="button" class="sv-btn ghost sv-btn-sm" @click="removeServer(i)">删除</button>
        </div>
      </div>

      <div class="sv-data-row" style="align-items: flex-start">
        <div class="info" style="flex: 1; min-width: 0">
          <b>新增服务器</b>
          <div class="sv-inp-row">
            <label class="sv-inp-tag">名称</label>
            <input v-model="newName" class="sv-input" placeholder="唯一名称" />
          </div>
          <div class="sv-inp-row">
            <label class="sv-inp-tag">命令</label>
            <input v-model="newCommand" class="sv-input" placeholder="可执行命令" />
          </div>
          <div class="sv-inp-row">
            <label class="sv-inp-tag">参数</label>
            <input v-model="newArgs" class="sv-input" placeholder="空格分隔参数(可空)" />
          </div>
        </div>
        <button type="button" class="sv-btn ghost" :disabled="!newName.trim() || !newCommand.trim()" @click="addServer">
          添加
        </button>
      </div>

      <div class="sv-btn-row">
        <button class="sv-btn ghost sv-btn-fill" :disabled="saving" @click="saveNow">
          {{ saving ? '保存中...' : '保存 MCP 设置' }}
        </button>
        <div v-if="saveMsg" class="sv-feedback ok sv-feedback-flex">{{ saveMsg }}</div>
      </div>
      <p class="sv-note">MCP 服务仅在本机启动时装配;修改开关或服务器列表后需重启生效。服务器启动失败仅禁用该台,不影响其他功能。</p>
    </div>
  </div>
</template>
