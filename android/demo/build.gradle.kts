plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.ophymx.chapbook.demo"
    compileSdk = 36

    defaultConfig {
        applicationId = "com.ophymx.chapbook.demo"
        minSdk = 24
        targetSdk = 36
        versionCode = 1
        versionName = "0.1"
    }
    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    implementation(project(":chapbook"))
}

kotlin {
    compilerOptions { jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17) }
}

// The demo reads one book, and the repository already has books. Copy the
// fixture in at build time rather than checking a second copy of it into
// git next to the first.
val stageBook by tasks.registering(Copy::class) {
    from(rootProject.file("../fixtures/corpus/moby-dick.epub"))
    into(layout.buildDirectory.dir("staged-assets"))
    rename { "book.epub" }
}

android.sourceSets["main"].assets.srcDir(layout.buildDirectory.dir("staged-assets"))

tasks.matching { it.name.startsWith("merge") && it.name.endsWith("Assets") }
    .configureEach { dependsOn(stageBook) }
