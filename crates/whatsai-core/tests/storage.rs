use whatsai_core::storage::{base_state, default_state, default_state_for};

#[test]
fn one_state_directory_per_person_unless_overridden() {
    let _guard = ENV.lock().unwrap();
    unsafe { std::env::remove_var("WHATSAI_STATE") };
    unsafe { std::env::remove_var("WHATSAI_PROFILE") };
    assert_eq!(default_state(), base_state());
    assert!(base_state().ends_with(".local/share/whatsai"));
    unsafe { std::env::set_var("WHATSAI_STATE", "/tmp/explicit-state") };
    assert_eq!(
        default_state(),
        std::path::PathBuf::from("/tmp/explicit-state")
    );
    assert_eq!(
        default_state_for(Some("alice")).unwrap(),
        std::path::PathBuf::from("/tmp/explicit-state"),
        "an explicit directory beats a profile"
    );
    unsafe { std::env::remove_var("WHATSAI_STATE") };
}

#[test]
fn profiles_separate_people_who_share_an_os_account() {
    let _guard = ENV.lock().unwrap();
    unsafe { std::env::remove_var("WHATSAI_STATE") };
    assert_eq!(
        default_state_for(Some("alice")).unwrap(),
        base_state().join("profiles").join("alice")
    );
    assert_eq!(default_state_for(Some("  ")).unwrap(), base_state());
    assert_eq!(default_state_for(None).unwrap(), base_state());
    for bad in ["../x", "a/b", "a b", &"x".repeat(33)] {
        assert!(default_state_for(Some(bad)).is_err(), "{bad}");
    }
    unsafe { std::env::set_var("WHATSAI_PROFILE", "bob") };
    assert_eq!(default_state(), base_state().join("profiles").join("bob"));
    unsafe { std::env::remove_var("WHATSAI_PROFILE") };
}

static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());
