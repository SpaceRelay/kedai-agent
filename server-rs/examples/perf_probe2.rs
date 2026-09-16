//! 补充探针:定量验证「预编译正则收益」与「token 计数放大」。
//! 运行: cargo run --release --example perf_probe2
use std::time::Instant;

fn bench<T>(label: &str, iters: u32, mut f: impl FnMut() -> T) -> T {
    let t = Instant::now();
    let mut last = None;
    for _ in 0..iters {
        last = Some(f());
    }
    let el = t.elapsed();
    println!(
        "  {:<56} {:>9.3} ms/次",
        label,
        el.as_secs_f64() * 1000.0 / iters as f64
    );
    last.unwrap()
}

fn main() {
    let data_dir = std::env::var("DATA_DIR").unwrap_or_else(|_| r"D:\kedai-bench\data".to_string());
    let db_path = std::path::Path::new(&data_dir).join("kedai.db");
    let db = kedai_server::models::db::Db::open(&db_path, std::path::Path::new(&data_dir)).unwrap();
    let conn = db.read().unwrap();
    let raw: String = conn
        .query_row(
            "SELECT data_raw FROM characters WHERE name LIKE '%吸血鬼%' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);
    drop(db);
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let entries = kedai_server::parsing::world_book::character_book_entries(&value);
    let regex_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.use_regex && e.enabled)
        .collect();

    let texts: Vec<String> = (0..12)
        .map(|i| {
            format!(
                "第 {} 轮用户输入:她走进便利店,霓虹灯在雨里晕开。{}",
                i,
                "雨声很密,她把手插进口袋,想起刚才那条消息。".repeat(6)
            )
        })
        .collect();

    // === A. 现状:每轮现场编译正则 ===
    bench(
        "A 现状:现场 RegexBuilder::build 编译 54 条",
        200,
        || {
            regex_entries
                .iter()
                .map(|e| {
                    let pats: Vec<&str> = e
                        .keys
                        .iter()
                        .chain(e.keys_secondary.iter())
                        .map(|s| s.trim())
                        .filter(|s| !s.is_empty())
                        .collect();
                    pats.iter()
                        .filter(|p| {
                            regex::RegexBuilder::new(p)
                                .case_insensitive(!e.case_sensitive)
                                .build()
                                .is_ok()
                        })
                        .count()
                })
                .sum::<usize>()
        },
    );

    // === B. 优化:预编译一次后复用 ===
    let precompiled: Vec<Vec<regex::Regex>> = regex_entries
        .iter()
        .map(|e| {
            e.keys
                .iter()
                .chain(e.keys_secondary.iter())
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .filter_map(|p| {
                    regex::RegexBuilder::new(p)
                        .case_insensitive(!e.case_sensitive)
                        .build()
                        .ok()
                })
                .collect()
        })
        .collect();
    bench(
        "B 优化:预编译正则仅执行匹配(54 条)",
        200,
        || {
            let mut hits = 0usize;
            for res in &precompiled {
                for re in res {
                    if texts.iter().any(|m| re.is_match(m)) {
                        hits += 1;
                        break;
                    }
                }
            }
            hits
        },
    );

    // === C. token 计数放大 ===
    let mut ts = kedai_server::services::token_service::TokenService::new();
    let model = "gpt-4o";
    let long_text = "雨声很密,她把手插进口袋。".repeat(400); // ~4k 字符
    bench("C1 count_tokens(单段 ~2k token)", 50, || {
        ts.count_tokens(&long_text, model)
    });

    let msgs: Vec<kedai_server::models::types::LlmMessage> = (0..30)
        .map(|i| kedai_server::models::types::LlmMessage {
            role: if i % 2 == 0 {
                "user".into()
            } else {
                "assistant".into()
            },
            content: long_text.clone(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        })
        .collect();
    bench(
        "C2 count_message_tokens(30 条 × ~2k token 历史)",
        20,
        || ts.count_message_tokens(&msgs, model),
    );

    // === D. settings 整体 clone 成本(对照静态审计 #2) ===
    let settings_raw =
        std::fs::read_to_string(std::path::Path::new(&data_dir).join("settings.json"))
            .unwrap_or_default();
    let sv: serde_json::Value = serde_json::from_str(&settings_raw).unwrap();
    bench("D settings.json(7KB)深拷贝对照", 2000, || sv.clone());
    println!("\n  → settings.json 体积: {} 字节", settings_raw.len());
}
