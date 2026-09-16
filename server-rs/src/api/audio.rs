// 音频路由:/api/audio(读取全量)/ PUT settings / PUT playlist
use crate::api::app_state::AppState;
use crate::api::{internal, validation};
use crate::services::audio_service::{AudioChannel, AudioTrack, ChannelSettingsPatch};
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

/// GET /api/audio:返回双通道全量状态
pub async fn get(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let audio = state.audio.lock().unwrap_or_else(|e| e.into_inner()).get();
    Json(json!({ "audio": audio }))
}

#[derive(Deserialize)]
pub struct SettingsBody {
    /// 通道:bgm | ambient
    pub r#type: String,
    pub settings: ChannelSettingsPatch,
}

/// PUT /api/audio/settings:更新单通道设置(部分字段合并)
pub async fn update_settings(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SettingsBody>,
) -> Response {
    let Some(channel) = AudioChannel::parse(&body.r#type) else {
        return validation("无效的音频通道");
    };
    // B-1:update_settings 内含同步 JSON 落盘,持锁 + 写文件整体挪阻塞线程池
    let audio = state.audio.clone();
    let result = state
        .db_call(move || {
            let mut svc = audio.lock().unwrap_or_else(|e| e.into_inner());
            svc.update_settings(channel, body.settings)
                .map(|()| svc.get())
        })
        .await;
    match result {
        Ok(Ok(state)) => Json(json!({ "ok": true, "audio": state })).into_response(),
        Ok(Err(e)) => validation(e),
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
pub struct PlaylistBody {
    /// 通道:bgm | ambient
    pub r#type: String,
    pub tracks: Vec<AudioTrack>,
}

/// PUT /api/audio/playlist:替换单通道播放列表(URL 协议白名单校验)
pub async fn update_playlist(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PlaylistBody>,
) -> Response {
    let Some(channel) = AudioChannel::parse(&body.r#type) else {
        return validation("无效的音频通道");
    };
    // B-1:update_playlist 内含同步 JSON 落盘,持锁 + 写文件整体挪阻塞线程池
    let audio = state.audio.clone();
    let result = state
        .db_call(move || {
            let mut svc = audio.lock().unwrap_or_else(|e| e.into_inner());
            svc.update_playlist(channel, body.tracks)
                .map(|()| svc.get())
        })
        .await;
    match result {
        Ok(Ok(state)) => Json(json!({ "ok": true, "audio": state })).into_response(),
        Ok(Err(e)) => validation(e),
        Err(e) => internal(e),
    }
}
