// 结构化错误码(2026-08 收尾):错误响应 JSON 在既有 { "error": msg } 基础上
// 增加 "code" 字段,前端按 code 分类提示;error 字段保持不变以向后兼容。
//
// 覆盖范围(务实版,不做全量重构):
//   - util.rs 的 db_err → code: DB(spawn_blocking JoinError / 连接池 / 服务内 String)
//   - chat.rs / sessions.rs / characters.rs / contract_history.rs / kaleido.rs
//     的典型 4xx 场景:参数校验(VALIDATION)、资源不存在(NOT_FOUND)、状态冲突(CONFLICT)
//   - 其余 handler 后续按同模式补;未覆盖处仍是裸 { "error" },前端按无 code 兜底处理
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use super::WithStatus;

/// API 错误码(响应 JSON 的 code 字段值,SCREAMING_SNAKE,属对外稳定契约)。
/// 按现有 handler 的错误场景归纳;新增场景优先复用现有码,确需新码时在此追加。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// 参数缺失 / 校验失败(HTTP 400;contract 回滚的 422 语义校验也归此码)
    Validation,
    /// 未认证 / token 失效(HTTP 401;预留给鉴权出口接入)
    Unauthorized,
    /// 资源不存在:角色 / 会话 / 消息 / 历史记录等(HTTP 404)
    NotFound,
    /// 状态冲突:会话正在生成中、重发/重生成锚点失效等(HTTP 409)
    Conflict,
    /// 数据库 / 阻塞任务失败(db_call / db_read / db_write 的 Err 出口,HTTP 500)
    Db,
    /// 上游 LLM 连接器错误(预留,供 connectors 错误出口接入)
    Upstream,
    /// 未归类的服务端内部错误(HTTP 500)
    Internal,
}

impl ErrorCode {
    /// 响应 JSON 的 code 字段值
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Validation => "VALIDATION",
            ErrorCode::Unauthorized => "UNAUTHORIZED",
            ErrorCode::NotFound => "NOT_FOUND",
            ErrorCode::Conflict => "CONFLICT",
            ErrorCode::Db => "DB",
            ErrorCode::Upstream => "UPSTREAM",
            ErrorCode::Internal => "INTERNAL",
        }
    }
}

/// 带错误码的 JSON 错误响应:{ "error": msg, "code": code } + HTTP status。
/// 错误码与状态码须匹配(见 errors.rs 各变体文档),调用方勿跨档使用。
pub(crate) fn err_with_code(code: ErrorCode, msg: impl AsRef<str>, status: StatusCode) -> Response {
    Json(json!({ "error": msg.as_ref(), "code": code.as_str() }))
        .into_response()
        .with_status(status)
}

/// 按 HTTP 状态码推断默认错误码(供仅知状态码的旧辅助函数平滑迁移):
/// 400 → VALIDATION,401 → UNAUTHORIZED,404 → NOT_FOUND,409 → CONFLICT,
/// 502 → UPSTREAM,其余 4xx → VALIDATION(参数类兜底),5xx → INTERNAL。
pub(crate) fn code_for_status(status: StatusCode) -> ErrorCode {
    match status {
        StatusCode::BAD_REQUEST => ErrorCode::Validation,
        StatusCode::UNAUTHORIZED => ErrorCode::Unauthorized,
        StatusCode::NOT_FOUND => ErrorCode::NotFound,
        StatusCode::CONFLICT => ErrorCode::Conflict,
        StatusCode::BAD_GATEWAY => ErrorCode::Upstream,
        s if s.is_client_error() => ErrorCode::Validation,
        _ => ErrorCode::Internal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 读取响应 (状态码, JSON body)
    async fn body_json(resp: Response) -> (StatusCode, serde_json::Value) {
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn err_with_code_carries_code_and_status() {
        let (status, body) = body_json(err_with_code(
            ErrorCode::NotFound,
            "会话不存在",
            StatusCode::NOT_FOUND,
        ))
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "会话不存在");
        assert_eq!(body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn err_with_code_validation_and_conflict() {
        let (status, body) = body_json(err_with_code(
            ErrorCode::Validation,
            "缺少 session_id",
            StatusCode::BAD_REQUEST,
        ))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "VALIDATION");

        let (status, body) = body_json(err_with_code(
            ErrorCode::Conflict,
            "该会话正在生成中",
            StatusCode::CONFLICT,
        ))
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], "CONFLICT");
    }

    #[tokio::test]
    async fn db_err_carries_db_code() {
        // DB 阻塞任务失败出口:500 + code "DB"(前端据此提示「服务端错误」)
        let (status, body) = body_json(super::super::util::db_err("连接池耗尽")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"], "连接池耗尽");
        assert_eq!(body["code"], "DB");
    }

    #[test]
    fn code_for_status_maps_typical_statuses() {
        assert_eq!(
            code_for_status(StatusCode::BAD_REQUEST),
            ErrorCode::Validation
        );
        assert_eq!(
            code_for_status(StatusCode::UNPROCESSABLE_ENTITY),
            ErrorCode::Validation
        );
        assert_eq!(
            code_for_status(StatusCode::UNAUTHORIZED),
            ErrorCode::Unauthorized
        );
        assert_eq!(code_for_status(StatusCode::NOT_FOUND), ErrorCode::NotFound);
        assert_eq!(code_for_status(StatusCode::CONFLICT), ErrorCode::Conflict);
        assert_eq!(
            code_for_status(StatusCode::BAD_GATEWAY),
            ErrorCode::Upstream
        );
        assert_eq!(
            code_for_status(StatusCode::INTERNAL_SERVER_ERROR),
            ErrorCode::Internal
        );
    }

    #[test]
    fn error_code_strings_are_stable_contract() {
        // code 字符串属前后端契约(前端 client.ts 按此分类),改动需两端同步
        assert_eq!(ErrorCode::Validation.as_str(), "VALIDATION");
        assert_eq!(ErrorCode::Unauthorized.as_str(), "UNAUTHORIZED");
        assert_eq!(ErrorCode::NotFound.as_str(), "NOT_FOUND");
        assert_eq!(ErrorCode::Conflict.as_str(), "CONFLICT");
        assert_eq!(ErrorCode::Db.as_str(), "DB");
        assert_eq!(ErrorCode::Upstream.as_str(), "UPSTREAM");
        assert_eq!(ErrorCode::Internal.as_str(), "INTERNAL");
    }
}
