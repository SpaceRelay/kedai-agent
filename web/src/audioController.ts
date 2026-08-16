// 音频控制器注册表(阶段五 5a)
// AudioPlayer.vue 挂载时注册实现,沙箱音频桥(characterScriptSandbox 的 TavernHelper
// 8 个音频 API RPC)经此调用。这样沙箱脚本触发播放时即使面板处于折叠态也能生效
// (与酒馆 audioenable/audioplaypause 等 slash 命令在无 UI 时可用的语义一致);
// 面板未挂载过/已卸载时控制器为空,沙箱调用静默 no-op(getter 回退默认值)。
import type {
  AudioChannelSettings,
  AudioChannelType,
  AudioTrack,
  CurrentAudio,
} from './api/types';

export interface AudioControllerImpl {
  /** 播放指定音频(不在播放列表则加入);无 title 时由 url 提取文件名 */
  playAudio(type: AudioChannelType, audio?: { title?: string; url: string }): void;
  /** 暂停指定通道 */
  pauseAudio(type: AudioChannelType): void;
  /** 当前播放列表(深拷贝) */
  getAudioList(type: AudioChannelType): AudioTrack[];
  /** 替换播放列表 */
  replaceAudioList(type: AudioChannelType, audioList: Array<{ title?: string; url: string }>): void;
  /** 追加播放列表(同 title 或 url 不重复添加) */
  appendAudioList(type: AudioChannelType, audioList: Array<{ title?: string; url: string }>): void;
  /** 当前通道设置 */
  getAudioSettings(type: AudioChannelType): AudioChannelSettings;
  /** 更新通道设置(部分字段合并) */
  setAudioSettings(type: AudioChannelType, settings: Partial<AudioChannelSettings>): void;
  /** 当前选中/播放曲目信息(含进度) */
  getCurrentAudio(type: AudioChannelType): CurrentAudio;
}

let impl: AudioControllerImpl | null = null;

/** 面板挂载/卸载时注册或注销实现 */
export function registerAudioController(controller: AudioControllerImpl | null): void {
  impl = controller;
}

/** 沙箱音频桥经此取当前控制器;无控制器返回 null(调用方 no-op) */
export function getAudioController(): AudioControllerImpl | null {
  return impl;
}
