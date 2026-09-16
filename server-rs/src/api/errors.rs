// 结构化错误码 + HTTP 错误响应**唯一出口**(2026-08 收尾;2026-09-16 err_json 收口)。
// 错误响应 JSON 在既有 { "error": msg } 基础上增加 "code" 字段,前端按 code 分类提示;
// error 字段保持不变以向后兼容。
//
// 出口清单(新增 handler 一律用这里的构造器,勿再就地拼 Json):
//   - err_status(msg, status):状态码由调用方给定,按 code_for_status 补齐 code。
//     各 handler 私有的 err_json 影子实现(曾散在 8 个文件)全部收敛到此。
//   - validation / not_found / conflict:4xx 语义构造器,原文透传(用户可据此纠正操作)。
//   - internal / upstream:5xx 语义构造器,入参是**内部错误串**,只进日志,
//     响应体给稳定文案(与 util.rs 的 db_err 同策略,防 SQLite 原文/盘符路径外泄)。
//   - util.rs 的 db_err → code: DB(spawn_blocking JoinError / 连接池 / 服务内 String)
//
// 收口进度:api/ 下裸 { "error" } 由 check-arch 规则 G 的 ratchet 基线守着(只降不升),
// 按域分批替换为 err_status;替换只增 code 字段、不改状态码。
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

/// 5xx 出口的对外稳定文案:内部失败原文只进日志。
const SERVER_ERR_USER_MESSAGE: &str = "服务端内部错误,详情见服务端日志";

/// 状态码由调用方给定的统一错误出口,code 按 [`code_for_status`] 补齐(**原文透传**)。
///
/// 这是各 handler 私有 `err_json` 影子实现的等价替换:HTTP 状态码与 `error` 文案
/// 逐字不变,只多一个 `code` 字段。**不改变既有响应语义**,故可安全地逐域替换。
///
/// 入参是**可能被用户看到**的文案(4xx 场景通常是「缺少 session_id」这类可纠正提示)。
/// 若入参是内部错误串(服务/DB 报错原文),请改走 [`internal`] / [`upstream`],
/// 它们把原文降级为日志。
pub(crate) fn err_status(msg: impl AsRef<str>, status: StatusCode) -> Response {
    err_with_code(code_for_status(status), msg, status)
}

/// 400 + `VALIDATION`:参数缺失 / 校验失败。
pub(crate) fn validation(msg: impl AsRef<str>) -> Response {
    err_with_code(ErrorCode::Validation, msg, StatusCode::BAD_REQUEST)
}

/// 404 + `NOT_FOUND`:资源不存在(角色 / 会话 / 消息 / 快照等)。
pub(crate) fn not_found(msg: impl AsRef<str>) -> Response {
    err_with_code(ErrorCode::NotFound, msg, StatusCode::NOT_FOUND)
}

/// 409 + `CONFLICT`:状态冲突(会话正在生成中、重发/重生成锚点失效等)。
pub(crate) fn conflict(msg: impl AsRef<str>) -> Response {
    err_with_code(ErrorCode::Conflict, msg, StatusCode::CONFLICT)
}

/// 500 + `INTERNAL`:服务端内部失败。`detail` 是内部错误串,**只写日志**——
/// 响应体给固定文案,不回显 SQLite 报错、SQL 片段或含盘符的路径。
pub(crate) fn internal(detail: impl AsRef<str>) -> Response {
    tracing::error!(error = %detail.as_ref(), "服务端内部错误");
    err_with_code(
        ErrorCode::Internal,
        SERVER_ERR_USER_MESSAGE,
        StatusCode::INTERNAL_SERVER_ERROR,
    )
}

/// 502 + `UPSTREAM`:上游 LLM/服务错误。同上,`detail` 只进日志。
pub(crate) fn upstream(detail: impl AsRef<str>) -> Response {
    tracing::error!(error = %detail.as_ref(), "上游服务错误");
    err_with_code(
        ErrorCode::Upstream,
        SERVER_ERR_USER_MESSAGE,
        StatusCode::BAD_GATEWAY,
    )
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
        assert_eq!(body["code"], "DB");
    }

    /// 泄露守卫(批次 1):内部错误原文只进日志,响应体只给稳定文案。
    /// 触发内部失败后不得回显 SQLite 报错、SQL 片段或盘符路径。
    #[tokio::test]
    async fn db_err_never_leaks_internal_detail() {
        let (status, body) = body_json(super::super::util::db_err(
            "DB 任务执行失败: SQLite error: no such column: character_id (C:\\Users\\ops\\AppData\\data\\kedai.db)",
        ))
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["code"], "DB");
        assert_eq!(body["error"], "数据库操作失败,详情见服务端日志");
        let text = body.to_string();
        for leaked in ["SQLite", "no such column", "C:\\", "kedai.db"] {
            assert!(
                !text.contains(leaked),
                "响应体泄露内部细节「{leaked}」:{text}"
            );
        }
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

    /// err_status 是各 handler 私有 err_json 的等价替换:状态码与原文逐字不变,只多 code。
    #[tokio::test]
    async fn err_status_preserves_status_and_message_adds_code() {
        let (status, body) =
            body_json(err_status("缺少 session_id", StatusCode::BAD_REQUEST)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "缺少 session_id");
        assert_eq!(body["code"], "VALIDATION");

        // 418 这类无专属映射的 4xx 归 VALIDATION(与 code_for_status 一致),原文仍透传
        let (status, body) = body_json(err_status("我是茶壶", StatusCode::IM_A_TEAPOT)).await;
        assert_eq!(status, StatusCode::IM_A_TEAPOT);
        assert_eq!(body["error"], "我是茶壶");
        assert_eq!(body["code"], "VALIDATION");
    }

    /// 语义构造器与状态码/code 的对应必须稳定(前端按 code 分类提示)。
    #[tokio::test]
    async fn semantic_constructors_pair_status_with_code() {
        let cases: [(Response, StatusCode, &str); 3] = [
            (
                validation("参数不合法"),
                StatusCode::BAD_REQUEST,
                "VALIDATION",
            ),
            (not_found("会话不存在"), StatusCode::NOT_FOUND, "NOT_FOUND"),
            (
                conflict("该会话正在生成中"),
                StatusCode::CONFLICT,
                "CONFLICT",
            ),
        ];
        for (resp, want_status, want_code) in cases {
            let (status, body) = body_json(resp).await;
            assert_eq!(status, want_status);
            assert_eq!(body["code"], want_code);
        }
    }

    /// 5xx 出口不得回显内部原文(与 db_err 同策略);原文只应进日志。
    #[tokio::test]
    async fn internal_and_upstream_never_leak_detail() {
        let leaky = "SQLite error: no such column: x (C:\\Users\\ops\\data\\kedai.db)";
        for resp in [internal(leaky), upstream(leaky)] {
            let (status, body) = body_json(resp).await;
            assert!(status.is_server_error(), "5xx 期望,实得 {status}");
            assert_eq!(body["error"], SERVER_ERR_USER_MESSAGE);
            let text = body.to_string();
            for leaked in ["SQLite", "no such column", "C:\\", "kedai.db"] {
                assert!(!text.contains(leaked), "响应体泄露「{leaked}」:{text}");
            }
        }
    }
}
