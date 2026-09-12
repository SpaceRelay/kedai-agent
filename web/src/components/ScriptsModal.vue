<script setup lang="ts">
// 用户脚本管理模态框(阶段三 3c):ScriptTree 树形编辑。
//   - 两级作用域:global(全应用)/ character(随角色卡 extensions.tavern_helper 导出)
//   - 树形列表:顶层 script / folder;folder 内可再含 script(一层)
//   - 编辑面板:名称/启用/内容(script)、图标/子脚本(folder)、按钮组(script)
//   - 保存:全量 PUT /api/scripts/tree(角色脚本在下次消息生成后由后端执行)
import { ref, computed, onMounted } from 'vue';
import { storeToRefs } from 'pinia';
import { useAppStore } from '../store';
import * as api from '../api';
import { parseScriptTreeImport, stripByExportWith } from '../scriptTreeIO';
import { saveExportFile } from '../exportFile';

const store = useAppStore();
const { characters, currentCharacterId } = storeToRefs(store);

const scope = ref<api.ScriptScope>('global');
/** 角色级脚本的目标角色(默认取当前选中角色) */
const characterId = ref<string>(currentCharacterId.value ?? '');
const trees = ref<api.ScriptTree>([]);
const loaded = ref(false);
const saving = ref(false);
const msg = ref<{ kind: 'ok' | 'err'; text: string } | null>(null);
/** 正在编辑的节点位置:folder=顶层索引(null=顶层 script),script=子脚本索引(null=节点本身是 folder/顶层 script) */
const editing = ref<{ folder: number | null; script: number | null } | null>(null);
// ===== 导入导出(阶段六 6e) =====
/** 导出时仅保留 data/button(按 export_with 语义剥离 content/info) */
const onlyDataButton = ref(false);
const exporting = ref(false);
const importing = ref(false);
/** 导入后已替换列表但未保存(提示用户点「保存脚本」写入) */
const imported = ref(false);
const importFile = ref<HTMLInputElement | null>(null);

function close(): void {
  store.scriptsOpen = false;
}

async function load(): Promise<void> {
  loaded.value = false;
  msg.value = null;
  try {
    if (scope.value === 'character' && !characterId.value) {
      trees.value = [];
      return;
    }
    trees.value = await api.getScriptTree(
      scope.value,
      scope.value === 'character' ? characterId.value : undefined,
    );
  } catch (err) {
    msg.value = { kind: 'err', text: `加载脚本失败:${(err as Error).message}` };
  } finally {
    loaded.value = true;
  }
}

function switchScope(s: api.ScriptScope): void {
  scope.value = s;
  editing.value = null;
  void load();
}

function onCharacterChange(): void {
  editing.value = null;
  void load();
}

// ===== 节点操作 =====
function uid(): string {
  return crypto?.randomUUID?.() ?? `s-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

function newScript(): void {
  const node: api.ScriptNode = {
    type: 'script', enabled: true, name: '新脚本', id: uid(), content: '',
    info: '', button: { enabled: true, buttons: [] }, data: {},
    export_with: { data: true, button: true },
  };
  trees.value.push(node);
  editing.value = { folder: null, script: trees.value.length - 1 };
}

function newFolder(): void {
  const node: api.ScriptFolder = {
    type: 'folder', enabled: true, name: '新文件夹', id: uid(),
    icon: 'fa-solid fa-folder', color: '', scripts: [],
  };
  trees.value.push(node);
  editing.value = { folder: trees.value.length - 1, script: null };
}

function newSubScript(folderIdx: number): void {
  const f = trees.value[folderIdx];
  if (f.type !== 'folder') return;
  const node: api.ScriptNode = {
    type: 'script', enabled: true, name: '新脚本', id: uid(), content: '',
    button: { enabled: true, buttons: [] },
  };
  f.scripts.push(node);
  editing.value = { folder: folderIdx, script: f.scripts.length - 1 };
}

function deleteNode(folder: number | null, script: number | null): void {
  if (folder === null && script !== null) {
    trees.value.splice(script, 1);
  } else if (folder !== null && script === null) {
    trees.value.splice(folder, 1);
  } else if (folder !== null && script !== null) {
    const f = trees.value[folder];
    if (f.type === 'folder') f.scripts.splice(script, 1);
  }
  editing.value = null;
}

/** 编辑中的节点引用(顶层 script / folder / 子 script) */
const editingNode = computed<api.ScriptTreeNode | null>(() => {
  if (!editing.value) return null;
  const { folder, script } = editing.value;
  if (folder === null) return script !== null ? (trees.value[script] ?? null) : null;
  const f = trees.value[folder];
  if (!f || f.type !== 'folder') return null;
  return script === null ? f : (f.scripts[script] ?? null);
});

function addButton(node: api.ScriptNode): void {
  if (!node.button) node.button = { enabled: true, buttons: [] };
  node.button.buttons.push({ name: '新按钮', visible: true });
}

function removeButton(node: api.ScriptNode, i: number): void {
  node.button?.buttons.splice(i, 1);
}

// ===== 保存 =====
async function save(): Promise<void> {
  if (saving.value) return;
  saving.value = true;
  msg.value = null;
  try {
    await api.saveScriptTree(
      trees.value,
      scope.value,
      scope.value === 'character' ? characterId.value : undefined,
    );
    imported.value = false;
    msg.value = { kind: 'ok', text: '脚本已保存(角色脚本在下次消息生成后自动执行)' };
    setTimeout(() => (msg.value = null), 2500);
  } catch (err) {
    msg.value = { kind: 'err', text: `保存失败:${(err as Error).message}` };
  } finally {
    saving.value = false;
  }
}

// ===== 导入导出(阶段六 6e) =====

/** 强制所有脚本节点标记仅导出 data/button(全局勾选的导出语义) */
function forceExportWith(tree: api.ScriptTree): api.ScriptTree {
  const force = (n: api.ScriptTreeNode): api.ScriptTreeNode => {
    if (n.type === 'folder') return { ...n, scripts: n.scripts.map(force) };
    return { ...n, export_with: { data: true, button: true } };
  };
  return tree.map(force);
}

async function exportNow(): Promise<void> {
  if (exporting.value) return;
  if (scope.value === 'character' && !characterId.value) {
    msg.value = { kind: 'err', text: '请先选择角色,再导出其角色卡脚本' };
    return;
  }
  exporting.value = true;
  msg.value = null;
  try {
    const data = await api.getScriptTree(
      scope.value,
      scope.value === 'character' ? characterId.value : undefined,
    );
    // 仅 data/button:先强制标记 export_with,再按语义剥离 content/info(结构保留)
    const out = onlyDataButton.value ? stripByExportWith(forceExportWith(data)) : data;
    const fileName = `脚本树-${scope.value}${onlyDataButton.value ? '-仅data-button' : ''}.json`;
    const saved = await saveExportFile(fileName, JSON.stringify(out, null, 2));
    if (saved) {
      msg.value = {
        kind: 'ok',
        text: `已导出 ${out.length} 个节点${onlyDataButton.value ? '(仅 data/button)' : ''}`,
      };
      setTimeout(() => (msg.value = null), 3000);
    }
  } catch (err) {
    msg.value = { kind: 'err', text: `导出失败:${(err as Error).message}` };
  } finally {
    exporting.value = false;
  }
}

function onImportPicked(e: Event): void {
  const input = e.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = '';
  if (!file || importing.value) return;
  void importNow(file);
}

async function importNow(file: File): Promise<void> {
  importing.value = true;
  msg.value = null;
  try {
    const text = await file.text();
    const parsed: unknown = JSON.parse(text);
    trees.value = parseScriptTreeImport(parsed);
    imported.value = true;
    editing.value = null;
    msg.value = {
      kind: 'ok',
      text: `已导入 ${trees.value.length} 个节点(id 已重分配;点「保存脚本」写入)`,
    };
  } catch (err) {
    msg.value = { kind: 'err', text: `导入失败:${(err as Error).message}` };
  } finally {
    importing.value = false;
  }
}

onMounted(() => {
  void load();
});
</script>

<template>
  <div class="sv-modal-mask" @click.self="close">
    <div class="sv-modal sv-scripts-modal">
      <!-- 头部 -->
      <div class="sv-modal-head">
        <h2 class="flex items-center gap-2">
          <span class="sv-supreme pink" /> 脚本管理
        </h2>
        <!-- 导出 / 导入(阶段六 6e) -->
        <div class="flex items-center gap-2" style="margin-right: 8px">
          <label class="sv-note" style="white-space: nowrap; display: flex; align-items: center; gap: 4px" title="导出时按酒馆 export_with 语义仅保留 data/button,剥离 content/info">
            <input v-model="onlyDataButton" type="checkbox" />
            仅 data/button
          </label>
          <button class="sv-btn ghost sv-btn-sm" :disabled="exporting" @click="exportNow">
            {{ exporting ? '导出中…' : '导出' }}
          </button>
          <button class="sv-btn ghost sv-btn-sm" :disabled="importing" @click="importFile?.click()">
            {{ importing ? '导入中…' : '导入' }}
          </button>
        </div>
        <button class="sv-btn ghost sv-btn-square" @click="close">✕</button>
        <input
          ref="importFile"
          type="file"
          accept=".json,application/json"
          class="hidden"
          @change="onImportPicked"
        />
      </div>

      <div class="sv-modal-body">
        <!-- 说明 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme blue" /> 说明</div>
          <p class="sv-note" style="margin: 0; line-height: 1.8">
            用户脚本(ScriptTree,对齐酒馆助手格式)在<code>消息生成完成后</code>由后端执行,
            可读写<code>global / character / chat / script</code>等作用域变量并调用
            <code>TavernHelper</code> 兼容 API(如
            <code>setVariables</code>/<code>triggerSlash</code>)。<br />
            <b>全局脚本</b>存于本机数据库;<b>角色脚本</b>存于角色卡
            <code>extensions.tavern_helper</code>,随角色卡导出。脚本仅做变量/数据处理,
            运行于隔离沙箱(1 秒超时 + 内存上限),不接触 DOM 与网络。<br />
            <b>导出/导入(阶段六 6e)</b>:导出 = 读取当前作用域脚本树下载 JSON(勾选
            「仅 data/button」时按 export_with 剥离正文,仅保留结构);导入 = 解析本地
            JSON 并<b>自动重分配 id</b>,点「保存脚本」后全量写入。
          </p>
        </div>

        <!-- 作用域与角色选择 -->
        <div class="sv-field sv-stack" style="gap: 10px">
          <div class="sv-field-label"><span class="sv-supreme green" /> 作用域</div>
          <div class="sv-stack" style="flex-direction: row; gap: 8px">
            <button
              class="sv-btn"
              :class="scope === 'global' ? 'primary' : 'ghost'"
              @click="switchScope('global')"
            >全局</button>
            <button
              class="sv-btn"
              :class="scope === 'character' ? 'primary' : 'ghost'"
              @click="switchScope('character')"
            >角色卡</button>
          </div>
          <div v-if="scope === 'character'" class="sv-stack" style="flex-direction: row; gap: 8px; align-items: center">
            <select class="sv-wb-select" :value="characterId" @change="characterId = ($event.target as HTMLSelectElement).value; onCharacterChange()">
              <option value="">选择角色…</option>
              <option v-for="c in characters" :key="c.id" :value="c.id">{{ c.chara_name }}</option>
            </select>
            <span class="sv-note" style="margin: 0">脚本写入该角色卡的 <code>extensions.tavern_helper</code></span>
          </div>
        </div>

        <!-- 工具栏 -->
        <div class="sv-field">
          <div class="sv-field-label"><span class="sv-supreme red" /> 脚本树</div>
          <div class="sv-stack" style="flex-direction: row; gap: 8px; margin-bottom: 8px">
            <button class="sv-btn ghost sv-btn-sm" @click="newScript">＋ 新增脚本</button>
            <button class="sv-btn ghost sv-btn-sm" @click="newFolder">＋ 新增文件夹</button>
          </div>
          <p v-if="!loaded" class="sv-note">加载中…</p>
          <div v-else-if="msg?.kind === 'err' && trees.length === 0" class="sv-empty" style="padding: 16px 12px">
            <p style="font-size: 12px; color: var(--sv-red)">{{ msg.text }}</p>
          </div>
          <div v-else-if="scope === 'character' && !characterId" class="sv-empty" style="padding: 16px 12px">
            <p style="font-size: 12px">请选择角色以查看/编辑其角色卡脚本。</p>
          </div>
          <div v-else-if="trees.length === 0" class="sv-empty" style="padding: 16px 12px">
            <p style="font-size: 12px">暂无脚本,点击「新增脚本」创建。</p>
          </div>
          <div v-else class="sv-scripts-layout">
            <!-- 树形列表 -->
            <div class="sv-scripts-tree">
              <template v-for="(node, i) in trees" :key="node.id">
                <div
                  v-if="node.type === 'folder'"
                  class="sv-script-row"
                  :class="{ active: editing?.folder === i && editing?.script === null }"
                  @click="editing = { folder: i, script: null }"
                >
                  <input v-model="node.enabled" type="checkbox" title="启用" @click.stop />
                  <span class="sv-script-name">📁 {{ node.name || '(未命名)' }}</span>
                  <span class="sv-script-meta">{{ node.scripts.length }} 个脚本</span>
                  <button class="sv-btn ghost sv-btn-sm" @click.stop="deleteNode(i, null)">删</button>
                </div>
                <div v-if="node.type === 'folder'" class="sv-script-subs">
                  <div
                    v-for="(sub, j) in node.scripts"
                    :key="sub.id"
                    class="sv-script-row sub"
                    :class="{ active: editing?.folder === i && editing?.script === j }"
                    @click="editing = { folder: i, script: j }"
                  >
                    <input v-model="sub.enabled" type="checkbox" title="启用" @click.stop />
                    <span class="sv-script-name">· {{ sub.name || '(未命名)' }}</span>
                    <!-- 子级按数据约束恒为 script(parseScriptFolder 只产出 script 节点),此处仅类型收窄 -->
                    <span class="sv-script-meta">{{ (sub as api.ScriptNode).content.length }} 字</span>
                    <button class="sv-btn ghost sv-btn-sm" @click.stop="deleteNode(i, j)">删</button>
                  </div>
                  <button class="sv-btn ghost sv-btn-sm sv-script-addsub" @click="newSubScript(i)">＋ 新增子脚本</button>
                </div>
                <div
                  v-else
                  class="sv-script-row"
                  :class="{ active: editing?.folder === null && editing?.script === i }"
                  @click="editing = { folder: null, script: i }"
                >
                  <input v-model="node.enabled" type="checkbox" title="启用" @click.stop />
                  <span class="sv-script-name">{{ node.name || '(未命名)' }}</span>
                  <span class="sv-script-meta">{{ node.content.length }} 字</span>
                  <button class="sv-btn ghost sv-btn-sm" @click.stop="deleteNode(null, i)">删</button>
                </div>
              </template>
            </div>

            <!-- 编辑面板 -->
            <div class="sv-scripts-editor">
              <template v-if="editingNode">
                <div class="sv-field">
                  <div class="sv-field-label">{{ editingNode.type === 'folder' ? '文件夹' : '脚本' }}</div>
                  <label class="sv-note sv-script-enabled">
                    <input v-model="editingNode.enabled" type="checkbox" />
                    启用{{ editingNode.type === 'folder' ? '(文件夹启用不影响其中脚本的独立启用状态)' : '' }}
                  </label>
                  <input v-model="editingNode.name" type="text" class="sv-input" placeholder="名称" spellcheck="false" />
                </div>
                <template v-if="editingNode.type === 'script'">
                  <div class="sv-field">
                    <div class="sv-field-label">脚本内容(JavaScript,消息生成后执行)</div>
                    <textarea
                      v-model="editingNode.content"
                      class="sv-input sv-script-code"
                      rows="10"
                      placeholder="// 例:TavernHelper.setVariables({ 回合数: (TavernHelper.getVariables({type:'global'}).回合数||0)+1 }, { type: 'global' })"
                      spellcheck="false"
                    ></textarea>
                  </div>
                  <div class="sv-field">
                    <div class="sv-field-label">按钮组</div>
                    <label class="sv-note sv-script-enabled">
                      <input v-model="editingNode.button!.enabled" type="checkbox" />
                      启用按钮
                    </label>
                    <div v-for="(b, bi) in editingNode.button!.buttons" :key="bi" class="sv-script-btnrow">
                      <input v-model="b.name" type="text" class="sv-input" placeholder="按钮名" spellcheck="false" />
                      <label class="sv-note" style="white-space: nowrap">
                        <input v-model="b.visible" type="checkbox" /> 可见
                      </label>
                      <button class="sv-btn ghost sv-btn-sm" @click="removeButton(editingNode, bi)">删</button>
                    </div>
                    <button class="sv-btn ghost sv-btn-sm" @click="addButton(editingNode)">＋ 按钮</button>
                  </div>
                </template>
                <template v-else>
                  <div class="sv-field">
                    <div class="sv-field-label">图标(可留空)</div>
                    <input v-model="editingNode.icon" type="text" class="sv-input" placeholder="fa-solid fa-folder" spellcheck="false" />
                  </div>
                </template>
              </template>
              <div v-else class="sv-empty" style="padding: 16px 12px">
                <p style="font-size: 12px">选中左侧节点进行编辑。</p>
              </div>
            </div>
          </div>
        </div>

        <!-- 反馈 -->
        <div v-if="msg" class="sv-feedback" :class="msg.kind" style="margin-top: 8px">{{ msg.text }}</div>
      </div>

      <!-- 底部 -->
      <div class="sv-modal-foot">
        <button class="sv-btn primary" :disabled="saving" @click="save">
          {{ saving ? '保存中…' : imported ? '保存导入的脚本' : '保存脚本' }}
        </button>
        <button class="sv-btn ghost" @click="close">完成</button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.sv-scripts-modal {
  width: min(880px, 94vw);
}
.sv-scripts-layout {
  display: grid;
  grid-template-columns: minmax(220px, 1fr) minmax(320px, 1.4fr);
  gap: 12px;
  min-height: 320px;
}
.sv-scripts-tree {
  border: 2px solid var(--sv-line-strong);
  background: var(--sv-surface-elevated);
  padding: 8px;
  overflow-y: auto;
  max-height: 420px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.sv-scripts-editor {
  border: 2px solid var(--sv-line-strong);
  background: var(--sv-surface-elevated);
  padding: 10px;
  overflow-y: auto;
  max-height: 420px;
}
.sv-script-row {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 6px 8px;
  border: 2px solid transparent;
  background: transparent;
  cursor: pointer;
  font-size: 12px;
}
.sv-script-row:hover {
  background: var(--sv-pink-light);
}
.sv-script-row.active {
  background: var(--sv-ink);
  border-color: var(--sv-ink);
  color: var(--sv-white);
}
.sv-script-row.sub {
  margin-left: 18px;
}
.sv-script-name {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.sv-script-meta {
  color: var(--sv-ink-faint);
  font-size: 11px;
  white-space: nowrap;
}
.sv-script-row.active .sv-script-meta {
  color: rgba(255, 255, 255, 0.7);
}
.sv-script-subs {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.sv-script-addsub {
  margin: 2px 0 6px 18px;
  width: fit-content;
}
.sv-script-enabled {
  display: flex;
  align-items: center;
  gap: 6px;
  margin: 0 0 8px;
}
.sv-script-code {
  font-family: var(--font-mono);
  font-size: 12px;
  line-height: 1.6;
  min-height: 180px;
  resize: vertical;
}
.sv-script-btnrow {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 6px;
}
</style>
