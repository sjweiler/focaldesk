fn main() {
    #[cfg(feature = "desktop")]
    {
        for path in [
            "frontend/src",
            "frontend/index.html",
            "frontend/package.json",
            "frontend/package-lock.json",
            "frontend/vite.config.ts",
            "frontend/svelte.config.js",
            "frontend/tsconfig.json",
        ] {
            println!("cargo:rerun-if-changed={path}");
        }
        let status = std::process::Command::new("npm")
            .args(["run", "build", "--prefix", "frontend"])
            .status()
            .expect("Building the desktop UI requires Node.js and npm. Run npm ci --prefix apps/focalmail/frontend first.");
        assert!(
            status.success(),
            "FocalMail frontend build failed. Run npm ci --prefix apps/focalmail/frontend and npm run check --prefix apps/focalmail/frontend."
        );
        tauri_build::build();
    }
}
