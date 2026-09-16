<script setup lang="ts">
// 综合设置弹窗:左侧两级导航(功能域 → 分区/工具) + 右侧内容区(嵌入 SettingsModal 全部功能)
// 2026-09 入口整合:侧边栏底部原 4 个工具按钮(脚本管理/宏调试/记忆库/仓库索引)全部收进
// 「界面与工具」功能域,与既有 quickActions 去重;左栏由「9 分区 + 14 快速操作」共 23 项
// 压平列表改为「5 功能域 → 二级项」两层结构,降低视觉密度。
import { ref, computed, watch, onMounted } from 'vue';
import { useAppStore } from '../store';
import { health, type HealthInfo } from '../api/health';
// 独立面板 flag 的类型取弹窗注册表(web/src/modals.ts 单点定义,新增弹窗此处零改动)
import type { ModalFlag } from '../modals';
import SettingsModal from './SettingsModal.vue';

const store = useAppStore();

// 版本与构建指纹:展示在导航底部,测试版/便携版的指纹一致即两版同步(服务端编译期注入)
const buildInfo = ref<HealthInfo | null>(null);
onMounted(async () => {
  try {
    buildInfo.value = await health();
  } catch {
    buildInfo.value = null; // 服务不可达时不显示,不影响设置功能
  }
});

const buildInfoText = computed(() => {
  const b = buildInfo.value;
  if (!b?.version) return '';
  const time = b.build_time
    ? new Date(Number(b.build_time) * 1000).toLocaleString('zh-CN', { hour12: false })
    : '未知时间';
  return `v${b.version} · 构建 ${time} · 指纹 ${b.build_id ? b.build_id.slice(0, 8) : '--------'}`;
});

const close = (): void => {
  store.settingsOpen = false;
};

/** 设置分区键(与 SettingsModal 的 activeSection 一一对应) */
type SectionKey =
  | 'api' | 'model' | 'mcp' | 'embedding'
  | 'prompt' | 'preset'
  | 'agent' | 'flow'
  | 'data'
  | 'ui'
  | 'about';

/** 二级项:section 切右侧内容区;action 打开独立面板/执行操作后关闭 */
type HubItem =
  | { type: 'section'; key: SectionKey; label: string }
  | { type: 'action'; key: string; label: string; run: () => void };

interface Domain {
  key: string;
  label: string;
  dot: string;
  items: HubItem[];
}

/** 打开独立面板的统一写法:设开关后关闭 Hub,避免两层弹窗叠着 */
function openPanel(flag: ModalFlag): () => void {
  return () => { store[flag] = true; close(); };
}

/** 五大功能域(左栏一级;dot 用 .sv-supreme 几何色块,与至上主义黑白风格一致) */
const domains: Domain[] = [
  {
    key: 'conn', label: '连接与模型', dot: 'red',
    items: [
      { type: 'section', key: 'api', label: 'API 连接' },
      { type: 'section', key: 'model', label: '模型与生成' },
      { type: 'section', key: 'mcp', label: 'MCP 服务' },
    ],
  },
  {
    key: 'dialog', label: '对话与提示词', dot: 'orange',
    items: [
      { type: 'section', key: 'prompt', label: '提示词注入' },
      { type: 'section', key: 'preset', label: '预设导入' },
    ],
  },
  {
    key: 'agent', label: 'Agent 与任务', dot: 'blue',
    items: [
      { type: 'section', key: 'agent', label: 'Agent 设置' },
      { type: 'section', key: 'flow', label: '执行流程' },
    ],
  },
  {
    key: 'data', label: '数据与记忆', dot: 'yellow',
    items: [
      { type: 'section', key: 'data', label: '数据管理' },
      { type: 'section', key: 'embedding', label: '向量化模型' },
      { type: 'action', key: 'memory', label: '记忆库', run: openPanel('memoryOpen') },
    ],
  },
  {
    key: 'tools', label: '界面与工具', dot: 'blue',
    items: [
      { type: 'section', key: 'ui', label: '界面' },
      { type: 'action', key: 'upload', label: '上传角色卡', run: () => { store.characterUploadRequested++; } },
      { type: 'action', key: 'records', label: '聊天记录', run: openPanel('chatRecordsOpen') },
      { type: 'action', key: 'worldbooks', label: '世界书', run: openPanel('worldBooksOpen') },
      { type: 'action', key: 'contracts', label: '契约编辑', run: openPanel('contractsOpen') },
      { type: 'action', key: 'plugins', label: '插件', run: openPanel('pluginsOpen') },
      { type: 'action', key: 'skills', label: '技能库', run: openPanel('skillsOpen') },
      { type: 'action', key: 'prompts', label: '提示词管理', run: openPanel('promptsOpen') },
      { type: 'action', key: 'scripts', label: '脚本管理', run: openPanel('scriptsOpen') },
      { type: 'action', key: 'quickReplies', label: '快速回复', run: openPanel('quickRepliesOpen') },
      { type: 'action', key: 'macros', label: '宏调试', run: openPanel('macrosOpen') },
      { type: 'action', key: 'events', label: '事件监控', run: openPanel('eventsOpen') },
      { type: 'action', key: 'optimize', label: '优化面板', run: openPanel('optimizeOpen') },
      { type: 'action', key: 'repoIndex', label: '仓库索引', run: openPanel('repoIndexOpen') },
      { type: 'action', key: 'clear', label: '清空会话', run: () => { if (confirm('确定清空当前会话?')) void store.clearCurrentChat(); close(); } },
    ],
  },
  {
    key: 'about', label: '关于', dot: 'red',
    items: [
      { type: 'section', key: 'about', label: '关于与教程' },
    ],
  },
];

/** 角色扮演专属分区:任务模式下隐藏。'flow'(执行流程)不在此列——任务模式的 custom 依赖
 *  流程库(后端 TaskFlowAccess::current_flow 读不到已启用流程即报错),必须两种模式下都可见
 *  (IFW-3 入口错位修复,见 docs/遗留.md §九 已封堵项索引) */
const ROLEPLAY_ONLY: ReadonlySet<SectionKey> = new Set<SectionKey>(['prompt', 'preset']);

/** 按顶层模式过滤二级 section 项;过滤后为空的功能域整域隐藏 */
const visibleDomains = computed<Domain[]>(() =>
  domains
    .map((d) => ({
      ...d,
      items: d.items.filter(
        (it) => it.type === 'action' || store.appMode !== 'task' || !ROLEPLAY_ONLY.has(it.key),
      ),
    }))
    .filter((d) => d.items.length > 0),
);

const activeDomain = ref<string>('conn');
const activeSection = ref<SectionKey>('api');

/** 当前功能域的二级项(左栏下半部分渲染) */
const currentItems = computed<HubItem[]>(
  () => visibleDomains.value.find((d) => d.key === activeDomain.value)?.items ?? [],
);

/** 一级切换:自动落到该域第一个 section 项,保证右侧始终有内容可显示 */
function selectDomain(key: string): void {
  activeDomain.value = key;
  const items = visibleDomains.value.find((d) => d.key === key)?.items ?? [];
  const firstSection = items.find((it) => it.type === 'section');
  if (firstSection && firstSection.type === 'section') {
    activeSection.value = firstSection.key;
  }
}

/** 二级项点击:section 切内容区;action 执行后关闭 */
function onItemClick(item: HubItem): void {
  if (item.type === 'section') {
    activeSection.value = item.key;
  } else {
    item.run();
  }
}

// 切换模式后若当前激活分区被隐藏,回退到第一个可见分区的首个 section
watch(
  () => store.appMode,
  () => {
    const visible = visibleDomains.value;
    if (!visible.length) return;
    const stillVisible = visible.some((d) =>
      d.items.some((it) => it.type === 'section' && it.key === activeSection.value),
    );
    if (!stillVisible) {
      const first = visible[0];
      activeDomain.value = first.key;
      const firstSection = first.items.find((it) => it.type === 'section');
      if (firstSection && firstSection.type === 'section') activeSection.value = firstSection.key;
    }
  },
);

/** 右侧标题:功能域 · 分区名(让用户知道自己在哪一层) */
const activeLabel = computed(() => {
  const domain = visibleDomains.value.find((d) => d.key === activeDomain.value);
  const item = currentItems.value.find(
    (it) => it.type === 'section' && it.key === activeSection.value,
  );
  if (!domain) return '';
  return item ? `${domain.label} · ${item.label}` : domain.label;
});
</script>

<template>
  <div class="sv-modal-mask" @click.self="close">
    <div class="sv-modal sv-hub-modal">
      <div class="sv-modal-head">
        <h2 class="flex items-center gap-2">
          <span class="sv-supreme pink" /> 综合设置
        </h2>
        <button class="sv-btn ghost sv-btn-square" @click="close">✕</button>
      </div>

      <div class="sv-hub-body">
        <!-- 左侧导航:一级功能域 + 二级分区/工具(2026-09 两层结构) -->
        <div class="sv-hub-nav">
          <button
            v-for="(d, idx) in visibleDomains"
            :key="d.key"
            class="sv-hub-nav-item"
            :class="{ active: activeDomain === d.key }"
            @click="selectDomain(d.key)"
          >
            <span class="sv-hub-nav-idx">{{ String(idx + 1).padStart(2, '0') }}</span>
            <span class="sv-supreme" :class="d.dot" style="width: 10px; height: 10px; flex: none" />
            <span>{{ d.label }}</span>
          </button>

          <!-- 二级:当前功能域下的分区与工具(缩进区分层级) -->
          <div class="hub-sub">
            <button
              v-for="it in currentItems"
              :key="`${it.type}-${it.key}`"
              class="hub-sub-item"
              :class="{
                active: it.type === 'section' && activeSection === it.key,
                tool: it.type === 'action',
              }"
              @click="onItemClick(it)"
            >
              <span class="hub-sub-mark" />
              <span>{{ it.label }}</span>
              <span v-if="it.type === 'action'" class="hub-sub-arrow">→</span>
            </button>
          </div>

          <!-- 版本与构建指纹:测试版/便携版指纹一致即两版同步 -->
          <div
            v-if="buildInfoText"
            :title="'构建指纹 = 内嵌前端的内容哈希;两端一致即同步'"
            style="margin-top: auto; padding: 8px 12px 10px; font-size: var(--text-2xs); color: var(--sv-ink-faint); letter-spacing: 0.02em; user-select: text"
          >{{ buildInfoText }}</div>
        </div>

        <!-- 右侧内容区:嵌入 SettingsModal(完整设置功能) -->
        <div class="sv-hub-content">
          <h3 class="sv-hub-content-title">{{ activeLabel }}</h3>

          <!-- 嵌入原 SettingsModal 全部内容 -->
          <SettingsModal embedded :active-section="activeSection" />
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
/* 二级导航(2026-09 两级结构):缩进 + 左侧细线区分层级。
   样式写在 scoped 内(MAINTENANCE D-5:新增组件样式禁止进 style.css)。 */
.hub-sub {
  margin: 2px 0 8px 12px;
  padding-left: 10px;
  border-left: var(--bw-thin) solid var(--sv-line);
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.hub-sub-item {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 6px 10px;
  border: var(--bw-thin) solid transparent;
  background: transparent;
  font-size: var(--text-sm);
  color: var(--sv-ink-dim);
  text-align: left;
  cursor: pointer;
  /* 2026-09 动效:二级项随功能域切换逐条淡入(40ms 递增,与 sv-list-in 语言一致) */
  animation: hub-sub-in var(--dur-normal) var(--ease-out) backwards;
  transition: background var(--transition-fast), color var(--transition-fast),
    border-color var(--transition-fast);
}
.hub-sub-item:nth-child(1) { animation-delay: 0ms; }
.hub-sub-item:nth-child(2) { animation-delay: 40ms; }
.hub-sub-item:nth-child(3) { animation-delay: 80ms; }
.hub-sub-item:nth-child(4) { animation-delay: 120ms; }
.hub-sub-item:nth-child(n + 5) { animation-delay: 160ms; }
@keyframes hub-sub-in {
  from {
    opacity: 0;
    transform: translateX(-4px);
  }
  to {
    opacity: 1;
    transform: none;
  }
}
.hub-sub-item:hover {
  background: var(--sv-white);
  border-color: var(--sv-pink);
  color: var(--sv-ink);
}
/* 当前分区:黑底白字,与一级 active 视觉语言一致但更轻(不做全黑,避免抢一级层级) */
.hub-sub-item.active {
  background: var(--sv-ink);
  color: var(--sv-white);
  border-color: var(--sv-ink);
}
/* 工具项:虚线段标记 + 尾部箭头,提示「点开是独立面板」而非切换内容区 */
.hub-sub-mark {
  width: 8px;
  height: 2px;
  flex: none;
  background: var(--sv-ink-faint);
}
.hub-sub-item.active .hub-sub-mark { background: var(--sv-red); }
.hub-sub-item.tool .hub-sub-mark {
  background: repeating-linear-gradient(
    90deg,
    var(--sv-ink-faint) 0 2px,
    transparent 2px 4px
  );
}
.hub-sub-arrow {
  margin-left: auto;
  font-size: var(--text-2xs);
  color: var(--sv-ink-faint);
}
</style>
