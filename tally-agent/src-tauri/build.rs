fn main() {
    println!("cargo:rerun-if-env-changed=FININSIGHT_API_BASE");
    tauri_build::build()
}