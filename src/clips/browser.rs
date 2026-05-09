//! Clips tab UI — onboarding wizard + grid browser for saved clips.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk::glib::subclass::prelude::*;

mod clip_object {
    use std::cell::RefCell;
    use std::path::PathBuf;

    use gtk::glib;
    use gtk::glib::subclass::prelude::*;

    use crate::clips::library::ClipMeta;

    #[derive(Default)]
    pub struct ClipObjectImpl {
        pub meta: RefCell<ClipMeta>,
        pub storage_dir: RefCell<PathBuf>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ClipObjectImpl {
        const NAME: &'static str = "ArctisClipObject";
        type Type = super::ClipObject;
    }

    impl ObjectImpl for ClipObjectImpl {}
}

glib::wrapper! {
    pub struct ClipObject(ObjectSubclass<clip_object::ClipObjectImpl>);
}

impl ClipObject {
    pub fn new(meta: crate::clips::library::ClipMeta, storage_dir: std::path::PathBuf) -> Self {
        let obj: Self = glib::Object::new();
        *obj.imp().meta.borrow_mut() = meta;
        *obj.imp().storage_dir.borrow_mut() = storage_dir;
        obj
    }

    pub fn meta(&self) -> crate::clips::library::ClipMeta {
        self.imp().meta.borrow().clone()
    }

    pub fn storage_dir(&self) -> std::path::PathBuf {
        self.imp().storage_dir.borrow().clone()
    }
}

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

    let app_store_btn = gtk::Button::builder()
        .label("Open in app store")
        .css_classes(["pill"])
        .halign(gtk::Align::Center)
        .build();
    app_store_btn.set_action_name(Some("app.gsr-open-in-app-store"));
    page.append(&app_store_btn);

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

/// Explicit refs to the per-card child widgets, attached to each `ListItem`
/// via GLib data so the bind step does not have to walk the widget tree
/// with `first_child()` / `last_child()` (which would silently break the
/// moment a kebab button or any other peer widget is added in Task 5.5).
#[derive(Clone)]
struct CardWidgets {
    image: gtk::Picture,
    title: gtk::Label,
}

fn build_clip_card() -> (gtk::Box, CardWidgets) {
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    card.add_css_class("clip-card");

    let image = gtk::Picture::builder()
        .height_request(180)
        .width_request(320)
        .build();
    image.add_css_class("clip-thumb");

    let title = gtk::Label::builder()
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(30)
        .xalign(0.0)
        .build();
    title.add_css_class("clip-title");

    card.append(&image);
    card.append(&title);

    (card, CardWidgets { image, title })
}

fn bind_clip_card(widgets: &CardWidgets, clip: &ClipObject) {
    let meta = clip.meta();
    let storage_dir = clip.storage_dir();
    let game = if meta.game_name.is_empty() {
        "Untitled"
    } else {
        meta.game_name.as_str()
    };
    widgets.title.set_label(game);

    // Spawn a worker thread to extract the thumbnail via ffmpeg, then hop
    // back to the GTK main thread to set the picture's filename.
    //
    // We use a `SendWeakRef` instead of a plain `WeakRef<gtk::Picture>`
    // because `gtk::Picture` is `!Send` (GTK widgets are bound to the main
    // thread), and `WeakRef<T>` requires `T: Send + Sync` to itself be
    // `Send`. `SendWeakRef` allows the weak reference to cross threads
    // and panics if dereferenced off the original thread — but we only
    // upgrade it inside the `MainContext::default().invoke(...)` closure,
    // which runs back on the main thread, so the invariant holds.
    let filename = meta.filename;
    let storage_for_worker = storage_dir;
    let img_weak: glib::SendWeakRef<gtk::Picture> = widgets.image.downgrade().into();
    std::thread::spawn(move || {
        if let Ok(thumb) = crate::clips::thumbnail::ensure_thumbnail(&storage_for_worker, &filename)
        {
            glib::MainContext::default().invoke(move || {
                if let Some(img) = img_weak.upgrade() {
                    img.set_filename(Some(&thumb));
                }
            });
        }
    });
}

fn loaded_page() -> gtk::Widget {
    use crate::clips::library;

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();

    let storage_dir = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join("Videos/Clips");
    let _ = std::fs::create_dir_all(&storage_dir);

    let model = gtk::gio::ListStore::new::<ClipObject>();
    for meta in library::reconcile(&storage_dir) {
        model.append(&ClipObject::new(meta, storage_dir.clone()));
    }

    // Phase 1's FIFO reader emits `Saved { duration_ms: 0 }` (ffprobe was
    // deferred out of the FIFO reader to avoid blocking the next save), and
    // entries created by `reconcile()` for files not yet in the index also
    // start at zero. Spawn a worker thread to backfill missing durations
    // by ffprobing each file. The worker writes the updated index in place
    // and the new values surface on the next browser-open / app launch
    // (see `library::backfill_durations` for the rationale).
    {
        let storage_for_backfill = storage_dir.clone();
        std::thread::spawn(move || {
            if let Err(e) = library::backfill_durations(&storage_for_backfill) {
                log::warn!("clip duration backfill failed: {e}");
            }
        });
    }

    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_factory, item| {
        let item = item
            .downcast_ref::<gtk::ListItem>()
            .expect("setup signal item must be a ListItem");
        let (card, widgets) = build_clip_card();
        // Stash the explicit widget refs on the ListItem so bind() can
        // retrieve them without walking the widget tree.
        //
        // SAFETY: We attach a `CardWidgets` value (a `Clone` struct of
        // owned widget refs) under a stable string key. The matching
        // retrieval in `connect_bind` reads back with the same `T`, and
        // `connect_unbind` (eventually, in Task 5.5+) would `steal_data`
        // with the same `T`. The only invariant `set_data` requires is
        // that the type at retrieval matches the type at storage; that
        // holds here because the key is unique to this factory.
        unsafe {
            item.set_data::<CardWidgets>("card-widgets", widgets);
        }
        item.set_child(Some(&card));
    });
    factory.connect_bind(|_factory, item| {
        let item = item
            .downcast_ref::<gtk::ListItem>()
            .expect("bind signal item must be a ListItem");
        // SAFETY: `card-widgets` was set in `connect_setup` with the same
        // `CardWidgets` type. The pointer remains valid for the life of
        // the ListItem (until `steal_data`, which we do not call), so the
        // `as_ref()` borrow + clone here is sound.
        let widgets: CardWidgets = unsafe {
            item.data::<CardWidgets>("card-widgets")
                .map(|p| p.as_ref().clone())
                .expect("card-widgets attached during setup")
        };
        let clip = item
            .item()
            .and_then(|o| o.downcast::<ClipObject>().ok())
            .expect("model item must be a ClipObject");
        bind_clip_card(&widgets, &clip);
    });

    let grid = gtk::GridView::builder()
        .model(&gtk::SingleSelection::new(Some(model)))
        .factory(&factory)
        .min_columns(2)
        .max_columns(5)
        .build();

    scroll.set_child(Some(&grid));
    scroll.upcast()
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

#[cfg(test)]
mod object_tests {
    //! Round-trip test for the `ClipObject` GLib subclass.
    //!
    //! Marked `#[ignore]` because `gtk::init()` must succeed for the type
    //! system to register the subclass, and that fails in headless CI
    //! environments without a display server. The test is preserved for
    //! manual verification:
    //!
    //!   distrobox enter fedora-dev -- cargo test \
    //!       clips::browser::object_tests -- --ignored
    //!
    //! Since the subclass logic is purely RefCell + clone (no GTK behavior
    //! beyond the type registration), running this on a developer machine
    //! with a session bus is sufficient — there is no value in gating the
    //! whole CI pipeline on it.
    use super::*;
    use crate::clips::library::ClipMeta;

    #[test]
    #[ignore]
    fn clip_object_round_trips_meta() {
        gtk::init().ok();
        let mut m = ClipMeta::default();
        m.filename = "test.mp4".into();
        m.duration_ms = 60_000;
        let dir = std::path::PathBuf::from("/tmp");
        let obj = ClipObject::new(m.clone(), dir.clone());
        assert_eq!(obj.meta(), m);
        assert_eq!(obj.storage_dir(), dir);
    }
}
