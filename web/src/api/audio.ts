// 音频播放器 API(bgm/ambient 双通道,阶段五 5a)
import { request } from './client';
import type {
  AudioChannelSettings,
  AudioChannelType,
  AudioState,
  AudioTrack,
} from './types';

/** GET /api/audio:读取双通道全量状态 */
export async function getAudio(): Promise<AudioState> {
  const data = await request<{ audio: AudioState }>('/audio');
  return data.audio;
}

/** PUT /api/audio/settings:更新单通道设置(部分字段合并) */
export async function updateAudioSettings(
  type: AudioChannelType,
  settings: Partial<AudioChannelSettings>,
): Promise<AudioState> {
  const data = await request<{ ok: boolean; audio: AudioState }>('/audio/settings', {
    method: 'PUT',
    body: JSON.stringify({ type, settings }),
  });
  return data.audio;
}

/** PUT /api/audio/playlist:替换单通道播放列表(URL 协议白名单由服务端校验) */
export async function updateAudioPlaylist(
  type: AudioChannelType,
  tracks: AudioTrack[],
): Promise<AudioState> {
  const data = await request<{ ok: boolean; audio: AudioState }>('/audio/playlist', {
    method: 'PUT',
    body: JSON.stringify({ type, tracks }),
  });
  return data.audio;
}
