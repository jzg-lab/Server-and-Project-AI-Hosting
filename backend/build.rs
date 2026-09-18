use std::{fs, path::Path, process::Command};

fn main() {
    let git_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.git");
    let head_path = git_dir.join("HEAD");
    println!("cargo:rerun-if-changed={}", head_path.display());
    println!(
        "cargo:rerun-if-changed={}",
        git_dir.join("packed-refs").display()
    );

    // HEAD stays constant while commits advance on a normal branch, so watch
    // the ref named by HEAD as well. This also handles branch names containing
    // path separators, such as `codex/real-host-acceptance`.
    if let Ok(head) = fs::read_to_string(&head_path)
        && let Some(reference) = head.strip_prefix("ref:").map(str::trim)
        && !reference.is_empty()
    {
        println!(
            "cargo:rerun-if-changed={}",
            git_dir.join(reference).display()
        );
    }

    let revision = Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|output| output.trim().to_owned())
        .filter(|revision| !revision.is_empty())
        .unwrap_or_else(|| "unknown".to_owned());

    println!("cargo:rustc-env=NETWORK_ATLAS_BUILD_REVISION={revision}");
}
