// 角色卡 / 导入导出 / 插件 / 技能 / 资源代理 / 任务 / 音频 / 脚本 / slash / 宏路由
// (自 api/mod.rs build_router 迁入)
use crate::api::app_state::AppState;
use crate::api::{
    audio, characters, exec, import_export, macros, plugins, resource, skills, slash_commands,
    tasks, user_scripts,
};
use axum::routing::{get, post, put};
use axum::Router;
use std::sync::Arc;

pub(crate) fn misc_routes() -> Router<Arc<AppState>> {
    Router::new()
        // 命令执行审计与执行器状态(阶段 B/E)
        .route("/api/exec/tier", get(exec::tier))
        .route(
            "/api/exec/audit",
            get(exec::list_audit).delete(exec::clear_audit),
        )
        // 角色卡
        .route("/api/characters", get(characters::list))
        .route("/api/characters/upload", post(characters::upload))
        .route(
            "/api/characters/{id}",
            get(characters::get)
                .put(characters::update)
                .delete(characters::delete),
        )
        // 导入导出
        .route("/api/export/chat", get(import_export::export_chat))
        .route("/api/import/chat", post(import_export::import_chat))
        // 插件(自定义工具)
        .route("/api/plugins/tools", get(plugins::list_tools))
        .route("/api/plugins/tools/reload", post(plugins::reload_tools))
        .route("/api/plugins/tools/upload", post(plugins::upload_tool))
        .route(
            "/api/plugins/tools/{name}",
            axum::routing::delete(plugins::delete_tool),
        )
        // 技能库(提示词技能)
        .route("/api/skills", get(skills::list).post(skills::import_skills))
        .route(
            "/api/skills/{id}",
            put(skills::update).delete(skills::delete),
        )
        // 角色卡远程资源界面代理(https + SSRF 防护)
        .route("/api/resource/proxy", get(resource::proxy))
        // 任务模式(task 工作台):列表/新建/详情/执行/停止/删除/全局 token 累计
        .route("/api/tasks", get(tasks::list).post(tasks::create))
        // 静态段优先于 {id} 参数段:全部任务 usage 累计(须在 {id} 之前注册,语义更清晰)
        .route("/api/tasks/usage-total", get(tasks::usage_total))
        // 任务事件 SSE 流(WP4):同为静态段,axum 静态段优先于 {id},不会被通配吃掉
        .route("/api/tasks/events", get(tasks::events))
        .route("/api/tasks/{id}", get(tasks::get).delete(tasks::delete))
        .route("/api/tasks/{id}/run", post(tasks::run))
        .route("/api/tasks/{id}/stop", post(tasks::stop))
        // 计划批准(plan 模式;批次 4.3a):planned 态批准,可携修改后计划,solo 续跑
        .route("/api/tasks/{id}/approve", post(tasks::approve))
        // 终态追加指令(批次 R2a):done/partial/error/ended 可追加,solo 续跑续写成果
        .route("/api/tasks/{id}/followup", post(tasks::followup))
        // 批准环节规划对话(批次 R2b):planned 态按反馈修订计划,保持 planned 待重新批准
        .route("/api/tasks/{id}/plan-chat", post(tasks::plan_chat))
        // 任务 LLM 调用追踪(批次 3「调用情况」面板全量补拉)
        .route("/api/tasks/{id}/calls", get(tasks::list_calls))
        // 音频播放器(bgm/ambient 双通道):读取 / 设置 / 播放列表
        .route("/api/audio", get(audio::get))
        .route("/api/audio/settings", put(audio::update_settings))
        .route("/api/audio/playlist", put(audio::update_playlist))
        // 用户脚本(ScriptTree,阶段三):global / character 两级脚本树全量读写
        .route(
            "/api/scripts/tree",
            get(user_scripts::get_tree).put(user_scripts::save_tree),
        )
        // 角色卡脚本授权台账(2026-09-14,known-limitations L12 后端授权门)
        // GET 查询授权态(含后端实时计算的 current_hash)/ PUT 授权 / DELETE 撤销
        .route(
            "/api/script-authorizations",
            get(user_scripts::get_authorization)
                .put(user_scripts::grant_authorization)
                .delete(user_scripts::revoke_authorization),
        )
        .route(
            "/api/script-authorizations/list",
            get(user_scripts::list_authorizations),
        )
        // slash 命令清单(阶段四 4a):前端输入框联想
        .route("/api/slash/commands", get(slash_commands::list_commands))
        // 宏调试(阶段六 6b):纯扁平 vars 展开,供前端「宏调试」面板验证模板结果
        .route("/api/macros/expand", post(macros::expand))
}
