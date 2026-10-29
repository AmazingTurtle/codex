use super::*;
use pretty_assertions::assert_eq;

#[test]
fn workflow_limits_accept_boundaries_and_reject_invalid_values() {
    for max_agents in [1, 32, 1024] {
        let limits: DynamicWorkflowsConfig =
            toml::from_str(&format!("max_agents = {max_agents}\nmax_tokens = 1000"))
                .expect("valid workflow limits");
        assert_eq!(
            limits,
            DynamicWorkflowsConfig {
                max_agents,
                max_tokens: NonZeroU64::new(1000)
            }
        );
    }
    for input in [
        "max_agents = 0",
        "max_agents = 1025",
        "max_agents = -1",
        "max_tokens = 0",
        "max_tokens = -1",
        "max_tokens = 9007199254740992",
    ] {
        assert!(
            toml::from_str::<DynamicWorkflowsConfig>(input).is_err(),
            "{input}"
        );
    }
    let limits: DynamicWorkflowsConfig =
        toml::from_str("max_tokens = 9007199254740991").expect("largest exact JS integer");
    assert_eq!(limits.max_tokens, NonZeroU64::new(9_007_199_254_740_991));
}
