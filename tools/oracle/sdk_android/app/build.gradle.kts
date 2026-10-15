plugins {
    id("com.android.application")
}

android {
    namespace = "dev.hidane.sdkandroid"
    compileSdk = 36
    defaultConfig {
        applicationId = "dev.hidane.sdkandroid"
        minSdk = 24
        targetSdk = 36
        versionCode = 1
        versionName = "1"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    implementation("com.google.firebase:firebase-firestore:27.0.0")
    androidTestImplementation("androidx.test:runner:1.7.0")
    androidTestImplementation("androidx.test.ext:junit:1.3.0")
}
