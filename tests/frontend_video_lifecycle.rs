#[test]
fn video_lifecycle_is_a_single_typed_state_machine() {
    let source = std::fs::read_to_string("frontend/src/features/video/lifecycle.ts").unwrap();
    for phase in [
        "connecting",
        "waiting_first_frame",
        "streaming",
        "no_signal",
        "device_lost",
        "device_busy",
        "error",
    ] {
        assert!(
            source.contains(&format!("'{phase}'")),
            "missing lifecycle phase {phase}"
        );
    }
    assert!(source.contains("transitionVideoLifecycle"));
    assert!(source.contains("presentVideoLifecycle"));
    assert!(source.contains("current.phase === 'no_signal'"));

    let console = std::fs::read_to_string("frontend/src/views/ConsoleView.vue").unwrap();
    assert!(console.contains("lifecyclePresentation.showSurface"));
    assert!(console.contains("lifecyclePresentation.mountSurface"));
    assert!(console.contains("lifecyclePresentation.showLoading"));
    assert!(console.contains("lifecyclePresentation.showSignalOverlay"));
    assert!(console.contains("lifecyclePresentation.showError"));
    assert!(!console.contains("v-if=\"videoLoading"));
    assert!(!console.contains("<video\n            v-show="));
    assert!(!console.contains("<canvas\n            v-show="));
}
