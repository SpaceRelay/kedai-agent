//! 测量任务写路径(record_usage / record_llm_call)的真实成本,
//! 为「事务化是否值得」提供数据依据(而不是凭假设改)。
//! 运行: cargo run --release --example perf_probe4
use std::time::Instant;

fn main() {
    let dir = std::env::temp_dir().join(format!("kedai-probe4-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = std::sync::Arc::new(
        kedai_server::models::db::Db::open(&dir.join("kedai.db"), &dir).unwrap(),
    );
    {
        let conn = db.write();
        conn.execute(
            "INSERT INTO characters (id, name, chara_name, description, file_path, data_raw, created_at)
             VALUES ('c1','c','c','','','{}','')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, character_id, title, created_at, updated_at)
             VALUES ('s1','c1','t','','')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, character_id, status, created_at, updated_at)
             VALUES ('t1','t','c1','pending','','')",
            [],
        )
        .unwrap();
    }

    // ① 单条 INSERT(task_usage),各自 autocommit → 各一次 WAL 提交
    let t = Instant::now();
    const N: usize = 200;
    for i in 0..N {
        let conn = db.write();
        conn.execute(
            "INSERT INTO task_usage (id, task_id, phase, step_index, model, prompt_tokens, completion_tokens, reasoning_tokens, created_at)
             VALUES (?1,'t1','agent',NULL,'m',10,20,0,'')",
            rusqlite::params![format!("u{i}")],
        )
        .unwrap();
    }
    let single = t.elapsed();
    println!(
        "① {} 条独立 INSERT(各一次提交)  : {:>8.2} ms 合计  → {:.3} ms/条",
        N,
        single.as_secs_f64() * 1000.0,
        single.as_secs_f64() * 1000.0 / N as f64
    );

    // ② 同一批量包在一个事务里
    let t = Instant::now();
    {
        let conn = db.write();
        conn.execute_batch("BEGIN").unwrap();
        for i in 0..N {
            conn.execute(
                "INSERT INTO task_usage (id, task_id, phase, step_index, model, prompt_tokens, completion_tokens, reasoning_tokens, created_at)
                 VALUES (?1,'t1','agent',NULL,'m',10,20,0,'')",
                rusqlite::params![format!("b{i}")],
            )
            .unwrap();
        }
        conn.execute_batch("COMMIT").unwrap();
    }
    let batched = t.elapsed();
    println!(
        "② {} 条同事务批量 INSERT        : {:>8.2} ms 合计  → {:.3} ms/条",
        N,
        batched.as_secs_f64() * 1000.0,
        batched.as_secs_f64() * 1000.0 / N as f64
    );

    println!(
        "\n单条 vs 批量: {:.1}× 差距",
        single.as_secs_f64() / batched.as_secs_f64()
    );
    println!(
        "推论:一次任务若含 6 轮调用、每轮 2 条写入 = 12 条独立提交 ≈ {:.2} ms;",
        single.as_secs_f64() * 1000.0 / N as f64 * 12.0
    );
    println!("      相对 LLM 往返的秒级延迟,该量级不足以单独构成优化理由。");

    drop(db);
    std::fs::remove_dir_all(dir).ok();
}
