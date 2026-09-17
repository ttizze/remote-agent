plugins { id("com.android.library") }
android {
    namespace = "com.termux.view"
    compileSdk = 37
    defaultConfig { minSdk = 37 }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}
dependencies { implementation("androidx.annotation:annotation:1.9.1") }
