//! Lines a background check leaves for the person, outside the
//! conversation: a newer Sterna installed (`update.rs`) and a subscription
//! window nearly spent (`usage.rs`). Both checks run on their own threads
//! at start; what they found is said once, at the next task's end.

use super::{ui, usage};

/// At the start of a terminal session: say a newer release already
/// installed, name a retired update opt-out, and start both background checks.
pub(super) fn at_start(gateway: &crate::gateway::Gateway) {
    if let Some(notice) =
        crate::update::Install::of_running().and_then(|i| crate::update::installed_notice(&i))
    {
        ui::output(notice);
    }
    if let Some(notice) = crate::update::retired_opt_out_notice() {
        ui::output(notice);
    }
    crate::update::check_in_background();
    usage::check_in_background(gateway);
}

/// After a task: whatever the checks have found since.
pub(super) fn at_task_end() {
    if let Some(notice) = crate::update::take_notice() {
        ui::output(notice);
    }
    for warning in usage::take_warnings() {
        ui::output(warning);
    }
    // Reaped only: each note reached the screen when its work ended.
    super::after::settle(false);
}
