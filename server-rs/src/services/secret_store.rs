// 敏感值静态加密:API Key 等凭据不再以明文落盘 data/settings.json。
//
// Windows:使用 DPAPI(CryptProtectData / CryptUnprotectData,CRYPTPROTECT_LOCAL_MACHINE 未设置,
// 即绑定当前用户账户)。密文经 base64 后以 `enc:v1:` 前缀写入 JSON,只有同一 Windows 用户
// 能解密;拷走 settings.json 到别的账户/机器无法还原。
// Android:使用 AndroidKeyStore 的 AES-256-GCM(经 JNI 调 Kotlin KeystoreBridge,见
// services/keystore_android.rs)。密钥由系统生成且不可导出,密文同样带 `enc:v1:` 前缀。
// 其它平台:默认拒绝持久化非空凭据。仅当显式设置
// `KEDAI_ALLOW_INSECURE_PLAINTEXT_SECRETS=1` 时,才允许带 `plain:v1:` 前缀明文落盘。
//
// 兼容性:load 时无前缀的值视为旧版明文,原样读取并在下次 save 时尝试迁移。
// 无安全存储且未显式 opt-in 时迁移会返回明确错误,不会覆盖原文件。

/// 密文前缀(DPAPI / Android Keystore 共用同一前缀:两者都是「本机可解、外拷不可解」,
/// 存储格式对上层无差异,故沿用同一标记避免迁移逻辑分叉)
const ENC_PREFIX: &str = "enc:v1:";
/// 明文前缀(无加密后端的平台显式标记,区别于旧版无前缀明文)
const PLAIN_PREFIX: &str = "plain:v1:";

/// 加密敏感值用于持久化:空串保持空串(表示「未配置」,不加密以便 is_empty 判断)。
/// Windows 必须成功使用 DPAPI;Android 必须成功使用 Keystore;其它平台默认拒绝明文。
pub fn protect(plain: &str) -> Result<String, String> {
    if plain.is_empty() {
        return Ok(String::new());
    }
    #[cfg(windows)]
    {
        let blob = win::protect(plain.as_bytes())?;
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(blob);
        Ok(format!("{ENC_PREFIX}{b64}"))
    }
    #[cfg(target_os = "android")]
    {
        // KeystoreBridge 自行处理 base64(格式为 IV长度+IV+密文),此处直接取返回值
        let b64 = crate::services::keystore_android::encrypt(plain)?;
        Ok(format!("{ENC_PREFIX}{b64}"))
    }
    #[cfg(not(any(windows, target_os = "android")))]
    {
        if std::env::var("KEDAI_ALLOW_INSECURE_PLAINTEXT_SECRETS").as_deref() == Ok("1") {
            eprintln!("[secret_store] 警告:已显式允许明文持久化 API Key");
            return Ok(format!("{PLAIN_PREFIX}{plain}"));
        }
        Err("当前平台未配置安全凭据存储,拒绝持久化明文 API Key;如已理解风险,可显式设置 KEDAI_ALLOW_INSECURE_PLAINTEXT_SECRETS=1".to_string())
    }
}

/// 解密持久化的敏感值:
/// - `enc:v1:` → DPAPI(Windows) / Android Keystore(Android)解密;失败返回空串,
///   视为未配置,避免把密文当 Key 发给上游
/// - `plain:v1:` → 去前缀
/// - 其他(含空串)→ 旧版明文,原样返回
pub fn unprotect(stored: &str) -> String {
    if stored.is_empty() {
        return String::new();
    }
    if let Some(b64) = stored.strip_prefix(ENC_PREFIX) {
        #[cfg(windows)]
        {
            use base64::Engine;
            match base64::engine::general_purpose::STANDARD.decode(b64.trim()) {
                Ok(blob) => match win::unprotect(&blob) {
                    Ok(bytes) => return String::from_utf8_lossy(&bytes).into_owned(),
                    Err(e) => {
                        eprintln!("[secret_store] DPAPI 解密失败(换用户/换机器?请在设置页重填 API Key):{e}");
                        return String::new();
                    }
                },
                Err(e) => {
                    eprintln!("[secret_store] 密文 base64 解析失败:{e}");
                    return String::new();
                }
            }
        }
        #[cfg(target_os = "android")]
        {
            return match crate::services::keystore_android::decrypt(b64.trim()) {
                Ok(plain) => plain,
                Err(e) => {
                    // 换设备/重装应用后 Keystore 中的密钥不再存在,旧密文无法解开
                    eprintln!("[secret_store] Android Keystore 解密失败(换设备或重装应用?请在设置页重填 API Key):{e}");
                    String::new()
                }
            };
        }
        #[cfg(not(any(windows, target_os = "android")))]
        {
            let _ = b64;
            eprintln!(
                "[secret_store] 当前平台无安全凭据存储,无法解密 enc:v1 值;请在设置页重填 API Key"
            );
            return String::new();
        }
    }
    if let Some(rest) = stored.strip_prefix(PLAIN_PREFIX) {
        return rest.to_string();
    }
    // 旧版无前缀明文:原样读取,下次保存自动升级为密文
    stored.to_string()
}

/// 是否为已加密形式(用于判断旧配置是否需要升级)
pub fn is_protected(stored: &str) -> bool {
    stored.starts_with(ENC_PREFIX)
}

#[cfg(windows)]
mod win {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB,
    };

    /// DPAPI 加密(绑定当前 Windows 用户)
    pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: input 指向本调用存活的 data;output 由 API 分配,随后拷出并 LocalFree。
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
                &mut output,
            )
        };
        if ok == 0 {
            return Err(format!("CryptProtectData 失败(code {})", last_error()));
        }
        Ok(take_blob(&mut output))
    }

    /// DPAPI 解密
    pub fn unprotect(blob: &[u8]) -> Result<Vec<u8>, String> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: blob.len() as u32,
            pbData: blob.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: 同 protect。
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
                &mut output,
            )
        };
        if ok == 0 {
            return Err(format!("CryptUnprotectData 失败(code {})", last_error()));
        }
        Ok(take_blob(&mut output))
    }

    /// 拷出 API 分配的缓冲并释放,避免内存泄漏
    fn take_blob(blob: &mut CRYPT_INTEGER_BLOB) -> Vec<u8> {
        // SAFETY: API 成功时 pbData 指向 cbData 字节的有效缓冲。
        let out = unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize).to_vec() };
        // SAFETY: DPAPI 输出缓冲须用 LocalFree 释放。
        unsafe {
            LocalFree(blob.pbData as *mut _);
        }
        blob.pbData = std::ptr::null_mut();
        blob.cbData = 0;
        out
    }

    fn last_error() -> u32 {
        // SAFETY: 无参数只读调用。
        unsafe { windows_sys::Win32::Foundation::GetLastError() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 加解密往返:非空值加密后带前缀、可还原原文
    #[cfg(windows)]
    #[test]
    fn protect_unprotect_roundtrip() {
        let key = "sk-abcdef1234567890";
        let stored = protect(key).expect("Windows DPAPI 加密应成功");
        assert_ne!(stored, key, "持久化值不应等于明文");
        assert!(!stored.contains(key), "持久化值不应包含明文片段");
        assert_eq!(unprotect(&stored), key, "解密应还原原文");
    }

    /// Windows 下应真正走 DPAPI(带 enc:v1: 前缀)
    #[cfg(windows)]
    #[test]
    fn windows_uses_dpapi_prefix() {
        let stored = protect("sk-test").expect("Windows DPAPI 加密应成功");
        assert!(
            is_protected(&stored),
            "Windows 应产生 enc:v1: 密文: {stored}"
        );
    }

    /// 空串保持空串(未配置语义,is_empty 判断不被破坏)
    #[test]
    fn empty_stays_empty() {
        assert_eq!(protect("").unwrap(), "");
        assert_eq!(unprotect(""), "");
    }

    /// 其它平台(非 Windows、非 Android)默认拒绝新增明文凭据持久化。
    /// Android 已由 Keystore 承接,不在此列。
    #[cfg(not(any(windows, target_os = "android")))]
    #[test]
    fn non_windows_rejects_plaintext_by_default() {
        std::env::remove_var("KEDAI_ALLOW_INSECURE_PLAINTEXT_SECRETS");
        let error = protect("sk-test").expect_err("默认必须拒绝明文 API Key");
        assert!(error.contains("拒绝持久化明文 API Key"));
    }

    /// Android:enc:v1: 前缀语义与桌面一致(is_protected 判定不因平台分叉)
    #[test]
    fn enc_prefix_is_recognized_on_all_platforms() {
        assert!(is_protected("enc:v1:AAAA"));
        assert!(!is_protected("plain:v1:xxxx"));
        assert!(!is_protected("sk-legacy"));
    }

    /// 旧版无前缀明文可直接读取(向后兼容)
    #[test]
    fn legacy_plaintext_is_read_as_is() {
        assert_eq!(unprotect("sk-legacy-key"), "sk-legacy-key");
        assert!(!is_protected("sk-legacy-key"), "旧明文不应被判定为已加密");
    }

    /// 显式明文前缀可去壳(非 Windows 平台路径)
    #[test]
    fn plain_prefix_is_stripped() {
        assert_eq!(unprotect("plain:v1:sk-x"), "sk-x");
    }

    /// 损坏密文不返回垃圾内容,按未配置处理(避免把密文当 Key 发上游)
    #[test]
    fn corrupted_ciphertext_returns_empty() {
        assert_eq!(unprotect("enc:v1:!!!not-base64!!!"), "");
        assert_eq!(unprotect("enc:v1:AAAAAAAAAAA="), "");
    }
}
