//! Executable purity, production bridge, and reviewed-inventory requirements.
use std::{collections::BTreeSet, path::Path};
#[path = "support/pure_model.rs"]
mod pure_model;
fn read(path: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap()
}
#[test]
fn backend_model_is_dependency_free_and_pure() {
    assert!(!read("crates/bmc-backend-model/Cargo.toml").contains("dependencies"));
    let lib = read("crates/bmc-backend-model/src/lib.rs");
    for requirement in [
        "#![no_std]",
        "#![forbid(unsafe_code)]",
        "#![forbid(clippy::wildcard_enum_match_arm)]",
    ] {
        assert!(lib.contains(requirement));
    }
    for entry in std::fs::read_dir(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/bmc-backend-model/src"),
    )
    .unwrap()
    {
        let path = entry.unwrap().path();
        assert!(
            path.is_file(),
            "nested models require explicit guardrail review"
        );
        if path.extension().is_some_and(|extension| extension == "rs") {
            assert_eq!(
                pure_model::violations(&std::fs::read_to_string(&path).unwrap()),
                Vec::<String>::new(),
                "{}",
                path.display()
            );
        }
    }
}
#[test]
fn protocol_shells_execute_the_models() {
    for backend in ["mahogany", "vnc", "redfish", "ipmi", "wol"] {
        assert!(
            read(&format!("crates/bmc-backend-{backend}/Cargo.toml")).contains("bmc-backend-model")
        );
    }
    for (file, required, forbidden) in [
        (
            "vnc/src/pool.rs",
            vec![
                "acquire(slot)",
                "SessionEvent::Established",
                "ConnectingGuard",
                "record.step(",
                "ClientCommand::Input(bytes)",
            ],
            vec!["Connecting(Arc<Notify>)", "connected: AtomicBool"],
        ),
        (
            "vnc/src/hid.rs",
            vec!["input.clone().step(", "Event::Reset"],
            vec!["enum MouseMode", "HashSet", "keys.drain()"],
        ),
        (
            "vnc/src/protocol.rs",
            vec!["rfb_minor(", "negotiation.step(", "needs_security_result("],
            vec!["match security_type as u8"],
        ),
        (
            "mahogany/src/pool.rs",
            vec![
                "acquire(snapshot)",
                "ConnectWaiter",
                "current.observe(",
                "state.permission(",
                "record.step(",
                "negotiation.step(",
            ],
            vec!["skipping byte", "has_full_control: Arc<AtomicBool>"],
        ),
        (
            "mahogany/src/hid.rs",
            vec![
                "input.clone()",
                ".step(event,",
                "tcp_tx.send(messages)",
                "Event::Reset",
            ],
            vec!["enum MouseMode"],
        ),
        (
            "mahogany/src/msd.rs",
            vec![
                "model.step(",
                "inner.attachment.step(",
                "AttachmentEvent::Uncertain",
            ],
            vec!["pub enum StorageShareStatus"],
        ),
        (
            "mahogany/src/storage_server.rs",
            vec![
                "model.step(Event::Setup",
                "Event::Command",
                "Action::Reject",
            ],
            vec![],
        ),
        (
            "redfish/src/client.rs",
            vec![
                "pages.enter(",
                "pages.finish(",
                "task.step(",
                "State::Confirmed",
            ],
            vec!["for _ in 0..30", "for _ in 0..MAX_COLLECTION_PAGES"],
        ),
        (
            "redfish/src/boot.rs",
            vec![
                "Override::from_wire(",
                "Override::Unknown",
                "Override::Disabled",
            ],
            vec!["targets.is_none_or(", "modes.is_none_or("],
        ),
        (
            "ipmi/src/client.rs",
            vec![
                "Event::Completion(",
                "impl Drop for SessionClient",
                ".retries(1)",
            ],
            vec![".retries(2)"],
        ),
        (
            "wol/src/lib.rs",
            vec![
                "State::Sending.step(",
                "State::Dispatched",
                "!cleaned.is_ascii()",
            ],
            vec![],
        ),
    ] {
        let source = read(&format!("crates/bmc-backend-{file}"));
        for marker in required {
            assert!(
                source
                    .split_whitespace()
                    .collect::<String>()
                    .contains(&marker.split_whitespace().collect::<String>()),
                "missing model bridge {file}: {marker}"
            );
        }
        for marker in forbidden {
            assert!(
                !source
                    .split_whitespace()
                    .collect::<String>()
                    .contains(&marker.split_whitespace().collect::<String>()),
                "parallel state bookkeeping {file}: {marker}"
            );
        }
    }
}
#[test]
fn every_remaining_backend_file_has_a_reviewed_owner() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    fn collect(root: &Path, path: &Path, files: &mut BTreeSet<String>) {
        if path.is_dir() {
            for entry in std::fs::read_dir(path).unwrap() {
                collect(root, &entry.unwrap().path(), files);
            }
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.insert(
                path.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    let mut actual = BTreeSet::new();
    for backend in [
        "model", "support", "mahogany", "vnc", "redfish", "ipmi", "wol",
    ] {
        collect(
            root,
            &root.join(format!("crates/bmc-backend-{backend}/src")),
            &mut actual,
        );
    }
    for adapter in ["mahogany", "vnc", "redfish", "ipmi", "wol.rs"] {
        collect(
            root,
            &root.join(format!("src/bmc/backends/{adapter}")),
            &mut actual,
        );
    }
    let document = read("docs/testing/backend-state-machine-coverage.md");
    let listed = document
        .lines()
        .filter_map(|line| {
            line.strip_prefix("- `")
                .and_then(|line| line.split('`').next())
                .map(str::to_string)
        })
        .collect();
    assert_eq!(
        actual, listed,
        "new or moved backend sources require a coverage review"
    );
}

/// Inspect syntax, not source spelling, so formatting and renamed comments cannot
/// hide a newly introduced protocol phase enum. Codecs and resource owners remain runtime code.
#[test]
fn native_workers_cannot_define_parallel_phase_enums() {
    use syn::visit::{self, Visit};
    struct Phases {
        violations: Vec<String>,
    }
    impl<'ast> Visit<'ast> for Phases {
        fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
            if module.attrs.iter().any(|attr| {
                attr.path().is_ident("cfg")
                    && attr
                        .parse_args::<syn::Path>()
                        .is_ok_and(|path| path.is_ident("test"))
            }) {
                return;
            }
            visit::visit_item_mod(self, module);
        }
        fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
            let name = item.ident.to_string();
            // This is a published video observation, not the worker's transition model.
            if name != "AvctVideoState"
                && ["State", "Phase", "Lifecycle"]
                    .iter()
                    .any(|suffix| name.ends_with(suffix))
            {
                self.violations.push(name);
            }
        }
    }
    fn check(path: &Path) {
        if path.is_dir() {
            for entry in std::fs::read_dir(path).unwrap() {
                check(&entry.unwrap().path());
            }
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let tree = syn::parse_file(&std::fs::read_to_string(path).unwrap()).unwrap();
            let mut visitor = Phases {
                violations: Vec::new(),
            };
            visitor.visit_file(&tree);
            assert!(
                visitor.violations.is_empty(),
                "{} defines runtime phases {:?}; move decisions into a model",
                path.display(),
                visitor.violations
            );
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for backend in [
        "mahogany",
        "vnc",
        "ipmi",
        "redfish",
        "wol",
        "idrac-avct",
        "idrac-legacy-media",
        "idrac-wsman",
    ] {
        check(&root.join(format!("crates/bmc-backend-{backend}/src")));
    }
}
