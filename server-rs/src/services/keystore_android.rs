//! Android Keystore 桥接(JNI):API Key 加密存储。
//!
//! 目的:AndroidKeyStore 中的 AES-256 密钥由系统生成且**不可导出**(应用进程拿不到
//! 密钥字节),因此 settings.json 或数据目录被拷走后,密文在其它设备上无法解密。
//! 这是 Android 上唯一等价于 Windows DPAPI 的持久化密钥方案。
//!
//! 为什么不直接在 Rust 里调 javax.crypto:
//! Keystore/Cipher/GCM 这套 Java API 从 Rust 逐个 JNI 调用会引入十几处胶水与类型转换,
//! 且极易出错;改为在 Kotlin 侧封装 `KeystoreBridge.encrypt/decrypt` 两个静态方法,
//! Rust 只做两次字符串进出,复杂度与出错面都最小。
//!
//! JNI 的 VM/类缓存与调用细节统一在 [`super::jni_bridge`](类加载器与 VM 获取两个
//! 必须注意的约束见该模块文档)。
//!
//! ⚠️ 混淆约束:KeystoreBridge 是被 native 按名调用的一方,R8 重命名会导致 release
//! 包运行时查找失败。双保险:`@Keep` 注解 + proguard-rules.pro 整类 keep。
#![cfg(target_os = "android")]

use super::jni_bridge::call_string_static;

/// KeystoreBridge 的全限定类名(JNI 用 `/` 分隔)
const BRIDGE_CLASS: &str = "com/kedai/app/KeystoreBridge";

/// 加密明文,返回 base64(IV 长度 + IV + GCM 密文)。空串由调用方短路,不入此函数。
pub fn encrypt(plain: &str) -> Result<String, String> {
    call_string_static(BRIDGE_CLASS, "encrypt", plain)
}

/// 解密 base64 密文。失败返回 Err(调用方按「未配置」处理,切勿把密文当 Key 使用)。
pub fn decrypt(encoded: &str) -> Result<String, String> {
    call_string_static(BRIDGE_CLASS, "decrypt", encoded)
}
