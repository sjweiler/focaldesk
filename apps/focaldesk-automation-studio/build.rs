fn main() {
    #[cfg(feature = "desktop")]
    {
        for path in [
            "frontend/src",
            "frontend/index.html",
            "frontend/package-lock.json",
            "frontend/vite.config.ts",
        ] {
            println!("cargo:rerun-if-changed={path}");
        }
        let status = std::process::Command::new("npm")
            .args(["run", "build", "--prefix", "frontend"])
            .status().expect("Node.js and npm are required; run npm ci --prefix apps/focaldesk-automation-studio/frontend");
        assert!(status.success(), "Automation Studio frontend build failed");
        tauri_build::build();
    }
}
