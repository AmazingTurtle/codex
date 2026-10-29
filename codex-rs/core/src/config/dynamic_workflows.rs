use super::DynamicWorkflowsMode;
use super::ManagedFeatures;
use codex_features::Feature;
use codex_features::FeatureToml;
use codex_features::FeaturesToml;

pub(super) fn resolve_dynamic_workflows_mode(
    config: Option<&FeaturesToml>,
    features: &ManagedFeatures,
) -> DynamicWorkflowsMode {
    if !features.enabled(Feature::DynamicWorkflows) {
        return DynamicWorkflowsMode::Off;
    }
    config
        .and_then(|features| features.dynamic_workflows.as_ref())
        .and_then(|feature| match feature {
            FeatureToml::Enabled(_) => None,
            FeatureToml::Config(config) => config.mode,
        })
        .unwrap_or_default()
}
