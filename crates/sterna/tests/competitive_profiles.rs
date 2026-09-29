use sterna::config::SternaConfig;

#[test]
fn selected_profile_merges_nested_values_without_changing_base() {
    let text = "[model]\nparent = 'base'\n[limits]\ncells = 40\nkeep_results = 2\n[profiles.review.model]\nparent = 'reviewer'\n[profiles.review.limits]\ncells = 12\n";
    let base = SternaConfig::parse(text).unwrap();
    assert_eq!(base.model.parent.as_deref(), Some("base"));
    let selected = SternaConfig::parse_profile(text, Some("review")).unwrap();
    assert_eq!(selected.model.parent.as_deref(), Some("reviewer"));
    assert_eq!(selected.limits.cells, Some(12));
    assert_eq!(
        selected.limits.keep_results, base.limits.keep_results,
        "a key the profile does not name keeps the base's value"
    );
}

#[test]
fn unknown_profiles_and_invalid_selected_settings_are_rejected() {
    assert!(
        SternaConfig::parse_profile("", Some("missing"))
            .unwrap_err()
            .contains("no profile")
    );
    assert!(
        SternaConfig::parse_profile("[profiles.bad.web]\nunknown = true\n", Some("bad")).is_err()
    );
    assert!(SternaConfig::parse("profiles = 'not a table'").is_err());
}
