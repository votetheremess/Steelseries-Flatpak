//! Clips tab UI — grid browser for saved clips.

use adw::prelude::*;

/// Build the Clips tab content. Initially shows the empty state; populated by
/// later tasks in this plan.
pub fn build_clips_page() -> gtk::Widget {
    let page = adw::StatusPage::builder()
        .icon_name("lucide-clapperboard-symbolic")
        .title("Clips")
        .description("No clips yet")
        .build();
    page.upcast()
}
