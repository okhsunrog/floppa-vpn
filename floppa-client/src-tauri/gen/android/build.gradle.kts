import com.android.build.api.dsl.ApplicationExtension
import com.android.build.api.dsl.LibraryExtension
import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.tasks.KotlinJvmCompile

buildscript {
    repositories {
        google()
        mavenCentral()
    }
    dependencies {
        classpath("com.android.tools.build:gradle:8.13.2")
        classpath("org.jetbrains.kotlin:kotlin-gradle-plugin:2.1.21")
    }
}

allprojects {
    repositories {
        google()
        mavenCentral()
    }
}

subprojects {
    afterEvaluate {
        plugins.withId("com.android.application") {
            extensions.configure<ApplicationExtension> {
                compileOptions {
                    sourceCompatibility = JavaVersion.VERSION_17
                    targetCompatibility = JavaVersion.VERSION_17
                }
            }
        }
        plugins.withId("com.android.library") {
            extensions.configure<LibraryExtension> {
                compileOptions {
                    sourceCompatibility = JavaVersion.VERSION_17
                    targetCompatibility = JavaVersion.VERSION_17
                }
                if (project.name != "tauri-plugin-vpn") {
                    defaultConfig.consumerProguardFiles.removeAll { !it.exists() }
                }
            }
        }

        tasks.withType<KotlinJvmCompile>().configureEach {
            compilerOptions.jvmTarget.set(JvmTarget.JVM_17)

            // These projects compile source shipped by external Tauri crates. Their
            // current releases still use deprecated Android compatibility APIs.
            if (project.name != "tauri-plugin-vpn") {
                compilerOptions.suppressWarnings.set(true)
            }
        }
    }
}

tasks.register("clean").configure {
    delete("build")
}
