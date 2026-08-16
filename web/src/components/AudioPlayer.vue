<script setup lang="ts">
// 音频播放器面板(阶段五 5a):bgm/ambient 双通道,原生 <audio> 单实例
// (两通道复用同一元素,切通道时换 src)。播放列表/设置经 store 持久化到
// data/audio.json;挂载时注册 audioController 供沙箱 TavernHelper 音频 API 调用。
import { computed, onBeforeUnmount, onMounted, reactive, ref } from 'vue';
import { useAppStore } from '../store';
import type {
  AudioChannelSettings,
  AudioChannelType,
  AudioTrack,
  CurrentAudio,
} from '../api/types';
import { registerAudioController } from '../audioController';

const store = useAppStore();

/** 默认通道设置(与后端 AudioState::default 对齐) */
function defaultSettings(mode: 'repeat_all' | 'play_one_and_stop'): AudioChannelSettings {
  return { enabled: true, mode, muted: false, volume: 50 };
}

/** 通道设置(store 未加载/失败时回退默认) */
function channelSettings(type: AudioChannelType): AudioChannelSettings {
  const state = store.audio;
  if (!state) return type === 'bgm' ? defaultSettings('repeat_all') : defaultSettings('play_one_and_stop');
  const c = state[type];
  return { enabled: c.enabled, mode: c.mode, muted: c.muted, volume: c.volume };
}

/** 通道播放列表(store 未加载时为空) */
function channelPlaylist(type: AudioChannelType): AudioTrack[] {
  return store.audio?.[type]?.playlist ?? [];
}

/** 面板折叠(折叠后仅保留「♪」悬浮按钮) */
const collapsed = ref(false);
/** 当前激活通道 */
const activeChannel = ref<AudioChannelType>('bgm');
/** 各通道当前选中曲目索引(切换通道时各自记忆) */
const indexByChannel = reactive<Record<AudioChannelType, number>>({ bgm: 0, ambient: 0 });
/** 是否正在播放 */
const playing = ref(false);
/** 播放进度(0-100) */
const progress = ref(0);
/** 播放时长(秒,用于进度条显示) */
const duration = ref(0);
/** 播放列表编辑器草稿(当前激活通道;null = 未打开编辑器) */
const editorTracks = ref<Array<{ title: string; url: string }> | null>(null);

const audioEl = ref<HTMLAudioElement | null>(null);

const modeLabels: Record<AudioChannelSettings['mode'], string> = {
  repeat_one: '单曲循环',
  repeat_all: '全部循环',
  shuffle: '随机',
  play_one_and_stop: '播完即止',
};
const modeIcons: Record<AudioChannelSettings['mode'], string> = {
  repeat_one: '⟳',
  repeat_all: '🔁',
  shuffle: '🎲',
  play_one_and_stop: '⏹',
};

/** 当前激活通道的曲目索引(越界钳制) */
function currentIndex(type: AudioChannelType): number {
  const len = channelPlaylist(type).length;
  const idx = indexByChannel[type] ?? 0;
  if (len === 0) return -1;
  return Math.min(idx, len - 1);
}

/** 当前激活通道选中曲目 */
const currentTrack = computed<AudioTrack | null>(() => {
  const idx = currentIndex(activeChannel.value);
  if (idx < 0) return null;
  return channelPlaylist(activeChannel.value)[idx] ?? null;
});

/** 从 URL 提取文件名作默认标题(对齐酒馆:无 title 时用 url 文件名) */
function titleFromUrl(url: string): string {
  try {
    const name = decodeURIComponent(new URL(url).pathname.split('/').pop() ?? '');
    return name || url;
  } catch {
    return url;
  }
}

function applyAudioElement(): void {
  const el = audioEl.value;
  if (!el) return;
  const settings = channelSettings(activeChannel.value);
  el.muted = settings.muted;
  el.volume = settings.volume / 100;
}

/** 播放指定索引曲目(缺省为当前选中);空列表时停止 */
function playAt(type: AudioChannelType, idx?: number): void {
  const playlist = channelPlaylist(type);
  if (playlist.length === 0) {
    stopPlayback();
    return;
  }
  const target = idx ?? currentIndex(type);
  const track = playlist[Math.min(Math.max(target, 0), playlist.length - 1)];
  indexByChannel[type] = playlist.indexOf(track);
  const el = audioEl.value;
  if (!el) return;
  el.src = track.url;
  el.muted = channelSettings(type).muted;
  el.volume = channelSettings(type).volume / 100;
  void el.play().then(() => { playing.value = true; }).catch(() => { playing.value = false; });
}

function togglePlay(): void {
  if (playing.value) {
    audioEl.value?.pause();
    playing.value = false;
  } else {
    playAt(activeChannel.value);
  }
}

function stopPlayback(): void {
  const el = audioEl.value;
  if (el) {
    el.pause();
    el.removeAttribute('src');
  }
  playing.value = false;
  progress.value = 0;
}

function nextTrack(): void {
  const playlist = channelPlaylist(activeChannel.value);
  if (playlist.length === 0) return;
  const idx = (currentIndex(activeChannel.value) + 1) % playlist.length;
  playAt(activeChannel.value, idx);
}

function prevTrack(): void {
  const playlist = channelPlaylist(activeChannel.value);
  if (playlist.length === 0) return;
  const idx = (currentIndex(activeChannel.value) - 1 + playlist.length) % playlist.length;
  playAt(activeChannel.value, idx);
}

/** onended 按模式切歌(对齐酒馆 dist/index.js:769 的 mode switch) */
function onEnded(): void {
  progress.value = 0;
  const type = activeChannel.value;
  const mode = channelSettings(type).mode;
  const playlist = channelPlaylist(type);
  switch (mode) {
    case 'repeat_one':
      if (audioEl.value) { audioEl.value.currentTime = 0; void audioEl.value.play().catch(() => { playing.value = false; }); }
      playing.value = true;
      break;
    case 'repeat_all': {
      if (playlist.length === 0) { playing.value = false; return; }
      const idx = (currentIndex(type) + 1) % playlist.length;
      playAt(type, idx);
      break;
    }
    case 'shuffle': {
      if (playlist.length === 0) { playing.value = false; return; }
      const idx = Math.floor(Math.random() * playlist.length);
      playAt(type, idx);
      break;
    }
    case 'play_one_and_stop':
      playing.value = false;
      break;
  }
}

function onTimeUpdate(): void {
  const el = audioEl.value;
  if (!el || !el.duration) return;
  progress.value = Math.min(100, (el.currentTime / el.duration) * 100);
  duration.value = el.duration;
}

function seek(e: Event): void {
  const el = audioEl.value;
  if (!el || !el.duration) return;
  const v = Number((e.target as HTMLInputElement).value);
  el.currentTime = (v / 100) * el.duration;
  progress.value = v;
}

/** 循环切换模式(对齐酒馆 mode 按钮:点击在 audio_mode_enum 循环) */
const modeOrder: AudioChannelSettings['mode'][] = ['repeat_one', 'repeat_all', 'shuffle', 'play_one_and_stop'];
function cycleMode(): void {
  const type = activeChannel.value;
  const current = channelSettings(type).mode;
  const next = modeOrder[(modeOrder.indexOf(current) + 1) % modeOrder.length];
  void saveSettings(type, { mode: next });
}

function toggleEnabled(): void {
  const type = activeChannel.value;
  void saveSettings(type, { enabled: !channelSettings(type).enabled });
}

function toggleMuted(): void {
  const type = activeChannel.value;
  void saveSettings(type, { muted: !channelSettings(type).muted });
}

function onVolumeInput(e: Event): void {
  // 拖动时仅本地生效(即时反馈),@change 才落盘
  const el = audioEl.value;
  if (el) el.volume = Number((e.target as HTMLInputElement).value) / 100;
}

function onVolumeChange(e: Event): void {
  const type = activeChannel.value;
  const volume = Number((e.target as HTMLInputElement).value);
  void saveSettings(type, { volume });
}

/** 保存设置(部分字段合并);失败静默(store 已 console.error) */
async function saveSettings(type: AudioChannelType, patch: Partial<AudioChannelSettings>): Promise<void> {
  try {
    await store.saveAudioSettings(type, patch);
  } catch { /* 已由 store 记录 */ }
}

function onSelectTrack(e: Event): void {
  const value = (e.target as HTMLSelectElement).value;
  const playlist = channelPlaylist(activeChannel.value);
  const idx = playlist.findIndex((t) => t.url === value);
  if (idx >= 0) playAt(activeChannel.value, idx);
}

// ===== 播放列表编辑器 =====

function openEditor(): void {
  editorTracks.value = channelPlaylist(activeChannel.value).map((t) => ({ ...t }));
}

function closeEditor(): void {
  editorTracks.value = null;
}

function addEditorRow(): void {
  editorTracks.value ??= [];
  editorTracks.value.push({ title: '', url: '' });
}

function removeEditorRow(i: number): void {
  const list = editorTracks.value;
  if (list) list.splice(i, 1);
}

/** 保存播放列表:过滤空行,URL 协议由服务端校验(非法会 400 并提示) */
async function savePlaylist(): Promise<void> {
  const type = activeChannel.value;
  const tracks = (editorTracks.value ?? [])
    .map((t) => ({ title: t.title.trim(), url: t.url.trim() }))
    .filter((t) => t.url.length > 0);
  try {
    await store.saveAudioPlaylist(type, tracks);
    indexByChannel[type] = 0;
    editorTracks.value = null;
  } catch { /* 已由 store 记录 */ }
}

// ===== 切换通道(单实例换 src) =====

function switchChannel(type: AudioChannelType): void {
  if (type === activeChannel.value) return;
  stopPlayback();
  activeChannel.value = type;
  applyAudioElement();
}

// ===== 沙箱音频桥实现(注册到 audioController) =====

onMounted(() => {
  void store.loadAudio();
  applyAudioElement();
  registerAudioController({
    playAudio(type, audio) {
      if (!audio) {
        // 仅指定通道,无具体曲目:播放该通道当前选中
        if (type === activeChannel.value) togglePlay();
        else { activeChannel.value = type; playAt(type); }
        return;
      }
      const playlist = channelPlaylist(type);
      const idx = playlist.findIndex((t) => t.url === audio.url);
      if (idx >= 0) {
        playAt(type, idx);
      } else {
        // 不在播放列表则加入(酒馆语义),再播放
        const track: AudioTrack = { title: audio.title?.trim() || titleFromUrl(audio.url), url: audio.url };
        void store.saveAudioPlaylist(type, [...playlist, track]).then(() => {
          const after = channelPlaylist(type);
          const i = after.findIndex((t) => t.url === track.url);
          if (i >= 0) playAt(type, i);
        }).catch(() => { /* 非法 URL 等服务端 400:store 已记录,不打断脚本 */ });
      }
    },
    pauseAudio(type) {
      if (type !== activeChannel.value) return;
      audioEl.value?.pause();
      playing.value = false;
    },
    getAudioList(type) {
      return channelPlaylist(type).map((t) => ({ ...t }));
    },
    replaceAudioList(type, audioList) {
      const tracks: AudioTrack[] = audioList.map((a) => ({ title: a.title?.trim() || titleFromUrl(a.url), url: a.url }));
      void store.saveAudioPlaylist(type, tracks);
      if (type === activeChannel.value) indexByChannel[type] = 0;
    },
    appendAudioList(type, audioList) {
      const existing = channelPlaylist(type);
      const tracks = [...existing];
      for (const a of audioList) {
        const url = a.url.trim();
        if (!url) continue;
        if (tracks.some((t) => t.url === url || (a.title?.trim() && t.title === a.title.trim()))) continue;
        tracks.push({ title: a.title?.trim() || titleFromUrl(url), url });
      }
      if (tracks.length !== existing.length) void store.saveAudioPlaylist(type, tracks);
    },
    getAudioSettings(type) {
      return { ...channelSettings(type) };
    },
    setAudioSettings(type, settings) {
      void saveSettings(type, settings);
    },
    getCurrentAudio(type): CurrentAudio {
      const track = currentTrack.value;
      if (type !== activeChannel.value || !track) {
        return { src: '', title: '', playing: false, progress: 0 };
      }
      return { src: track.url, title: track.title, playing: playing.value, progress: progress.value };
    },
  });
});

onBeforeUnmount(() => {
  registerAudioController(null);
  audioEl.value?.pause();
});
</script>

<template>
  <div class="sv-audio-panel" :class="{ collapsed }">
    <!-- 标题栏 -->
    <div class="sv-audio-head">
      <span class="sv-audio-title">♪ 播放器</span>
      <button class="sv-audio-mini" :title="collapsed ? '展开' : '折叠'" @click="collapsed = !collapsed">{{ collapsed ? '▢' : '—' }}</button>
    </div>

    <template v-if="!collapsed">
      <!-- 通道切换 -->
      <div class="sv-audio-tabs">
        <button
          class="sv-audio-tab"
          :class="{ active: activeChannel === 'bgm' }"
          @click="switchChannel('bgm')"
        >音乐 BGM</button>
        <button
          class="sv-audio-tab"
          :class="{ active: activeChannel === 'ambient' }"
          @click="switchChannel('ambient')"
        >音效</button>
      </div>

      <!-- 通道控制 -->
      <div class="sv-audio-channel">
        <div class="sv-audio-row">
          <label class="sv-audio-toggle-label">
            <input type="checkbox" :checked="channelSettings(activeChannel).enabled" @change="toggleEnabled" />
            启用
          </label>
          <button class="sv-audio-mini" :title="modeLabels[channelSettings(activeChannel).mode]" @click="cycleMode">
            {{ modeIcons[channelSettings(activeChannel).mode] }}
          </button>
        </div>

        <div class="sv-audio-row">
          <button class="sv-audio-btn" title="上一首" @click="prevTrack">⏮</button>
          <button class="sv-audio-btn sv-audio-play" :title="playing ? '暂停' : '播放'" @click="togglePlay">
            {{ playing ? '❚❚' : '▶' }}
          </button>
          <button class="sv-audio-btn" title="下一首" @click="nextTrack">⏭</button>
          <select
            class="sv-audio-select"
            :value="currentTrack?.url ?? ''"
            :disabled="channelPlaylist(activeChannel).length === 0"
            @change="onSelectTrack"
          >
            <option value="" disabled>{{ channelPlaylist(activeChannel).length ? '选择曲目' : '播放列表为空' }}</option>
            <option v-for="t in channelPlaylist(activeChannel)" :key="t.url" :value="t.url">{{ t.title || t.url }}</option>
          </select>
        </div>

        <div class="sv-audio-row">
          <input
            class="sv-audio-range"
            type="range" min="0" max="100" step="1"
            :value="progress"
            :disabled="!currentTrack"
            @input="seek"
          />
          <span class="sv-audio-time">{{ Math.round(progress) }}%</span>
        </div>

        <div class="sv-audio-row">
          <button class="sv-audio-btn" :class="{ active: channelSettings(activeChannel).muted }" title="静音" @click="toggleMuted">
            {{ channelSettings(activeChannel).muted ? '🔇' : '🔊' }}
          </button>
          <input
            class="sv-audio-range"
            type="range" min="0" max="100" step="1"
            :value="channelSettings(activeChannel).volume"
            @input="onVolumeInput"
            @change="onVolumeChange"
          />
          <span class="sv-audio-time">{{ channelSettings(activeChannel).volume }}</span>
        </div>

        <div class="sv-audio-row sv-audio-actions">
          <button class="sv-audio-btn" @click="openEditor">编辑列表</button>
          <span class="sv-audio-count">{{ channelPlaylist(activeChannel).length }} 首</span>
        </div>

        <!-- 播放列表编辑器 -->
        <div v-if="editorTracks" class="sv-audio-editor">
          <div v-for="(t, i) in editorTracks" :key="i" class="sv-audio-editor-row">
            <input v-model="t.title" class="sv-audio-input" placeholder="标题" />
            <input v-model="t.url" class="sv-audio-input" placeholder="https://…" />
            <button class="sv-audio-btn" title="删除" @click="removeEditorRow(i)">✕</button>
          </div>
          <div class="sv-audio-row sv-audio-actions">
            <button class="sv-audio-btn" @click="addEditorRow">＋ 新增</button>
            <button class="sv-audio-btn sv-audio-save" @click="savePlaylist">保存</button>
            <button class="sv-audio-btn" @click="closeEditor">取消</button>
          </div>
        </div>
      </div>
    </template>

    <!-- 原生音频单实例(隐藏) -->
    <audio ref="audioEl" class="sv-audio-native" @ended="onEnded" @timeupdate="onTimeUpdate" />
  </div>
</template>
