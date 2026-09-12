// 构建脚本:让 cargo 感知前端 web/dist 的变化,并向二进制注入构建指纹。
// kedai-server 用 include_dir! 在编译期把 web/dist 整体嵌入二进制(见 src/api/mod.rs),
// 但 include_dir 宏本身不输出 rerun-if-changed,cargo 增量编译只看 .rs 文件,
// 导致改完前端后 kedai-server 被跳过重编、新 dist 进不了二进制。
// 这里显式声明 dist 目录变化即触发 build script 重跑与 crate 重编。
//
// 构建指纹(KEDAI_DIST_HASH / KEDAI_BUILD_TIME)的用途:
// 测试版(kedai-server.exe)与便携版(Kedai.exe)是两条独立编译链,各自把编译那一刻
// 的 web/dist 冻结进二进制。两端链接同一个 kedai-server crate,同一次构建产出的指纹
// 必然一致;/api/health 暴露指纹后,两端是否同步一眼可查(见 MAINTENANCE.md)。
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    println!("cargo:rerun-if-changed=../web/dist");
    // 源码/清单变化也要重跑本脚本:KEDAI_BUILD_TIME 才会随之刷新,
    // 否则改了 Rust 代码后二进制里的构建时间仍停留在上次前端变更时。
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let dist = manifest_dir.join("../web/dist");
    let hash = fnv_hash_dir(&dist);
    println!("cargo:rustc-env=KEDAI_DIST_HASH={hash:016x}");

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=KEDAI_BUILD_TIME={now}");
}

/// 对目录内全部文件(相对路径 + 内容)做 FNV-1a 64-bit 哈希。
/// 零依赖实现;只用于构建指纹,不是安全用途。
/// dist 不存在(全新克隆、尚未构建前端)时退化为空哈希,不阻断编译。
fn fnv_hash_dir(dir: &Path) -> u64 {
    let mut files: Vec<PathBuf> = Vec::new();
    collect_files(dir, dir, &mut files);
    files.sort();

    let mut hash: u64 = 0xcbf29ce484222325;
    let mut mix = |bytes: &[u8]| {
        for b in bytes {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
    };
    for rel in files {
        // 路径分隔符统一为正斜杠,保证跨平台指纹一致
        mix(rel.to_string_lossy().replace('\\', "/").as_bytes());
        mix(&[0]);
        if let Ok(content) = fs::read(dir.join(&rel)) {
            mix(&content);
        }
        mix(&[0xff]);
    }
    hash
}

fn collect_files(base: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(base, &path, out);
        } else if let Ok(rel) = path.strip_prefix(base) {
            out.push(rel.to_path_buf());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_changes_with_content() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = env::temp_dir().join(format!("kedai-buildrs-{stamp}"));
        fs::create_dir_all(&dir).unwrap();

        fs::write(dir.join("a.txt"), b"hello").unwrap();
        let h1 = fnv_hash_dir(&dir);
        fs::write(dir.join("a.txt"), b"hello!").unwrap();
        let h2 = fnv_hash_dir(&dir);
        fs::write(dir.join("b.txt"), b"other").unwrap();
        let h3 = fnv_hash_dir(&dir);

        assert_ne!(h1, h2, "内容变化必须改变指纹");
        assert_ne!(h2, h3, "文件集合变化必须改变指纹");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hash_is_deterministic_and_missing_dir_ok() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = env::temp_dir().join(format!("kedai-buildrs-det-{stamp}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("x.txt"), b"same").unwrap();

        assert_eq!(
            fnv_hash_dir(&dir),
            fnv_hash_dir(&dir),
            "同一内容指纹必须稳定"
        );
        fs::remove_dir_all(&dir).ok();

        let missing = env::temp_dir().join(format!("kedai-buildrs-missing-{stamp}"));
        let _ = fnv_hash_dir(&missing); // 不 panic 即可
    }
}
