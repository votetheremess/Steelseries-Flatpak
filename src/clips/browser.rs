//! Clips tab UI — onboarding wizard + grid browser for saved clips.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;
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
    /// The remix panel is visible. Entered via a card kebab's "Remix"
    /// item; exits back to `Loaded` via the panel's Close button or after
    /// a successful Export.
    Remix,
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
    /// Backing model for the loaded-state `GridView`. Held here so that
    /// per-card kebab actions (Rename / Delete) can clear+repopulate it
    /// after mutating the storage dir without having to walk down through
    /// the `gtk::Stack` to find it. `gio::ListStore` is a GObject; clones
    /// are cheap reference bumps.
    pub model: gio::ListStore,
    /// Filesystem directory where the user's saved `.mp4` clips live.
    /// Wrapped in `Rc<RefCell>` so `set_storage_dir` (called from
    /// Settings → Clips → Pick folder, and from the wizard Page 3 picker)
    /// can update it live without rebuilding the page. All consumers
    /// (kebab actions, on_remix, factory bind) hold a clone of the same
    /// `Rc` and read on demand, so a path change reaches every closure
    /// without per-closure rewiring.
    pub storage_dir: Rc<RefCell<PathBuf>>,
}

pub struct WizardWidgets {
    pub stack: gtk::Stack,
    pub step: RefCell<WizardStep>,
    // Page 1
    pub install_status_label: gtk::Label,
    pub install_btn: gtk::Button,
    pub install_manually_btn: gtk::Button,
    pub install_next_btn: gtk::Button,
    // Page 2
    pub screen_picked_label: gtk::Label,
    pub pick_screen_btn: gtk::Button,
    pub screen_next_btn: gtk::Button,
    // Page 3
    pub hotkey_label: gtk::Label,
    pub buffer_scale: gtk::Scale,
    pub storage_label: gtk::Label,
}

impl WizardWidgets {
    /// State A → B transition: GSR detected as installed. Hide the install
    /// controls and reveal the Next button so the user can advance.
    /// Mirrors `show_not_installed_state` so install-status flips are
    /// always centralised here rather than scattered across app.rs.
    pub fn show_installed_state(&self) {
        self.install_btn.set_visible(false);
        self.install_manually_btn.set_visible(false);
        self.install_next_btn.set_visible(true);
    }

    /// State B → A transition: GSR went missing (uninstalled while we
    /// were running, or never installed). Restore the install controls
    /// and hide Next so the user can re-install.
    pub fn show_not_installed_state(&self) {
        self.install_btn.set_visible(true);
        self.install_manually_btn.set_visible(true);
        self.install_next_btn.set_visible(false);
    }

    /// Page 2 State A → B transition: portal pick succeeded. Hide the
    /// "Pick screen" button and reveal Next so only one primary action is
    /// visible at a time (mirrors Page 1's install-then-Next flip).
    pub fn show_screen_picked_state(&self) {
        self.pick_screen_btn.set_visible(false);
        self.screen_next_btn.set_visible(true);
    }

    /// Page 2 State B → A transition: capture source was reset (Settings →
    /// Reset). Restore the "Pick screen" button and hide Next so the user
    /// can pick again.
    pub fn show_screen_not_picked_state(&self) {
        self.pick_screen_btn.set_visible(true);
        self.screen_next_btn.set_visible(false);
    }
}

pub fn build_clips_page() -> ClipsPage {
    let stack = gtk::Stack::builder()
        .vexpand(true)
        .hexpand(true)
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();

    // Storage dir + model are owned by `ClipsPage` so the per-card kebab
    // handlers (Rename / Delete) can call `refresh_clips_model()` after
    // mutating disk state. `loaded_page()` builds the GridView against
    // these — `refresh_clips_model()` then keeps the same model alive,
    // just clearing+repopulating it from a fresh `library::reconcile()`.
    //
    // Storage dir defaults to `~/Videos/Clips` but is overridden by
    // `set_storage_dir()` when the user picks a different folder in
    // Settings or the wizard. The settings-loaded value is reapplied
    // soon after construction by the caller (app.rs) so the page
    // briefly shows the default before the override lands.
    let initial_storage = PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join("Videos/Clips");
    let _ = std::fs::create_dir_all(&initial_storage);
    let storage_dir: Rc<RefCell<PathBuf>> = Rc::new(RefCell::new(initial_storage));
    let model = gio::ListStore::new::<ClipObject>();

    let state = Rc::new(RefCell::new(PageState::Onboarding));

    // The remix slot is a wrapper Box that holds the active `RemixPanel`
    // root. We rebuild the panel for each clip the user remixes (rather
    // than caching one and rebinding) — clips are infrequent and the
    // panel only has 6 sliders + 12 toggles, so the rebuild cost is
    // dwarfed by the user's reaction time. Reuse-via-rebind would
    // require a `RemixPanel::set_clip_path` API that's pure cost for v1.
    let remix_container = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .vexpand(true)
        .hexpand(true)
        .build();

    // The on_remix closure: invoked from a per-card kebab's `clip.remix`
    // action with the clicked clip's full path. Builds a fresh RemixPanel
    // for that clip and switches the page Stack to "remix". Captures
    // clones of stack / state / model / storage_dir / remix_container so
    // it doesn't hold a ClipsPage ref (which doesn't exist yet at this
    // point in construction).
    let on_remix: Rc<dyn Fn(PathBuf)> = {
        let stack_for_remix = stack.clone();
        let state_for_remix = state.clone();
        let remix_container = remix_container.clone();
        let model_for_remix = model.clone();
        let storage_dir_for_remix = storage_dir.clone();
        Rc::new(move |clip_path: PathBuf| {
            // Tear down the previous panel (if any) before building the
            // new one. `while let Some(child) = container.first_child()`
            // is the canonical drain pattern for gtk::Box in gtk-rs.
            while let Some(child) = remix_container.first_child() {
                remix_container.remove(&child);
            }

            // on_close: switch the page back to "loaded".
            let stack_for_close = stack_for_remix.clone();
            let state_for_close = state_for_remix.clone();
            let on_close = move || {
                *state_for_close.borrow_mut() = PageState::Loaded;
                stack_for_close.set_visible_child_name("loaded");
            };

            // on_exported: refresh the grid model so the new
            // `*-remix.mp4` file shows up. Read the storage_dir live so a
            // change between remix open and export close still hits the
            // right directory.
            let model_for_export = model_for_remix.clone();
            let storage_dir_for_export = storage_dir_for_remix.clone();
            let on_exported = move |_out_path: PathBuf| {
                let dir = storage_dir_for_export.borrow().clone();
                refresh_model_in_place(&model_for_export, &dir);
            };

            let panel = crate::clips::remix::build_remix_panel(
                &clip_path,
                on_close,
                on_exported,
            );
            remix_container.append(&panel.root);

            *state_for_remix.borrow_mut() = PageState::Remix;
            stack_for_remix.set_visible_child_name("remix");
        })
    };

    let wizard = Rc::new(build_wizard());
    stack.add_named(&wizard.stack, Some("onboarding"));
    stack.add_named(&empty_page(), Some("empty"));
    stack.add_named(
        &loaded_page(storage_dir.clone(), &model, on_remix),
        Some("loaded"),
    );
    stack.add_named(&remix_container, Some("remix"));

    stack.set_visible_child_name("onboarding");

    let page = ClipsPage { root: stack, state, wizard, model, storage_dir };
    // Initial population (and one-time backfill spawn).
    page.refresh_clips_model();
    page.spawn_duration_backfill();
    page
}

fn build_wizard() -> WizardWidgets {
    let stack = gtk::Stack::builder()
        .vexpand(true)
        .hexpand(true)
        .transition_type(gtk::StackTransitionType::SlideLeftRight)
        .transition_duration(200)
        .build();

    let (page1, install_status_label, install_btn, install_manually_btn, install_next_btn) =
        build_page1_install();
    let (page2, screen_picked_label, pick_screen_btn, screen_next_btn) = build_page2_screen();
    let (page3, hotkey_label, buffer_scale, storage_label) = build_page3_settings();

    stack.add_named(&page1, Some("wizard-1-install"));
    stack.add_named(&page2, Some("wizard-2-screen"));
    stack.add_named(&page3, Some("wizard-3-settings"));
    stack.set_visible_child_name("wizard-1-install");

    WizardWidgets {
        stack,
        step: RefCell::new(WizardStep::InstallGsr),
        install_status_label,
        install_btn,
        install_manually_btn,
        install_next_btn,
        screen_picked_label,
        pick_screen_btn,
        screen_next_btn,
        hotkey_label,
        buffer_scale,
        storage_label,
    }
}

fn step_indicator(current: u8, total: u8) -> gtk::Label {
    // No `caption` class — body-size text reads as a header above the
    // wizard pages. The layout pins this label near the top of each
    // page (margin_top ~16) while the title/body/buttons sit centred
    // in the remaining space, so the indicator visually belongs to the
    // page chrome rather than the content cluster.
    let lbl = gtk::Label::new(Some(&format!("Step {current} of {total}")));
    lbl.add_css_class("dim-label");
    lbl.set_xalign(0.5);
    lbl
}

fn build_page1_install() -> (gtk::Widget, gtk::Label, gtk::Button, gtk::Button, gtk::Button) {
    // Outer page box: step indicator pinned near top (margin_top 16),
    // then a vexpanding centre box that holds the title/body/button
    // cluster vertically centred in the remaining space.
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(16)
        .margin_bottom(40)
        .margin_start(40)
        .margin_end(40)
        .halign(gtk::Align::Center)
        .vexpand(true)
        .build();

    page.append(&step_indicator(1, 3));

    let center_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(24)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .vexpand(true)
        .build();

    let title = gtk::Label::new(Some("Install gpu-screen-recorder"));
    title.add_css_class("title-1");
    center_box.append(&title);

    let body = gtk::Label::new(Some(
        "Clips uses gpu-screen-recorder, a free open-source Flatpak, \
         to capture gameplay.",
    ));
    body.set_wrap(true);
    body.set_max_width_chars(50);
    body.set_xalign(0.5);
    body.set_justify(gtk::Justification::Center);
    center_box.append(&body);

    // Primary install button. State A control (visible when GSR is
    // not installed); hidden by `show_installed_state()` once install
    // succeeds.
    let install_btn = gtk::Button::builder()
        .label("Install")
        .css_classes(["pill", "suggested-action"])
        .halign(gtk::Align::Center)
        .width_request(200)
        .build();
    install_btn.set_action_name(Some("app.gsr-install"));
    center_box.append(&install_btn);

    // Secondary "Install Manually" button. Opens an AlertDialog with the
    // app-store + copy-command options, so Page 1 stays uncluttered for
    // the happy path (one Install button + one Next button).
    let install_manually_btn = gtk::Button::builder()
        .label("Install Manually")
        .css_classes(["pill"])
        .halign(gtk::Align::Center)
        .width_request(200)
        .build();
    install_manually_btn.set_action_name(Some("app.gsr-install-manually"));
    center_box.append(&install_manually_btn);

    // Status label (reflects install progress when active).
    let install_status_label = gtk::Label::new(None);
    install_status_label.add_css_class("dim-label");
    install_status_label.set_visible(false);
    center_box.append(&install_status_label);

    // Next button: hidden until install detection sees GSR installed
    // (driven by the bidirectional install watcher in app.rs +
    // `WizardWidgets::show_installed_state`). Mutually exclusive with
    // the Install / Install Manually pair above — page 1 either shows
    // install controls or the Next button, never both.
    let install_next_btn = gtk::Button::builder()
        .label("Next")
        .css_classes(["pill", "suggested-action"])
        .halign(gtk::Align::Center)
        .width_request(200)
        .visible(false)
        .build();
    install_next_btn.set_action_name(Some("app.wizard-next"));
    center_box.append(&install_next_btn);

    page.append(&center_box);

    (
        page.upcast(),
        install_status_label,
        install_btn,
        install_manually_btn,
        install_next_btn,
    )
}

fn build_page2_screen() -> (gtk::Widget, gtk::Label, gtk::Button, gtk::Button) {
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(16)
        .margin_bottom(40)
        .margin_start(40)
        .margin_end(40)
        .halign(gtk::Align::Center)
        .vexpand(true)
        .build();

    page.append(&step_indicator(2, 3));

    let center_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(24)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .vexpand(true)
        .build();

    let title = gtk::Label::new(Some("Pick the screen to record"));
    title.add_css_class("title-1");
    center_box.append(&title);

    let body = gtk::Label::new(Some(
        "Choose which display Clips should capture from. \
         You can change this later in Settings.",
    ));
    body.set_wrap(true);
    body.set_max_width_chars(50);
    body.set_xalign(0.5);
    body.set_justify(gtk::Justification::Center);
    center_box.append(&body);

    // State A button: visible when no portal pick yet. Mutually exclusive
    // with screen_next_btn — `WizardWidgets::show_screen_picked_state` /
    // `show_screen_not_picked_state` flip the visibility together so only
    // one primary action shows at a time (mirrors Page 1's Install/Next
    // flip).
    let pick_screen_btn = gtk::Button::builder()
        .label("Pick screen")
        .css_classes(["pill", "suggested-action"])
        .halign(gtk::Align::Center)
        .width_request(200)
        .build();
    pick_screen_btn.set_action_name(Some("app.setup-clips"));
    center_box.append(&pick_screen_btn);

    let screen_picked_label = gtk::Label::new(None);
    screen_picked_label.add_css_class("dim-label");
    screen_picked_label.set_visible(false);
    center_box.append(&screen_picked_label);

    // State B button: hidden until the portal pick succeeds. We hide
    // (rather than set_sensitive(false)) so the layout looks like Page 1
    // post-install — only one button visible at a time.
    let screen_next_btn = gtk::Button::builder()
        .label("Next")
        .css_classes(["pill", "suggested-action"])
        .halign(gtk::Align::Center)
        .width_request(200)
        .visible(false)
        .build();
    screen_next_btn.set_action_name(Some("app.wizard-next"));
    center_box.append(&screen_next_btn);

    page.append(&center_box);

    (page.upcast(), screen_picked_label, pick_screen_btn, screen_next_btn)
}

fn build_page3_settings() -> (gtk::Widget, gtk::Label, gtk::Scale, gtk::Label) {
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(16)
        .margin_bottom(40)
        .margin_start(40)
        .margin_end(40)
        .halign(gtk::Align::Center)
        .vexpand(true)
        .build();

    page.append(&step_indicator(3, 3));

    let center_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(24)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .vexpand(true)
        .build();

    let title = gtk::Label::new(Some("Configure clips"));
    title.add_css_class("title-1");
    center_box.append(&title);

    let body = gtk::Label::new(Some(
        "All settings have sensible defaults. Tweak now \
         or later in Settings.",
    ));
    body.set_wrap(true);
    body.set_max_width_chars(50);
    body.set_xalign(0.5);
    body.set_justify(gtk::Justification::Center);
    center_box.append(&body);

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
    center_box.append(&hotkey_row);

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
    center_box.append(&buffer_row);

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
    center_box.append(&storage_row);

    // Done button
    let done_btn = gtk::Button::builder()
        .label("Done")
        .css_classes(["pill", "suggested-action"])
        .halign(gtk::Align::Center)
        .width_request(200)
        .build();
    done_btn.set_action_name(Some("app.wizard-next"));
    center_box.append(&done_btn);

    page.append(&center_box);

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
/// moment a kebab button or any other peer widget is added).
///
/// The `card` field is the outer `gtk::Overlay` — we hold it explicitly
/// because `connect_bind` needs to call `insert_action_group("clip", …)`
/// on it to wire the kebab menu's per-card actions to *this row's* clip.
#[derive(Clone)]
struct CardWidgets {
    card: gtk::Overlay,
    image: gtk::Picture,
    title: gtk::Label,
    kebab: gtk::MenuButton,
}

fn build_clip_card() -> (gtk::Overlay, CardWidgets) {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();

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

    content.append(&image);
    content.append(&title);

    // Outer card is a gtk::Overlay so the kebab MenuButton can float over
    // the thumbnail without resizing the layout when shown/hidden.
    let card = gtk::Overlay::builder().build();
    card.add_css_class("clip-card");
    card.set_child(Some(&content));

    // Per-card menu model. The action names are scoped under the "clip"
    // prefix — connect_bind will install a SimpleActionGroup under that
    // prefix on this overlay, capturing the bound clip's filename and
    // storage_dir into each action's callback.
    let menu = gio::Menu::new();
    menu.append(Some("Remix…"), Some("clip.remix"));
    menu.append(Some("Rename…"), Some("clip.rename"));
    menu.append(Some("Open in Folder"), Some("clip.open-folder"));
    // Section break so Delete sits visually separate from the safe
    // actions (matches the GNOME HIG "destructive last, in its own group"
    // pattern; also avoids accidentally clicking past Open-in-Folder
    // straight onto Delete).
    let danger = gio::Menu::new();
    danger.append(Some("Delete…"), Some("clip.delete"));
    menu.append_section(None, &danger);

    let kebab = gtk::MenuButton::builder()
        .icon_name("lucide-ellipsis-vertical-symbolic")
        .has_frame(false)
        .css_classes(["circular", "clip-card-kebab"])
        .halign(gtk::Align::End)
        .valign(gtk::Align::Start)
        .margin_top(6)
        .margin_end(6)
        .tooltip_text("Clip actions")
        .build();
    kebab.set_menu_model(Some(&menu));
    card.add_overlay(&kebab);

    (card.clone(), CardWidgets { card, image, title, kebab })
}

fn bind_clip_card(
    widgets: &CardWidgets,
    clip: &ClipObject,
    storage_dir: &Rc<RefCell<PathBuf>>,
    model: &gio::ListStore,
    on_remix: &Rc<dyn Fn(PathBuf)>,
) {
    let meta = clip.meta();
    let clip_storage_dir = clip.storage_dir();
    let game = if meta.game_name.is_empty() {
        "Untitled"
    } else {
        meta.game_name.as_str()
    };
    widgets.title.set_label(game);

    // Wire the per-card actions for the kebab menu. Each invocation
    // creates a fresh SimpleActionGroup with closures that capture *this*
    // clip's filename, so re-binding to a different model item replaces
    // the action group atomically (any in-flight click on the previous
    // group's actions would have already fired by the time the new model
    // item is bound — GTK's ListItem recycling is synchronous on the
    // main thread).
    let actions = build_clip_actions(
        widgets,
        meta.filename.clone(),
        storage_dir.clone(),
        model.clone(),
        on_remix.clone(),
    );
    widgets.card.insert_action_group("clip", Some(&actions));

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
    let storage_for_worker = clip_storage_dir;
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

/// Build a fresh per-card action group for the kebab menu. Captures the
/// bound clip's filename + a clone of the page-level `Rc<RefCell<PathBuf>>`
/// for the storage_dir + model so that Rename / Delete / Open-in-Folder /
/// Remix can mutate disk state or switch to the remix panel without
/// walking widget trees or holding ClipsPage refs. The Rc means a live
/// `set_storage_dir()` change is seen by any subsequent kebab activation.
fn build_clip_actions(
    widgets: &CardWidgets,
    filename: String,
    storage_dir: Rc<RefCell<PathBuf>>,
    model: gio::ListStore,
    on_remix: Rc<dyn Fn(PathBuf)>,
) -> gio::SimpleActionGroup {
    let group = gio::SimpleActionGroup::new();

    // Remix — switches the page Stack to the remix panel for this clip.
    // The on_remix callback (provided by `build_clips_page`) takes the
    // full clip path and is responsible for rebuilding the panel and
    // flipping the page state.
    {
        let remix = gio::SimpleAction::new("remix", None);
        let storage_dir = storage_dir.clone();
        let filename = filename.clone();
        remix.connect_activate(move |_, _| {
            let dir = storage_dir.borrow().clone();
            on_remix(dir.join(&filename));
        });
        group.add_action(&remix);
    }

    // Rename
    {
        let rename = gio::SimpleAction::new("rename", None);
        let kebab = widgets.kebab.clone();
        let storage_dir = storage_dir.clone();
        let model = model.clone();
        let filename = filename.clone();
        rename.connect_activate(move |_, _| {
            show_rename_dialog(
                &kebab,
                filename.clone(),
                storage_dir.borrow().clone(),
                model.clone(),
            );
        });
        group.add_action(&rename);
    }

    // Delete
    {
        let delete = gio::SimpleAction::new("delete", None);
        let kebab = widgets.kebab.clone();
        let storage_dir = storage_dir.clone();
        let model = model.clone();
        let filename = filename.clone();
        delete.connect_activate(move |_, _| {
            show_delete_dialog(
                &kebab,
                filename.clone(),
                storage_dir.borrow().clone(),
                model.clone(),
            );
        });
        group.add_action(&delete);
    }

    // Open in Folder — spawn xdg-open <storage_dir>. On Bazzite KDE this
    // opens Dolphin focused on the directory. We don't try to reveal the
    // specific clip (no portable cross-DE way without DBus integration);
    // the user just gets the folder.
    {
        let open = gio::SimpleAction::new("open-folder", None);
        let storage_dir = storage_dir.clone();
        open.connect_activate(move |_, _| {
            let dir = storage_dir.borrow().clone();
            if let Err(e) = std::process::Command::new("xdg-open")
                .arg(&dir)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                log::warn!("xdg-open failed for {}: {e}", dir.display());
            }
        });
        group.add_action(&open);
    }

    group
}

/// Walk up from `widget` to find the toplevel `gtk::Window` to anchor a
/// dialog onto. Returns None only if the widget isn't currently parented
/// in a window (shouldn't happen during a kebab-menu activation, but the
/// dialog API gracefully handles `None`).
fn parent_window_of(widget: &impl IsA<gtk::Widget>) -> Option<gtk::Window> {
    widget.root().and_then(|r| r.downcast::<gtk::Window>().ok())
}

/// Replace `filename` in the on-disk index with `new_filename`, preserving
/// the metadata (game_name, duration_ms, bitrate, resolution). Without
/// this, `library::reconcile()` would drop the renamed file's index entry
/// (file gone) and re-add a default-meta entry for the new name, losing
/// game name / duration. Best-effort — index errors are logged and
/// swallowed since the .mp4 itself has already been renamed at this point.
fn rename_in_index(old: &str, new: &str) {
    let mut idx = crate::clips::library::load_index();
    let mut changed = false;
    for m in idx.iter_mut() {
        if m.filename == old {
            m.filename = new.to_string();
            changed = true;
        }
    }
    if changed {
        if let Err(e) = crate::clips::library::save_index(&idx) {
            log::warn!("rename: failed to update clips index: {e}");
        }
    }
}

/// Validate a user-typed rename stem. Reject anything that could escape
/// the storage dir, smuggle a leading dash into a future ffmpeg/ffprobe
/// invocation, masquerade as a hidden file, hide a control character in
/// a terminal listing, or trip case-insensitive filesystem confusion.
///
/// Returns `Err(reason)` with a short user-facing message on failure;
/// the caller surfaces that as a toast / inline error label and aborts
/// the rename.
///
/// `pub(crate)` so the validator is unit-testable in isolation. Tests
/// in `mod rename_validator_tests` exercise every rejection branch.
pub(crate) fn validate_rename_stem(stem: &str) -> Result<(), &'static str> {
    if stem.is_empty() {
        return Err("Name cannot be empty.");
    }
    // Trim-then-empty catches all-whitespace ("   "), which would look
    // like a normal name to the user but be empty after we trim before
    // joining — that's the same bug class as an empty stem.
    if stem.trim().is_empty() {
        return Err("Name cannot be only whitespace.");
    }
    // Trailing whitespace and trailing `.` cause case-insensitive
    // filesystem confusion (Windows / NTFS / case-insensitive volume on
    // macOS) where `name. ` and `name` resolve to the same dirent. We
    // store on Linux but the user might sync the folder to a Windows
    // share; reject defensively.
    if stem.ends_with(' ') || stem.ends_with('\t') || stem.ends_with('.') {
        return Err("Name cannot end with whitespace or a period.");
    }
    if stem.starts_with('.') {
        return Err("Name cannot start with a period (would create a hidden file).");
    }
    if stem.starts_with('-') {
        // ffmpeg / ffprobe parse a leading '-' as a flag. A future invocation
        // (e.g. remix export, thumbnail extraction) that accidentally puts
        // the rendered filename in the args list without `--` would then
        // execute attacker-controlled flags.
        return Err("Name cannot start with a dash.");
    }
    if stem.contains("..") {
        // library::resolve_collision ends up calling storage_dir.join(stem),
        // and `..` in `stem` lets the joined path resolve outside the
        // storage dir. Belt-and-suspenders: even though the rename target
        // is `storage_dir.join("foo..bar.mp4")` (joined unconditionally,
        // not interpreted as a path), the caller's collision-resolution
        // helper path-walks the result and could produce `<storage>/../...
        // .mp4` if a future change relaxes the join discipline.
        return Err("Name cannot contain '..' (path traversal).");
    }
    if stem.contains('/') || stem.contains('\\') || stem.contains('\0') {
        return Err("Name contains an invalid character.");
    }
    if stem.chars().any(|c| c.is_control()) {
        // Terminal escape sequences (ESC, BEL, ANSI CSI introducers)
        // could rewrite the user's terminal when an `ls` listing of the
        // storage dir is rendered. Rare but cheap to guard.
        return Err("Name contains a control character.");
    }
    Ok(())
}

/// Look up the active toast overlay for a widget so rename / delete
/// failures (and any other transient UI feedback) can show via an
/// `adw::Toast` with consistent styling. Returns `None` if the widget
/// isn't currently parented in a window with a `ToastOverlay` — the
/// caller should fall back to logging in that case.
pub(crate) fn find_toast_overlay_for(widget: &impl IsA<gtk::Widget>) -> Option<adw::ToastOverlay> {
    let window = widget.root().and_then(|r| r.downcast::<gtk::Window>().ok())?;
    let mut current = window.child();
    while let Some(w) = current {
        if let Ok(o) = w.clone().downcast::<adw::ToastOverlay>() {
            return Some(o);
        }
        current = w.first_child();
    }
    None
}

/// Show a transient failure message via the toast overlay. Falls back
/// to a `gio::Notification` (which most desktop environments render as
/// a tray-style notification) when no toast overlay is reachable —
/// e.g. if the window was hidden between the action and its callback.
pub(crate) fn surface_failure(
    anchor: &impl IsA<gtk::Widget>,
    title: &str,
) {
    if let Some(overlay) = find_toast_overlay_for(anchor) {
        let toast = adw::Toast::builder()
            .title(title)
            .timeout(6)
            .build();
        overlay.add_toast(toast);
        return;
    }
    // Fallback: best-effort desktop notification. We don't have an
    // app handle here (the kebab callback only sees the widget), so
    // we render via gio::Notification with a static id. The id is
    // shared with notify_saved's; that's fine because user-visible
    // text will replace it. If even this fails (no session bus,
    // headless), the warning we logged above is the user's only
    // signal.
    let notif = gio::Notification::new(title);
    if let Some(app) = anchor
        .root()
        .and_then(|r| r.downcast::<gtk::Window>().ok())
        .and_then(|w| w.application())
        .and_then(|a| a.downcast::<gtk::Application>().ok())
    {
        app.send_notification(Some("clip-action-error"), &notif);
    }
}

fn show_rename_dialog(
    anchor: &impl IsA<gtk::Widget>,
    old_filename: String,
    storage_dir: PathBuf,
    model: gio::ListStore,
) {
    use crate::clips::library;

    // Pre-fill the entry with the current basename (no extension). The
    // user types a new stem and we re-attach the original extension on
    // save. Falls back to the full filename if there's no extension.
    let old_stem = std::path::Path::new(&old_filename)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| old_filename.clone());
    let old_ext = std::path::Path::new(&old_filename)
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "mp4".to_string());

    let dialog = adw::AlertDialog::new(
        Some("Rename clip"),
        Some(&format!("Pick a new name for {old_filename}.")),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("save", "Save");
    dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("save"));
    dialog.set_close_response("cancel");

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    let entry = gtk::Entry::builder()
        .text(&old_stem)
        .activates_default(true)
        .hexpand(true)
        .build();
    entry.select_region(0, -1);
    body.append(&entry);
    let status = gtk::Label::builder()
        .label("")
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .build();
    status.add_css_class("error");
    body.append(&status);
    dialog.set_extra_child(Some(&body));

    let parent = parent_window_of(anchor);
    let anchor_for_response: gtk::Widget = anchor.clone().upcast();
    dialog.connect_response(None, move |_dlg, response| {
        if response != "save" {
            // "cancel" or any non-save response: AlertDialog closes itself.
            return;
        }
        // Note: trim() before validation so the user's leading/trailing
        // whitespace doesn't cause spurious failures, but the validator
        // still rejects internal trailing whitespace via its own rules.
        let new_stem = entry.text().trim().to_string();
        if new_stem == old_stem {
            // No-op rename.
            return;
        }
        if let Err(reason) = validate_rename_stem(&new_stem) {
            // Surface BOTH inline (in the dialog's status label, in case
            // the dialog is still presented after AdwAlertDialog's
            // response handling) AND via toast (covers the case where
            // the dialog has dismissed by the time we get here).
            status.set_label(reason);
            status.set_visible(true);
            surface_failure(&anchor_for_response, &format!("Rename failed: {reason}"));
            log::warn!("rename: rejecting stem {new_stem:?}: {reason}");
            return;
        }
        // Resolve collisions by appending -2, -3, … via the same helper
        // used by the FIFO save path. The user gets a deterministic
        // disambiguated name rather than a clobbered file.
        let new_filename = library::resolve_collision(&storage_dir, &new_stem, &old_ext);
        let old_path = storage_dir.join(&old_filename);
        let new_path = storage_dir.join(&new_filename);
        match std::fs::rename(&old_path, &new_path) {
            Ok(()) => {
                // Best-effort thumb rename so the existing thumbnail isn't
                // wasted (if it exists). reconcile() will regenerate
                // anyway on next bind, but skipping the regen is faster.
                let old_thumb =
                    crate::clips::thumbnail::thumb_path(&storage_dir, &old_filename);
                let new_thumb =
                    crate::clips::thumbnail::thumb_path(&storage_dir, &new_filename);
                let _ = std::fs::rename(&old_thumb, &new_thumb);
                rename_in_index(&old_filename, &new_filename);
                refresh_model_in_place(&model, &storage_dir);
            }
            Err(e) => {
                log::warn!(
                    "rename failed: {} -> {}: {e}",
                    old_path.display(),
                    new_path.display()
                );
                surface_failure(
                    &anchor_for_response,
                    &format!("Couldn't rename clip: {e}"),
                );
            }
        }
    });

    dialog.present(parent.as_ref().map(|w| w.upcast_ref::<gtk::Widget>()));
}

fn show_delete_dialog(
    anchor: &impl IsA<gtk::Widget>,
    filename: String,
    storage_dir: PathBuf,
    model: gio::ListStore,
) {
    let dialog = adw::AlertDialog::new(
        Some("Delete this clip?"),
        Some(&format!(
            "{filename} will be permanently removed from your clips folder.\nThis cannot be undone."
        )),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("delete", "Delete");
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    let parent = parent_window_of(anchor);
    let anchor_for_response: gtk::Widget = anchor.clone().upcast();
    dialog.connect_response(None, move |_dlg, response| {
        if response != "delete" {
            return;
        }
        let clip_path = storage_dir.join(&filename);
        if let Err(e) = std::fs::remove_file(&clip_path) {
            log::warn!("delete failed: {}: {e}", clip_path.display());
            // Surface the failure so the user knows the click didn't
            // succeed silently. Common cases this hits: the user
            // deleted the file in another file manager between
            // dialog-open and confirm (NotFound), or the storage dir
            // is on a read-only mount (PermissionDenied).
            surface_failure(
                &anchor_for_response,
                &format!("Couldn't delete clip: {e}"),
            );
            return;
        }
        // Best-effort: remove the cached thumbnail too. If absent, ignore.
        let thumb_path = crate::clips::thumbnail::thumb_path(&storage_dir, &filename);
        let _ = std::fs::remove_file(&thumb_path);
        refresh_model_in_place(&model, &storage_dir);
    });

    dialog.present(parent.as_ref().map(|w| w.upcast_ref::<gtk::Widget>()));
}

/// Clear and repopulate the GridView's `ListStore` from a fresh
/// `library::reconcile()` against `storage_dir`. This is the single
/// chokepoint that both internal kebab handlers and the public
/// `ClipsPage::refresh_clips_model()` go through.
fn refresh_model_in_place(model: &gio::ListStore, storage_dir: &PathBuf) {
    use crate::clips::library;
    model.remove_all();
    for meta in library::reconcile(storage_dir) {
        model.append(&ClipObject::new(meta, storage_dir.clone()));
    }
}

fn loaded_page(
    storage_dir: Rc<RefCell<PathBuf>>,
    model: &gio::ListStore,
    on_remix: Rc<dyn Fn(PathBuf)>,
) -> gtk::Widget {
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();

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
        // `connect_unbind` clears the per-card action group but does not
        // touch this data — the next `connect_bind` reads the same key
        // with the same `T`. The only invariant `set_data` requires is
        // that the type at retrieval matches the type at storage; that
        // holds here because the key is unique to this factory.
        unsafe {
            item.set_data::<CardWidgets>("card-widgets", widgets);
        }
        item.set_child(Some(&card));
    });
    let storage_dir_for_bind = storage_dir;
    let model_for_bind = model.clone();
    let on_remix_for_bind = on_remix;
    factory.connect_bind(move |_factory, item| {
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
        bind_clip_card(
            &widgets,
            &clip,
            &storage_dir_for_bind,
            &model_for_bind,
            &on_remix_for_bind,
        );
    });
    factory.connect_unbind(|_factory, item| {
        // Clear the per-card action group so the action closures (which
        // capture this item's previous filename) are dropped when the
        // pooled card is swapped onto a different model item. Without
        // this, the closures would briefly outlive their relevance — not
        // a correctness issue (connect_bind replaces the group on the
        // next bind anyway), but cleaner.
        let item = item
            .downcast_ref::<gtk::ListItem>()
            .expect("unbind signal item must be a ListItem");
        let widgets: CardWidgets = unsafe {
            item.data::<CardWidgets>("card-widgets")
                .map(|p| p.as_ref().clone())
                .expect("card-widgets attached during setup")
        };
        widgets
            .card
            .insert_action_group("clip", None::<&gio::SimpleActionGroup>);
    });

    let grid = gtk::GridView::builder()
        .model(&gtk::SingleSelection::new(Some(model.clone())))
        .factory(&factory)
        .min_columns(2)
        .max_columns(5)
        .build();

    scroll.set_child(Some(&grid));
    scroll.upcast()
}

impl ClipsPage {
    /// Clear and repopulate the GridView's backing `ListStore` from a
    /// fresh `library::reconcile()` against the page's storage dir.
    /// Called by per-card kebab handlers (Rename / Delete) after they
    /// mutate the directory; safe to call from any GTK-main-thread
    /// context.
    pub fn refresh_clips_model(&self) {
        let dir = self.storage_dir.borrow().clone();
        refresh_model_in_place(&self.model, &dir);
    }

    /// Update the storage directory shown by the browser. Called from
    /// Settings → Clips → Pick folder and the wizard's Page 3 picker
    /// whenever the user changes `clips_settings.storage_path`. The
    /// new directory is reconciled immediately so the GridView reflects
    /// what's on disk under the new path; previously-bound kebab actions
    /// pick up the new directory on their next activation because they
    /// hold a clone of the same `Rc<RefCell<PathBuf>>`.
    ///
    /// SHOULD be called whenever `ClipSettings::storage_path` changes.
    pub fn set_storage_dir(&self, new_dir: PathBuf) {
        {
            let mut g = self.storage_dir.borrow_mut();
            if *g == new_dir {
                return;
            }
            *g = new_dir;
        }
        self.refresh_clips_model();
    }

    /// Kick off the one-time duration-backfill worker. Phase 1's FIFO
    /// reader emits `Saved { duration_ms: 0 }` (ffprobe was deferred out
    /// of the FIFO reader to avoid blocking the next save), and entries
    /// created by `reconcile()` for files not yet in the index also
    /// start at zero. The worker writes the updated index in place; new
    /// values surface on the next browser-open / app launch (see
    /// `library::backfill_durations` for the rationale).
    fn spawn_duration_backfill(&self) {
        use crate::clips::library;
        let storage_for_backfill = self.storage_dir.borrow().clone();
        std::thread::spawn(move || {
            if let Err(e) = library::backfill_durations(&storage_for_backfill) {
                log::warn!("clip duration backfill failed: {e}");
            }
        });
    }

    pub fn set_state(&self, new_state: PageState) {
        *self.state.borrow_mut() = new_state;
        match new_state {
            PageState::Onboarding => self.root.set_visible_child_name("onboarding"),
            PageState::Empty => self.root.set_visible_child_name("empty"),
            PageState::Loaded => self.root.set_visible_child_name("loaded"),
            PageState::Remix => self.root.set_visible_child_name("remix"),
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

#[cfg(test)]
mod rename_validator_tests {
    //! Tests for `validate_rename_stem` covering security M-1: every
    //! way a malicious or accidentally-tricky rename could escape the
    //! storage dir, smuggle a leading flag into a future ffmpeg call,
    //! masquerade as a hidden file, hide a control character, or trip
    //! case-insensitive filesystem confusion.
    use super::*;

    #[test]
    fn rejects_empty_and_whitespace_only() {
        assert!(validate_rename_stem("").is_err());
        assert!(validate_rename_stem("   ").is_err());
        assert!(validate_rename_stem("\t\t").is_err());
    }

    #[test]
    fn rejects_path_traversal_double_dot() {
        assert!(validate_rename_stem("..").is_err());
        assert!(validate_rename_stem("..hello").is_err());
        assert!(validate_rename_stem("hello..world").is_err());
        assert!(validate_rename_stem("foo/../bar").is_err());
    }

    #[test]
    fn rejects_leading_dot() {
        assert!(validate_rename_stem(".hidden").is_err());
        assert!(validate_rename_stem(".").is_err());
    }

    #[test]
    fn rejects_leading_dash() {
        assert!(validate_rename_stem("-flag").is_err());
        assert!(validate_rename_stem("--flag").is_err());
    }

    #[test]
    fn rejects_trailing_whitespace_or_period() {
        assert!(validate_rename_stem("name ").is_err());
        assert!(validate_rename_stem("name\t").is_err());
        assert!(validate_rename_stem("name.").is_err());
        assert!(validate_rename_stem("with.dots.").is_err());
    }

    #[test]
    fn rejects_control_characters() {
        assert!(validate_rename_stem("name\nwith\nnewline").is_err());
        assert!(validate_rename_stem("escape\x1bhere").is_err());
        assert!(validate_rename_stem("bell\x07").is_err());
        // NUL is also a control char; the explicit `\0` check is
        // technically redundant with the is_control() branch but harmless.
        assert!(validate_rename_stem("nul\x00here").is_err());
    }

    #[test]
    fn rejects_path_separators() {
        assert!(validate_rename_stem("dir/file").is_err());
        assert!(validate_rename_stem("c:\\windows").is_err());
    }

    #[test]
    fn accepts_normal_names() {
        assert!(validate_rename_stem("Cyberpunk - epic moment").is_ok());
        assert!(validate_rename_stem("clip_2026-01-01").is_ok());
        assert!(validate_rename_stem("game.session.1").is_ok());
        assert!(validate_rename_stem("ARC Raiders win").is_ok());
        // Internal periods are fine; trailing one is the only rejection.
        assert!(validate_rename_stem("foo.bar.baz").is_ok());
    }
}
