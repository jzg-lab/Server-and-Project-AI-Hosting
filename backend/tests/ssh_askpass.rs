use std::process::Command;

#[test]
fn backend_binary_serves_as_an_exact_ssh_askpass_program() {
    let fixture_password = "fixture-special!?value";
    let output = Command::new(env!("CARGO_BIN_EXE_network-atlas"))
        .arg("OpenSSH password prompt")
        .env("NETWORK_ATLAS_SSH_ASKPASS_MODE", "1")
        .env("NETWORK_ATLAS_SSH_PASSWORD", fixture_password)
        .output()
        .expect("backend binary must start in SSH askpass mode");

    assert!(output.status.success());
    assert_eq!(output.stdout, fixture_password.as_bytes());
    assert!(output.stderr.is_empty());
}
