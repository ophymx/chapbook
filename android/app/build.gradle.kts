plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

android {
    namespace = "com.ophymx.chapbook.app"
    compileSdk = 36

    defaultConfig {
        applicationId = "com.ophymx.chapbook"
        minSdk = 24
        targetSdk = 36
        versionCode = 1
        versionName = "0.1"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
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
    buildFeatures { compose = true }
}

kotlin {
    compilerOptions { jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17) }
}

// Pinned to the last releases built against compileSdk 36: everything
// after mid-2026 wants compileSdk 37 and AGP 9.1, which is a toolchain
// move for all three modules and a task of its own.
dependencies {
    implementation(project(":chapbook"))

    val compose = platform("androidx.compose:compose-bom:2026.06.01")
    implementation(compose)
    androidTestImplementation(compose)
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-core")
    implementation("androidx.compose.ui:ui-tooling-preview")
    debugImplementation("androidx.compose.ui:ui-tooling")

    implementation("androidx.activity:activity-compose:1.12.4")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.10.0")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.10.0")
    implementation("androidx.navigation:navigation-compose:2.9.8")
    implementation("androidx.core:core-ktx:1.18.0")
    implementation("io.coil-kt.coil3:coil-compose:3.5.0")
    implementation("io.coil-kt.coil3:coil-network-okhttp:3.5.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.11.0")
    // The app's networking: the catalog, downloads and covers all go
    // through one client, which is where credentials are attached and
    // where the device's trust store is consulted. No Rust TLS ships.
    implementation("com.squareup.okhttp3:okhttp:5.4.0")
    // A transfer that must survive the app being suspended is a job.
    implementation("androidx.work:work-runtime-ktx:2.10.5")

    androidTestImplementation("androidx.test:runner:1.6.2")
    androidTestImplementation("androidx.work:work-testing:2.10.5")
    androidTestImplementation("com.squareup.okhttp3:mockwebserver3:5.4.0")
    androidTestImplementation("androidx.test.ext:junit:1.2.1")
    androidTestImplementation("junit:junit:4.13.2")
}

// The model layer is tested against the repository's own books, staged
// into the test APK the way the library module stages its fixtures.
val stageTestFixtures by tasks.registering(Copy::class) {
    from(rootProject.file("../fixtures")) {
        include("epub/minimal.epub", "epub/series.epub", "cbz/minimal.cbz", "opds/navigation.atom.xml", "opds/acquisition.atom.xml", "opds/acquisition-sync.atom.xml", "opds/authentication.opds-auth.json")
    }
    into(layout.buildDirectory.dir("staged-test-assets"))
}

android.sourceSets["androidTest"].assets.srcDir(layout.buildDirectory.dir("staged-test-assets"))

tasks.matching { it.name.startsWith("merge") && it.name.endsWith("AndroidTestAssets") }
    .configureEach { dependsOn(stageTestFixtures) }
