use crate::interface::config::Config;

#[test]
fn test_config_example() {
    assert!(Config::example().is_ok());
}
