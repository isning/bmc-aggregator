use std::sync::Arc;

use futures::future::BoxFuture;

use crate::bmc::feature::connection::FeatureConnection;

pub type RequirementInspectFn = Arc<
    dyn Fn(
            Arc<FeatureConnection>,
        ) -> BoxFuture<
            'static,
            Result<Vec<crate::bmc::feature::requirements::BackendRequirement>, String>,
        > + Send
        + Sync,
>;
pub type RequirementRemediateFn = Arc<
    dyn Fn(
            Arc<FeatureConnection>,
            crate::bmc::feature::requirements::RequirementId,
            crate::bmc::feature::requirements::RemediationAction,
        ) -> BoxFuture<
            'static,
            Result<crate::bmc::feature::requirements::RemediationResult, String>,
        > + Send
        + Sync,
>;

pub struct BackendRequirementHooks {
    pub inspect: RequirementInspectFn,
    pub remediate: RequirementRemediateFn,
}

type FeatureConnectFn =
    Arc<dyn Fn(Arc<FeatureConnection>) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

/// Template registered by each feature backend.
pub struct FeatureBackendTemplate {
    pub backend: &'static str,
    pub feature: &'static str,
    pub connect: FeatureConnectFn,
    pub requirements: Option<BackendRequirementHooks>,
}

impl std::fmt::Debug for FeatureBackendTemplate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeatureBackendTemplate")
            .field("backend", &self.backend)
            .field("feature", &self.feature)
            .field("requirements", &self.requirements.is_some())
            .finish()
    }
}

#[macro_export]
macro_rules! define_backends {
    (@requirements $feature_mod:ident requirements) => {
        Some($crate::bmc::feature::registry::BackendRequirementHooks {
            inspect: std::sync::Arc::new(|fc| Box::pin($feature_mod::requirements(fc))),
            remediate: std::sync::Arc::new(|fc, requirement_id, action| Box::pin($feature_mod::remediate_requirement(fc, requirement_id, action))),
        })
    };
    (@requirements $feature_mod:ident) => { None };
    ( $( $backend:expr => $feature_id:expr => $feature_mod:ident $(=> $requirements:ident)? ),+ $(,)? ) => {{
        use std::sync::Arc;
        use std::sync::OnceLock;
        use $crate::bmc::feature::registry::FeatureBackendTemplate;
        static BACKENDS: OnceLock<Vec<FeatureBackendTemplate>> = OnceLock::new();
        BACKENDS.get_or_init(|| {
            vec![
                $(
                    FeatureBackendTemplate {
                        backend: $backend,
                        feature: $feature_id,
                        connect: Arc::new(|fc: Arc<$crate::bmc::feature::connection::FeatureConnection>| Box::pin($feature_mod::connect(fc))),
                        requirements: $crate::define_backends!(@requirements $feature_mod $($requirements)?),
                    },
                )+
            ]
        })
    }};
}
