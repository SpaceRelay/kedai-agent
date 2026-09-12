<script setup lang="ts">
// 开场选择器模态框(自 ChatWindow 拆出):多开场角色在两种模式下选择开场——
// mode='new' 新建会话时选择开场;mode='switch' 当前会话内重置开场。
// 选择动作直落 store,失败 alert 提示;开关状态由父组件持有(v-if 控制挂载)。
import { storeToRefs } from 'pinia';
import { useAppStore } from '../store';

const props = defineProps<{
  /** new=新建会话时选择开场;switch=当前会话内重置开场 */
  mode: 'new' | 'switch';
}>();
const emit = defineEmits<{ (e: 'close'): void }>();

const store = useAppStore();
const { currentGreetings } = storeToRefs(store);

/** 选定开场:mode=new → 用该开场新建会话;mode=switch → 当前会话清空并重置为该开场 */
async function pickGreeting(index: number): Promise<void> {
  emit('close');
  try {
    if (props.mode === 'new') {
      await store.newSession(index);
    } else {
      await store.switchGreeting(index);
    }
  } catch (err) {
    alert(`开场切换失败:${(err as Error).message}`);
  }
}
</script>

<template>
  <div class="sv-modal-mask" @click.self="emit('close')">
    <div class="sv-modal sm">
      <div class="sv-modal-head">
        <h2 class="flex items-center gap-2">
          <span class="sv-supreme pink-deep" />
          {{ mode === 'new' ? '选择开场 · 新建会话' : '选择开场 · 重置当前会话' }}
        </h2>
        <button class="sv-btn ghost sv-btn-square" @click="emit('close')">✕</button>
      </div>
      <div class="sv-modal-body">
        <p class="sv-note sv-mb12">
          {{ mode === 'new'
            ? '选择一个开场,以其作为新会话的第一条消息。'
            : '切换开场将清空当前会话全部消息,并以所选开场重新开始。' }}
        </p>
        <div class="sv-datalist">
          <button
            v-for="(g, i) in currentGreetings"
            :key="i"
            class="sv-data-row sv-greet-row"
            @click="pickGreeting(i)"
          >
            <div class="info">
              <b>
                {{ i === 0 ? '主开场' : `备用 ${i}` }}
                <span v-if="i === 0" class="sv-tag sv-tag-on">默认</span>
              </b>
              <span class="preview">{{ g }}</span>
            </div>
          </button>
        </div>
      </div>
      <div class="sv-modal-foot">
        <button class="sv-btn ghost" @click="emit('close')">取消</button>
      </div>
    </div>
  </div>
</template>
