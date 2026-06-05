
use crate::bmc::connection::registry::ConnectionTemplate;
use crate::bmc::registry::HasId;

/// A protocol backend registered in the PROTOCOL_BACKENDS registry.
/// Each backend declares one or more ConnectionTemplates.
pub struct ProtocolBackend {
    pub id: &'static str,
    pub name: &'static str,
    pub connections: &'static [ConnectionTemplate],
}

impl HasId for ProtocolBackend {
    fn id(&self) -> &str {
        self.id
    }
}

impl HasId for ConnectionTemplate {
    fn id(&self) -> &str {
        self.id
    }
}

pub(crate) static PROTOCOL_BACKENDS: &[ProtocolBackend] = &[
    ProtocolBackend {
        id: "mahogany",
        name: "Mahogany KVM",
        connections: &[
            ConnectionTemplate {
                id: "kvm",
                name: "KVM over TCP (video + HID + MSD)",
                features: &["video", "hid", "msd"],
                connect: crate::bmc::backends::general::mahogany::connection::mahogany_kvm_connect,
            },
            ConnectionTemplate {
                id: "power",
                name: "IPMI Power Control",
                features: &["power"],
                connect: crate::bmc::backends::general::ipmi::power::ipmi_power_connect,
            },
        ],
    },
];

/// Returns the list of feature IDs that a given protocol provides.
pub fn features_for_protocol(protocol: &str) -> Vec<String> {
    let mut features = Vec::new();
    if let Some(backend) = crate::bmc::registry::find_backend(PROTOCOL_BACKENDS, protocol) {
        for ct in backend.connections {
            for f in ct.features {
                if !features.contains(&f.to_string()) {
                    features.push(f.to_string());
                }
            }
        }
    }
    features
}
