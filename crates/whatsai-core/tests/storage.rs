use whatsai_core::storage::{base_state, default_state};

#[test]
fn one_state_directory_per_person_unless_overridden() {
    let _guard = ENV.lock().unwrap();
    unsafe { std::env::remove_var("WHATSAI_STATE") };
    assert_eq!(default_state(), base_state());
    assert!(base_state().ends_with(".local/share/whatsai"));
    unsafe { std::env::set_var("WHATSAI_STATE", "/tmp/explicit-state") };
    assert_eq!(
        default_state(),
        std::path::PathBuf::from("/tmp/explicit-state")
    );
    unsafe { std::env::remove_var("WHATSAI_STATE") };
}

static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());
