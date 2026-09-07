fn main() {
    for key in ["PROFILE", "OPT_LEVEL", "TARGET"] {
        let value = std::env::var(key).expect("Cargo supplies build metadata");
        println!("cargo:rustc-env=POSTLY_BENCH_{key}={value}");
    }
}
