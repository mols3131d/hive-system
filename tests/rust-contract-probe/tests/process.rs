use std::process::Command;

#[test]
fn help_and_version_are_successful_without_workspace() {
    for option in ["--help", "--version"] {
        let result = Command::new(env!("CARGO_BIN_EXE_hive-contract-probe"))
            .arg(option)
            .output()
            .unwrap();
        assert!(result.status.success());
        assert!(!result.stdout.is_empty());
    }
}

#[test]
fn conflicting_options_return_one_json_failure() {
    let result = Command::new(env!("CARGO_BIN_EXE_hive-contract-probe"))
        .args(["context", "--json", "--format", "text"])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "INVALID_ARGUMENT");
    assert!(result.stderr.is_empty());
}

#[test]
fn unimplemented_operation_is_not_success() {
    let result = Command::new(env!("CARGO_BIN_EXE_hive-contract-probe"))
        .args(["context", "--json"])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["ok"], false);
}
