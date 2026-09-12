# Add project specific ProGuard rules here.
# You can control the set of applied configuration files using the
# proguardFiles setting in build.gradle.
#
# For more details, see
#   http://developer.android.com/guide/developing/tools/proguard.html

# ============================================================================
# Kedai:JNI 按名查找的类必须原样保留(类名 + 方法名 + 方法体)。
#
# 背景:Rust 侧(server-rs/src/services/keystore_android.rs)通过
# FindClass("com/kedai/app/KeystoreBridge") 与 CallStaticMethod("encrypt"/"decrypt")
# 反射式调用本类。R8 在 release 下会重命名成员,导致 JNI 查找失败。
#
# 实测症状:release 包保存 API Key 报
#   500「设置保存失败: 调用 KeystoreBridge::encrypt 失败: Java exception was thrown」
# 而 debug 包(不混淆)完全正常 —— 属只在正式包暴露的问题。
#
# 踩坑记录:**逐成员写签名(public static java.lang.String encrypt(java.lang.String))不可靠**。
# 实测该写法下类名被保留、但方法名仍被混淆(dex 中搜得到 KeystoreBridge、
# 搜不到 encrypt/decrypt),因为 Kotlin object + @JvmStatic 的实际描述符与手写规则
# 未必逐字匹配。改用整类保留 `{ *; }` 后稳定生效。
#
# 为什么生成的 proguard-wry.pro 不够:它只保留
#   `com.kedai.app.* { native <methods>; }`
# 即「Java 声明、native 实现」的方法;本类是反向的(Kotlin 实现、被 native 调用)。
# ============================================================================
-keep class com.kedai.app.KeystoreBridge { *; }

# ============================================================================
# Kedai:同理,KedaiNative 也被 native 按名调用
# (server-rs/src/services/native_bridge_android.rs 经 jni_bridge 的
#  FindClass("com/kedai/app/KedaiNative") + CallStaticMethod 反射调用)。
# 方法:openExternal / shareFile / startKeepAlive / stopKeepAlive。
# 同样整类保留(逐成员签名的写法在 KeystoreBridge 上已被证明不可靠)。
# ============================================================================
-keep class com.kedai.app.KedaiNative { *; }

# 前台服务由清单按类名声明,混淆后系统找不到(虽非 JNI,但同样必须保留类名)
-keep class com.kedai.app.KeepAliveService { *; }

# If your project uses WebView with JS, uncomment the following
# and specify the fully qualified class name to the JavaScript interface
# class:
#-keepclassmembers class fqcn.of.javascript.interface.for.webview {
#   public *;
#}

# Uncomment this to preserve the line number information for
# debugging stack traces.
#-keepattributes SourceFile,LineNumberTable

# If you keep the line number information, uncomment this to
# hide the original source file name.
#-renamesourcefileattribute SourceFile