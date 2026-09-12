<script setup lang="ts">
// 设置区:授权管理(三档模式 + 始终需授权清单 + 授权等待超时 + 已授权限撤销)。
// 三档模式与「始终需授权」在 roleplay 与 task 两侧共用一份授权裁决(授权按会话/角色),
// 故本区不按模式隐藏;撤销列表依赖当前会话(未选会话时给提示)。
//
// 设计背景(2026-09 授权改造):
// - 原「授权/放行」二档语义粗糙:放行会连写文件、改变量一起自动放行,黑名单默认值
//   又是四个不存在的工具名,形同虚设。
// - 现改为按「操作类型 × 路径区域」判定的三档,并把「始终需授权」清单交还用户。
// - 授权一旦授予原先无法查看与撤销(该角色所有会话永久生效),本区补上出口。
import { computed, onMounted, ref, watch } from 'vue';
import { useAppStore } from '../../store';
import { storeToRefs } from 'pinia';
import * as api from '../../api';

withDefaults(defineProps<{ show?: boolean }>(), { show: true });

const store = useAppStore();
const {
  authorizationMode, authorizationAlwaysRequired, toolAuthorizationTimeoutSecs,
  currentSessionId, currentCharacterId,
} = storeToRefs(store);

/** 三档说明(与后端 AuthorizationMode 一致) */
const MODES: Array<{ key: 'strict' | 'loose' | 'bypass'; label: string; desc: string }> = [
  { key: 'strict', label: '严格', desc: '读文件、写文件、删文件都需授权' },
  { key: 'loose', label: '宽松', desc: '读/写文件自动放行;删文件需授权' },
  { key: 'bypass', label: '放行', desc: '仅写/删系统路径(如 C 盘)需授权,其余放行' },
];

const saving = ref(false);
const msg = ref('');
const loadErr = ref('');

/** 全部工具(名称/风险/当前是否放行),用于「始终需授权」勾选与授权查看 */
const tools = ref<api.ToolPermission[]>([]);
/** 当前会话/角色的显式授权 */
const grants = ref<{ session: string[]; role: string[] }>({ session: [], role: [] });
const loading = ref(false);

const RISK_LABEL: Record<string, string> = { safe: '安全', sensitive: '敏感', dangerous: '危险' };

/** 「始终需授权」勾选集(与 store 同步,保存时整体写回) */
const alwaysRequired = ref<string[]>([]);
watch(authorizationAlwaysRequired, (v) => { alwaysRequired.value = [...(v ?? [])]; }, { immediate: true });
watch(toolAuthorizationTimeoutSecs, (v) => { timeout.value = v ?? 300; });

const timeout = ref(toolAuthorizationTimeoutSecs.value ?? 300);

async function load(): Promise<void> {
  if (!currentSessionId.value) return;
  loading.value = true;
  loadErr.value = '';
  try {
    // 工具清单随会话查询(需会话/角色上下文以判定 allowed)
    const list = await api.getToolPermissions(currentSessionId.value, currentCharacterId.value ?? '');
    tools.value = list;
    const raw = await api.getGrants(currentSessionId.value, currentCharacterId.value ?? '');
    grants.value = { session: raw.session ?? [], role: raw.role ?? [] };
  } catch (e) {
    loadErr.value = `读取授权状态失败:${(e as Error).message}`;
  } finally {
    loading.value = false;
  }
}

onMounted(() => { void load(); });
watch(currentSessionId, () => { void load(); });

/** 保存三档模式(切换即持久化,与输入框底部开关同一入口) */
async function pickMode(mode: 'strict' | 'loose' | 'bypass'): Promise<void> {
  if (authorizationMode.value === mode) return;
  saving.value = true;
  msg.value = '';
  try {
    await store.setAuthorizationMode(mode);
    msg.value = `已切换为「${MODES.find((m) => m.key === mode)?.label}」模式`;
  } catch (e) {
    msg.value = `保存失败:${(e as Error).message}`;
  } finally {
    saving.value = false;
  }
}

/** 保存「始终需授权」清单与超时(同一次 patch) */
async function savePolicy(): Promise<void> {
  saving.value = true;
  msg.value = '';
  try {
    await store.queueSettingsSave({
      bypass_blacklist: alwaysRequired.value,
      tool_authorization_timeout_secs: timeout.value,
    });
    msg.value = '已保存授权策略';
  } catch (e) {
    msg.value = `保存失败:${(e as Error).message}`;
  } finally {
    saving.value = false;
  }
}

/** 撤销一条显式授权(session / role 两种作用域) */
async function revoke(tool: string, scope: 'session' | 'role'): Promise<void> {
  if (!currentSessionId.value) return;
  saving.value = true;
  msg.value = '';
  try {
    await api.revokeTool(tool, scope, currentSessionId.value);
    msg.value = `已撤销「${tool}」的${scope === 'session' ? '会话' : '角色'}授权`;
    await load();
  } catch (e) {
    msg.value = `撤销失败:${(e as Error).message}`;
  } finally {
    saving.value = false;
  }
}

const dangerousTools = computed(() => tools.value.filter((t) => t.risk === 'dangerous'));
const grantedCount = computed(() => grants.value.session.length + grants.value.role.length);
</script>

<template>
  <div v-show="show" class="sv-stack">
    <div class="sv-field-label sub">授权模式</div>
    <p class="sv-note">
      按「操作类型 × 路径区域」判定:文件操作分读/写/删,系统路径(盘符/UNC/绝对路径)写删在三档下都需授权。
      写对话正文(气泡)不受文件规则限制。
    </p>
    <div class="sv-auth-modes">
      <button
        v-for="m in MODES"
        :key="m.key"
        class="sv-auth-mode-card"
        :class="{ active: authorizationMode === m.key }"
        :disabled="saving"
        @click="pickMode(m.key)"
      >
        <b>{{ m.label }}</b>
        <span>{{ m.desc }}</span>
      </button>
    </div>

    <div class="sv-separator" />

    <div class="sv-field-label sub">始终需授权</div>
    <p class="sv-note">
      勾选后,这些工具在三档模式下都需手动授权(不受模式自动放行影响)。默认空。
    </p>
    <div v-if="loading" class="sv-note">加载工具清单…</div>
    <div v-else-if="loadErr" class="sv-feedback err">{{ loadErr }}</div>
    <div v-else class="sv-auth-tools">
      <label v-for="t in tools" :key="`ar-${t.name}`" class="sv-auth-tool-row">
        <input v-model="alwaysRequired" type="checkbox" :value="t.name" />
        <code>{{ t.name }}</code>
        <span class="sv-badge" :class="t.risk">{{ RISK_LABEL[t.risk] ?? t.risk }}</span>
        <span class="sv-note">{{ t.description }}</span>
      </label>
    </div>

    <div class="sv-inp-row">
      <label class="sv-inp-tag">授权等待超时</label>
      <input v-model.number="timeout" type="number" min="30" max="1800" step="10" class="sv-input inject-num" />
      <span class="sv-note">秒(30-1800,默认 300);仅影响需要授权的聊天路径</span>
    </div>
    <button class="sv-btn ghost sv-btn-fill" :disabled="saving" @click="savePolicy">
      {{ saving ? '保存中...' : '保存授权策略' }}
    </button>

    <div class="sv-separator" />

    <div class="sv-field-label sub">已授权限(可撤销)</div>
    <p class="sv-note">
      授权一经授予会持续生效(角色授权对该角色所有会话生效),此处可查看并撤销。
      <template v-if="grantedCount > 0">当前共 {{ grantedCount }} 条。</template>
    </p>
    <div v-if="!currentSessionId" class="sv-note">未选择会话:选择会话后可查看与撤销授权。</div>
    <template v-else>
      <div v-if="grants.session.length" class="sv-stack">
        <b class="sv-note">当前会话授权</b>
        <div v-for="name in grants.session" :key="`s-${name}`" class="sv-auth-grant-row">
          <code>{{ name }}</code>
          <button class="sv-btn ghost sv-btn-sm" :disabled="saving" @click="revoke(name, 'session')">撤销</button>
        </div>
      </div>
      <div v-if="grants.role.length" class="sv-stack">
        <b class="sv-note">当前角色授权(该角色所有会话生效)</b>
        <div v-for="name in grants.role" :key="`r-${name}`" class="sv-auth-grant-row">
          <code>{{ name }}</code>
          <button class="sv-btn ghost sv-btn-sm" :disabled="saving" @click="revoke(name, 'role')">撤销</button>
        </div>
      </div>
      <div v-if="!grantedCount && !loading" class="sv-note">当前没有显式授权(安全工具与模式放行的操作不记入此列表)。</div>
    </template>

    <div v-if="msg" class="sv-feedback" :class="msg.startsWith('保存失败') || msg.startsWith('撤销失败') ? 'err' : 'ok'">{{ msg }}</div>

    <div v-if="dangerousTools.length" class="sv-note">
      危险级工具:{{ dangerousTools.map((t) => t.name).join('、') }}
    </div>
  </div>
</template>

<style scoped>
.sv-auth-modes { display: flex; gap: 8px; flex-wrap: wrap; }
.sv-auth-mode-card {
  flex: 1 1 140px; text-align: left; padding: 8px 10px; cursor: pointer;
  border: 1px solid var(--sv-line, #d8d8d8); background: transparent;
  display: flex; flex-direction: column; gap: 2px;
}
.sv-auth-mode-card.active { border-color: #111; box-shadow: inset 0 -2px 0 #111; }
.sv-auth-mode-card b { font-size: 13px; }
.sv-auth-mode-card span { font-size: 11px; opacity: 0.7; }
.sv-auth-tools { max-height: 260px; overflow: auto; display: flex; flex-direction: column; gap: 4px; }
.sv-auth-tool-row { display: flex; align-items: center; gap: 6px; font-size: 12px; }
.sv-auth-tool-row .sv-note { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.sv-auth-grant-row { display: flex; align-items: center; gap: 8px; justify-content: space-between; }
</style>
