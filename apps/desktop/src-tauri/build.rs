fn main() {
    println!("cargo:rerun-if-env-changed=OKYTES_UPDATE_PUBLIC_KEY");
    tauri_build::build();
}
