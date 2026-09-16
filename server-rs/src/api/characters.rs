// 角色卡路由:/api/characters(列表/上传/详情/更新/删除)
// services 同步 DB 调用均经 state.db_call 挪进阻塞线程池(DB 并发改造)
use crate::api::app_state::AppState;
use crate::api::json_body::JsonBody;
use crate::api::{db_err, err_status, WithStatus};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct UpdateBody {
    #[serde(default)]
    pub chara_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub first_mes: Option<String>,
    /// 备用开场列表(空数组 = 清空)
    #[serde(default)]
    pub alternate_greetings: Option<Vec<String>>,
}

pub async fn list(State(state): State<Arc<AppState>>) -> Response {
    let svc = state.characters.clone();
    // 阻塞任务失败(线程池 JoinError 等)返回 500,不 expect panic——
    // panic 会杀掉连接,前端只能看到「网络错误」,列表永远空白且无任何可读原因
    match state.db_call(move || svc.list()).await {
        Ok(chars) => Json(json!({ "characters": chars })).into_response(),
        Err(e) => db_err(&e),
    }
}

const MAX_UPLOAD: usize = 30 * 1024 * 1024;

/// 手动解析 multipart(完整字节在内存中):字段名 file,限制 30MB。
/// 不依赖 multer 流式解析(其在测试 one-shot 流下会挂起)。
pub async fn upload(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let Some(boundary) = parse_boundary(content_type) else {
        return err_status("缺少文件字段(file)", StatusCode::BAD_REQUEST);
    };
    let Some((file_name, file_bytes)) = parse_multipart_file(&body, &boundary) else {
        return err_status("缺少文件字段(file)", StatusCode::BAD_REQUEST);
    };
    if file_bytes.len() > MAX_UPLOAD {
        return err_status("文件超过 30MB 上限", StatusCode::BAD_REQUEST);
    }
    let svc = state.characters.clone();
    let upload_result = state
        .db_call(move || svc.upload(&file_bytes, &file_name))
        .await;
    match upload_result {
        Err(e) => db_err(&e),
        Ok(Ok(c)) => {
            // 契约缓存失效:上传即覆盖同名片,data_raw(契约来源)可能变化
            state.invalidate_contracts_for_character(&c.id);
            Json(c).into_response().with_status(StatusCode::CREATED)
        }
        Ok(Err(e)) => err_status(e, StatusCode::BAD_REQUEST),
    }
}

/// 从 Content-Type 提取 boundary(值保留原大小写)
pub(crate) fn parse_boundary(content_type: &str) -> Option<String> {
    let lower = content_type.to_lowercase();
    let idx = lower.find("boundary=")?;
    // 在原字符串中定位同位置
    let rest = &content_type[idx + "boundary=".len()..];
    let rest = rest.trim().trim_matches('"');
    let end = rest.find(';').unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

/// 在完整 multipart 字节中查找 file 字段,返回 (filename, content)
fn parse_multipart_file(body: &[u8], boundary: &str) -> Option<(String, Vec<u8>)> {
    parse_multipart(body, boundary)
        .into_iter()
        .find_map(|p| p.filename.map(|f| (f, p.content)))
}

/// 通用 multipart 解析:返回全部 part(字段名 / 可选文件名 / 内容),供 world_books 复用
pub(crate) struct MultipartPart {
    pub field: String,
    pub filename: Option<String>,
    pub content: Vec<u8>,
}

pub(crate) fn parse_multipart(body: &[u8], boundary: &str) -> Vec<MultipartPart> {
    let mut parts = Vec::new();
    let delim = format!("--{boundary}");
    let delim_bytes = delim.as_bytes();
    let mut search_from = 0usize;
    while let Some(rel) = find_subsequence(&body[search_from..], delim_bytes) {
        // 跳过 boundary + 后续 CRLF
        let part_start = search_from + rel + delim_bytes.len() + 2;
        let Some(hdr_end_rel) = find_subsequence(&body[part_start..], b"\r\n\r\n") else {
            break;
        };
        let headers_block = &body[part_start..part_start + hdr_end_rel];
        let content_start = part_start + hdr_end_rel + 4;
        let rest = &body[content_start..];
        // 内容到下一个 "\r\n--boundary" 为止
        let Some(next_rel) = find_subsequence(rest, b"\r\n--") else {
            break;
        };
        let content = &rest[..next_rel];
        let headers_text = String::from_utf8_lossy(headers_block);
        let mut field = String::new();
        let mut filename: Option<String> = None;
        for line in headers_text.lines() {
            let l = line.trim_start();
            if l.to_lowercase().starts_with("content-disposition") {
                let s = l.to_string();
                if let Some(i) = s.find("name=") {
                    let rest = &s[i + "name=".len()..];
                    let rest = rest.trim_start().trim_start_matches('"');
                    let end = rest.find('"').unwrap_or(rest.len());
                    field = rest[..end].to_string();
                }
                if let Some(i) = s.find("filename=") {
                    let rest = &s[i + "filename=".len()..];
                    let rest = rest.trim_start().trim_start_matches('"');
                    let end = rest.find('"').unwrap_or(rest.len());
                    filename = Some(rest[..end].to_string());
                }
            }
        }
        if !field.is_empty() {
            parts.push(MultipartPart {
                field,
                filename,
                content: content.to_vec(),
            });
        }
        // 下一个 boundary 起点:指向 "\r\n--" 处(不 +4,让 find_subsequence 重新定位 "--BOUND")
        search_from = content_start + next_rel;
    }
    parts
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

pub async fn get(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let svc = state.characters.clone();
    let found = state.db_call(move || svc.get(&id)).await;
    match found {
        Err(e) => db_err(&e),
        Ok(Some(mut c)) => {
            // 角色卡内嵌插件检测(酒馆助手等):基于 data_raw 的 character_book
            if let Some(raw) = c.data_raw.as_ref() {
                let plugins = crate::parsing::assistant::detect_card_plugins(raw);
                c.card_plugins = if plugins.is_empty() {
                    None
                } else {
                    Some(plugins)
                };
            }
            Json(c).into_response()
        }
        Ok(None) => err_status("角色卡不存在", StatusCode::NOT_FOUND),
    }
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<UpdateBody>,
) -> Response {
    let svc = state.characters.clone();
    let updated = state
        .db_call(move || {
            svc.update(
                &id,
                body.chara_name.as_deref(),
                body.description.as_deref(),
                body.first_mes.as_deref(),
                body.alternate_greetings.as_deref(),
            )
        })
        .await;
    match updated {
        Err(e) => db_err(&e),
        Ok(Some(c)) => {
            state.invalidate_contracts_for_character(&c.id);
            Json(c).into_response()
        }
        Ok(None) => err_status("角色卡不存在", StatusCode::NOT_FOUND),
    }
}

pub async fn delete(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let svc = state.characters.clone();
    let del_id = id.clone();
    match state.db_call(move || svc.delete(&del_id)).await {
        Err(e) => db_err(&e),
        Ok(true) => {
            state.invalidate_contracts_for_character(&id);
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => err_status("角色卡不存在", StatusCode::NOT_FOUND),
    }
}
