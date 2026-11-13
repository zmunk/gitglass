use std::{
    fs,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

fn invoke(config: Option<&str>, args: &[&str]) -> std::process::Output {
    let root = std::env::temp_dir().join(format!(
        "gitglass-default-{}-{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(root.join("gitglass")).unwrap();
    if let Some(contents) = config {
        fs::write(root.join("gitglass/config.toml"), contents).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_gitglass"))
        .env("XDG_CONFIG_HOME", &root)
        .args(args)
        .output()
        .unwrap();
    fs::remove_dir_all(root).unwrap();
    output
}

#[test]
fn no_default_shows_help() {
    for config in [None, Some("")] {
        let output = invoke(config, &[]);
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Usage:"));
    }
}

#[test]
fn invalid_default_is_reported() {
    let output = invoke(Some("default_command = 'unknown'"), &[]);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("Cannot load gitglass configuration"));
    assert!(error.contains("config.toml"));
}

#[test]
fn explicit_help_bypasses_default_and_config_errors() {
    for config in ["default_command = 'status'", "not valid toml"] {
        for flag in ["help", "--help", "-h"] {
            let output = invoke(Some(config), &[flag]);
            assert!(output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("Usage:"));
        }
    }
}

#[test]
fn viewers_reject_unknown_or_duplicate_inline_options() {
    for command in ["status", "branch", "log"] {
        for args in [
            vec![command, "--unknown"],
            vec![command, "--inline", "--inline"],
            vec![command, "--inline", "extra"],
            vec![command, "--inline", "--fullscreen"],
            vec![command, "--fullscreen", "--fullscreen"],
        ] {
            let output = invoke(None, &args);
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains(&format!(
                "Usage: gitglass {command} [--inline | --fullscreen]"
            )));
        }
    }
}

#[test]
fn bare_mode_flags_apply_to_the_configured_default() {
    for command in ["status", "log", "branch"] {
        let config = format!("default_command = '{command}'");
        let output = invoke(Some(&config), &["--fullscreen", "--inline"]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(&format!(
            "Usage: gitglass {command} [--inline | --fullscreen]"
        )));
    }
}

#[test]
fn bare_mode_flags_without_a_default_explain_how_to_proceed() {
    for flag in ["--inline", "--fullscreen"] {
        let output = invoke(None, &[flag]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("No default_command configured"));
    }
}

#[test]
fn explicit_command_takes_precedence() {
    let output = invoke(Some("default_command = 'status'"), &["diff"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("diff requires exactly one file argument")
    );
}
