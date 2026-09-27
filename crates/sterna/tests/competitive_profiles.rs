use sterna::config::SternaConfig;

#[test]
fn selected_profile_merges_nested_values_without_changing_base() {
    let text = "[model]\nparent = 'base'\n[helpers.effort]\nfind = 'low'\ncheck = 'high'\n[profiles.review.model]\nparent = 'reviewer'\n[profiles.review.helpers.effort]\nfind = 'medium'\n";
    let base = SternaConfig::parse(text).unwrap();
    assert_eq!(base.model.parent.as_deref(), Some("base"));
    let selected = SternaConfig::parse_profile(text, Some("review")).unwrap();
    assert_eq!(selected.model.parent.as_deref(), Some("reviewer"));
    assert_eq!(selected.helpers.effort.find, sterna::wire::Effort::Medium);
    assert_eq!(selected.helpers.effort.check, sterna::wire::Effort::High);
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
