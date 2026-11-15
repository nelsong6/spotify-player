fn main() {
    for name in ["VERSION", "GITHUB_REPOSITORY", "GITHUB_SERVER_URL"] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    if let Ok(version) = std::env::var("VERSION") {
        println!("cargo:rustc-env=SPT_BUILD_VERSION={version}");
        let built = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("build time follows the Unix epoch")
            .as_secs();
        println!("cargo:rustc-env=SPT_BUILD_TIME={built}");
    }
}
