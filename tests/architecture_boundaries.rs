use std::fs;
use std::path::{Path, PathBuf};

fn repo_file(path: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path))
        .unwrap_or_else(|err| panic!("failed to read {path}: {err}"))
}

fn runtime_mentions_identifier(source: &str, identifier: &str) -> bool {
    use syn::visit::Visit;
    struct RuntimeIdentifiers<'a> {
        identifier: &'a str,
        found: bool,
    }
    impl<'ast> Visit<'ast> for RuntimeIdentifiers<'_> {
        fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
            if module.attrs.iter().any(|attribute| {
                attribute.path().is_ident("cfg")
                    && matches!(attribute.parse_args::<syn::Meta>(),
                        Ok(syn::Meta::Path(path)) if path.is_ident("test"))
            }) {
                return;
            }
            syn::visit::visit_item_mod(self, module);
        }
        fn visit_ident(&mut self, identifier: &'ast syn::Ident) {
            self.found |= identifier == self.identifier;
        }
    }
    let syntax = syn::parse_file(source).expect("architecture source must parse");
    let mut visitor = RuntimeIdentifiers {
        identifier,
        found: false,
    };
    visitor.visit_file(&syntax);
    visitor.found
}

#[test]
fn runtime_identifier_check_excludes_test_fixtures_but_checks_production() {
    let fixture = "#[cfg(test)] mod tests { struct Config { backend_params: () } }";
    assert!(!runtime_mentions_identifier(fixture, "backend_params"));
    assert!(runtime_mentions_identifier(
        &format!("{fixture} fn configure() {{ let backend_params = (); }}"),
        "backend_params"
    ));
}

fn rust_files_under(path: &str) -> Vec<PathBuf> {
    fn visit(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", dir.display()))
        {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }

    let mut files = Vec::new();
    visit(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join(path),
        &mut files,
    );
    files
}

fn files_under(path: &str) -> Vec<PathBuf> {
    fn visit(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", dir.display()))
        {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(&path, out);
            } else {
                out.push(path);
            }
        }
    }

    let mut files = Vec::new();
    visit(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join(path),
        &mut files,
    );
    files
}

#[test]
fn planned_features_are_not_exposed_as_live_controls() {
    let frontend_api = repo_file("frontend/src/api/config.ts");
    for forbidden in [
        "extensionsApi",
        "rustdeskConfigApi",
        "rtspConfigApi",
        "redfishConfigApi",
        "restart: ()",
        "'/system/restart'",
        "'/update/",
        "'/extensions",
    ] {
        assert!(
            !frontend_api.contains(forbidden),
            "frontend config API must not expose unimplemented planned feature `{forbidden}`"
        );
    }

    let routes = repo_file("src/web/routes.rs");
    for forbidden in [
        "/devices/usb",
        "/system/restart",
        "/update/",
        "/extensions",
        "extension_update_gostc",
        "extension_update_easytier",
    ] {
        assert!(
            !routes.contains(forbidden),
            "backend routes must not mount unimplemented planned feature `{forbidden}`"
        );
    }

    let device_handler = repo_file("src/web/handlers/device.rs");
    assert!(
        !device_handler.contains("\"extensions\"")
            && !device_handler.contains("ttyd_available")
            && !device_handler.contains("rustdesk_available"),
        "device discovery must not report fake extension availability"
    );
}

#[test]
fn backend_requirements_are_typed_backend_owned_and_device_scoped() {
    let feature_mod = repo_file("src/bmc/feature/mod.rs");
    let registry = repo_file("src/bmc/feature/registry.rs");
    let requirements = repo_file("src/bmc/feature/requirements.rs");
    let boot = repo_file("src/bmc/feature/boot/mod.rs");
    let frontend_api = repo_file("frontend/src/api/config.ts");
    let requirements_sheet = repo_file("frontend/src/components/RequirementsSheet.vue");
    let sheet_content = repo_file("frontend/src/components/ui/sheet/SheetContent.vue");

    assert!(feature_mod.contains("requirements::mount(router, \"/bmc/:id/api/requirements\")"));
    assert!(!feature_mod.contains("\"idrac-boot\" => \"boot\""));
    assert!(repo_file("src/bmc/providers/boot.rs").contains("BootProviderRegistry"));
    assert!(feature_mod.contains("\"redfish\" => \"boot\" => redfish_boot"));
    assert!(registry.contains("BackendRequirementHooks"));
    assert!(registry.contains("RequirementInspectFn"));
    assert!(registry.contains("RequirementRemediateFn"));
    assert!(requirements.contains("RequirementId"));
    assert!(requirements.contains("RemediationAction"));
    assert!(requirements.contains("advertised_remediation"));
    assert!(boot.contains("BootFeatureRuntime"));
    assert!(boot.contains("Feature-owned Boot state"));
    assert!(!registry.contains("pub execute"));
    assert!(!runtime_mentions_identifier(
        &requirements,
        "backend_params"
    ));
    assert!(!frontend_api.contains("DCIM_"));
    assert!(!requirements_sheet.contains("idrac-wsman"));
    assert!(!requirements_sheet.contains("DCIM_"));
    assert!(sheet_content.contains("pointer-events-auto fixed z-[51]"));
    assert!(sheet_content.contains("fixed inset-0 z-50"));
}

#[test]
fn boot_protocol_backends_do_not_fallback_across_backend_boundaries() {
    let wsman = repo_file("src/bmc/backends/idrac_wsman/mod.rs");
    assert!(!wsman.contains("backends::ipmi"));
    assert!(!wsman.contains("redfish"));
    assert!(!wsman.contains("backends::mahogany"));
    assert!(!wsman.contains("boot_selection("));
    assert!(!wsman.contains("set_boot_selection("));

    let redfish = repo_file("src/bmc/backends/redfish/boot.rs");
    for vendor_detail in ["DCIM_", "WS-MAN", "wsman", "idrac", "iDRAC"] {
        assert!(
            !redfish.contains(vendor_detail),
            "generic Redfish boot backend must not contain vendor detail `{vendor_detail}`"
        );
    }
}

#[test]
fn boot_provider_composition_is_feature_owned() {
    let registry = repo_file("src/bmc/feature/mod.rs");
    let adapter = repo_file("src/bmc/providers/boot.rs");
    let providers = repo_file("src/bmc/feature/boot/provider.rs");

    assert!(!registry.contains("\"idrac-boot\" => \"boot\""));
    assert!(adapter.contains("BootProviderRegistry::new()"));
    assert!(adapter.contains("register_snapshot_provider(provider.id.clone()"));
    assert!(adapter.contains("register_discovery_provider("));
    assert!(adapter.contains("\"idrac-wsman\""));
    assert!(adapter.contains("bmc_backend_idrac_wsman"));
    assert!(adapter.contains("bmc_backend_ipmi"));
    assert!(providers.contains("entry_selection(&status.selection)"));
    assert!(!providers.contains("entry.target"));
    assert!(!adapter.contains("or_else(|_|"));

    for backend in ["idrac-wsman", "ipmi"] {
        let manifest = repo_file(&format!("crates/bmc-backend-{backend}/Cargo.toml"));
        assert!(!manifest.contains("bmc-backend-ipmi") || backend == "ipmi");
        assert!(!manifest.contains("bmc-backend-idrac-wsman") || backend == "idrac-wsman");
    }
}

#[test]
fn generic_boot_contract_contains_only_provider_driven_entries() {
    let contract = repo_file("crates/bmc-feature-api/src/boot.rs");
    let feature = repo_file("src/bmc/feature/boot/mod.rs");
    let provider = repo_file("src/bmc/feature/boot/provider.rs");
    let handler = repo_file("src/bmc/feature/boot/handler.rs");

    for forbidden in [
        "BootTarget",
        "BootFirmware",
        "BootEntryScope",
        "BootEfiBehavior",
        "efi_supported",
    ] {
        assert!(
            !contract.contains(forbidden),
            "generic Boot contract must not expose `{forbidden}`"
        );
    }
    for source in [&feature, &provider, &handler] {
        for forbidden in [
            "entry.target",
            "entry.firmware",
            "entry.scope",
            "selection.efi",
        ] {
            assert!(
                !source.contains(forbidden),
                "generic Boot feature must not interpret `{forbidden}`"
            );
        }
    }
    assert!(handler.contains("entry_id: Some(params.entry_id.clone())"));
    assert!(provider.contains("entry_selection(&status.selection)"));
}

#[test]
fn backend_support_is_a_protocol_neutral_leaf_crate() {
    let root_manifest = repo_file("Cargo.toml");
    let support_manifest = repo_file("crates/bmc-backend-support/Cargo.toml");
    let support_source = repo_file("crates/bmc-backend-support/src/lib.rs");

    assert!(root_manifest.contains("\"crates/bmc-backend-support\""));
    assert!(!support_manifest.contains("bmc-aggregator"));
    assert_eq!(support_manifest.matches("bmc-backend-").count(), 1);
    assert!(!support_manifest.contains("path ="));
    assert!(!support_source.contains("crate::bmc::backends"));
}

#[test]
fn protocol_backend_crates_do_not_depend_on_each_other() {
    let backends = [
        "idrac-avct",
        "idrac-legacy-media",
        "idrac-wsman",
        "ipmi",
        "mahogany",
        "redfish",
        "vnc",
        "wol",
    ];
    for backend in backends {
        let manifest = repo_file(&format!("crates/bmc-backend-{backend}/Cargo.toml"));
        assert!(
            !manifest.contains("bmc-aggregator"),
            "backend crate {backend} must not depend on the application crate"
        );
        for other in backends {
            if backend != other {
                assert!(
                    !manifest.contains(&format!("bmc-backend-{other}")),
                    "backend crate {backend} must not depend on backend crate {other}"
                );
            }
        }
    }
}

#[test]
fn feature_api_is_a_stable_leaf_contract_crate() {
    let manifest = repo_file("crates/bmc-feature-api/Cargo.toml");
    let source = repo_file("crates/bmc-feature-api/src/lib.rs");
    let power = repo_file("crates/bmc-feature-api/src/power.rs");
    let root_power = repo_file("src/bmc/feature/power/mod.rs");

    assert!(!manifest.contains("bmc-aggregator"));
    assert!(!manifest.contains("bmc-backend-"));
    assert!(!source.contains("FeatureConnection"));
    assert!(!source.contains("axum"));
    assert!(power.contains("pub trait PowerBackend"));
    let identify = repo_file("crates/bmc-feature-api/src/identify.rs");
    assert!(identify.contains("pub trait IdentifyBackend"));
    assert!(root_power.contains("pub use bmc_feature_api::power"));
    assert!(!root_power.contains("pub trait PowerBackend"));
}

#[test]
fn configuration_catalog_is_protocol_neutral_and_separate_from_runtime_registry() {
    let feature_api = repo_file("crates/bmc-feature-api/src/configuration.rs");
    let feature_api_root = repo_file("crates/bmc-feature-api/src/lib.rs");
    let registry = repo_file("src/bmc/feature/registry.rs");

    assert!(feature_api_root.contains("pub mod configuration;"));
    assert!(feature_api.contains("pub struct ConfigurationCatalog"));
    assert!(feature_api.contains("pub enum FeatureAssignmentDto"));
    assert!(feature_api.contains("pub enum SecretMutation"));
    assert!(!feature_api.contains("FeatureConnection"));
    assert!(!feature_api.contains("axum"));
    assert!(!feature_api.contains("frontend"));

    for presentation_type in [
        "ConfigurationCatalog",
        "BackendConfigDescriptor",
        "ConfigFieldDescriptor",
        "ConfigFieldKind",
    ] {
        assert!(
            !registry.contains(presentation_type),
            "runtime FeatureBackendTemplate must not own {presentation_type}"
        );
    }
}

#[test]
fn wol_protocol_is_owned_by_an_isolated_backend_crate() {
    let root_manifest = repo_file("Cargo.toml");
    let wol_manifest = repo_file("crates/bmc-backend-wol/Cargo.toml");
    let adapter = repo_file("src/bmc/backends/wol.rs");
    let feature_mod = repo_file("src/bmc/feature/mod.rs");

    assert!(root_manifest.contains("crates/bmc-backend-wol"));
    assert!(wol_manifest.contains("bmc-backend-model"));
    assert!(!wol_manifest.contains("bmc-backend-support"));
    assert!(!wol_manifest.contains("bmc-aggregator"));
    assert!(!wol_manifest.contains("bmc-backend-ipmi"));
    assert!(adapter.contains("bmc_backend_wol::WolBackend"));
    assert!(!adapter.contains("UdpSocket"));
    assert!(!adapter.contains("impl PowerBackend"));
    assert!(feature_mod.contains("backends::wol as wol_power"));
    assert!(!feature_mod.contains("ipmi::wol"));

    let ipmi_power = repo_file("src/bmc/backends/ipmi/power.rs");
    assert!(!ipmi_power.contains("send_magic_packet"));
    assert!(!ipmi_power.contains("super::wol"));
}

#[test]
fn management_features_do_not_clone_parameters_from_other_features() {
    let registry = repo_file("src/bmc/feature/device_registry.rs");
    assert!(registry.contains("management_backend_params"));
    assert!(!registry.contains("feat_cfgs.features[\"power\"].backend_params.clone()"));
}

#[test]
fn ipmi_protocol_and_feature_implementations_are_isolated_in_their_crate() {
    let manifest = repo_file("crates/bmc-backend-ipmi/Cargo.toml");
    assert!(manifest.contains("bmc-feature-api"));
    assert!(!manifest.contains("bmc-aggregator"));
    assert!(!manifest.contains("bmc-backend-wol"));
    assert!(!manifest.contains("bmc-backend-redfish"));

    let adapters = repo_file("src/bmc/backends/mod.rs");
    assert!(adapters.contains("pub mod ipmi;"));
    assert!(!adapters.contains("pub mod general;"));
    assert!(!Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/bmc/backends/general")
        .exists());
    let ipmi = repo_file("src/bmc/backends/ipmi/mod.rs");
    for feature in ["boot", "identify", "power"] {
        assert!(ipmi.contains(&format!("pub mod {feature};")));
    }
    assert!(!ipmi.contains("#[path"));
    assert!(!adapters.contains("#[path"));

    for adapter in ["boot.rs", "identify.rs", "power.rs"] {
        let source = repo_file(&format!("src/bmc/backends/ipmi/{adapter}"));
        assert!(source.contains("bmc_backend_ipmi"));
        assert!(!source.contains("impl BootBackend"));
        assert!(!source.contains("impl IdentifyBackend"));
        assert!(!source.contains("impl PowerBackend"));
        assert!(!source.contains("send_raw("));
    }
}

#[test]
fn redfish_power_boot_and_msd_are_isolated_in_their_backend_crate() {
    let manifest = repo_file("crates/bmc-backend-redfish/Cargo.toml");
    assert!(manifest.contains("bmc-feature-api"));
    assert!(!manifest.contains("bmc-aggregator"));
    assert!(!manifest.contains("bmc-backend-ipmi"));
    assert!(!manifest.contains("bmc-backend-wol"));

    for feature in ["power", "boot", "msd"] {
        let adapter = repo_file(&format!("src/bmc/backends/redfish/{feature}.rs"));
        assert!(adapter.contains("bmc_backend_redfish"));
        assert!(!adapter.contains("impl PowerBackend"));
        assert!(!adapter.contains("impl BootBackend"));
        assert!(!adapter.contains("impl MsdBackend"));
        assert!(!adapter.contains("reqwest"));
        assert!(!adapter.contains("serde_json::Value"));
    }

    for source in ["client.rs", "power.rs", "boot.rs", "msd.rs"] {
        let implementation = repo_file(&format!("crates/bmc-backend-redfish/src/{source}"));
        assert!(!implementation.contains("FeatureConnection"));
        assert!(!implementation.contains("crate::bmc"));
        assert!(!implementation.contains("DCIM_"));
    }

    assert!(!std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/bmc/backends/redfish/client.rs")
        .exists());
}

#[test]
fn idrac_wsman_protocol_is_isolated_in_its_backend_crate() {
    let manifest = repo_file("crates/bmc-backend-idrac-wsman/Cargo.toml");
    assert!(manifest.contains("bmc-feature-api"));
    assert!(manifest.contains("bmc-backend-support"));
    assert!(!manifest.contains("bmc-aggregator"));
    assert!(!manifest.contains("bmc-backend-redfish"));
    assert!(!manifest.contains("bmc-backend-ipmi"));

    let adapter = repo_file("src/bmc/backends/idrac_wsman/mod.rs");
    assert!(adapter.contains("bmc_backend_idrac_wsman"));
    for protocol_detail in ["quick_xml", "SslConnector", "DCIM_", "impl BootBackend"] {
        assert!(!adapter.contains(protocol_detail));
    }

    let implementation = repo_file("crates/bmc-backend-idrac-wsman/src/lib.rs");
    assert!(!implementation.contains("FeatureConnection"));
    assert!(!implementation.contains("crate::bmc"));
    assert!(implementation.contains("impl BootBackend"));
}

#[test]
fn idrac_legacy_media_wire_protocol_is_isolated_in_its_backend_crate() {
    let manifest = repo_file("crates/bmc-backend-idrac-legacy-media/Cargo.toml");
    assert!(manifest.contains("bmc-feature-api"));
    assert!(manifest.contains("bmc-backend-support"));
    assert!(!manifest.contains("bmc-aggregator"));
    for backend in ["redfish", "ipmi", "wol", "wsman"] {
        assert!(!manifest.contains(&format!("bmc-backend-{backend}")));
    }

    for removed in ["apcp.rs", "protocol.rs", "session.rs"] {
        assert!(!std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/bmc/backends/idrac_legacy_media")
            .join(removed)
            .exists());
    }
    let implementation = repo_file("crates/bmc-backend-idrac-legacy-media/src/session.rs");
    assert!(!implementation.contains("FeatureConnection"));
    assert!(!implementation.contains("BackendParams"));
    assert!(!implementation.contains("crate::bmc"));
    let backend = repo_file("crates/bmc-backend-idrac-legacy-media/src/msd.rs");
    assert!(backend.contains("impl MsdBackendTrait"));
    assert!(!backend.contains("FeatureConnection"));
    assert!(!backend.contains("BackendParams"));

    let adapter = repo_file("src/bmc/backends/idrac_legacy_media/msd.rs");
    assert!(adapter.contains("bmc_backend_idrac_legacy_media"));
    assert!(!adapter.contains("impl MsdBackend"));
    assert!(!adapter.contains("AvmpPacket"));
}

#[test]
fn idrac_avct_protocol_and_feature_backends_are_isolated_in_their_crate() {
    let manifest = repo_file("crates/bmc-backend-idrac-avct/Cargo.toml");
    assert!(manifest.contains("bmc-feature-api"));
    assert!(!manifest.contains("bmc-aggregator"));
    for backend in ["redfish", "ipmi", "wol", "wsman", "legacy-media"] {
        assert!(!manifest.contains(&format!("bmc-backend-{backend}")));
    }
    for removed in [
        "apcp.rs",
        "avct.rs",
        "block_video.rs",
        "decoder.rs",
        "display.rs",
        "dvc.rs",
        "input.rs",
        "login.rs",
        "pool.rs",
    ] {
        assert!(!std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/bmc/backends/idrac_avct")
            .join(removed)
            .exists());
    }
    for source in ["pool.rs", "hid.rs", "video.rs"] {
        let implementation = repo_file(&format!("crates/bmc-backend-idrac-avct/src/{source}"));
        assert!(!implementation.contains("FeatureConnection"));
        assert!(!implementation.contains("BackendParams"));
        assert!(!implementation.contains("crate::bmc"));
    }
    for adapter in ["hid.rs", "video.rs"] {
        let adapter = repo_file(&format!("src/bmc/backends/idrac_avct/{adapter}"));
        assert!(adapter.contains("bmc_backend_idrac_avct"));
        assert!(!adapter.contains("impl HidBackend"));
        assert!(!adapter.contains("impl VideoSource"));
    }
}

#[test]
fn vnc_protocol_and_feature_backends_are_isolated_in_their_crate() {
    let manifest = repo_file("crates/bmc-backend-vnc/Cargo.toml");
    assert!(manifest.contains("bmc-feature-api"));
    assert!(!manifest.contains("bmc-aggregator"));
    assert!(!manifest.contains("bmc-backend-idrac"));
    assert!(!manifest.contains("bmc-backend-mahogany"));
    for removed in ["pool.rs", "protocol.rs"] {
        assert!(!std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/bmc/backends/vnc")
            .join(removed)
            .exists());
    }
    for source in ["pool.rs", "hid.rs", "video.rs"] {
        let implementation = repo_file(&format!("crates/bmc-backend-vnc/src/{source}"));
        assert!(!implementation.contains("FeatureConnection"));
        assert!(!implementation.contains("BackendParams"));
        assert!(!implementation.contains("crate::bmc"));
    }
    for adapter in ["hid.rs", "video.rs"] {
        let adapter = repo_file(&format!("src/bmc/backends/vnc/{adapter}"));
        assert!(adapter.contains("bmc_backend_vnc"));
        assert!(!adapter.contains("impl HidBackend"));
        assert!(!adapter.contains("impl VideoSource"));
    }
}

#[test]
fn mahogany_protocol_and_feature_backends_are_isolated_in_their_crate() {
    let manifest = repo_file("crates/bmc-backend-mahogany/Cargo.toml");
    assert!(manifest.contains("bmc-feature-api"));
    assert!(manifest.contains("bmc-backend-support"));
    assert!(!manifest.contains("bmc-aggregator"));
    for backend in [
        "idrac-avct",
        "idrac-legacy-media",
        "idrac-wsman",
        "ipmi",
        "redfish",
        "vnc",
        "wol",
    ] {
        assert!(!manifest.contains(&format!("bmc-backend-{backend}")));
    }
    for removed in [
        "auth.rs",
        "connection.rs",
        "framebuffer.rs",
        "perf.rs",
        "pool.rs",
        "protocol.rs",
        "storage_server.rs",
    ] {
        assert!(!std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/bmc/backends/mahogany")
            .join(removed)
            .exists());
    }
    for source in ["pool.rs", "hid.rs", "msd.rs", "video.rs"] {
        let implementation = repo_file(&format!("crates/bmc-backend-mahogany/src/{source}"));
        assert!(!implementation.contains("FeatureConnection"));
        assert!(!implementation.contains("BackendParams"));
        assert!(!implementation.contains("crate::bmc"));
        assert!(!implementation.contains("crate::config"));
    }
    for adapter in ["hid.rs", "msd.rs", "video.rs"] {
        let adapter = repo_file(&format!("src/bmc/backends/mahogany/{adapter}"));
        assert!(adapter.contains("bmc_backend_mahogany"));
        assert!(!adapter.contains("impl HidBackend"));
        assert!(!adapter.contains("impl MsdBackend"));
        assert!(!adapter.contains("impl VideoSource"));
    }
}

#[test]
fn web_layer_does_not_downcast_video_runtime() {
    let forbidden = [
        "downcast_ref::<Arc<dyn VideoSource>>",
        "downcast_ref::<std::sync::Arc<dyn VideoSource>>",
        "use crate::bmc::feature::video::VideoSource",
    ];

    for file in rust_files_under("src/web") {
        let content = fs::read_to_string(&file).unwrap();
        for pattern in forbidden {
            assert!(
                !content.contains(pattern),
                "{} leaks video backend runtime with forbidden pattern `{pattern}`",
                file.display()
            );
        }
    }
}

#[test]
fn webrtc_handlers_live_in_video_feature() {
    assert!(
        !Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/web/handlers/webrtc.rs")
            .exists(),
        "WebRTC handlers must stay in src/bmc/feature/video/webrtc.rs"
    );
    assert!(
        !Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/bmc/webrtc")
            .exists(),
        "Top-level bmc::webrtc must not exist; WebRTC belongs to the video feature"
    );

    let video_mod = repo_file("src/bmc/feature/video/mod.rs");
    assert!(video_mod.contains("pub mod ice_config;"));
    assert!(video_mod.contains("pub mod stream_config;"));
    assert!(!video_mod.contains("legacy_config"));
    assert!(video_mod.contains("pub mod webrtc;"));
    assert!(video_mod.contains("webrtc::webrtc_offer"));
    assert!(video_mod.contains("webrtc::webrtc_ice"));
    assert!(video_mod.contains("webrtc::webrtc_close"));

    assert!(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/bmc/feature/video/ice_config.rs")
            .exists(),
        "WebRTC config must live under the video feature"
    );
}

#[test]
fn legacy_config_urls_are_not_mounted() {
    let routes = repo_file("src/web/routes.rs");
    for forbidden in [
        "/config/video",
        "/config/auth",
        "/config/web",
        "{bmc_api}/config",
        "config_video",
        "config_auth",
        "config_web",
        "config_generic",
        "legacy_config",
    ] {
        assert!(
            !routes.contains(forbidden),
            "legacy BMC config URL or handler `{forbidden}` must not be mounted"
        );
    }

    assert!(
        !Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/web/handlers/config.rs")
            .exists(),
        "legacy web::handlers::config module must be removed"
    );
    assert!(
        !Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/bmc/feature/video/legacy_config.rs")
            .exists(),
        "legacy video config adapter must be removed"
    );

    let frontend_api = repo_file("frontend/src/api/config.ts");
    for forbidden in [
        "authConfigApi",
        "videoConfigApi",
        "webConfigApi",
        "getAll",
        "'/config",
        "\"/config",
    ] {
        assert!(
            !frontend_api.contains(forbidden),
            "frontend config API must not call legacy config URL/client `{forbidden}`"
        );
    }
}

#[test]
fn bmc_scoped_root_compatibility_urls_are_not_mounted() {
    let routes = repo_file("src/web/routes.rs");
    for forbidden in [
        "{bmc_api}/auth/",
        "{bmc_api}/setup",
        "{bmc_api}/system/info",
        "{bmc_api}/stream/http",
        "{bmc_api}/devices/atx",
        "{bmc_api}/ws/hid",
    ] {
        assert!(
            !routes.contains(forbidden),
            "BMC-scoped compatibility URL `{forbidden}` must not be mounted from web routes"
        );
    }
}

#[test]
fn video_feature_handlers_live_in_video_feature() {
    assert!(
        !Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/web/handlers/video.rs")
            .exists(),
        "Video feature handlers must stay in src/bmc/feature/video/handler.rs"
    );

    let video_mod = repo_file("src/bmc/feature/video/mod.rs");
    assert!(video_mod.contains("get(handler::stream_status)"));
    assert!(video_mod.contains("get(mode::get_mode)"));
    assert!(video_mod.contains("get(handler::stream_codecs)"));

    let video_handler = repo_file("src/bmc/feature/video/handler.rs");
    assert!(
        !video_handler.contains("stream_mode"),
        "Video mode endpoints must live in src/bmc/feature/video/mode.rs"
    );
    let video_mode = repo_file("src/bmc/feature/video/mode.rs");
    assert!(video_mode.contains("pub async fn get_mode"));
    assert!(video_mode.contains("pub async fn post_mode"));
}

#[test]
fn video_http_stream_lives_in_video_feature() {
    assert!(
        !Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/web/http_stream.rs")
            .exists(),
        "HTTP video stream transport must stay in src/bmc/feature/video/http_stream.rs"
    );

    let video_mod = repo_file("src/bmc/feature/video/mod.rs");
    assert!(
        video_mod.contains("get(http_stream::http_stream)"),
        "video feature routes should mount the HTTP stream handler"
    );
}

#[test]
fn feature_runtime_handlers_do_not_live_in_web_handlers() {
    for feature in ["audio", "hid", "msd", "power", "video", "webrtc"] {
        let path = format!("src/web/handlers/{feature}.rs");
        assert!(
            !Path::new(env!("CARGO_MANIFEST_DIR")).join(&path).exists(),
            "{path} must not exist; feature runtime handlers belong under src/bmc/feature"
        );
    }
}

#[test]
fn non_video_feature_routes_point_to_feature_handlers() {
    let hid_mod = repo_file("src/bmc/feature/hid/mod.rs");
    assert!(hid_mod.contains("get(handler::hid_status)"));
    assert!(hid_mod.contains("post(handler::hid_keyboard)"));
    assert!(hid_mod.contains("get(handler::ws_hid_upgrade)"));

    let msd_mod = repo_file("src/bmc/feature/msd/mod.rs");
    assert!(msd_mod.contains("get(handler::msd_status)"));
    assert!(msd_mod.contains("post(handler::msd_connect)"));

    let power_mod = repo_file("src/bmc/feature/power/mod.rs");
    assert!(power_mod.contains("get(handler::power_status)"));
    assert!(power_mod.contains("get(handler::power_atx_devices)"));
    assert!(power_mod.contains("post(handler::power_control)"));
    assert!(power_mod.contains("post(handler::power_wol_send)"));

    let audio_mod = repo_file("src/bmc/feature/audio/mod.rs");
    assert!(audio_mod.contains("get(handler::audio_status)"));
    assert!(audio_mod.contains("post(handler::audio_start)"));

    let routes = repo_file("src/web/routes.rs");
    assert!(!routes.contains("ws_hid_upgrade"));
}

#[test]
fn feature_routes_do_not_use_web_handlers() {
    for feature in ["audio", "hid", "msd", "power", "video"] {
        let path = format!("src/bmc/feature/{feature}/mod.rs");
        let content = repo_file(&path);
        if let Some(line) = content
            .lines()
            .find(|line| line.contains(".route(") && line.contains("crate::web::handlers::"))
        {
            panic!("{path} leaks feature route to web handlers: {line}");
        }
    }
}

#[test]
fn feature_lifecycle_helpers_stay_in_feature_layer() {
    let web_handlers = repo_file("src/web/handlers/mod.rs");
    assert!(
        !web_handlers.contains("fn acquire_connected_feature"),
        "Feature connection acquisition must live in src/bmc/feature, not web handlers"
    );
    assert!(
        !web_handlers.contains("fn feature_connect_status"),
        "Feature connection error mapping must live in src/bmc/feature, not web handlers"
    );

    let feature_mod = repo_file("src/bmc/feature/mod.rs");
    assert!(feature_mod.contains("pub async fn acquire_connected_feature"));
    assert!(feature_mod.contains("pub fn feature_connect_status"));

    for file in rust_files_under("src/bmc/feature") {
        let content = fs::read_to_string(&file).unwrap();
        assert!(
            !content.contains("crate::web::handlers::{acquire_connected_feature")
                && !content.contains("crate::web::handlers::{feature_connect_status")
                && !content.contains("crate::web::handlers::acquire_connected_feature")
                && !content.contains("crate::web::handlers::feature_connect_status"),
            "{} must not import feature lifecycle helpers from web handlers",
            file.display()
        );
    }
}

#[test]
fn web_state_is_not_defined_or_reexported_from_handlers() {
    let handlers_mod = repo_file("src/web/handlers/mod.rs");
    assert!(
        !handlers_mod.contains("pub struct AppState")
            && !handlers_mod.contains("pub use crate::web::state::AppState"),
        "AppState must live at web::state; handlers/mod.rs should only expose handlers"
    );

    let web_mod = repo_file("src/web/mod.rs");
    assert!(
        web_mod.contains("pub mod state;"),
        "web::state must be an explicit module"
    );

    let state_mod = repo_file("src/web/state.rs");
    assert!(state_mod.contains("pub struct AppState"));

    for file in rust_files_under("src") {
        let content = fs::read_to_string(&file).unwrap();
        assert!(
            !content.contains("web::handlers::AppState"),
            "{} must import AppState from web::state, not web::handlers",
            file.display()
        );
    }
}

#[test]
fn wol_is_registered_as_power_backend() {
    let feature_mod = repo_file("src/bmc/feature/mod.rs");
    assert!(feature_mod.contains("\"wol\" => \"power\" => wol_power"));
    assert!(feature_mod.contains("use crate::bmc::backends::wol as wol_power;"));

    let power_trait = repo_file("crates/bmc-feature-api/src/power.rs");
    assert!(power_trait.contains("async fn wake(&self, mac_address: &str)"));

    assert!(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/bmc/backends/wol.rs")
            .exists(),
        "WOL power backend adapter must exist"
    );
}

#[test]
fn frontend_feature_presence_comes_from_backend() {
    let websocket = repo_file("frontend/src/composables/useWebSocket.ts");
    assert!(
        websocket.contains("let subscribedFeatures: string[] = []"),
        "WebSocket feature subscriptions must not default to a hardcoded frontend feature list"
    );
    assert!(
        !websocket.contains("['video', 'hid', 'msd', 'power']"),
        "WebSocket feature subscriptions must come from backend feature discovery"
    );

    let console_events = repo_file("frontend/src/composables/useConsoleEvents.ts");
    assert!(
        console_events.contains("systemStore.availableFeatureIds.includes('video')")
            && console_events.contains("setFeatures(")
            && console_events.contains("setFeatures([])")
            && console_events.contains("watch(")
            && console_events.contains("stopFeatureLeaseWatch"),
        "Console video lease subscription must react to asynchronous backend feature discovery"
    );
    assert!(
        !console_events.contains("setFeatures(['video', 'hid', 'msd', 'power'])"),
        "Console event subscriptions must not hardcode feature ids"
    );
}

#[test]
fn backend_features_endpoint_reports_registry_features() {
    let handler = repo_file("src/web/handlers/features.rs");
    assert!(
        handler.contains("state.registry.get_dev"),
        "/features must be based on the device registry, not static capabilities alone"
    );
    assert!(
        handler.contains("\"features\""),
        "/features must expose feature entries for frontend discovery"
    );
    assert!(
        handler.contains("\"backend\""),
        "/features entries should include backend identity from FeatureConnection"
    );
}

#[test]
fn frontend_actionbar_status_uses_unified_status_strip() {
    let console = repo_file("frontend/src/views/ConsoleView.vue");
    assert!(
        console.contains("FeatureStatusStrip"),
        "Console ActionBar-adjacent status area should use the unified FeatureStatusStrip"
    );
    assert!(
        !console.contains("<StatusCard"),
        "ConsoleView must not hand-roll ActionBar-adjacent feature status cards"
    );

    let strip = repo_file("frontend/src/components/FeatureStatusStrip.vue");
    assert!(
        strip.contains("systemStore.startFeatureStatusPolling"),
        "FeatureStatusStrip must refresh through the unified feature status polling lifecycle"
    );
    assert!(
        strip.contains("systemStore.featureStatuses"),
        "FeatureStatusStrip must render the unified feature status collection"
    );
    assert!(
        strip.contains("featureRegistry.getStatusCard"),
        "FeatureStatusStrip should render the original feature-specific status card components"
    );

    let api = repo_file("frontend/src/api/config.ts");
    assert!(
        api.contains("request<FeatureStatusesResponse>('/features/status')"),
        "frontend unified feature status data source must be /features/status"
    );

    let routes = repo_file("src/web/routes.rs");
    assert!(
        routes.contains("/features/status"),
        "backend must expose the unified feature status endpoint"
    );
}

#[test]
fn frontend_actionbar_feature_visibility_comes_from_backend_features() {
    let actionbar = repo_file("frontend/src/components/ActionBar.vue");
    assert!(
        actionbar.contains("systemStore.isFeatureAvailable('msd')"),
        "ActionBar feature visibility must use backend feature discovery"
    );

    let forbidden = [
        "systemStore.msd?.available",
        "systemStore.hid?.backend",
        "isCh9329Backend",
        "hidBackend",
    ];
    for pattern in forbidden {
        assert!(
            !actionbar.contains(pattern),
            "ActionBar must not infer feature visibility from feature implementation details: `{pattern}`"
        );
    }

    let store = repo_file("frontend/src/stores/system.ts");
    assert!(
        store.contains("isFeatureAvailable"),
        "system store must expose a single frontend feature availability query"
    );
    assert!(
        store.contains("featureStatuses.value") && store.contains("features.value"),
        "feature availability should be derived from backend /features and /features/status data"
    );

    let hid_status = repo_file("frontend/src/features/hid/HidStatusCard.vue");
    assert!(
        hid_status.contains("systemStore.isFeatureAvailable('msd')"),
        "HID status layout must use backend feature discovery when accounting for MSD"
    );
    assert!(
        !hid_status.contains("systemStore.msd?.available"),
        "HID status card must not infer MSD presence from MSD implementation state"
    );
    assert!(
        hid_status.contains("hidBus.networkErrorMessage.value")
            && hid_status.contains("hidBus.lastSendError.value")
            && hid_status.contains("hidBus.droppedEvents.value"),
        "HID status card must surface feature event-bus failure details from the backend"
    );
}

#[test]
fn frontend_lifecycle_work_is_owned_by_activation_and_store_pollers() {
    let strip = repo_file("frontend/src/components/FeatureStatusStrip.vue");
    assert!(
        strip.contains("systemStore.startFeatureStatusPolling"),
        "FeatureStatusStrip must delegate polling lifecycle to the system store"
    );
    assert!(
        !strip.contains("setInterval"),
        "FeatureStatusStrip instances must not each create their own polling timer"
    );

    let store = repo_file("frontend/src/stores/system.ts");
    assert!(
        store.contains("featureStatusPollerRefs"),
        "feature status polling must be reference counted across multiple mounted strips"
    );

    let console = repo_file("frontend/src/views/ConsoleView.vue");
    assert!(
        console.contains("consoleEventsSubscribed"),
        "KeepAlive ConsoleView must guard console event subscription state"
    );
    assert!(
        console.contains("consoleEvents.subscribe()")
            && console.contains("consoleEvents.unsubscribe()"),
        "ConsoleView activation/deactivation must subscribe and unsubscribe console events"
    );
    assert!(
        console.contains("WatchStopHandle") && console.contains("stopHandles"),
        "async-created ConsoleView watchers must be stopped explicitly"
    );

    let websocket = repo_file("frontend/src/composables/useWebSocket.ts");
    assert!(
        websocket.contains("shouldReconnect") && websocket.contains("reconnectTimeout"),
        "global WebSocket reconnect lifecycle must be cancellable on disconnect"
    );
    assert!(
        websocket.contains("WebSocket.CONNECTING")
            && websocket.contains("wsUrl === url")
            && websocket.contains("wsInstance !== socket"),
        "global WebSocket lifecycle must be single-flight and replace device-bound sockets"
    );

    let feature_event_bus = repo_file("frontend/src/composables/useFeatureEventBus.ts");
    assert!(
        feature_event_bus.contains("let shouldReconnect = false")
            && feature_event_bus.contains("if (!shouldReconnect || currentDeviceScope() !== ownerDevice)")
            && feature_event_bus.contains("settleConnection(false)")
            && feature_event_bus.contains("connected.value = false"),
        "feature event bus reconnect lifecycle must be cancellable and must settle pending connects"
    );

    let console = repo_file("frontend/src/views/ConsoleView.vue");
    assert!(
        console.contains("hidBus.disconnect()"),
        "ConsoleView deactivation must release the HID feature event-bus connection"
    );

    let stats_sheet = repo_file("frontend/src/features/video/StatsSheet.vue");
    assert!(
        stats_sheet.contains("chartInitTimeout") && stats_sheet.contains("cancelChartInit"),
        "StatsSheet delayed chart init must be cancellable when closed or unmounted"
    );

    let msd_dialog = repo_file("frontend/src/features/msd/MsdDialog.vue");
    assert!(
        msd_dialog.contains("downloadCompleteTimeout")
            && msd_dialog.contains("window.clearTimeout(downloadCompleteTimeout)"),
        "MsdDialog delayed download completion cleanup must be cancellable on unmount"
    );

    for settings_path in [
        "frontend/src/views/settings/GlobalSettingsView.vue",
        "frontend/src/views/settings/DeviceSettingsView.vue",
    ] {
        let settings = repo_file(settings_path);
        for forbidden in [
            "updateApi",
            "stopUpdatePolling",
            "systemApi.restart",
            "extensionsApi",
            "rtspConfigApi",
            "rustdeskConfigApi",
            "redfishConfigApi",
        ] {
            assert!(
                !settings.contains(forbidden),
                "{settings_path} must not expose unimplemented control {forbidden}"
            );
        }
    }
}

#[test]
fn frontend_routing_uses_native_device_routes_without_injected_base() {
    let routes = repo_file("src/web/routes.rs");
    for forbidden in [
        "/bmc/:id/config.js",
        "__KVM_API_BASE",
        "__KVM_ROUTER_BASE",
        "<base href",
    ] {
        assert!(
            !routes.contains(forbidden),
            "backend must not inject frontend routing/API base through `{forbidden}`"
        );
    }

    let router = repo_file("frontend/src/router/index.ts");
    assert!(
        router.contains("path: '/devices'")
            && router.contains("path: '/devices/:bmcId/console'")
            && router.contains("path: '/devices/:bmcId/settings'"),
        "frontend router must expose product-level multi-device routes"
    );
    assert!(
        !router.contains("path: '/bmc/:bmcId'"),
        "frontend router must not expose backend API namespace as a UI route"
    );
    assert!(
        router.contains("createWebHistory('/')"),
        "frontend router history must not depend on an injected base path"
    );
    assert!(
        !router.contains("__KVM_ROUTER_BASE") && !router.contains("routerBase"),
        "frontend router must not read injected router base globals"
    );

    for file in files_under("frontend/src") {
        let content = fs::read_to_string(&file).unwrap();
        for forbidden in ["__KVM_API_BASE", "__KVM_ROUTER_BASE"] {
            assert!(
                !content.contains(forbidden),
                "{} must not use injected frontend base global `{forbidden}`",
                file.display()
            );
        }
    }

    let request = repo_file("frontend/src/api/request.ts");
    assert!(
        request.contains("currentBmcIdFromLocation")
            && request.contains("window.location.pathname.match")
            && request.contains("^\\/devices\\/")
            && !request.contains("^\\/bmc\\/")
            && request.contains("`/bmc/${encodeURIComponent(bmcId)}/api`"),
        "device API base must map product-level /devices/:bmcId UI routes to backend /bmc/:id/api routes"
    );

    let websocket = repo_file("frontend/src/types/websocket.ts");
    assert!(
        websocket.contains("currentBmcIdFromPathname")
            && websocket.contains("^\\/devices\\/")
            && websocket.contains("`/bmc/${encodeURIComponent(bmcId)}/api${normalized.slice('/api'.length)}`")
            && !websocket.contains("return '.' + path"),
        "WebSocket API base must map product-level /devices/:bmcId UI routes to backend /bmc/:id/api routes"
    );
}

#[test]
fn device_settings_follow_feature_config_tree() {
    let settings = repo_file("frontend/src/views/settings/DeviceSettingsView.vue");
    let editor = repo_file("frontend/src/components/settings/FeatureAssignmentEditor.vue");
    assert!(
        settings.contains("assignments")
            && settings.contains("featureAssignmentToMutation")
            && editor.contains("backendFor(props.catalog")
            && editor.contains("FeatureAssignmentMutation"),
        "device settings must edit explicit catalog-described feature assignments"
    );
    assert!(
        !settings.contains("hidBackendOptions")
            && !settings.contains("v-model=\"config.hid_backend\"")
            && !settings.contains("defaultDeviceFeatures")
            && !settings.contains("protocol preset"),
        "HID backend must not be configured as a separate non-device setting"
    );

    let api = repo_file("frontend/src/api/config.ts");
    assert!(
        !api.contains("ManagedDeviceConfig")
            && !api.contains("deviceConfigApi")
            && repo_file("frontend/src/views/settings/GlobalSettingsView.vue")
                .contains("rootRequestMeta('/server/web')"),
        "device config API must not retain the removed legacy DTO path"
    );

    let config = repo_file("src/config.rs");
    assert!(
        config.contains("pub struct DeviceFeatureConfig")
            && config.contains("pub backend_params: HashMap<String, serde_json::Value>")
            && config.contains("pub feature_params: HashMap<String, serde_json::Value>"),
        "persisted device config must model backend_params and feature_params under each feature"
    );

    let device_handler = repo_file("src/web/handlers/device.rs");
    assert!(
        device_handler.contains("device_configuration_dto")
            && device_handler.contains("apply_device_configuration_mutation")
            && device_handler.contains("ConfigurationScope::Device"),
        "backend device config CRUD must own the canonical device assignment DTO"
    );

    let registry = repo_file("src/bmc/feature/device_registry.rs");
    assert!(
        registry.contains("dev_cfg.feature_backend(\"video\")")
            && registry.contains("dev_cfg.feature_backend(\"hid\")")
            && registry.contains("dev_cfg.feature_backend(\"msd\")")
            && registry.contains("dev_cfg.feature_backend(\"power\")")
            && registry.contains("backend_params_for(dev_cfg, \"video\"")
            && registry.contains("feature_params_for(")
            && registry.contains("\"video\",")
            && registry.contains("FeatureParams {"),
        "feature registry must initialize each feature from the device-owned feature config tree"
    );
    assert!(
        config.contains("pub fn feature_backend(&self, feature: &str) -> Option<&str>")
            && !config.contains("feature_backend(&self, feature: &str, fallback")
            && !registry.contains("feature_backend(\"video\",")
            && !registry.contains("feature_backend(\"hid\",")
            && !registry.contains("feature_backend(\"msd\",")
            && !registry.contains("feature_backend(\"power\","),
        "runtime feature registration must consume explicit backend selections without cross-backend fallback"
    );
}

#[test]
fn frontend_config_state_is_feature_owned() {
    assert!(
        !Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("frontend/src/stores/config.ts")
            .exists(),
        "frontend config state must not be centralized in a shallow useConfigStore module"
    );

    for store in [
        "videoConfig",
        "hidConfig",
        "audioConfig",
        "deviceConfiguration",
    ] {
        assert!(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("frontend/src/stores/{store}.ts"))
                .exists(),
            "frontend config state for {store} must have its own module"
        );
    }

    let video_store = repo_file("frontend/src/stores/videoConfig.ts");
    assert!(
        video_store.contains("videoInputConfigApi.get(bmcId)")
            && video_store.contains("videoInputConfigApi.update(videoEtag.value, update, bmcId)")
            && !video_store.contains("deviceConfigApi.list()")
            && !video_store.contains("deviceConfigApi.update("),
        "video input config must use the video feature config interface"
    );
}

#[test]
fn hid_direct_input_waits_for_backend_ready_and_drops_backpressure() {
    let event_bus = repo_file("frontend/src/composables/useFeatureEventBus.ts");
    assert!(
        event_bus.contains("const MAX_BUFFERED_BYTES")
            && event_bus.contains("transport.bufferedAmount > MAX_BUFFERED_BYTES")
            && event_bus.contains("droppedEvents.value++")
            && event_bus.contains("lastSendError.value"),
        "feature event bus must expose backpressure/drop state instead of silently queueing input forever"
    );
    assert!(
        event_bus.contains("connected.value = true")
            && event_bus.contains("code === codec.responseOk")
            && !event_bus.contains("transport.onopen = () => {\n          connected.value = true"),
        "feature event bus must report connected only after the backend feature-ready ack, not raw WebSocket open"
    );
    assert!(
        event_bus.contains("if (!connectionResolved || unavailable.value)")
            && event_bus.contains("event bus not ready"),
        "direct feature input must reject sends before backend-ready ack to avoid delayed stale input"
    );
    assert!(
        event_bus.contains("decodeErrorPayload")
            && event_bus.contains("lastSendError.value = networkErrorMessage.value")
            && event_bus.contains("networkErrorMessage.value = decodeErrorPayload"),
        "feature event bus must expose backend rejection details for direct-input diagnostics"
    );

    let hid_input = repo_file("frontend/src/features/hid/useHidInput.ts");
    assert!(
        hid_input.contains("const alreadyPressed = pressedKeys.value.includes(canonicalKey)")
            && hid_input.contains("if (alreadyPressed) return")
            && !hid_input.contains("if (e.repeat || alreadyPressed) return"),
        "physical keyboard input must forward one HID keydown and one HID keyup; guest OS repeat belongs to the remote host"
    );
    assert!(
        hid_input.contains("function releasePressedInputs(")
            && hid_input.contains("for (const key of keysToRelease)")
            && hid_input.contains("sendKeyboardEvent('up', key, immediate)")
            && hid_input.contains("window.addEventListener('beforeunload', handleBlur)")
            && hid_input.contains("releasePressedInputs()"),
        "frontend HID input lifecycle must release all pressed keys on blur/dispose/beforeunload"
    );
    assert!(
        hid_input.contains("return enqueueReliableInput(() => type === 'down'")
            && hid_input.contains("if (!hidBus.connected.value) await hidBus.connect().catch(() => false)")
            && hid_input.contains("!pressedKeys.value.includes(key)")
            && !hid_input.contains("setInterval(() => sendKeyboardEvent")
            && !hid_input.contains("setTimeout(() => sendKeyboardEvent"),
        "frontend HID input must bridge the backend-ready race without synthesizing repeat or stale taps"
    );
    let virtual_keyboard = repo_file("frontend/src/components/VirtualKeyboard.vue");
    assert!(
        virtual_keyboard.contains("const keyboard = props.captureKeyboard()")
            && virtual_keyboard.contains("if (!keyboard.current()) return")
            && !virtual_keyboard.contains("hidBus.sendKeyboard"),
        "virtual keyboard taps and macros must use the console-owned keyboard queue"
    );

    let hid_handler = repo_file("src/bmc/feature/hid/handler.rs");
    assert!(
        hid_handler.contains("struct SocketInputs")
            && hid_handler.contains("keys.insert(key.scancode)")
            && hid_handler.contains("keys.remove(&key.scancode)")
            && hid_handler.contains("release_hid_socket_inputs")
            && hid_handler.contains("buttons.insert(*button)")
            && hid_handler.contains("Weak<dyn HidBackend>")
            && hid_handler.contains("backend.keyboard(scancode, false).await"),
        "backend HID websocket lifecycle must release its owned keys/buttons without targeting a replacement backend"
    );

    let mahogany_hid = repo_file("crates/bmc-backend-mahogany/src/hid.rs");
    assert!(
        mahogany_hid.contains("tcp_tx.try_send(messages)")
            && mahogany_hid.contains("TrySendError::Full(_)) => return Ok(())"),
        "lossy mouse move events must not block keyboard/button events behind stale movement"
    );
    assert!(
        mahogany_hid.contains("wait_for_keyboard_control")
            && mahogany_hid.contains("preTranslated = true")
            && mahogany_hid.contains("Event::Key(_, true) | Event::Consumer(_)"),
        "Mahogany HID keydown must wait for keyboard/full-control state while preserving USB HID usage semantics"
    );
    let mahogany_adapter = repo_file("src/bmc/backends/mahogany/hid.rs");
    assert!(
        !mahogany_adapter.contains(".map_err(|e| format!(\"mahogany hid control: {}\", e))")
            && mahogany_adapter.contains("fc.set_backend_runtime")
            && mahogany_adapter.contains("wait_for_keyboard_control"),
        "Mahogany HID feature readiness must not be blocked on full-control state"
    );

    let mahogany_pool = repo_file("crates/bmc-backend-mahogany/src/pool.rs");
    assert!(
        mahogany_pool.contains("ServerMessage::MultiUserState(state)")
            && mahogany_pool
                .contains("current.observe(state.has_full_control, state.can_take_full_control)")
            && mahogany_pool.contains("state.permission(origin.elapsed(), timeout)")
            && mahogany_pool.contains("request_primary_control"),
        "Mahogany pool must track MultiUserState/full-control before HID keydown sends"
    );
}

#[test]
fn paste_hid_batch_uses_explicit_modifier_events() {
    let paste = repo_file("frontend/src/components/PasteModal.vue");
    assert!(
        paste.contains("const shiftScancode = canonicalKeyToHidUsage(keys.ShiftLeft)")
            && paste.contains("scancode: shiftScancode, pressed: true")
            && paste.contains("scancode: shiftScancode, pressed: false"),
        "PasteModal must encode shifted characters as explicit HID modifier key events"
    );
    assert!(
        !paste.contains("modifier }") && !paste.contains("modifier:"),
        "PasteModal must not rely on a magic modifier field that the backend event boundary does not execute"
    );

    let hid_types = repo_file("frontend/src/types/hid.ts");
    let hid_protocol = repo_file("src/bmc/hid_protocol.rs");
    assert!(
        hid_types.contains("byte 3 reserved")
            && hid_types.contains("view.setUint8(3, 0)")
            && !hid_types.contains("modifier?:"),
        "HID event-bus keyboard protocol must not expose a modifier field that is not executed by the backend"
    );
    assert!(
        hid_protocol.contains("usage_low, reserved")
            && !hid_protocol.contains("usage_low, modifier"),
        "backend HID protocol docs must keep the keyboard reserved byte explicit"
    );
}

#[test]
fn frontend_feature_specific_ui_lives_under_feature_modules() {
    for stale_path in [
        "frontend/src/components/VideoConfigPopover.vue",
        "frontend/src/components/StatsSheet.vue",
        "frontend/src/components/HidConfigPopover.vue",
        "frontend/src/components/AudioConfigPopover.vue",
        "frontend/src/components/MsdDialog.vue",
        "frontend/src/components/MsdSheet.vue",
    ] {
        assert!(
            !Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(stale_path)
                .exists(),
            "{stale_path} is feature-specific UI and must live under frontend/src/features"
        );
    }

    for expected_path in [
        "frontend/src/features/video/VideoConfigPopover.vue",
        "frontend/src/features/video/StatsSheet.vue",
        "frontend/src/features/hid/HidConfigPopover.vue",
        "frontend/src/features/audio/AudioConfigPopover.vue",
        "frontend/src/features/msd/MsdDialog.vue",
        "frontend/src/features/msd/MsdSheet.vue",
    ] {
        assert!(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(expected_path)
                .exists(),
            "{expected_path} must exist"
        );
    }

    for feature in ["audio", "hid", "msd", "video"] {
        let index = repo_file(&format!("frontend/src/features/{feature}/index.ts"));
        assert!(
            !index.contains("@/components/"),
            "feature barrel frontend/src/features/{feature}/index.ts must not import feature UI from shared components"
        );
    }
}

#[test]
fn frontend_source_tree_does_not_contain_generated_vue_js() {
    let generated: Vec<_> = files_under("frontend/src")
        .into_iter()
        .filter(|path| {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            name.ends_with(".vue.js") || name.ends_with(".ts.js") || name.ends_with(".tsx.js")
        })
        .collect();
    assert!(
        generated.is_empty(),
        "frontend/src must not contain generated JS shadow files: {generated:?}"
    );
}

#[test]
fn console_view_does_not_reimplement_feature_status_cards() {
    let console = repo_file("frontend/src/views/ConsoleView.vue");
    let forbidden = [
        "const videoStatus = computed",
        "const hidStatus = computed",
        "const audioStatus = computed",
        "const msdStatus = computed",
        "const videoDetails = computed",
        "const hidDetails = computed",
        "const audioDetails = computed",
        "const msdDetails = computed",
        "StatusDetail",
        "formatVideoDeviceLabel",
    ];

    for pattern in forbidden {
        assert!(
            !console.contains(pattern),
            "ConsoleView must not reimplement feature status card logic: `{pattern}`"
        );
    }
}

#[test]
fn console_view_delegates_hid_input_to_hid_feature() {
    let console = repo_file("frontend/src/views/ConsoleView.vue");
    assert!(
        console.contains("useHidInput"),
        "ConsoleView should delegate HID input behavior to the HID feature module"
    );

    let forbidden = [
        "sendKeyboardEvent",
        "sendMouseEvent",
        "shouldBlockKey",
        "keyboardEventToCanonicalKey",
        "updateModifierMaskForKey",
        "mouseFlushTimer",
        "pendingMouseMove",
        "accumulatedDelta",
        "requestPointerLock",
        "exitPointerLock",
        "activeModifierMask",
        "pressedMouseButton",
        "function handleKey",
        "function handleMouse",
        "function registerInteractionListeners",
        "function unregisterInteractionListeners",
        "HidKeyboardEvent",
        "HidMouseEvent",
        "HidBackend",
    ];

    for pattern in forbidden {
        assert!(
            !console.contains(pattern),
            "ConsoleView must not reimplement HID input internals: `{pattern}`"
        );
    }
}

#[test]
fn console_view_delegates_video_stream_to_video_feature() {
    let console = repo_file("frontend/src/views/ConsoleView.vue");
    assert!(
        console.contains("useVideoStream"),
        "ConsoleView should delegate video stream lifecycle to the video feature module"
    );

    let forbidden = [
        "retryTimeoutId",
        "gracePeriodTimeoutId",
        "consecutiveErrors",
        "pendingWebRTCReadyGate",
        "webrtcConnectTask",
        "webrtcRecovery",
        "frameOverlayUrl = ref",
        "captureFrameOverlay",
        "waitForVideoFirstFrame",
        "shouldSuppressAutoReconnect",
        "markWebRTCFailure",
        "connectWebRTCSerial",
        "function handleVideoLoad",
        "function handleVideoError",
        "function handleStreamDeviceLost",
        "function handleStreamRecovered",
        "function handleStreamConfig",
        "function handleWebRTCReady",
        "function handleStreamMode",
        "function handleStreamStateChanged",
        "function handleStreamStatsUpdate",
        "initialDeviceInfoReceived",
        "initialModeRestore",
        "function refreshVideo",
        "const mjpegTimestamp",
        "const mjpegUrl",
        "function connectWebRTCOnly",
        "function rebindWebRTCVideo",
        "function switchTo",
        "function syncToServerMode",
        "watch(() => webrtc",
        "watch(webrtc",
        "webrtcReconnect",
        "videoDebugLog",
        "generateUUID",
        "fetchHttpModes",
        "StreamDeviceLostEventData",
    ];

    for pattern in forbidden {
        assert!(
            !console.contains(pattern),
            "ConsoleView must not reimplement video stream internals: `{pattern}`"
        );
    }
}

#[test]
fn frontend_device_info_updates_are_centralized_in_system_store() {
    assert!(
        !Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("frontend/src/composables/useVideoStream.ts")
            .exists(),
        "legacy MJPEG-only useVideoStream composable must not coexist with frontend/src/features/video/useVideoStream.ts"
    );

    let console = repo_file("frontend/src/views/ConsoleView.vue");
    assert!(
        !console.contains("systemStore.updateFeatureInstances"),
        "ConsoleView must not manually fan out feature instance snapshots from device_info"
    );

    let store = repo_file("frontend/src/stores/system.ts");
    assert!(
        store.contains("updateFeatureInstances('video', data.video.instances)")
            && store.contains("updateFeatureInstances('hid', data.hid.instances)")
            && store.contains("updateFeatureInstances('msd', data.msd?.instances)")
            && store.contains("updateFeatureInstances('power', data.atx?.instances)"),
        "system store updateFromDeviceInfo must be the single frontend owner of feature instance snapshots"
    );
    assert!(
        store.contains("audioChanged"),
        "system store updateFromDeviceInfo should return audio change information for feature coordinators"
    );
}

#[test]
fn frontend_terminal_controls_are_not_exposed_until_ttyd_backend_exists() {
    let console = repo_file("frontend/src/views/ConsoleView.vue");
    assert!(
        !console.contains("showTerminalDialog")
            && !console.contains("openTerminal")
            && !console.contains("/api/terminal/")
            && !console.contains("systemStore.ttyd"),
        "ConsoleView must not expose ttyd terminal controls until the ttyd backend exists"
    );
}

#[test]
fn encoder_backend_display_names_do_not_leak_gstreamer_elements() {
    let encoder_registry = repo_file("src/bmc/stream/encoder.rs");
    assert!(
        encoder_registry.contains("name: \"NVENC\""),
        "NVENC backend display name should be the backend id/name, not a codec-specific element"
    );
    assert!(
        !encoder_registry.contains("NVENC (nvh264enc)")
            && !encoder_registry.contains("Software (x264enc)"),
        "stream encoder registry must not expose concrete GStreamer element names as frontend labels"
    );

    let video_config = repo_file("frontend/src/features/video/VideoConfigPopover.vue");
    assert!(
        video_config.contains("lowerBackend === 'nvenc'")
            && video_config.contains("return 'NVENC'"),
        "frontend must render nvenc as NVENC instead of leaking nvh264enc/nvh265enc/nvav1enc"
    );
}

#[test]
fn webrtc_pipeline_state_changes_are_recoverable_errors() {
    let gst_webrtc = repo_file("src/bmc/stream/gst_webrtc.rs");
    assert!(
        gst_webrtc.contains("pub fn start_main_loop(&self) -> Result<(), String>"),
        "GstWebRtcSession::start_main_loop must return a Result so PLAYING failures do not panic tokio workers"
    );
    assert!(
        !gst_webrtc.contains("expect(\"set webrtc pipeline to PLAYING\")")
            && !gst_webrtc.contains("expect(\"set webrtc pipeline to Null\")")
            && !gst_webrtc.contains("expect(\"drop: set webrtc pipeline to Null\")"),
        "WebRTC pipeline state changes must not use expect() in production lifecycle paths"
    );

    let video_webrtc = repo_file("src/bmc/feature/video/webrtc.rs");
    assert!(
        video_webrtc.contains("if let Err(e) = session.start_main_loop()")
            && video_webrtc.contains("format!(\"start session: {e}\")"),
        "video WebRTC handler must convert start_main_loop errors into HTTP signaling errors"
    );
}

#[test]
fn feature_fault_contract_is_shared_without_a_video_shim() {
    let feature_fault = repo_file("src/bmc/feature/fault.rs");
    assert!(
        feature_fault.contains("pub struct FeatureFault")
            && feature_fault.contains("pub struct FeatureFaultStore"),
        "FeatureFault must be the shared feature-layer contract"
    );
    assert!(
        !Path::new("src/bmc/feature/video/fault.rs").exists(),
        "video must use the shared FeatureFault directly, not a parallel fault shim"
    );
    let status = repo_file("src/bmc/feature/mod.rs");
    assert!(
        status.contains("\"fault\": fault"),
        "common feature status must expose the shared active fault"
    );
    for handler in [
        "src/bmc/feature/video/handler.rs",
        "src/bmc/feature/hid/handler.rs",
        "src/bmc/feature/msd/handler.rs",
        "src/bmc/feature/power/handler.rs",
        "src/bmc/feature/audio/handler.rs",
    ] {
        assert!(
            repo_file(handler).contains("FeatureFault"),
            "{handler} must use the shared structured fault contract"
        );
    }
}

#[test]
fn frontend_webrtc_state_ignores_stale_peer_connection_events() {
    let use_webrtc = repo_file("frontend/src/composables/useWebRTC.ts");
    assert!(
        use_webrtc.contains("let peerGeneration = 0")
            && use_webrtc.contains("function isActivePeer")
            && use_webrtc.contains("if (!isActivePeer(pc, generation)) return"),
        "frontend WebRTC state must ignore late events from stale RTCPeerConnection instances"
    );
    assert!(
        use_webrtc.contains("function hasUsableRemoteVideo")
            && use_webrtc.contains("Ignoring WebRTC failed state while video is active")
            && use_webrtc.contains("lastVideoActivityAt"),
        "frontend WebRTC state must not report failed while the active connection is receiving video"
    );

    let video_stream = repo_file("frontend/src/features/video/useVideoStream.ts");
    assert!(
        video_stream.contains("Suppressing stale WebRTC failure because video is active")
            && video_stream
                .contains("Ignoring WebRTC failed state watcher because video is active"),
        "video stream UI must suppress stale WebRTC failure overlays when video is visibly active"
    );
    assert!(
        video_stream.contains("Ignoring WebRTC failure outside active WebRTC transport")
            && video_stream.contains("isHttpMode.value || isWebTransportMode.value")
            && video_stream.contains("if (shouldSuppressAutoReconnect()) return"),
        "WebRTC failures must not write global video errors while MJPEG or WebTransport is active"
    );
}

#[test]
fn hid_reset_is_backend_capability_and_paste_cleanup_is_silent() {
    let paste = repo_file("frontend/src/components/PasteModal.vue");
    assert!(
        paste.contains("async function resetHidSilently()")
            && paste.contains("hidApi.reset({ toastOnError: false })")
            && !paste.contains("hidApi.reset()"),
        "PasteModal cleanup/stop must not surface optional HID reset failures as global toasts"
    );
    assert!(
        paste.contains("ref<InstanceType<typeof Textarea> | null>")
            && !paste.contains("ref<HTMLTextAreaElement | null>"),
        "PasteModal must type its Textarea ref as a component instance, not a native DOM textarea"
    );

    let textarea = repo_file("frontend/src/components/ui/textarea/Textarea.vue");
    assert!(
        textarea.contains("function focus()")
            && textarea.contains("defineExpose({ focus })")
            && textarea.contains("ref=\"textareaRef\""),
        "Textarea component must expose a focus() API for component refs"
    );

    let api = repo_file("frontend/src/api/index.ts");
    assert!(
        api.contains("type ApiRequestConfig")
            && api.contains("reset: (config?: ApiRequestConfig)")
            && api.contains(
                "request<{ success: boolean }>('/feature/hid/reset', { method: 'POST' }, config)",
            ),
        "hidApi.reset must allow callers to opt out of global error toasts"
    );

    let hid_trait = repo_file("crates/bmc-feature-api/src/hid.rs");
    assert!(
        hid_trait.contains("async fn reset(&self) -> Result<(), String>")
            && hid_trait.contains("HID reset is not supported by this backend"),
        "HID reset must be an explicit backend capability, not a web-layer assumption"
    );

    let hid_handler = repo_file("src/bmc/feature/hid/handler.rs");
    assert!(
        hid_handler.contains("backend.reset().await")
            && !hid_handler.contains("HID reset not supported in aggregator mode"),
        "HID reset handler must delegate to the HID backend and must not hardcode aggregator-mode failure"
    );

    let mahogany_hid = repo_file("crates/bmc-backend-mahogany/src/hid.rs");
    assert!(
        mahogany_hid.contains("async fn reset(&self) -> Result<(), String>")
            && mahogany_hid.contains("self.apply(Event::Reset).await")
            && repo_file("crates/bmc-backend-model/src/input.rs").contains("Action::Key(0, false)"),
        "Mahogany HID backend should implement best-effort reset/release"
    );
}

#[test]
fn frontend_streaming_mode_switch_does_not_preflight_http_mode_endpoint() {
    let video_stream = repo_file("frontend/src/features/video/useVideoStream.ts");
    let switch_to_webrtc = video_stream
        .split("async function switchToWebRTC")
        .nth(1)
        .and_then(|rest| rest.split("async function switchToStreaming").next())
        .expect("switchToWebRTC function should exist");
    assert!(
        !switch_to_webrtc.contains("streamApi.setMode"),
        "WebRTC mode switch must not call /feature/video/mode before signaling; offer carries codec/backend"
    );
    assert!(
        switch_to_webrtc.contains("connectWebRTCSerial('switchToWebRTC', codec)")
            && switch_to_webrtc.contains("const MAX_ATTEMPTS = 2"),
        "WebRTC switching should use a short direct signaling path"
    );

    let use_webrtc = repo_file("frontend/src/composables/useWebRTC.ts");
    assert!(
        use_webrtc.contains("let activeCodec")
            && use_webrtc.contains("let activeEncoder")
            && use_webrtc.contains("activeCodec === requestedCodec")
            && use_webrtc.contains("activeEncoder === requestedEncoder"),
        "WebRTC connect should reuse existing sessions only when codec and encoder match"
    );
}

#[test]
fn connection_id_replaces_legacy_webrtc_identifier_everywhere() {
    let forbidden = [
        ["session", "_id"].concat(),
        ["session", "Id"].concat(),
        ["Session", "Id"].concat(),
    ];
    // The Dell APCP/AVMP protocol has an established opaque session field;
    // protocol.  This architectural rule is about application-level WebRTC
    // identity, so protocol decoders must remain outside its text scan.
    for path in ["src/bmc/feature", "src/web", "frontend/src", "tests"] {
        for file in files_under(path) {
            let content = fs::read_to_string(&file).unwrap();
            for pattern in &forbidden {
                assert!(
                    !content.contains(pattern),
                    "{} must not use legacy `{pattern}` terminology; use connection_id",
                    file.display()
                );
            }
        }
    }
}

#[test]
fn video_source_does_not_expose_processor_or_encoder_control() {
    let video_mod = repo_file("crates/bmc-feature-api/src/video.rs");
    let trait_start = video_mod.find("pub trait VideoSource").unwrap();
    let trait_body = &video_mod[trait_start..];

    for forbidden in [
        "processor(",
        "set_bitrate",
        "set_encoder",
        "encoder_self_check",
        "constraints",
        "config_get",
        "stream_mode",
    ] {
        assert!(
            !trait_body.contains(forbidden),
            "VideoSource must not expose `{forbidden}`; VideoProcessor owns encoder control"
        );
    }
}

#[test]
fn video_source_uses_video_runtime_to_publish_frames() {
    let mahogany_video = repo_file("src/bmc/backends/mahogany/video.rs");
    assert!(
        mahogany_video.contains("runtime::ensure_runtime"),
        "mahogany video backend should attach through video runtime"
    );
    let forwarder = repo_file("src/bmc/backends/mahogany/video_forwarder.rs");
    assert!(
        forwarder.contains("publish_raw_frame_with_trace"),
        "mahogany video frame forwarder should publish frames to video runtime"
    );
}

#[test]
fn mahogany_video_reset_requests_bmc_full_frame() {
    let protocol = repo_file("crates/bmc-backend-mahogany/src/protocol.rs");
    assert!(
        protocol.contains("pub fn build_invalidate_full_frame")
            && protocol.contains("msg::INVALIDATE"),
        "Mahogany protocol must expose an INVALIDATE builder for full-frame refresh"
    );

    let pool = repo_file("crates/bmc-backend-mahogany/src/pool.rs");
    assert!(
        pool.contains("pub async fn request_full_frame")
            && pool.contains("build_invalidate_full_frame")
            && pool.contains("initial framebuffer invalidate requested")
            && pool.contains("display changed to {}x{}, framebuffer invalidate requested={}"),
        "Mahogany pool must request BMC full-frame refresh on connect and display resize"
    );

    let video = repo_file("crates/bmc-backend-mahogany/src/video.rs");
    let video_adapter = repo_file("src/bmc/backends/mahogany/video.rs");
    assert!(
        video.contains("self.lease.entry().request_full_frame().await")
            && video_adapter.contains("entry.request_full_frame().await"),
        "Mahogany video lifecycle must request a BMC full frame on reset and new video connection"
    );
    assert!(
        !video.contains("publish_snapshot().await"),
        "Mahogany video lifecycle must not synthesize base frames from local snapshots"
    );

    let framebuffer = repo_file("crates/bmc-backend-mahogany/src/framebuffer.rs");
    assert!(
        framebuffer.contains("self.dirty = false;\n        self.frame_start = None;"),
        "Framebuffer resize must clear the cache without publishing a blank frame"
    );
}

#[test]
fn mahogany_pool_does_not_expose_debug_lease_queries() {
    let pool = repo_file("crates/bmc-backend-mahogany/src/pool.rs");
    for forbidden in ["pub fn contains", "pub fn lease_count"] {
        assert!(
            !pool.contains(forbidden),
            "DevicePool must not expose shallow debug interface `{forbidden}`"
        );
    }
}

#[test]
fn frontend_transport_buttons_reflect_active_transport() {
    let popover = repo_file("frontend/src/features/video/VideoConfigPopover.vue");
    assert!(
        popover.contains("const activeTransport = computed(() => videoStore.activeTransport)"),
        "Video transport controls must read activeTransport for current transport state"
    );
    assert!(
        popover.contains(":aria-pressed=\"activeTransport === 'webrtc'\"")
            && popover.contains(":aria-pressed=\"activeTransport === 'webtransport'\""),
        "WebRTC/WebTransport buttons must expose pressed state from activeTransport"
    );
    assert!(
        popover.contains(":class=\"activeTransport === 'webrtc' && 'border-primary bg-primary/10'")
            && popover.contains(
                ":class=\"activeTransport === 'webtransport' && 'border-primary bg-primary/10'",
            ),
        "WebRTC/WebTransport visual pressed state must reflect activeTransport"
    );
    assert!(
        !popover.contains("transportPreference === 'webrtc'")
            && !popover.contains("transportPreference === 'webtransport'"),
        "Transport button pressed state must not be driven by preference"
    );
    assert!(
        popover.contains("(e: 'update:transport', value: 'webrtc' | 'webtransport'): void")
            && popover.contains("emit('update:transport', value)"),
        "Transport button clicks must request an actual transport switch"
    );

    let action_bar = repo_file("frontend/src/components/ActionBar.vue");
    assert!(
        action_bar.contains("@update:transport=\"emit('updateVideoTransport', $event)\""),
        "ActionBar must forward video transport switch requests"
    );

    let console = repo_file("frontend/src/views/ConsoleView.vue");
    assert!(
        console.contains("async function handleVideoTransportChange")
            && console.contains("await connectWebTransportOnly(videoMode.value)")
            && console.contains("await connectWebRTCOnly(videoMode.value)")
            && console.contains("@update-video-transport=\"handleVideoTransportChange\""),
        "ConsoleView must execute transport switches for the current codec"
    );
}

#[test]
fn frontend_connection_stats_are_transport_and_connection_scoped() {
    let stats_sheet = repo_file("frontend/src/features/video/StatsSheet.vue");
    assert!(
        stats_sheet.contains("activeTransport: 'http' | 'webrtc' | 'webtransport'")
            && stats_sheet.contains("webtransportStats?: WebTransportStats")
            && stats_sheet.contains("webrtcConnectionId?: string | null")
            && stats_sheet.contains("webtransportConnectionId?: string | null"),
        "StatsSheet must receive explicit active transport, per-transport stats, and connection ids"
    );
    assert!(
        stats_sheet.contains("findTraceSession(traceSnapshot.value?.webrtc_sessions, props.webrtcConnectionId)")
            && stats_sheet.contains(
                "findTraceSession(traceSnapshot.value?.webtransport_sessions, props.webtransportConnectionId)",
            ),
        "StatsSheet trace data must be selected by the active connection_id"
    );
    assert!(
        !stats_sheet.contains("webrtc_sessions?.[0]?.gstreamer")
            && !stats_sheet.contains("webtransport_sessions?.[0]")
            && !stats_sheet.contains("isWebRTC ? 'WebRTC' : 'MJPEG'"),
        "StatsSheet must not silently use the first trace session or a WebRTC/MJPEG-only transport label"
    );
    assert!(
        stats_sheet.contains("{{ transportLabel }}")
            && stats_sheet.contains("v-if=\"isWebTransport\"")
            && stats_sheet.contains("WebTransport Pipeline")
            && stats_sheet.contains("processor.webtransport_connections"),
        "StatsSheet must expose WebTransport stats and backend trace alongside WebRTC/MJPEG"
    );

    let console = repo_file("frontend/src/views/ConsoleView.vue");
    assert!(
        console.contains(":active-transport=\"activeTransport\"")
            && console.contains(":webtransport-stats=\"webtransport.stats.value\"")
            && console.contains(":webrtc-connection-id=\"webrtc.connectionId.value\"")
            && console.contains(":webtransport-connection-id=\"webtransport.connectionId.value\"")
            && !console.contains(":ws-latency=\"0\""),
        "ConsoleView must pass real transport state into StatsSheet and must not inject fake latency"
    );

    let api = repo_file("frontend/src/api/index.ts");
    assert!(
        api.contains("webtransport_sessions?: Array")
            && api.contains("trace: (connectionId?: string | null)")
            && api.contains(
                "`/feature/video/trace?connection_id=${encodeURIComponent(connectionId)}`"
            )
            && api.contains("mjpeg_encoder?: Record<string, unknown>")
            && api.contains("expected_caps?: Record<string, unknown>")
            && api.contains("encoder?: Record<string, unknown>")
            && api.contains("stream?: Record<string, unknown>"),
        "frontend trace API must support per-connection stats and WebTransport/MJPEG trace typing"
    );

    let processor = repo_file("src/bmc/feature/video/processor.rs");
    assert!(
        processor.contains("\"webtransport_sessions\": webtransport_sessions")
            && processor.contains("\"mjpeg_encoder\"")
            && processor.contains("pub fn record_mjpeg_encode_time")
            && processor.contains("\"webtransport_connections\": webtransport_sessions.len()")
            && processor.contains("pub async fn register_webtransport_connection")
            && processor.contains("impl Drop for WebTransportRegistration"),
        "video processor trace must include per-connection WebTransport sessions and MJPEG encode stats"
    );

    let handler = repo_file("src/bmc/feature/video/handler.rs");
    assert!(
        handler.contains("pub struct TraceQuery")
            && handler.contains("pub connection_id: Option<String>")
            && handler.contains("Query(query): Query<TraceQuery>")
            && handler.contains("dev.get_instance(\"video\", connection_id).await"),
        "video trace endpoint must be able to read the active connection_id feature instance"
    );

    let http_stream = repo_file("src/bmc/feature/video/http_stream.rs");
    assert!(
        http_stream.contains("processor.record_mjpeg_encode_time(encode_start.elapsed())"),
        "HTTP MJPEG transport must record real encode timing into the video processor trace"
    );

    let webtransport = repo_file("src/bmc/feature/video/webtransport.rs");
    assert!(
        webtransport.contains("register_webtransport_connection")
            && webtransport.contains("let _registration = processor")
            && webtransport.contains("record_packet(packet.data.len(), keyframe)")
            && webtransport.contains("record_encode_error()")
            && webtransport.contains("record_write_error()"),
        "WebTransport handler must register trace sessions and record packet/error counters"
    );
}

#[test]
fn connection_stats_do_not_drop_webrtc_latency_sources() {
    let gst_webrtc = repo_file("src/bmc/stream/gst_webrtc.rs");
    assert!(
        gst_webrtc.contains("pub fn perf_snapshot(&self) -> serde_json::Value")
            && gst_webrtc.contains("self.inner.encode_count.load(Ordering::Relaxed)")
            && gst_webrtc.contains("self.inner.encode_time_total_us.load(Ordering::Relaxed)")
            && !gst_webrtc.contains("self.inner.encode_count.swap(0")
            && !gst_webrtc.contains("self.inner.encode_time_total_us.swap(0"),
        "GStreamer trace snapshots must not clear encode timing; the stats sheet polls this endpoint"
    );
    assert!(
        gst_webrtc.contains("pipeline.by_name(\"enc\")")
            && gst_webrtc.contains("push_times.push_back(Instant::now())")
            && gst_webrtc.contains("pop_front()"),
        "GStreamer encode latency should sample frame-in to encoder-output without relying on RTP payloader PTS"
    );

    let use_webrtc = repo_file("frontend/src/composables/useWebRTC.ts");
    assert!(
        use_webrtc.contains("selectedCandidatePairId")
            && use_webrtc.contains("stat.type === 'transport'")
            && use_webrtc.contains("stat.type === 'remote-inbound-rtp'")
            && use_webrtc.contains("remoteInboundRtt")
            && use_webrtc
                .contains("stats.value.roundTripTime = selectedPairRtt || remoteInboundRtt"),
        "WebRTC stats must read RTT from selected candidate pair and remote-inbound-rtp fallbacks"
    );
}

#[test]
fn webtransport_accepts_before_video_backend_setup() {
    let webtransport = repo_file("src/bmc/feature/video/webtransport.rs");
    let handler = webtransport
        .split("async fn handle_incoming_session")
        .nth(1)
        .expect("webtransport handler should exist");
    let accept_pos = handler
        .find(".accept()")
        .expect("webtransport handler must accept the session");
    assert!(
        handler.contains("let fc = ticket")
            && handler.contains(".connection")
            && handler.contains(".upgrade()")
            && !handler.contains("get_or_create_feature_instance"),
        "WebTransport handshakes must use the ticket's original connection without resolving a replacement"
    );
    let acquire_pos = handler
        .find("acquire_connected_feature")
        .expect("webtransport handler must acquire the video feature");
    let replay_pos = handler
        .find("replay_initial_base_frame")
        .expect("webtransport handler must request an initial base frame");

    assert!(
        accept_pos < acquire_pos && accept_pos < replay_pos,
        "WebTransport must accept the browser session before waiting on BMC/video setup"
    );
    assert!(
        handler.contains("connection.close(VarInt::from_u32(1), b\"webtransport_setup_failed\")")
            && !handler.contains("connection.close(VarInt::from_u32(1), err.as_bytes())"),
        "Accepted WebTransport sessions must close with a sanitized public reason when backend setup fails"
    );
}

#[test]
fn frontend_webtransport_lifecycle_is_isolated_and_first_frame_driven() {
    let webtransport = repo_file("frontend/src/composables/useWebTransport.ts");
    assert!(
        webtransport.contains("WEBTRANSPORT_READY_TIMEOUT_MS")
            && webtransport.contains("'webtransport_ready_timeout'")
            && webtransport.contains("await withTimeout(nextTransport.ready"),
        "WebTransport frontend connect must timeout ready instead of waiting indefinitely"
    );
    assert!(
        webtransport.contains("connectionGeneration")
            && webtransport.contains("assertActiveGeneration(generation, deviceId)")
            && webtransport.contains("deviceId !== currentBmcIdFromLocation()")
            && webtransport.contains("isActiveTransport(activeTransport, generation)")
            && webtransport.contains("generation !== connectionGeneration"),
        "WebTransport frontend must isolate stale transports, readers, and decoder callbacks"
    );
    let connect = webtransport
        .split("async function connect(")
        .nth(1)
        .expect("WebTransport connect must exist");
    let generation_pos = connect
        .find("const generation = connectionGeneration + 1")
        .expect("WebTransport must reserve its attempt before cleanup");
    let cleanup_pos = connect
        .find("await disconnect()")
        .expect("WebTransport must clean up before connecting");
    assert!(
        generation_pos < cleanup_pos
            && connect.contains("createWebTransportApi(captureDeviceRequest().request)")
            && connect.contains("await api.connectWebTransport(")
            && webtransport.contains("capabilitiesDeviceId === deviceId"),
        "WebTransport must reserve cancellation ownership before its first await and keep requests/capabilities device-scoped"
    );
    assert!(
        webtransport.contains("function abortCurrentConnection")
            && webtransport.contains("error.value = message")
            && webtransport.contains("state.value = 'failed'")
            && webtransport.contains("abortController?.abort()")
            && webtransport.contains("function closeDecoder()")
            && webtransport.contains("activeDecoder.state === 'closed'")
            && webtransport.contains("InvalidStateError")
            && webtransport.contains("closeDecoder()"),
        "WebTransport frontend must surface connection failure reasons and tear down failed transports/decoders idempotently"
    );

    let video_stream = repo_file("frontend/src/features/video/useVideoStream.ts");
    assert!(
        video_stream.contains("const webTransportProgressStages")
            && video_stream.contains("const webTransportConnectStage")
            && video_stream.contains("opts.webtransport.stats.value.framesDecoded === 0"),
        "Video UI must expose WebTransport-specific connect and first-frame stages"
    );
    assert!(
        video_stream.contains("watch(opts.webtransport.stats")
            && video_stream.contains("stats.framesDecoded > lastWebTransportDecodedFrames")
            && video_stream.contains("transitionLifecycle({ type: 'frame_received' })")
            && video_stream.contains("videoLoading.value = false")
            && video_stream.contains("systemStore.setStreamOnline(true)"),
        "WebTransport video readiness must be driven by a fresh decoded-frame delta"
    );
    assert!(
        video_stream.contains("function isSupersededWebTransportConnect")
            && video_stream
                .contains("if (isSupersededWebTransportConnect(webTransportError)) return"),
        "Superseded WebTransport connect attempts must not trigger transport fallback"
    );
    assert!(
        video_stream.contains("options: { fallbackToWebRTC?: boolean } = {}")
            && video_stream.contains("if (!options.fallbackToWebRTC)")
            && video_stream.contains("await connectWebTransportOnly(codec, { fallbackToWebRTC: true })"),
        "Explicit WebTransport selection must not silently fall back to WebRTC; only automatic preference-based selection may fall back"
    );
    assert!(
        video_stream.contains("watch(() => opts.webtransport.state.value")
            && video_stream
                .contains("opts.webtransport.error.value || 'WebTransport connection failed'")
            && video_stream.contains("systemStore.setStreamOnline(false)"),
        "WebTransport failed state must update the shared video status"
    );

    let popover = repo_file("frontend/src/features/video/VideoConfigPopover.vue");
    assert!(
        popover.contains("activeTransport.value !== value || value === 'webtransport'"),
        "WebTransport transport button must be able to re-emit when already selected so a failed connection can be retried"
    );

    let console = repo_file("frontend/src/views/ConsoleView.vue");
    assert!(
        console.contains("&& !isWebTransportMode.value")
            && console.contains("await connectWebRTCOnly(videoMode.value)"),
        "Console activation must not auto-connect WebRTC while WebTransport is the active transport"
    );
}

#[test]
fn frontend_mjpeg_first_frame_deadline_does_not_invent_no_signal() {
    let source = repo_file("frontend/src/features/video/useVideoStream.ts");
    assert!(
        source.contains("MJPEG_FIRST_FRAME_DEADLINE_MS")
            && source.contains("handleStreamStateChanged(status.signal)")
            && source.contains("mjpegTimestamp.value = 0"),
        "MJPEG may bound a stalled request, but no_signal must come from typed backend status"
    );
    let deadline = source
        .find("mjpegFirstFrameDeadlineId = window.setTimeout")
        .unwrap();
    let end = source[deadline..]
        .find("}, MJPEG_FIRST_FRAME_DEADLINE_MS)")
        .unwrap();
    assert!(
        !source[deadline..deadline + end].contains("streamSignalState.value = 'no_signal'"),
        "elapsed time must not be interpreted as no_signal"
    );
}

#[test]
fn mjpeg_producer_releases_its_lease_when_the_browser_surface_unmounts() {
    let source = repo_file("src/bmc/feature/video/http_stream.rs");
    assert!(
        source.contains("_ = tx.closed()") && source.contains("let _guard = guard"),
        "silent MJPEG producers must stop and release their FeatureGuard when the response receiver closes"
    );

    let console = repo_file("frontend/src/views/ConsoleView.vue");
    assert!(
        console.contains(
            "v-if=\"isHttpMode && lifecyclePresentation.mountSurface && mjpegFrameUrl\""
        ),
        "the MJPEG image must follow the same lifecycle-owned surface mount policy as video and canvas"
    );

    let lifecycle = repo_file("frontend/src/features/video/useVideoStream.ts");
    assert!(
        lifecycle.contains("http: () => {\n        stopMjpegStream()")
            && lifecycle.contains("mjpegTimestamp.value = 0")
            && lifecycle.contains("mjpegFrameReceived.value = false"),
        "an inactive HTTP video signal must abort Fetch and release its Blob-backed surface"
    );
}

#[test]
fn msd_frontend_and_feature_routes_keep_a_typed_runtime_contract() {
    let api = repo_file("frontend/src/api/index.ts");
    assert!(
        api.contains("export interface MsdImageCatalog")
            && api.contains("location: MsdStorageLocation")
            && api.contains("images: MsdImage[]")
            && api.contains("listImageCatalog:")
            && api.contains("(await msdApi.listImageCatalog(location)).images")
            && api.contains("const response = await request<{ items: DriveFile[] }>")
            && api.contains("return response.items"),
        "MSD API helpers must expose typed catalog resources and unwrap list envelopes for legacy callers"
    );
    assert!(
        api.contains("type MsdMediaReference")
            && api.contains("location, image_id: id")
            && api.contains("operations: MsdStorageOperation[]")
            && api.contains("size_mb: sizeMb")
            && api.contains("mutation_id: mutationId")
            && api.contains("downloadStatus: (downloadId: string)")
            && !api.contains("connect: (type: string, ...args: any[])"),
        "MSD frontend requests must use typed storage/media references and poll the backend download status endpoint"
    );

    let handler = repo_file("src/bmc/feature/msd/handler.rs");
    assert!(
        handler.contains("struct MsdConnect")
            && handler.contains("normalize_media_type")
            && handler.contains("resolve_managed_image")
            && handler.contains("image_file(bmc_id, image_id)")
            && handler.contains("MAX_ACTIVE_REMOTE_DOWNLOADS")
            && handler.contains("msd_images_download_status"),
        "MSD handlers must validate storage/media capabilities, resolve managed IDs, bound downloads, and expose a pollable status record"
    );

    let routes = repo_file("src/bmc/feature/msd/mod.rs");
    assert!(
        routes.contains("images/download/status") && routes.contains("msd_images_download_status"),
        "MSD download status must be mounted by the MSD feature module"
    );

    let dialog = repo_file("frontend/src/features/msd/MsdDialog.vue");
    assert!(
        dialog.contains("msdApi.downloadStatus")
            && !dialog.contains("msd.download_progress")
            && !dialog.contains("headersToRecord")
            && !dialog.contains("mountMode"),
        "MSD UI must poll implemented progress and not expose unsupported request headers or mount-mode controls"
    );
}

#[test]
fn image_publication_and_device_scopes_use_the_common_interfaces() {
    for path in [
        "src/bmc/feature/msd/handler.rs",
        "src/bmc/feature/msd/transfer.rs",
    ] {
        let source = repo_file(path);
        assert!(
            !source.contains(".upload_image_file("),
            "{path} bypasses verified publication"
        );
        assert!(
            !source.contains("catalog_digests("),
            "{path} trusts an unversioned remote identity"
        );
        assert!(source.contains(".publish_image_file("));
    }
    for store in ["hid", "audio", "video"] {
        let source = repo_file(&format!("frontend/src/stores/{store}Config.ts"));
        assert!(source.contains("new DeviceScope("));
    }
    assert!(repo_file("frontend/src/stores/system.ts").contains("new DeviceScope("));
    assert!(repo_file("frontend/src/feature-data/resource.ts").contains("new DeviceScope("));
}
