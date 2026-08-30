import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.okhsunrog.floppavpn.vpn"
    compileSdk = 37

    defaultConfig {
        minSdk = 24

        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        consumerProguardFiles("consumer-rules.pro")
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
        }
    }

    // 17, not the template's 1.8. The androidx libraries this module inlines from are built for
    // JVM 11, and Kotlin refuses to inline higher bytecode into lower ("Cannot inline bytecode
    // built with JVM target 11"). Inlining the other way is fine, which is why the Tauri modules
    // can stay at 1.8 while ours do not have to.
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    lint {
        // CI runs this; a finding should stop the build the way a clippy warning does.
        abortOnError = true
        warningsAsErrors = true
        // Two checks lint cannot judge from inside a library module, both verified on a device:
        //
        // ForegroundServicePermission wants SCHEDULE_EXACT_ALARM or USE_EXACT_ALARM alongside
        // `systemExempted`. That is one exemption path; being a VpnService is another, and it is
        // ours — the service starts, and the system's own always-on and lockdown toggles drive it.
        // Asking for an alarm permission we have no use for to quiet a check would be worse.
        //
        // QueryPermissionsNeeded wants QUERY_ALL_PACKAGES for the split-tunnelling app list. It is
        // declared, in the app manifest, because that is where app-level permissions belong and
        // manifest merging unions the two — which a library module linted on its own cannot see.
        disable += listOf("ForegroundServicePermission", "QueryPermissionsNeeded")
        textReport = true
    }
}

dependencies {
    // appcompat matches the app module so the two do not resolve to different versions of the
    // same library inside one build.
    implementation("androidx.core:core-ktx:1.19.0")
    implementation("androidx.appcompat:appcompat:1.8.0")
    implementation(project(":tauri-android"))
}

kotlin {
    compilerOptions {
        jvmTarget = JvmTarget.JVM_17
    }
}
