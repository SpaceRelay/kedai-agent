<script setup lang="ts">
// 设置区:命令执行(阶段 E)。授权面板 + 执行器等级 + 审计日志。
//
// 平台可见性:本组件只在 Android Tauri 壳内展示 Android 专属档位(ROOT/Shizuku);
// 桌面/浏览器仍可看到「总开关」与审计(命令行工具在桌面同样可用)。
//
// 设计背景(docs/计划.md 阶段 3 / docs/契约-协议与配置.md):
// - 默认**全关**:root 能力不适合上架 Google Play,须用户显式开启(合规要求);
// - 等级可见:当前实际用哪一档(ROOT / Shizuku / 沙箱)必须让用户看得到;
// - 高危命令独立确认:破坏性/提权命令不受授权模式豁免,逐条确认;
// - 审计留痕:每次尝试(含被拒绝的)可在下方查看,root/ADB 级命令可回溯。
import { computed, onMounted, ref, watch } from 'vue';
import { useAppStore } from '../../store';
import { storeToRefs } from 'pinia';
import * as execApi from '../../api/exec';
import { isAndroidTauri } from '../../platform';

withDefaults(defineProps<{ show?: boolean }>(), { show: true });

const store = useAppStore();
const { execEnabled, execAllowRoot, execAllowShizuku, execAllowSandbox } = storeToRefs(store);

const saving = ref(false);
const msg = ref('');
const msgKind = ref<'ok' | 'err'>('ok');

/** 当前执行器等级(从后端探测;不依赖设置项,反映真实可用性) */
const tier = ref<execApi.ShellTier>('disabled');
const tierLabel = ref('');
const tierLoading = ref(false);

/** 审计列表 */
const audit = ref<execApi.ExecAuditEntry[]>([]);
const auditLoading = ref(false);
const auditError = ref('');
/** 审计过滤:来源与风险 */
const filterSource = ref('');
const filterRisk = ref('');

const RISK_LABEL: Record<string, string> = {
  safe: '只读',
  sensitive: '写入',
  destructive: '破坏性',
  admin: '提权/系统',
};
const TIER_LABEL: Record<string, string> = {
  root: 'ROOT 提权',
  shizuku: 'Shizuku(ADB 权限)',
  sandbox: '沙箱(应用自身权限)',
  disabled: '不可用',
};

/** 是否展示 Android 专属档位(桌面无 su/Shizuku 概念) */
const showAndroidTiers = computed(() => isAndroidTauri);

async function loadTier(): Promise<void> {
  tierLoading.value = true;
  try {
    const r = await execApi.getExecTier();
    tier.value = r.tier;
    tierLabel.value = r.label;
  } catch {
    tier.value = 'disabled';
    tierLabel.value = '读取失败';
  } finally {
    tierLoading.value = false;
  }
}

async function loadAudit(): Promise<void> {
  auditLoading.value = true;
  auditError.value = '';
  try {
    audit.value = await execApi.listExecAudit({
      limit: 100,
      source: filterSource.value || undefined,
      risk: filterRisk.value || undefined,
    });
  } catch (e) {
    auditError.value = `读取审计失败:${(e as Error).message}`;
  } finally {
    auditLoading.value = false;
  }
}

/** 保存开关(逐项 patch;后端 exec_* 为扁平字段,不分模式覆盖层) */
async function saveFlags(): Promise<void> {
  saving.value = true;
  msg.value = '';
  try {
    await store.queueSettingsSave({
      exec_enabled: execEnabled.value,
      exec_allow_root: execAllowRoot.value,
      exec_allow_shizuku: execAllowShizuku.value,
      exec_allow_sandbox: execAllowSandbox.value,
    });
    msgKind.value = 'ok';
    msg.value = '已保存命令执行设置';
    await loadTier();
  } catch (e) {
    msgKind.value = 'err';
    msg.value = `保存失败:${(e as Error).message}`;
  } finally {
    saving.value = false;
  }
}

/** 请求 Shizuku 授权(仅 Android 且已安装 Shizuku 时有意义) */
async function requestShizuku(): Promise<void> {
  msg.value = '';
  try {
    await store.requestShizukuPermission();
    msgKind.value = 'ok';
    msg.value = '已请求 Shizuku 授权,请在系统弹窗中允许';
    await loadTier();
  } catch (e) {
    msgKind.value = 'err';
    msg.value = `请求失败:${(e as Error).message}`;
  }
}

async function clearAudit(): Promise<void> {
  if (!window.confirm('确定清空全部命令审计记录?此操作不可恢复。')) return;
  try {
    const n = await execApi.clearExecAudit();
    msgKind.value = 'ok';
    msg.value = `已清空 ${n} 条审计记录`;
    await loadAudit();
  } catch (e) {
    msgKind.value = 'err';
    msg.value = `清空失败:${(e as Error).message}`;
  }
}

onMounted(() => {
  void loadTier();
  void loadAudit();
});
watch([filterSource, filterRisk], () => void loadAudit());
</script>

<template>
  <div v-show="show" class="sv-stack">
    <div class="sv-field-label sub">命令执行</div>
    <p class="sv-note">
      允许 Agent 通过 <code>bash</code> 工具执行 shell 命令。默认关闭。
      <b>破坏性与提权命令(删除/格式化/su/sudo/包管理等)不受授权模式豁免,一律逐条确认</b>;
      任务模式下此类高危命令不可用(无确认通道,直接拒绝),普通命令可用。
      每次尝试(含被拒绝的)都会留下审计记录。
    </p>

    <label class="sv-auth-tool-row">
      <input v-model="execEnabled" type="checkbox" />
      <b>启用命令执行</b>
      <span class="sv-note">总开关;关闭时 bash 工具一律拒绝并在审计中留痕</span>
    </label>

    <template v-if="showAndroidTiers">
      <div class="sv-separator" />
      <div class="sv-field-label sub">执行器等级(Android)</div>
      <div class="sv-inp-row">
        <label class="sv-inp-tag">当前等级</label>
        <span class="sv-tag" :class="tier === 'root' ? 'dangerous' : tier === 'disabled' ? '' : 'sensitive'">
          {{ tierLoading ? '探测中…' : (tierLabel || TIER_LABEL[tier]) }}
        </span>
        <button class="sv-btn ghost sv-btn-sm" :disabled="tierLoading" @click="loadTier">刷新</button>
        <button
          v-if="tier !== 'shizuku' && tier !== 'root'"
          class="sv-btn ghost sv-btn-sm"
          @click="requestShizuku"
        >请求 Shizuku 授权</button>
      </div>
      <p class="sv-note">
        ROOT 需设备已 root(su 可用);Shizuku 需先安装 Shizuku 应用并经 ADB 启动其服务;
        沙箱档为应用自身权限,能力等同本应用。逐档放行后才会以该档执行:
      </p>
      <label class="sv-auth-tool-row">
        <input v-model="execAllowRoot" type="checkbox" />
        <b>ROOT 提权</b>
        <span class="sv-badge dangerous">最高风险</span>
        <span class="sv-note">以 root 执行任意命令;不适合上架 Google Play,建议侧载</span>
      </label>
      <label class="sv-auth-tool-row">
        <input v-model="execAllowShizuku" type="checkbox" />
        <b>Shizuku(ADB 权限)</b>
        <span class="sv-badge sensitive">需 ADB 激活</span>
        <span class="sv-note">以 shell(UID 2000)权限执行,无需 root</span>
      </label>
      <label class="sv-auth-tool-row">
        <input v-model="execAllowSandbox" type="checkbox" />
        <b>沙箱档</b>
        <span class="sv-note">应用自身 UID;只能访问应用可及的文件与命令</span>
      </label>
    </template>

    <button class="sv-btn ghost sv-btn-fill" :disabled="saving" @click="saveFlags">
      {{ saving ? '保存中...' : '保存命令执行设置' }}
    </button>

    <div class="sv-separator" />

    <div class="sv-field-label sub">执行审计</div>
    <div class="sv-inp-row">
      <label class="sv-inp-tag">来源</label>
      <select v-model="filterSource" class="sv-select floor-select">
        <option value="">全部</option>
        <option value="chat">聊天</option>
        <option value="task">任务</option>
        <option value="android">Android</option>
      </select>
      <label class="sv-inp-tag">风险</label>
      <select v-model="filterRisk" class="sv-select floor-select">
        <option value="">全部</option>
        <option value="safe">只读</option>
        <option value="sensitive">写入</option>
        <option value="destructive">破坏性</option>
        <option value="admin">提权/系统</option>
      </select>
      <button class="sv-btn ghost sv-btn-sm" :disabled="auditLoading" @click="loadAudit">刷新</button>
      <button class="sv-btn ghost sv-btn-sm" @click="clearAudit">清空</button>
    </div>

    <div v-if="auditLoading" class="sv-note">加载审计…</div>
    <div v-else-if="auditError" class="sv-feedback err">{{ auditError }}</div>
    <div v-else-if="!audit.length" class="sv-note">暂无执行记录。</div>
    <div v-else class="sv-audit-list">
      <div v-for="e in audit" :key="e.id" class="sv-audit-row" :class="{ denied: e.decision === 'denied' }">
        <div class="sv-audit-head">
          <span class="sv-badge" :class="e.risk">{{ RISK_LABEL[e.risk] ?? e.risk }}</span>
          <span class="sv-tag sm">{{ e.tier }}</span>
          <span class="sv-tag sm">{{ e.source }}</span>
          <span v-if="e.decision === 'denied'" class="sv-badge-off">已拒绝</span>
          <span v-if="e.exit_code !== null" class="sv-note">退出码 {{ e.exit_code }}</span>
          <span class="sv-note">{{ e.ts }}</span>
        </div>
        <code class="sv-audit-cmd">{{ e.command }}</code>
        <div v-if="e.stdout_summary" class="sv-note sv-audit-out">{{ e.stdout_summary }}</div>
        <div v-if="e.stderr_summary" class="sv-note sv-audit-err">{{ e.stderr_summary }}</div>
      </div>
    </div>

    <div v-if="msg" class="sv-feedback" :class="msgKind === 'err' ? 'err' : 'ok'">{{ msg }}</div>
  </div>
</template>

<style scoped>
.sv-audit-list { max-height: 320px; overflow: auto; display: flex; flex-direction: column; gap: 8px; }
.sv-audit-row {
  border: 1px solid var(--sv-line, #d8d8d8);
  padding: 6px 8px;
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 12px;
}
.sv-audit-row.denied { border-color: var(--sv-red, #c0392b); opacity: 0.85; }
.sv-audit-head { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; }
.sv-audit-cmd {
  display: block;
  white-space: pre-wrap;
  word-break: break-all;
  background: rgba(0, 0, 0, 0.04);
  padding: 2px 4px;
}
.sv-audit-out { white-space: pre-wrap; word-break: break-all; opacity: 0.8; }
.sv-audit-err { white-space: pre-wrap; word-break: break-all; color: var(--sv-red, #c0392b); }
</style>
