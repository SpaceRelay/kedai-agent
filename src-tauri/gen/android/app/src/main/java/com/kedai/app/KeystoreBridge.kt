package com.kedai.app

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import androidx.annotation.Keep
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Android Keystore 桥接(供 Rust 侧 secret_store 经 JNI 调用)。
 *
 * 职责:用 AndroidKeyStore 中的 AES-256 密钥对 API Key 做 AES-GCM 加解密。
 * 密钥由系统生成并驻留在 Keystore 内,应用进程拿不到密钥字节(不可导出),
 * 因此即便 settings.json 或数据目录被拷走,也无法在别的设备上解密。
 *
 * 密文格式(返回/接收 base64):[1 字节 IV 长度][IV][GCM 密文+标签]
 * 存储该长度而非写死 12,避免不同实现下 IV 长度变化导致解密失败。
 *
 * Rust 侧经 JNI 调用静态方法 encrypt/decrypt(见 server-rs/src/services/keystore_android.rs)。
 *
 * ⚠️ 混淆约束:本类是**被 native 调用**的一方(Rust 按字符串名 FindClass/FindMethod),
 * release 的 R8 若重命名它或它的方法,JNI 查找会在运行时失败(且只在正式包暴露)。
 * 双保险:
 *   1) 类与成员上加 @Keep(androidx 注解,R8 识别后强制保留);
 *   2) app/proguard-rules.pro 里 `-keep class com.kedai.app.KeystoreBridge { *; }`。
 * 修改本类名/方法名时,必须同步改 Rust 侧常量与 proguard 规则。
 */
@Keep
object KeystoreBridge {
    private const val ALIAS = "kedai.api_key.v1"
    private const val TRANSFORM = "AES/GCM/NoPadding"
    private const val TAG_BITS = 128

    /** 取已有密钥;首次调用时生成并写入 AndroidKeyStore。 */
    private fun key(): SecretKey {
        val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        val entry = store.getEntry(ALIAS, null)
        if (entry is KeyStore.SecretKeyEntry) return entry.secretKey

        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        generator.init(
            KeyGenParameterSpec.Builder(
                ALIAS,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
            )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                // 不要求用户认证:API Key 需在后台任务(无交互)时也能解密
                .setUserAuthenticationRequired(false)
                .build()
        )
        return generator.generateKey()
    }

    /** 加密明文,返回 base64(IV 长度 + IV + 密文)。 */
    @JvmStatic
    @Keep
    fun encrypt(plain: String): String {
        val cipher = Cipher.getInstance(TRANSFORM)
        cipher.init(Cipher.ENCRYPT_MODE, key())
        val cipherText = cipher.doFinal(plain.toByteArray(Charsets.UTF_8))
        val iv = cipher.iv
        val out = ByteArray(1 + iv.size + cipherText.size)
        out[0] = iv.size.toByte()
        System.arraycopy(iv, 0, out, 1, iv.size)
        System.arraycopy(cipherText, 0, out, 1 + iv.size, cipherText.size)
        return Base64.encodeToString(out, Base64.NO_WRAP)
    }

    /** 解密 base64(IV 长度 + IV + 密文),失败抛异常由 Rust 侧转成错误。 */
    @JvmStatic
    @Keep
    fun decrypt(encoded: String): String {
        val all = Base64.decode(encoded, Base64.NO_WRAP)
        if (all.isEmpty()) throw IllegalArgumentException("密文为空")
        val ivLength = all[0].toInt()
        if (ivLength <= 0 || 1 + ivLength >= all.size) {
            throw IllegalArgumentException("密文格式非法(IV 长度 $ivLength / 总长 ${all.size})")
        }
        val iv = all.copyOfRange(1, 1 + ivLength)
        val cipherText = all.copyOfRange(1 + ivLength, all.size)
        val cipher = Cipher.getInstance(TRANSFORM)
        cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(TAG_BITS, iv))
        return String(cipher.doFinal(cipherText), Charsets.UTF_8)
    }
}
