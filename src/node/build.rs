/// Item 49: Capture git commit hash at compile time.
fn main() {
    // Get the current git commit hash.
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output();

    let git_hash = match output {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        }
        _ => "unknown".to_string(),
    };

    println!("cargo:rustc-env=GIT_HASH={}", git_hash);

    // Re-run if the git HEAD changes.
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs/heads/");
}
