use whatsai_core::storage::{base_state, default_state_for};

#[test]
fn each_harness_gets_its_own_state_directory() {
    // Serialise env changes with the override test below.
    let _guard = ENV.lock().unwrap();
    unsafe { std::env::remove_var("WHATSAI_STATE") };
    assert_eq!(default_state_for(None).unwrap(), base_state().join("cli"));
    assert_eq!(
        default_state_for(Some("")).unwrap(),
        base_state().join("cli")
    );
    assert_eq!(
        default_state_for(Some("claude")).unwrap(),
        base_state().join("claude")
    );
    assert_eq!(
        default_state_for(Some(" codex ")).unwrap(),
        base_state().join("codex")
    );
    for bad in ["../other", "a/b", "name with space", &"x".repeat(33)] {
        assert!(default_state_for(Some(bad)).is_err(), "{bad}");
    }
}

#[test]
fn an_explicit_state_directory_overrides_the_harness() {
    let _guard = ENV.lock().unwrap();
    unsafe { std::env::set_var("WHATSAI_STATE", "/tmp/explicit-state") };
    assert_eq!(
        default_state_for(Some("claude")).unwrap(),
        std::path::PathBuf::from("/tmp/explicit-state")
    );
    unsafe { std::env::remove_var("WHATSAI_STATE") };
}

static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());
