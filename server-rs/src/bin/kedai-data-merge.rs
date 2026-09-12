use kedai_server::migration::merge_data_dirs;
use std::path::PathBuf;

fn main() {
    let args = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if args.len() != 3 {
        eprintln!("用法: kedai-data-merge <基线数据目录> <源数据目录> <工作目录>");
        std::process::exit(2);
    }
    match merge_data_dirs(&args[0], &args[1], &args[2]) {
        Ok(report) => match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(error) => {
                eprintln!("序列化合并报告失败: {error}");
                std::process::exit(1);
            }
        },
        Err(error) => {
            eprintln!("数据合并失败: {error}");
            std::process::exit(1);
        }
    }
}
