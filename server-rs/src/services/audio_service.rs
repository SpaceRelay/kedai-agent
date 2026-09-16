// 音频服务:bgm/ambient 双通道播放器状态,持久化到 data/audio.json
// 契约对齐酒馆助手 @types/function/audio.d.ts(setAudioSettings 为部分字段合并,
// volume clamp 0-100,replace/append 播放列表 URL 协议白名单)。
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 播放模式(与酒馆助手 audio.d.ts 的 AudioSettings.mode 对齐)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AudioMode {
    /// 单曲循环
    #[default]
    RepeatOne,
    /// 全部循环
    RepeatAll,
    /// 随机播放
    Shuffle,
    /// 播放一首后停止
    PlayOneAndStop,
}

/// 音频曲目(仅 URL 播放,本地文件上传留扩展位)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AudioTrack {
    pub title: String,
    pub url: String,
}

/// 单通道状态
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelState {
    pub enabled: bool,
    pub mode: AudioMode,
    pub muted: bool,
    /// 音量 0-100
    pub volume: u8,
    pub playlist: Vec<AudioTrack>,
}

impl Default for ChannelState {
    fn default() -> Self {
        // 默认值对齐酒馆 dist/index.js:73:bgm repeat_all / ambient play_one_and_stop,
        // 两者 volume=50、muted=false、enabled=true
        ChannelState {
            enabled: true,
            mode: AudioMode::RepeatAll,
            muted: false,
            volume: 50,
            playlist: Vec::new(),
        }
    }
}

/// 双通道状态
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioState {
    pub bgm: ChannelState,
    pub ambient: ChannelState,
}

impl Default for AudioState {
    fn default() -> Self {
        AudioState {
            bgm: ChannelState::default(),
            ambient: ChannelState {
                mode: AudioMode::PlayOneAndStop,
                ..ChannelState::default()
            },
        }
    }
}

/// 通道标识(API body 的 type 字段解析用)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioChannel {
    Bgm,
    Ambient,
}

impl AudioChannel {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "bgm" => Some(AudioChannel::Bgm),
            "ambient" => Some(AudioChannel::Ambient),
            _ => None,
        }
    }
}

/// 设置补丁:部分字段合并,缺字段沿用原设置(对齐酒馆 setAudioSettings 的 Partial 语义)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelSettingsPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<AudioMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<u8>,
}

/// 音频服务:持有 data_dir,内存缓存状态,读写 data/audio.json
pub struct AudioService {
    data_dir: PathBuf,
    state: AudioState,
}

impl AudioService {
    pub fn new(data_dir: PathBuf) -> Self {
        let state = AudioState::load(&data_dir);
        AudioService { data_dir, state }
    }

    pub fn get(&self) -> AudioState {
        self.state.clone()
    }

    /// 更新单通道设置(部分字段合并,volume clamp 0-100);校验失败返回 Err(不落盘)
    pub fn update_settings(
        &mut self,
        channel: AudioChannel,
        patch: ChannelSettingsPatch,
    ) -> Result<(), String> {
        let target = self.state.channel_mut(channel);
        if let Some(v) = patch.enabled {
            target.enabled = v;
        }
        if let Some(v) = patch.mode {
            target.mode = v;
        }
        if let Some(v) = patch.muted {
            target.muted = v;
        }
        if let Some(v) = patch.volume {
            target.volume = v.min(100);
        }
        self.save()
    }

    /// 替换单通道播放列表;URL 协议白名单校验失败返回 Err(不落盘)
    pub fn update_playlist(
        &mut self,
        channel: AudioChannel,
        tracks: Vec<AudioTrack>,
    ) -> Result<(), String> {
        for track in &tracks {
            validate_audio_url(&track.url)?;
        }
        self.state.channel_mut(channel).playlist = tracks;
        self.save()
    }

    /// 持久化当前状态
    pub fn save(&self) -> Result<(), String> {
        self.state.save(&self.data_dir)
    }
}

impl AudioState {
    /// 从 data/audio.json 加载;缺失回退默认,损坏记录日志后回退默认(不静默归零)
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join("audio.json");
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<AudioState>(&text) {
                Ok(state) => state,
                Err(e) => {
                    tracing::error!(error = e.to_string(), "音频配置解析失败,已回退默认配置");
                    AudioState::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => AudioState::default(),
            Err(e) => {
                tracing::error!(error = e.to_string(), "音频配置读取失败,已回退默认配置");
                AudioState::default()
            }
        }
    }

    /// 持久化到 data/audio.json(原子写:崩溃不留半截 JSON)
    pub fn save(&self, data_dir: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        crate::utils::fs_atomic::write_atomic(&data_dir.join("audio.json"), text.as_bytes())
            .map_err(|e| e.to_string())
    }

    fn channel_mut(&mut self, channel: AudioChannel) -> &mut ChannelState {
        match channel {
            AudioChannel::Bgm => &mut self.bgm,
            AudioChannel::Ambient => &mut self.ambient,
        }
    }
}

/// URL 协议白名单:仅 https/http(拒 file/javascript/data 等);空串拒绝
fn validate_audio_url(url: &str) -> Result<(), String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("音频 URL 不能为空".to_string());
    }
    let scheme = trimmed
        .split("://")
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(scheme.as_str(), "http" | "https") {
        Ok(())
    } else {
        Err(format!("不支持的音频 URL 协议: {}", scheme))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_support::TempDataDir;
    use std::fs;

    /// 隔离临时数据目录(uuid 唯一:此前按 pid 命名会被同进程内并发测试共用)
    fn temp_dir(tag: &str) -> TempDataDir {
        TempDataDir::new(&format!("audio-{tag}"))
    }

    #[test]
    fn default_state_matches_tavern_defaults() {
        let state = AudioState::default();
        assert!(state.bgm.enabled);
        assert_eq!(state.bgm.mode, AudioMode::RepeatAll);
        assert_eq!(state.ambient.mode, AudioMode::PlayOneAndStop);
        assert_eq!(state.bgm.volume, 50);
        assert!(!state.bgm.muted);
        assert!(state.bgm.playlist.is_empty());
    }

    #[test]
    fn save_then_load_roundtrip() {
        let dir = temp_dir("roundtrip");
        let mut service = AudioService::new(dir.path().to_path_buf());
        service
            .update_playlist(
                AudioChannel::Bgm,
                vec![AudioTrack {
                    title: "开场曲".to_string(),
                    url: "https://example.com/a.mp3".to_string(),
                }],
            )
            .unwrap();
        service
            .update_settings(
                AudioChannel::Ambient,
                ChannelSettingsPatch {
                    volume: Some(80),
                    ..Default::default()
                },
            )
            .unwrap();

        let reloaded = AudioService::new(dir.path().to_path_buf());
        let state = reloaded.get();
        assert_eq!(state.bgm.playlist.len(), 1);
        assert_eq!(state.bgm.playlist[0].title, "开场曲");
        assert_eq!(state.ambient.volume, 80);
    }

    #[test]
    fn corrupted_file_falls_back_to_default() {
        let dir = temp_dir("corrupt");
        fs::write(dir.join("audio.json"), "{ 不是合法 JSON").unwrap();
        let service = AudioService::new(dir.path().to_path_buf());
        assert_eq!(service.get().bgm.mode, AudioMode::RepeatAll);
    }

    #[test]
    fn volume_clamped_to_100() {
        let dir = temp_dir("volume");
        let mut service = AudioService::new(dir.path().to_path_buf());
        service
            .update_settings(
                AudioChannel::Bgm,
                ChannelSettingsPatch {
                    volume: Some(255),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(service.get().bgm.volume, 100);
    }

    #[test]
    fn playlist_rejects_disallowed_schemes() {
        let dir = temp_dir("scheme");
        let mut service = AudioService::new(dir.path().to_path_buf());
        let err = service
            .update_playlist(
                AudioChannel::Bgm,
                vec![AudioTrack {
                    title: "本地".to_string(),
                    url: "file:///C:/x.mp3".to_string(),
                }],
            )
            .unwrap_err();
        assert!(err.contains("file"), "实际错误: {err}");
        // 校验失败不落盘、不改内存
        assert!(service.get().bgm.playlist.is_empty());
        assert!(!dir.join("audio.json").exists());
    }

    #[test]
    fn channel_parse_accepts_bgm_ambient_only() {
        assert_eq!(AudioChannel::parse("bgm"), Some(AudioChannel::Bgm));
        assert_eq!(AudioChannel::parse("ambient"), Some(AudioChannel::Ambient));
        assert_eq!(AudioChannel::parse("music"), None);
    }

    #[test]
    fn settings_patch_merges_partial_fields() {
        let dir = temp_dir("patch");
        let mut service = AudioService::new(dir.path().to_path_buf());
        // 只改 muted,其余保持默认
        service
            .update_settings(
                AudioChannel::Bgm,
                ChannelSettingsPatch {
                    muted: Some(true),
                    ..Default::default()
                },
            )
            .unwrap();
        let state = service.get();
        assert!(state.bgm.muted);
        assert_eq!(state.bgm.volume, 50);
        assert_eq!(state.bgm.mode, AudioMode::RepeatAll);
    }
}
