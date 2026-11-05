//! Executable guardrails for the pure iDRAC pilot, including negative fixtures.
use std::path::Path;
#[path = "support/pure_model.rs"]
mod pure_model;
use pure_model::violations;
fn repo_file(path: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap()
}
#[test]
fn idrac_decision_models_are_dependency_free_and_have_no_runtime_capabilities() {
    let manifest = repo_file("crates/bmc-idrac-model/Cargo.toml");
    assert!(
        !manifest.contains("dependencies"),
        "pure model must not depend on I/O or other crates"
    );
    let root = repo_file("crates/bmc-idrac-model/src/lib.rs");
    assert!(root.contains("#![no_std]"));
    assert!(root.contains("#![forbid(unsafe_code)]"));
    assert!(root.contains("#![forbid(clippy::wildcard_enum_match_arm)]"));
    for file in
        std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/bmc-idrac-model/src"))
            .unwrap()
    {
        let path = file.unwrap().path();
        assert!(
            path.is_file(),
            "model subdirectories require explicit guardrail coverage"
        );
        if path.extension().is_some_and(|extension| extension == "rs") {
            assert_eq!(
                violations(&std::fs::read_to_string(&path).unwrap()),
                Vec::<String>::new(),
                "{}",
                path.display()
            );
        }
    }
}
#[test]
fn purity_guard_rejects_io_clocks_global_state_and_mutable_public_interfaces() {
    for source in [
        "use std::net::TcpStream;", "use tokio::time::Instant;", "extern crate std;",
        "fn step(callback: fn()) { callback(); }",
        "fn step(state: &core::cell::Cell<u8>) { state.set(1); }",
        r#"extern "C" { fn side_effect(); }"#,
        "static FLAG: bool = false;", "async fn step() {}",
        "struct State; impl State { pub fn step(&mut self) {} }",
        "pub fn step(context: &mut u8) { *context += 1; }",
        "pub fn step<T: Iterator>(mut context: T) { context.next(); }",
        "fn step() { let _ = core::sync::atomic::AtomicUsize::new(1).load(core::sync::atomic::Ordering::Relaxed); }",
        "fn step() { let _ = std::time::Instant::now(); }", "fn step() { println!(\"side effect\"); }",
        "#[path = \"runtime.rs\"] mod hidden;",
    ] { assert!(!violations(source).is_empty(), "guard missed: {source}"); }
    assert!(violations(
        "struct State; impl State { pub fn step(self, event: u8) -> (Self, u8) { (self, event) } }"
    )
    .is_empty());
}
#[test]
fn pilot_backends_execute_the_pure_models_instead_of_defining_parallel_phase_enums() {
    for backend in ["idrac-avct", "idrac-legacy-media", "idrac-wsman"] {
        assert!(
            repo_file(&format!("crates/bmc-backend-{backend}/Cargo.toml"))
                .contains("bmc-idrac-model")
        );
    }
    let avct = repo_file("crates/bmc-backend-idrac-avct/src/pool.rs");
    assert!(avct.contains(".step(event)"));
    assert!(!avct.contains("enum DvcStartupPhase"));
    assert!(!avct.contains("enum DvcProfileSwitchPhase"));
    let media = repo_file("crates/bmc-backend-idrac-legacy-media/src/session.rs");
    assert!(media.contains("ReconnectEffect::Connect"));
    assert!(media.contains("DriveRefreshEvent::CountResponse"));
    assert!(!media.contains("enum DriveRefreshPhase"));
    let wsman = repo_file("crates/bmc-backend-idrac-wsman/src/lib.rs");
    assert!(wsman.contains("Authentication::default()"));
    assert!(wsman.contains("EnumerationEvent::Page"));
    assert!(!wsman.contains("for _ in 0..16"));
}

#[test]
fn every_idrac_source_file_is_in_the_reviewed_inventory() {
    use std::collections::BTreeSet;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    fn collect(root: &Path, path: &Path, files: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect(root, &path, files);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    let mut actual = BTreeSet::new();
    for path in [
        "crates/bmc-idrac-model/src",
        "crates/bmc-backend-idrac-avct/src",
        "crates/bmc-backend-idrac-legacy-media/src",
        "crates/bmc-backend-idrac-wsman/src",
        "src/bmc/backends/idrac_avct",
        "src/bmc/backends/idrac_legacy_media",
        "src/bmc/backends/idrac_wsman",
    ] {
        collect(root, &root.join(path), &mut actual);
    }
    let inventory = repo_file("docs/testing/idrac-state-machine-coverage.md");
    let listed: BTreeSet<_> = inventory
        .lines()
        .filter_map(|line| {
            line.strip_prefix("- `")
                .and_then(|line| line.split('`').next())
                .map(str::to_string)
        })
        .collect();
    assert_eq!(
        actual, listed,
        "new or moved iDRAC sources require a coverage review"
    );
}

#[test]
fn idrac_runtimes_keep_recovery_confirmation_and_input_decisions_in_models() {
    let avct = repo_file("crates/bmc-backend-idrac-avct/src/pool.rs");
    for required in [
        "PoolEvent::Acquire",
        "SessionEvent::TransportLost",
        "RecoveryEvent::Finished",
        "OperationEvent::Observation",
        "BootstrapEvent::Requested",
        "control_action(",
        "command_is_current(",
        "ConnectingGuard",
    ] {
        assert!(
            avct.contains(required),
            "missing AVCT model bridge: {required}"
        );
    }
    assert!(!avct.contains("for attempt in 0..3"));
    assert!(!avct.contains("Connecting(Arc<Notify>)"));
    assert!(!avct.contains("idrac8_default_dvc_request_sent"));
    let decoder = repo_file("crates/bmc-backend-idrac-avct/src/decoder.rs");
    assert!(decoder.contains("AssemblyEvent::Input"));
    assert!(decoder.contains("AssemblyEvent::Decoded"));
    let hid = repo_file("crates/bmc-backend-idrac-avct/src/hid.rs");
    assert!(hid.contains("HidEvent::Reset"));
    assert!(!hid.contains("enum MouseMode"));
    let media = repo_file("crates/bmc-backend-idrac-legacy-media/src/session.rs");
    for required in [
        "FramingEvent::Bytes",
        "UploadEvent::DataAcknowledged",
        "OperationEvent::Observation",
        "SessionEvent::TransportLost",
        "run_cancellable",
        ".admits(",
        "live_packet_action(",
        "ReadEvent::Written",
        "pending.model.step_at(",
    ] {
        assert!(
            media.contains(required),
            "missing AVMP model bridge: {required}"
        );
    }
    let worker = media
        .split("async fn run_inner(")
        .nth(1)
        .unwrap()
        .split("fn record_reconnect_session")
        .next()
        .unwrap();
    assert!(
        !worker.contains("read_packet("),
        "worker must retain framing state across select cancellation"
    );
    assert!(
        !worker.contains("drive_refresh_deadline = Some("),
        "device progress must not extend the refresh deadline"
    );
    let wsman = repo_file("crates/bmc-backend-idrac-wsman/src/lib.rs");
    for required in [
        "ExchangeEvent::BodyComplete",
        "OperationEvent::Observation",
        "InventoryEvent::CommitFinished",
        "InventoryEvent::Observed",
        "cached_connection.take()",
        "BootReadbackEffect::QueryActivation",
        "BootReadbackEvent::Selection",
    ] {
        assert!(
            wsman.contains(required),
            "missing WS-MAN model bridge: {required}"
        );
    }
    assert!(
        !wsman.contains("verify_wsman_one_time_selection"),
        "selection verification must not detach from its operation"
    );
}
