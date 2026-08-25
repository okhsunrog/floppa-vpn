plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.okhsunrog.floppavpn.vpn"
    compileSdk = 36

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

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    lint {
        // CI runs this; a finding should stop the build the way a clippy warning does.
        abortOnError = true
        warningsAsErrors = true
        // The one thing lint cannot know: core-ktx 1.18+ requires compileSdk 37 and AGP 9.1, so
        // "a newer version is available" is advice we have already considered and declined.
        disable += "GradleDependency"
        textReport = true
    }
}

dependencies {
    // Held to what Android Gradle plugin 8.13 can compile: core-ktx 1.18+ demands compileSdk 37
    // and AGP 9.1. appcompat matches the app module so the two do not resolve to different
    // versions of the same library inside one build.
    implementation("androidx.core:core-ktx:1.17.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation(project(":tauri-android"))
}
