fn main() {
    println!("cargo:rerun-if-env-changed=FININSIGHT_API_BASE");

    if std::env::var("PROFILE").as_deref() == Ok("release") {
        let api_base = std::env::var("FININSIGHT_API_BASE")
            .expect("FININSIGHT_API_BASE must be set for release builds");
        let normalized = api_base.to_ascii_lowercase();
        if api_base.trim().is_empty()
            || normalized.contains("localhost")
            || normalized.contains("127.0.0.1")
            || normalized.contains("[::1]")
        {
            panic!("FININSIGHT_API_BASE must be a non-local production URL for release builds");
        }
    }

    tauri_build::build()
}
