//! Clips tab UI — onboarding wizard + grid browser for saved clips.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageState {
    Onboarding,
    Empty,
    Loaded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardStep {
    InstallGsr,
    PickScreen,
    Settings,
}

pub struct ClipsPage {
    pub root: gtk::Stack,
    pub state: Rc<RefCell<PageState>>,
    pub wizard: Rc<WizardWidgets>,
}

pub struct WizardWidgets {
    pub stack: gtk::Stack,
    pub step: RefCell<WizardStep>,
    // Page 1
    pub install_status_label: gtk::Label,
    pub install_next_btn: gtk::Button,
    // Page 2
    pub screen_picked_label: gtk::Label,
    pub screen_next_btn: gtk::Button,
    // Page 3
    pub hotkey_label: gtk::Label,
    pub buffer_scale: gtk::Scale,
    pub storage_label: gtk::Label,
}

pub fn build_clips_page() -> ClipsPage {
    let stack = gtk::Stack::builder()
        .vexpand(true)
        .hexpand(true)
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();

    let wizard = Rc::new(build_wizard());
    stack.add_named(&wizard.stack, Some("onboarding"));
    stack.add_named(&empty_page(), Some("empty"));
    stack.add_named(&loaded_page(), Some("loaded"));

    let state = Rc::new(RefCell::new(PageState::Onboarding));
    stack.set_visible_child_name("onboarding");

    ClipsPage { root: stack, state, wizard }
}

fn build_wizard() -> WizardWidgets {
    let stack = gtk::Stack::builder()
        .vexpand(true)
        .hexpand(true)
        .transition_type(gtk::StackTransitionType::SlideLeftRight)
        .transition_duration(200)
        .build();

    let (page1, install_status_label, install_next_btn) = build_page1_install();
    let (page2, screen_picked_label, screen_next_btn) = build_page2_screen();
    let (page3, hotkey_label, buffer_scale, storage_label) = build_page3_settings();

    stack.add_named(&page1, Some("wizard-1-install"));
    stack.add_named(&page2, Some("wizard-2-screen"));
    stack.add_named(&page3, Some("wizard-3-settings"));
    stack.set_visible_child_name("wizard-1-install");

    WizardWidgets {
        stack,
        step: RefCell::new(WizardStep::InstallGsr),
        install_status_label,
        install_next_btn,
        screen_picked_label,
        screen_next_btn,
        hotkey_label,
        buffer_scale,
        storage_label,
    }
}

fn step_indicator(current: u8, total: u8) -> gtk::Label {
    let lbl = gtk::Label::new(Some(&format!("Step {current} of {total}")));
    lbl.add_css_class("dim-label");
    lbl.add_css_class("caption");
    lbl.set_xalign(0.5);
    lbl
}

fn build_page1_install() -> (gtk::Widget, gtk::Label, gtk::Button) {
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_top(40)
        .margin_bottom(40)
        .margin_start(40)
        .margin_end(40)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();

    page.append(&step_indicator(1, 3));

    let title = gtk::Label::new(Some("Install gpu-screen-recorder"));
    title.add_css_class("title-1");
    page.append(&title);

    let body = gtk::Label::new(Some(
        "Clips uses gpu-screen-recorder, a free open-source Flatpak \
         from Flathub, to capture gameplay efficiently. Pick any of \
         the install methods below — Clips will detect when it's ready.",
    ));
    body.set_wrap(true);
    body.set_max_width_chars(60);
    body.set_xalign(0.5);
    page.append(&body);

    // Primary install button.
    let install_btn = gtk::Button::builder()
        .label("Install")
        .css_classes(["pill", "suggested-action"])
        .halign(gtk::Align::Center)
        .build();
    install_btn.set_action_name(Some("app.gsr-install"));
    page.append(&install_btn);

    // Secondary actions row.
    let alt_label = gtk::Label::new(Some("— or install manually —"));
    alt_label.add_css_class("dim-label");
    page.append(&alt_label);

    let bazaar_btn = gtk::Button::builder()
        .label("Open in Bazaar")
        .css_classes(["pill"])
        .halign(gtk::Align::Center)
        .build();
    bazaar_btn.set_action_name(Some("app.gsr-open-in-bazaar"));
    page.append(&bazaar_btn);

    // Terminal command in a code block + Copy button.
    let code_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .css_classes(["card"])
        .build();
    let cmd_label = gtk::Label::new(Some(crate::clips::gsr_install::GSR_TERMINAL_INSTALL_COMMAND));
    cmd_label.add_css_class("monospace");
    cmd_label.set_selectable(true);
    cmd_label.set_xalign(0.0);
    cmd_label.set_hexpand(true);
    code_box.append(&cmd_label);
    let copy_btn = gtk::Button::builder().label("Copy").build();
    copy_btn.set_action_name(Some("app.gsr-copy-cli"));
    code_box.append(&copy_btn);
    page.append(&code_box);

    // Status label (reflects install progress when active).
    let install_status_label = gtk::Label::new(None);
    install_status_label.add_css_class("dim-label");
    install_status_label.set_visible(false);
    page.append(&install_status_label);

    // Next button — disabled until is_installed() returns true.
    let install_next_btn = gtk::Button::builder()
        .label("Next")
        .css_classes(["pill", "suggested-action"])
        .halign(gtk::Align::End)
        .sensitive(false)
        .build();
    install_next_btn.set_action_name(Some("app.wizard-next"));
    page.append(&install_next_btn);

    (page.upcast(), install_status_label, install_next_btn)
}

fn build_page2_screen() -> (gtk::Widget, gtk::Label, gtk::Button) {
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_top(40)
        .margin_bottom(40)
        .margin_start(40)
        .margin_end(40)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();

    page.append(&step_indicator(2, 3));

    let title = gtk::Label::new(Some("Pick the screen to record"));
    title.add_css_class("title-1");
    page.append(&title);

    let body = gtk::Label::new(Some(
        "Choose which display Clips should capture from. \
         You can change this later in Settings.",
    ));
    body.set_wrap(true);
    body.set_max_width_chars(60);
    body.set_xalign(0.5);
    page.append(&body);

    let pick_btn = gtk::Button::builder()
        .label("Pick screen")
        .css_classes(["pill", "suggested-action"])
        .halign(gtk::Align::Center)
        .build();
    pick_btn.set_action_name(Some("app.setup-clips"));
    page.append(&pick_btn);

    let screen_picked_label = gtk::Label::new(None);
    screen_picked_label.add_css_class("dim-label");
    screen_picked_label.set_visible(false);
    page.append(&screen_picked_label);

    let screen_next_btn = gtk::Button::builder()
        .label("Next")
        .css_classes(["pill", "suggested-action"])
        .halign(gtk::Align::End)
        .sensitive(false)
        .build();
    screen_next_btn.set_action_name(Some("app.wizard-next"));
    page.append(&screen_next_btn);

    (page.upcast(), screen_picked_label, screen_next_btn)
}

fn build_page3_settings() -> (gtk::Widget, gtk::Label, gtk::Scale, gtk::Label) {
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_top(40)
        .margin_bottom(40)
        .margin_start(40)
        .margin_end(40)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();

    page.append(&step_indicator(3, 3));

    let title = gtk::Label::new(Some("Configure clips"));
    title.add_css_class("title-1");
    page.append(&title);

    let body = gtk::Label::new(Some(
        "All settings have sensible defaults. Tweak now \
         or later in Settings.",
    ));
    body.set_wrap(true);
    body.set_max_width_chars(60);
    body.set_xalign(0.5);
    page.append(&body);

    // Hotkey row
    let hotkey_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    hotkey_row.append(&gtk::Label::new(Some("Save hotkey")));
    let hotkey_label = gtk::Label::new(Some("Super+Shift+R"));
    hotkey_label.set_hexpand(true);
    hotkey_label.set_xalign(1.0);
    hotkey_row.append(&hotkey_label);
    let rebind_btn = gtk::Button::builder().label("Change…").build();
    rebind_btn.set_action_name(Some("app.rebind-clip-hotkey"));
    hotkey_row.append(&rebind_btn);
    page.append(&hotkey_row);

    // Buffer length scale
    let buffer_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    buffer_row.append(
        &gtk::Label::builder()
            .label("Buffer length (seconds)")
            .xalign(0.0)
            .build(),
    );
    let buffer_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 30.0, 300.0, 5.0);
    buffer_scale.set_value(60.0);
    buffer_scale.set_draw_value(true);
    buffer_scale.set_value_pos(gtk::PositionType::Right);
    buffer_row.append(&buffer_scale);
    page.append(&buffer_row);

    // Storage path row
    let storage_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    storage_row.append(&gtk::Label::new(Some("Save clips to")));
    let storage_label = gtk::Label::new(Some("~/Videos/Clips"));
    storage_label.set_hexpand(true);
    storage_label.set_xalign(1.0);
    storage_label.set_ellipsize(gtk::pango::EllipsizeMode::Start);
    storage_row.append(&storage_label);
    let pick_storage_btn = gtk::Button::builder().label("Pick folder").build();
    pick_storage_btn.set_action_name(Some("app.pick-clip-storage"));
    storage_row.append(&pick_storage_btn);
    page.append(&storage_row);

    // Done button
    let done_btn = gtk::Button::builder()
        .label("Done")
        .css_classes(["pill", "suggested-action"])
        .halign(gtk::Align::End)
        .build();
    done_btn.set_action_name(Some("app.wizard-next"));
    page.append(&done_btn);

    (page.upcast(), hotkey_label, buffer_scale, storage_label)
}

fn empty_page() -> gtk::Widget {
    let page = adw::StatusPage::builder()
        .icon_name("lucide-clapperboard-symbolic")
        .title("No clips yet")
        .description("Press the save hotkey while gaming to capture the last 60 seconds.")
        .build();
    page.upcast()
}

fn loaded_page() -> gtk::Widget {
    // Real grid view added in Phase 5.
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build()
        .upcast()
}

impl ClipsPage {
    pub fn set_state(&self, new_state: PageState) {
        *self.state.borrow_mut() = new_state;
        match new_state {
            PageState::Onboarding => self.root.set_visible_child_name("onboarding"),
            PageState::Empty => self.root.set_visible_child_name("empty"),
            PageState::Loaded => self.root.set_visible_child_name("loaded"),
        }
    }

    pub fn set_wizard_step(&self, step: WizardStep) {
        *self.wizard.step.borrow_mut() = step;
        let name = match step {
            WizardStep::InstallGsr => "wizard-1-install",
            WizardStep::PickScreen => "wizard-2-screen",
            WizardStep::Settings => "wizard-3-settings",
        };
        self.wizard.stack.set_visible_child_name(name);
    }

    pub fn current_wizard_step(&self) -> WizardStep {
        *self.wizard.step.borrow()
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }
}
