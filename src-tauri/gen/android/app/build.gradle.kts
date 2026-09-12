import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("rust")
}

val tauriProperties = Properties().apply {
    val propFile = file("tauri.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

// Kedai: release 签名配置。
// 密钥与口令从 gradle.properties 读取(该文件含口令,不应提交仓库;
// 也可改用环境变量以便 CI)。缺省时回落到无签名 release 构建,
// 使「只跑 debug 验证」的流程不因缺密钥而中断。
val keystoreProps = Properties().apply {
    val f = rootProject.file("keystore.properties")
    if (f.exists()) f.inputStream().use { load(it) }
}
fun ksProp(key: String, env: String): String? =
    keystoreProps.getProperty(key) ?: System.getenv(env)

android {
    compileSdk = 36
    namespace = "com.kedai.app"
    defaultConfig {
        manifestPlaceholders["usesCleartextTraffic"] = "false"
        applicationId = "com.kedai.app"
        minSdk = 24
        targetSdk = 36
        versionCode = tauriProperties.getProperty("tauri.android.versionCode", "1").toInt()
        versionName = tauriProperties.getProperty("tauri.android.versionName", "1.0")
    }
    buildTypes {
        getByName("debug") {
            manifestPlaceholders["usesCleartextTraffic"] = "true"
            isDebuggable = true
            isJniDebuggable = true
            isMinifyEnabled = false
            // Kedai: 打包时剥离 Rust .so 的调试符号。
            // 未 strip 的 debug 版 libkedai_desktop_lib.so 达 386MB(Release 仅 33MB),
            // 会让单 ABI 的 APK 膨胀到 780MB,每次安装/迭代都要等数分钟。
            // 需要 ndk-stack 级 JNI 调试时,把这两行注释掉即可换回带符号版本。
            packaging {
                jniLibs.keepDebugSymbols.clear()
            }
        }
        getByName("release") {
            isMinifyEnabled = true
            proguardFiles(
                *fileTree(".") { include("**/*.pro") }
                    .plus(getDefaultProguardFile("proguard-android-optimize.txt"))
                    .toList().toTypedArray()
            )
            // release 必须允许 loopback 明文:内嵌 axum 服务在 http://127.0.0.1:<port>/
            // 提供前端与 API,release 默认值 false 会让正式包白屏。
            // 真正的明文放行范围由 network_security_config.xml 限制为 loopback,
            // 这里只是解除 manifest 层的全局禁令。
            manifestPlaceholders["usesCleartextTraffic"] = "true"

            // 有密钥则签名;无密钥时保留 unsigned 产物(便于纯验证流程)
            val storePath = ksProp("storeFile", "KEDAI_KEYSTORE_FILE")
            if (storePath != null) {
                signingConfig = signingConfigs.create("release") {
                    storeFile = file(storePath)
                    storePassword = ksProp("storePassword", "KEDAI_KEYSTORE_PASSWORD")
                    keyAlias = ksProp("keyAlias", "KEDAI_KEY_ALIAS")
                    keyPassword = ksProp("keyPassword", "KEDAI_KEY_PASSWORD")
                }
            }
        }
    }
    kotlinOptions {
        jvmTarget = "1.8"
    }
    buildFeatures {
        buildConfig = true
    }
}

rust {
    rootDirRel = "../../../"
}

dependencies {
    implementation("androidx.webkit:webkit:1.14.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.activity:activity-ktx:1.10.1")
    implementation("com.google.android.material:material:1.12.0")
    implementation("androidx.lifecycle:lifecycle-process:2.10.0")
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.4")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.0")
}

apply(from = "tauri.build.gradle.kts")