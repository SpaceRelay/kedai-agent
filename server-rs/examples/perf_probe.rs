//! 本地热路径定量探针(dev-only,非产品代码)。
//! 用真实角色卡数据测量被审计点名的本地计算成本,与 LLM 往返耗时对照。
//! 运行: cargo run --release --example perf_probe

use std::time::Instant;

fn bench<T>(label: &str, iters: u32, mut f: impl FnMut() -> T) -> T {
    let t = Instant::now();
    let mut last = None;
    for _ in 0..iters {
        last = Some(f());
    }
    let el = t.elapsed();
    println!(
        "  {:<52} {:>9.3} ms/次  ({} 次共 {:.1} ms)",
        label,
        el.as_secs_f64() * 1000.0 / iters as f64,
        iters,
        el.as_secs_f64() * 1000.0
    );
    last.unwrap()
}

fn main() {
    let data_dir = std::env::var("DATA_DIR").unwrap_or_else(|_| r"D:\kedai-bench\data".to_string());
    let db_path = std::path::Path::new(&data_dir).join("kedai.db");

    let db = kedai_server::models::db::Db::open(&db_path, std::path::Path::new(&data_dir))
        .expect("打开数据库失败");
    let conn = db.read().expect("取只读连接失败");
    let raw: String = conn
        .query_row(
            "SELECT data_raw FROM characters WHERE name LIKE '%吸血鬼%' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .expect("未找到测试角色卡");
    drop(conn);
    drop(db);

    println!("角色卡 data_raw 长度: {} 字节\n", raw.len());

    // 1) JSON 解析(每请求一次)
    let value: serde_json::Value = bench("serde_json::from_str(542KB 角色卡)", 50, || {
        serde_json::from_str::<serde_json::Value>(&raw).unwrap()
    });

    // 2) 世界书条目归一化(每请求一次)
    let entries = bench("character_book_entries(54 条归一化)", 200, || {
        kedai_server::parsing::world_book::character_book_entries(&value)
    });
    println!("  → 条目数: {}", entries.len());
    let non_const: Vec<_> = entries
        .iter()
        .filter(|e| !e.constant && e.enabled)
        .collect();
    println!(
        "  → 非 constant 启用条目: {} 条 | use_regex: {} 条",
        non_const.len(),
        entries.iter().filter(|e| e.use_regex).count()
    );

    // 3) 构造扫描窗口(模拟一段中等长度的用户消息历史)
    let texts: Vec<String> = (0..12)
        .map(|i| {
            format!(
                "第 {} 轮用户输入:她走进便利店,霓虹灯在雨里晕开。{}",
                i,
                "雨声很密,她把手插进口袋,想起刚才那条消息。".repeat(6)
            )
        })
        .collect();
    let total_chars: usize = texts.iter().map(|t| t.len()).sum();
    println!(
        "\n  扫描窗口: {} 条消息 / {} 字符\n",
        texts.len(),
        total_chars
    );

    // 4) 条目匹配(热路径核心:每轮每条 non-constant 条目一次)
    bench("entry_matches_texts × 33 条(一轮匹配)", 300, || {
        non_const
            .iter()
            .filter(|e| kedai_server::parsing::world_book::entry_matches_texts(e, &texts))
            .count()
    });

    // 5) 单独量化:正则现场编译 vs 复用
    let regex_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.use_regex && e.enabled)
        .collect();
    bench(
        "仅正则编译(54 条 × RegexBuilder::build)",
        300,
        || {
            regex_entries
                .iter()
                .map(|e| {
                    let patterns: Vec<&str> = e
                        .keys
                        .iter()
                        .chain(e.keys_secondary.iter())
                        .map(|s| s.trim())
                        .filter(|s| !s.is_empty())
                        .collect();
                    patterns
                        .iter()
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

    // 6) 非正则分支的 to_lowercase 放大(对照:needle×消息 重复小写化)
    let sample_keys: Vec<String> = (0..8).map(|i| format!("tigger_keyword_{}", i)).collect();
    bench(
        "对照:8 关键词 × 12 消息 反复 to_lowercase",
        300,
        || {
            let mut hits = 0usize;
            for k in &sample_keys {
                for m in &texts {
                    if m.to_lowercase().contains(k) {
                        hits += 1;
                    }
                }
            }
            hits
        },
    );
    bench(
        "对照:12 消息先小写一次再匹配(优化后形态)",
        300,
        || {
            let lowered: Vec<String> = texts.iter().map(|m| m.to_lowercase()).collect();
            let mut hits = 0usize;
            for k in &sample_keys {
                for m in &lowered {
                    if m.contains(k) {
                        hits += 1;
                    }
                }
            }
            hits
        },
    );

    // 7) 占位符渲染(6 次链式 replace)
    let prompt = "{{character_name}}|{{char}}|{{character_description}}|{{personality}}|{{scenario}}|{{world_info}}"
        .to_string()
        + &"填充文本".repeat(2000);
    let world_text = "世界书注入内容".repeat(1500);
    let extra = vec![("{{extra1}}".to_string(), "值".to_string())];
    bench(
        "render_character_placeholders(6 次链式 replace)",
        500,
        || {
            kedai_server::services::prompt_kit::render_character_placeholders(
                &prompt,
                None,
                &world_text,
                &extra,
            )
        },
    );

    // 8) token 计数(tiktoken)
    let mut ts = kedai_server::services::token_service::TokenService::new();
    let model = "gpt-4o";
    bench("count_tokens(单段 8.8k token 文本)", 30, || {
        ts.count_tokens(&world_text, model)
    });

    println!("\n(单位说明:ms/次 = 单次调用平均耗时)");
}
