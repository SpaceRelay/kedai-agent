// 引擎单测(自 engine/mod.rs 拆分迁入,纯代码移动,测试内容零改动):
// rebuild_content_keeping_blocks 的补丁块保留、旧 run 收尾不清新 run 登记。
// 经 engine/mod.rs 的 `#[cfg(test)] mod run_generation_tests;` 挂载,
// 模块路径与测试名不变。
use super::run_finish::finish_run_generation;
use super::*;

/// censor 修正后的正文替换,<UpdateVariable> 补丁块必须原样保留(大小写变体同样容错),
/// 避免误伤 JSONPatch 中的变量键/值。
#[test]
fn rebuild_content_keeping_blocks_preserves_update_block() {
    let content = "正文含禁词示例。\n\n<UpdateVariable><JSONPatch>[{\"op\":\"replace\",\"path\":\"stat_data.世界.时间\",\"value\":[\"14:30\",\"初始\"]}]</JSONPatch></UpdateVariable>";
    let rebuilt = rebuild_content_keeping_blocks(content, "修正后的正文。");
    assert_eq!(
        rebuilt,
        "修正后的正文。<UpdateVariable><JSONPatch>[{\"op\":\"replace\",\"path\":\"stat_data.世界.时间\",\"value\":[\"14:30\",\"初始\"]}]</JSONPatch></UpdateVariable>"
    );
    assert!(!rebuilt.contains("禁词"), "正文应被替换:{rebuilt}");
}

#[test]
fn rebuild_content_keeping_blocks_handles_lowercase_and_no_block() {
    // 小写变体标签:仍应保留块
    let lower = "正文。<updatevariable><JSONPatch>[{\"op\":\"add\"}]</JSONPatch></updatevariable>";
    let rebuilt = rebuild_content_keeping_blocks(lower, "新正文。");
    assert!(rebuilt.starts_with("新正文。"));
    assert!(rebuilt.contains("<updatevariable>"), "{rebuilt}");
    // 无补丁块:整体替换
    let plain = rebuild_content_keeping_blocks("旧正文禁词。", "新正文。");
    assert_eq!(plain, "新正文。");
}

#[test]
fn old_run_finishing_does_not_remove_new_generation() {
    let session_id = "same-session";
    let old_run_id = uuid::Uuid::new_v4();
    let new_run_id = uuid::Uuid::new_v4();
    let (new_flag, _) = AbortFlag::new();
    let mut runs = HashMap::from([(
        session_id.to_string(),
        RunHandle {
            run_id: new_run_id,
            flag: new_flag,
            active: true,
        },
    )]);

    finish_run_generation(&mut runs, session_id, old_run_id);

    assert!(runs
        .get(session_id)
        .is_some_and(|run| run.run_id == new_run_id && run.active));
    finish_run_generation(&mut runs, session_id, new_run_id);
    assert!(!runs.contains_key(session_id));
}
