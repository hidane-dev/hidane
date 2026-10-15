// The Android SDK harness (tools/oracle/README.md): firebase-android-sdk's firebase-firestore in an
// instrumentation test, run in an Android emulator against an emulator on the host.
pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}
dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}
rootProject.name = "sdk_android"
include(":app")
