use crate::api::WithStatus;
// JSON 请求体提取器统一收口(批次 1 · 错误面收口)。
//
// 背景(实测):axum 内建 `Json<T>` 的 `JsonRejection` 直接 `IntoResponse` 时,
// 畸形 JSON 返回 **400 text/plain**(如 `Failed to deserialize the JSON body into the
// target type: ...`),前端 `client.ts` 的 `request()` 走 `res.json()` 解析会抛错,
// 只能拿到「请求失败 400」——丢失了「哪个字段不对」这一用户可纠正的信息。
//
// 本模块提供薄包装 `JsonBody<T>`:把任何 `JsonRejection` 映射为
// `{ error, code: "VALIDATION" }` + 400,与 `api/errors.rs` 的错误契约一致。
//
// 接入方式(增量):handler 签名 `Json(body): Json<T>` → `JsonBody(body): JsonBody<T>`,
// 路由 `body` 类型不变(仍是 `T`)。未接入的 handler 保持 axum 内建行为,不影响功能。
//
// **已知例外:413 请求体过大**。`DefaultBodyLimit`(api/mod.rs 的 35MB 层)在
// **提取器被调用前**就由 tower 中间件拒绝,产生的是 `LengthLimitError`,本模块的
// `FromRequest` 根本不执行——要统一 413 的 JSON 形状必须自定义 tower 层拦截 body
// `Frame` 错误,成本(改动全站 body 流)远高于收益(体积超限本就罕见且前端上传前已校验),
// 故本轮登记为已知例外,不做改造。
use axum::extract::rejection::JsonRejection;
use axum::extract::FromRequest;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// 畸形/不可解析 JSON 体的用户文案:不含 serde 内部类型名与行列细节
/// (那些进 `tracing::warn!` 日志),只说明「哪里出错了 + 怎么办」。
const INVALID_JSON_MESSAGE: &str = "请求体不是合法 JSON,请检查字段格式";

/// JSON 体提取器:`Json<T>` 的收口包装,拒绝时输出 `{error, code}` + 400(JSON)。
#[derive(Debug, Clone, Copy, Default)]
pub struct JsonBody<T>(pub T);

impl<S, T> FromRequest<S> for JsonBody<T>
where
    Json<T>: FromRequest<S, Rejection = JsonRejection>,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(JsonBody(value)),
            Err(rejection) => {
                // 原始 rejection 只进日志:它带 serde 类型名/字节偏移,对用户无行动价值
                tracing::warn!(error = %rejection, "JSON 请求体解析失败");
                Err(invalid_json_response())
            }
        }
    }
}

/// 统一的「非法 JSON」响应:400 + `{error, code: VALIDATION}`。
fn invalid_json_response() -> Response {
    Json(json!({
        "error": INVALID_JSON_MESSAGE,
        "code": crate::api::ErrorCode::Validation.as_str(),
    }))
    .into_response()
    .with_status(StatusCode::BAD_REQUEST)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::routing::post;
    use axum::Router;
    use serde::Deserialize;
    use tower::ServiceExt;

    #[derive(Deserialize)]
    struct Payload {
        name: String,
    }

    fn app() -> Router {
        Router::new().route(
            "/echo",
            post(|JsonBody(p): JsonBody<Payload>| async move { p.name }),
        )
    }

    /// 读取响应 (状态码, content-type, JSON/文本 body)
    async fn call(body: &str) -> (StatusCode, String, String) {
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/echo")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = app().oneshot(req).await.unwrap();
        let status = resp.status();
        let ct = resp
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, ct, String::from_utf8_lossy(&bytes).to_string())
    }

    #[tokio::test]
    async fn valid_body_passes_through() {
        let (status, _, body) = call(r#"{"name":"kedai"}"#).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "kedai");
    }

    #[tokio::test]
    async fn malformed_json_is_json_400_with_validation_code() {
        let (status, ct, body) = call("{不是合法 JSON").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // 关键:不再返回 text/plain,前端 request() 可解析出 code
        assert!(
            ct.starts_with("application/json"),
            "content-type 应为 JSON:{ct}"
        );
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["code"], "VALIDATION");
        assert!(v["error"].as_str().unwrap().contains("合法 JSON"));
        // 泄露守卫:响应体不得回显 serde 的解析细节
        assert!(
            !body.contains("Failed to deserialize"),
            "泄露 serde 原文:{body}"
        );
    }

    #[tokio::test]
    async fn missing_required_field_is_json_400_with_validation_code() {
        // 字段缺失(结构合法但不符合目标类型)同样收口为 JSON + VALIDATION
        let (status, ct, body) = call(r#"{"other":1}"#).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            ct.starts_with("application/json"),
            "content-type 应为 JSON:{ct}"
        );
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["code"], "VALIDATION");
        assert!(!body.contains("missing field"), "泄露 serde 原文:{body}");
    }

    #[tokio::test]
    async fn wrong_content_type_is_json_400_with_validation_code() {
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/echo")
            .header("content-type", "text/plain")
            .body(Body::from(r#"{"name":"kedai"}"#))
            .unwrap();
        let resp = app().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let ct = resp
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert!(
            ct.starts_with("application/json"),
            "content-type 应为 JSON:{ct}"
        );
    }
}
