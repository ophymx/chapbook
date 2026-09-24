plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.ophymx.chapbook"
    compileSdk = 36

    defaultConfig {
        minSdk = 24
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    // cargo-ndk writes straight into this layout, so the Rust build output
    // is the source of truth and nothing is copied by hand. See
    // `../tools/build-jni.sh`.
    sourceSets["main"].jniLibs.srcDirs("src/main/jniLibs")
}

kotlin {
    compilerOptions { jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17) }
}

// The instrumented tests are the only thing in this directory that
// *calls* the binding — `build-jni.sh` proves every `external fun` has a
// symbol, not that any of them answers correctly. They run on a device or
// emulator (`./gradlew :chapbook:connectedDebugAndroidTest`) because the
// native library links `libjnigraphics`, which no JVM on a desk has.
dependencies {
    androidTestImplementation("androidx.test:runner:1.6.2")
    androidTestImplementation("androidx.test.ext:junit:1.2.1")
    androidTestImplementation("junit:junit:4.13.2")
}

// The tests drive the binding against the repository's own fixtures,
// staged into the test APK's assets at build time rather than copied
// into git a second time — the demo does the same for its book.
val stageTestFixtures by tasks.registering(Copy::class) {
    from(rootProject.file("../fixtures")) {
        include("opds/navigation.atom.xml", "opds/acquisition.atom.xml", "opds/acquisition-sync.atom.xml", "epub/minimal.epub")
    }
    into(layout.buildDirectory.dir("staged-test-assets"))
}

android.sourceSets["androidTest"].assets.srcDir(layout.buildDirectory.dir("staged-test-assets"))

tasks.matching { it.name.startsWith("merge") && it.name.endsWith("AndroidTestAssets") }
    .configureEach { dependsOn(stageTestFixtures) }
