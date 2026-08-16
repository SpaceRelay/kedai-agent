// 自定义 Agent 执行流程(custom 模式):流程库选择/新建/复制/删除/导入/导出 + 步骤编辑/拖拽。
import { computed, ref } from 'vue';
import { useAppStore } from '../store';
import * as api from '../api';
import {
  cleanStepsTools,
  setStepToolMode,
  setStepToolsText,
  stepToolMode,
  stepToolsText,
} from '../utils/agentFlowTools';

/** 工具模式选择(与后端 tools 语义对齐:null=不使用,[]=全部,list=白名单) */
export const TOOL_MODE_LABELS = {
  none: '不使用工具',
  all: '全部工具',
  list: '白名单',
} as const;

export function useAgentFlow() {
  const store = useAppStore();

  const flowDraft = ref<api.AgentFlowConfig | null>(null);
  const flowSaving = ref(false);
  const flowMsg = ref('');
  const editingStepId = ref<string | null>(null);
  const dragStepId = ref<string | null>(null);
  /** 当前正在编辑的流程 id(库中已存在或新建待保存) */
  const flowId = ref<string | null>(null);
  const flowName = ref('');
  const flowDesc = ref('');
  const flowImportInput = ref<HTMLInputElement | null>(null);
  const flowImporting = ref(false);

  /** 流程库全部流程(选择器选项) */
  const flowLibFlows = computed(() => store.agentFlowLibrary?.flows ?? []);

  /** 从服务端加载流程库并克隆当前选中流程为可编辑草稿(保存时才提交) */
  async function loadFlowConfig(): Promise<void> {
    await store.loadAgentFlow();
    const lib = store.agentFlowLibrary;
    if (!lib) {
      flowDraft.value = null;
      return;
    }
    const cur = lib.flows.find((f) => f.id === lib.current_flow_id) ?? lib.flows[0] ?? null;
    flowDraft.value = cur ? (JSON.parse(JSON.stringify(cur)) as api.AgentFlowConfig) : null;
    flowId.value = cur?.id ?? null;
    flowName.value = cur?.name ?? '';
    flowDesc.value = cur?.description ?? '';
  }

  /** 切换流程:确认后改选中并重载草稿(丢弃未保存修改) */
  async function onFlowSelect(): Promise<void> {
    if (!flowId.value) return;
    if (flowDraft.value && !window.confirm('切换流程将丢弃当前未保存的修改,确定继续?')) {
      flowId.value = flowDraft.value.id ?? null;
      return;
    }
    try {
      await store.selectAgentFlow(flowId.value);
      await loadFlowConfig();
      flowMsg.value = `已切换到「${flowName.value || '未命名流程'}」`;
      setTimeout(() => (flowMsg.value = ''), 2500);
    } catch (e) {
      flowMsg.value = `切换失败:${(e as Error).message}`;
    }
  }

  function newFlowId(): string {
    return (crypto.randomUUID?.() ?? `flow-${Date.now()}`) as string;
  }

  /** 新建流程:空流程立即入库并选中(未启用,校验放行),随后可编辑步骤 */
  async function newFlow(): Promise<void> {
    const count = flowLibFlows.value.length + 1;
    const draft: api.AgentFlowConfig = {
      id: newFlowId(),
      name: `新流程 ${count}`,
      description: '',
      enabled: false,
      steps: [],
    };
    try {
      await store.saveAgentFlowConfig(draft);
      await loadFlowConfig();
      editingStepId.value = null;
      flowMsg.value = '已新建流程,点击「＋ 新增步骤」开始编辑';
      setTimeout(() => (flowMsg.value = ''), 3000);
    } catch (e) {
      flowMsg.value = `新建失败:${(e as Error).message}`;
    }
  }

  /** 复制当前流程(新 id,名称加「副本」,立即入库并选中) */
  async function duplicateFlow(): Promise<void> {
    const cur = flowDraft.value;
    if (!cur || !flowId.value) return;
    const copy: api.AgentFlowConfig = JSON.parse(JSON.stringify(cur)) as api.AgentFlowConfig;
    copy.id = newFlowId();
    copy.name = `${cur.name || '未命名流程'} 副本`;
    try {
      await store.saveAgentFlowConfig(copy);
      await loadFlowConfig();
      flowMsg.value = '已复制当前流程';
      setTimeout(() => (flowMsg.value = ''), 2500);
    } catch (e) {
      flowMsg.value = `复制失败:${(e as Error).message}`;
    }
  }

  /** 删除当前流程(服务端删除当前流程后回退到第一个) */
  async function deleteFlowNow(): Promise<void> {
    if (!flowId.value) return;
    if (!window.confirm(`确定删除流程「${flowName.value || '未命名流程'}」?该操作不可撤销。`)) return;
    try {
      await store.deleteAgentFlow(flowId.value);
      await loadFlowConfig();
      flowMsg.value = '流程已删除';
      setTimeout(() => (flowMsg.value = ''), 2500);
    } catch (e) {
      flowMsg.value = `删除失败:${(e as Error).message}`;
    }
  }

  /** 导入流程 JSON:支持单流程 {name,enabled,steps}、{config:{…}} 包装或流程库 {flows:[…]}
   *  (库格式取当前选中流程);无 id 时自动分配,导入后立即入库并选中。 */
  async function onFlowImport(e: Event): Promise<void> {
    const input = e.target as HTMLInputElement;
    const file = input.files?.[0];
    input.value = '';
    if (!file || flowImporting.value) return;
    flowImporting.value = true;
    flowMsg.value = '';
    try {
      const raw = JSON.parse(await file.text()) as unknown;
      let cfg: api.AgentFlowConfig | null = null;
      if (raw && typeof raw === 'object') {
        const obj = raw as Record<string, unknown>;
        if (Array.isArray(obj.steps)) {
          cfg = obj as unknown as api.AgentFlowConfig;
        } else if (obj.config && typeof obj.config === 'object') {
          cfg = (obj.config as Record<string, unknown>) as unknown as api.AgentFlowConfig;
        } else if (Array.isArray(obj.flows)) {
          const lib = obj as unknown as api.AgentFlowLibrary;
          cfg = lib.flows.find((f) => f.id === lib.current_flow_id) ?? lib.flows[0] ?? null;
        }
      }
      if (!cfg || !Array.isArray(cfg.steps)) {
        throw new Error('无法识别的流程文件:需包含 steps 数组(单流程 / {config} 包装 / 流程库)');
      }
      // 恒分配新 id:导入 = 创建全新流程,避免文件内 id 与库中已有流程撞车被静默覆盖
      cfg.id = newFlowId();
      cfg.name = cfg.name || '导入流程';
      cfg.enabled = !!cfg.enabled;
      if (!Array.isArray(cfg.steps)) throw new Error('流程缺少 steps 数组');
      await store.saveAgentFlowConfig(cfg);
      await loadFlowConfig();
      flowMsg.value = `已导入流程「${cfg.name}」(${cfg.steps.length} 步)`;
      setTimeout(() => (flowMsg.value = ''), 3000);
    } catch (err) {
      flowMsg.value = `导入失败:${(err as Error).message}`;
    } finally {
      flowImporting.value = false;
    }
  }

  /** 导出当前流程为 JSON 文件(不含 id,便于重新导入为全新流程) */
  function exportFlowNow(): void {
    const cur = flowDraft.value;
    if (!cur) return;
    const payload = {
      name: flowName.value || cur.name || '未命名流程',
      description: flowDesc.value || cur.description || undefined,
      enabled: cur.enabled,
      steps: cur.steps,
    };
    const blob = new Blob([JSON.stringify(payload, null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `kedai-flow-${(flowName.value || '未命名').replace(/[\\/:*?"<>|]/g, '_')}.json`;
    a.click();
    URL.revokeObjectURL(url);
  }

  async function saveFlowNow(): Promise<void> {
    if (!flowDraft.value || flowSaving.value) return;
    flowSaving.value = true;
    flowMsg.value = '';
    try {
      flowDraft.value.id = flowId.value ?? undefined;
      flowDraft.value.name = flowName.value;
      flowDraft.value.description = flowDesc.value || null;
      // 保存前清洗各步骤 tools:去空白项;白名单仅剩占位空串 → 不使用;显式 [] (全部)保留
      cleanStepsTools(flowDraft.value.steps);
      await store.saveAgentFlowConfig(flowDraft.value);
      await loadFlowConfig();
      flowMsg.value = '执行流程已保存';
      setTimeout(() => (flowMsg.value = ''), 2500);
    } catch (e) {
      flowMsg.value = `保存失败:${(e as Error).message}`;
    } finally {
      flowSaving.value = false;
    }
  }

  function newStepId(): string {
    return (crypto.randomUUID?.() ?? `s-${Date.now()}`) as string;
  }

  function addStep(): void {
    if (!flowDraft.value) return;
    const id = newStepId();
    flowDraft.value.steps.push({
      id,
      name: '新步骤',
      enabled: true,
      goal: '',
      action: 'direct',
      generates: true,
      system_prompt: null,
      temperature: null,
      max_tokens: null,
      tools: null,
      tool_choice: 'auto',
      tool_choice_function: null,
      parallel_tool_calls: null,
    });
    editingStepId.value = id;
  }

  function removeStep(id: string): void {
    if (!flowDraft.value) return;
    flowDraft.value.steps = flowDraft.value.steps.filter((s) => s.id !== id);
    if (editingStepId.value === id) editingStepId.value = null;
  }

  /** ↑/↓ 移动(拖拽的兜底操作;数组顺序即执行顺序) */
  function moveStep(id: string, dir: -1 | 1): void {
    const steps = flowDraft.value?.steps;
    if (!steps) return;
    const idx = steps.findIndex((s) => s.id === id);
    const to = idx + dir;
    if (idx < 0 || to < 0 || to >= steps.length) return;
    [steps[idx], steps[to]] = [steps[to], steps[idx]];
  }

  /** 切换步骤动作:反思步骤不允许生成/系统提示词;直接步骤默认生成 */
  function onStepActionChange(s: api.AgentFlowStep): void {
    if (s.action === 'reflect') {
      s.generates = undefined;
      s.system_prompt = null;
      s.temperature = null;
      s.max_tokens = null;
      s.tools = null;
      s.tool_choice = 'auto';
      s.tool_choice_function = null;
      s.parallel_tool_calls = null;
    } else if (s.generates === undefined) {
      s.generates = true;
    }
  }

  // ===== 步骤拖拽(HTML5 DnD;数组顺序即执行顺序) =====
  function onStepDragStart(e: DragEvent, id: string): void {
    dragStepId.value = id;
    if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move';
  }
  function onStepDragOver(e: DragEvent, id: string): void {
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'move';
  }
  function onStepDrop(e: DragEvent, targetId: string): void {
    e.preventDefault();
    const fromId = dragStepId.value;
    dragStepId.value = null;
    if (!fromId || fromId === targetId) return;
    const steps = flowDraft.value?.steps;
    if (!steps) return;
    const from = steps.findIndex((s) => s.id === fromId);
    const to = steps.findIndex((s) => s.id === targetId);
    if (from < 0 || to < 0) return;
    const [item] = steps.splice(from, 1);
    steps.splice(to, 0, item);
  }
  function onStepDragEnd(): void {
    dragStepId.value = null;
  }

  return {
    flowDraft, flowSaving, flowMsg, editingStepId, dragStepId, flowId, flowName, flowDesc,
    flowImportInput, flowImporting, flowLibFlows,
    loadFlowConfig, onFlowSelect, newFlow, duplicateFlow, deleteFlowNow, onFlowImport,
    exportFlowNow, saveFlowNow, addStep, removeStep, moveStep, onStepActionChange,
    onStepDragStart, onStepDragOver, onStepDrop, onStepDragEnd,
    // 工具模式三态 UI 与后端字段互转(utils/agentFlowTools)
    stepToolMode, setStepToolMode, stepToolsText, setStepToolsText,
  };
}
