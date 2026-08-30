buildscript {
    repositories {
        google()
        mavenCentral()
    }
    dependencies {
        classpath("com.android.tools.build:gradle:9.3.1")
        classpath("org.jetbrains.kotlin:kotlin-gradle-plugin:2.2.10")
    }
}

allprojects {
    repositories {
        google()
        mavenCentral()
    }
}

subprojects {
    // These projects compile source shipped by external Tauri crates, whose released templates
    // still use deprecated Android compatibility APIs. Ours is held to the opposite standard —
    // `just lint-kotlin` fails the build on a warning — so only the others are silenced.
    if (name != "tauri-plugin-vpn") {
        tasks.withType<org.jetbrains.kotlin.gradle.tasks.KotlinJvmCompile>().configureEach {
            compilerOptions.suppressWarnings.set(true)
        }
    }
}

tasks.register("clean").configure {
    delete("build")
}

