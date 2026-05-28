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
    /// Dim "N clips" count label in the loaded-page header. Updated from
    /// `model.n_items()` after every `refresh_clips_model()` so it tracks
    /// adds/deletes live. Held here (built in `loaded_page`) so the refresh
    /// chokepoint can reach it without walking the widget tree.
    count_label: gtk::Label,
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
    pub hotkey_btn: gtk::Button,
    pub clip_length_scale: gtk::Scale,
    pub clip_length_label: gtk::Label,
    pub storage_btn: gtk::Button,
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

    /// Update the storage-folder button label on Page 3 to the basename of
    /// the supplied path. Falls back to "Clips" if the path has no usable
    /// final component (e.g. root, empty path).
    pub fn update_storage_label(&self, path: &std::path::Path) {
        let basename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Clips".to_string());
        self.storage_btn.set_label(&basename);
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

    // The on_remix closure: invoked from a card's click-to-edit gesture
    // (attached in `bind_clip_card` to the inner content Box) with the
    // clicked clip's full path. Builds a fresh RemixPanel for that clip
    // and switches the page Stack to "remix". Captures clones of stack /
    // state / model / storage_dir / remix_container so it doesn't hold a
    // ClipsPage ref (which doesn't exist yet at this point in
    // construction).
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
    let (loaded, count_label) = loaded_page(storage_dir.clone(), &model, on_remix);
    stack.add_named(&loaded, Some("loaded"));
    stack.add_named(&remix_container, Some("remix"));

    stack.set_visible_child_name("onboarding");

    let page = ClipsPage { root: stack, state, wizard, model, storage_dir, count_label };
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
    let (page3, hotkey_btn, clip_length_scale, clip_length_label, storage_btn) =
        build_page3_settings();

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
        hotkey_btn,
        clip_length_scale,
        clip_length_label,
        storage_btn,
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

fn build_page3_settings() -> (gtk::Widget, gtk::Button, gtk::Scale, gtk::Label, gtk::Button) {
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

    // Container for the three setting rows. Vertical spacing(12) gives the
    // visible gap between cards so each row reads as its own surface (vs.
    // the old flat row layout). width_request(500) keeps the cards a
    // consistent fixed width — full-stretch on the centered wizard layout
    // looks awkward because the column has no other anchor to align to.
    let rows_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .width_request(500)
        .halign(gtk::Align::Center)
        .build();

    // ------------------------------------------------------------------
    // Row 1: Save hotkey. Layout: [label]   [keybind button]
    //
    // The right-hand button's label IS the current chord. Clicking it
    // triggers `app.rebind-clip-hotkey` which re-opens the KDE portal's
    // shortcut picker. We don't try to capture chords in-app — that's the
    // OS-level UX the portal owns.
    //
    // The chord label is currently static ("Alt+S") because we don't
    // track post-bind chord changes; future work could subscribe to the
    // GlobalShortcuts portal's ShortcutsChanged signal to keep this
    // live, but that's out of scope for the v1 polish pass.
    // ------------------------------------------------------------------
    let hotkey_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(16)
        .css_classes(["card", "clips-row-card"])
        .build();
    let hotkey_label = gtk::Label::builder()
        .label("Save hotkey")
        .css_classes(["heading"])
        .build();
    hotkey_row.append(&hotkey_label);
    hotkey_row.append(&gtk::Box::builder().hexpand(true).build());
    let hotkey_btn = gtk::Button::builder()
        .label("Alt+S")
        .css_classes(["pill"])
        .build();
    hotkey_btn.set_action_name(Some("app.rebind-clip-hotkey"));
    hotkey_row.append(&hotkey_btn);
    rows_box.append(&hotkey_row);

    // ------------------------------------------------------------------
    // Row 2: Clip length. Layout: [label]   [scale]   [value label]
    //
    // The display name is "Clip length" but the underlying setting is
    // `buffer_length` (kept for backward compatibility with persisted
    // configs). The scale's built-in value display is hidden and we draw
    // a separate right-aligned label so digits don't jiggle as the user
    // drags — `width_request(50)` + `xalign(1.0)` keeps the layout stable.
    // ------------------------------------------------------------------
    let clip_length_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(16)
        .css_classes(["card", "clips-row-card"])
        .build();
    let clip_length_title = gtk::Label::builder()
        .label("Clip length")
        .css_classes(["heading"])
        .build();
    clip_length_row.append(&clip_length_title);
    clip_length_row.append(&gtk::Box::builder().width_request(8).build());
    let clip_length_scale =
        gtk::Scale::with_range(gtk::Orientation::Horizontal, 30.0, 300.0, 5.0);
    clip_length_scale.set_value(60.0);
    clip_length_scale.set_hexpand(true);
    clip_length_scale.set_draw_value(false);
    clip_length_row.append(&clip_length_scale);
    clip_length_row.append(&gtk::Box::builder().width_request(8).build());
    let clip_length_label = gtk::Label::builder()
        .label("60s")
        .width_request(50)
        .xalign(1.0)
        .build();
    clip_length_row.append(&clip_length_label);
    {
        let value_label = clip_length_label.clone();
        clip_length_scale.connect_value_changed(move |s| {
            value_label.set_label(&format!("{}s", s.value() as u32));
        });
    }
    rows_box.append(&clip_length_row);

    // ------------------------------------------------------------------
    // Row 3: Save clips to. Layout: [label]   [folder-name button]
    //
    // The right-hand button's label is the basename of the saved folder
    // (e.g. "Clips"). Initial label is "Clips" (default storage path
    // basename); app.rs reapplies via `update_storage_label` once
    // ClipSettings is loaded so the wizard reflects user-customised paths.
    // ------------------------------------------------------------------
    let storage_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(16)
        .css_classes(["card", "clips-row-card"])
        .build();
    let storage_label = gtk::Label::builder()
        .label("Save clips to")
        .css_classes(["heading"])
        .build();
    storage_row.append(&storage_label);
    storage_row.append(&gtk::Box::builder().hexpand(true).build());
    let storage_btn = gtk::Button::builder()
        .label("Clips")
        .css_classes(["pill"])
        .build();
    storage_btn.set_action_name(Some("app.pick-clip-storage"));
    storage_row.append(&storage_btn);
    rows_box.append(&storage_row);

    center_box.append(&rows_box);

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

    (
        page.upcast(),
        hotkey_btn,
        clip_length_scale,
        clip_length_label,
        storage_btn,
    )
}

fn empty_page() -> gtk::Widget {
    let page = adw::StatusPage::builder()
        .icon_name("lucide-clapperboard-symbolic")
        .title("No clips yet")
        .description("Press the save hotkey to capture the last 60 seconds of screen recording.")
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
///
/// `content` is the inner vertical box (thumb + title). We attach the
/// click-to-edit `GestureClick` to this widget rather than `card` so the
/// kebab MenuButton (which sits on the overlay layer) keeps its own
/// click swallowing — clicking the kebab opens the menu without also
/// triggering the editor.
/// Render a clip duration (milliseconds) as a compact `m:ss` string, e.g.
/// `0:32`, `1:05`, `10:30`. Floors any sub-second remainder — the card
/// subtitle is an at-a-glance hint, not a frame-accurate readout. Callers
/// only show this when `duration_ms > 0`, so `0:00` never surfaces in the UI.
fn format_duration_ms(duration_ms: u64) -> String {
    let total_secs = duration_ms / 1000;
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{mins}:{secs:02}")
}

#[derive(Clone)]
struct CardWidgets {
    card: gtk::Overlay,
    content: gtk::Box,
    image: gtk::Picture,
    title: gtk::Label,
    /// Dim duration subtitle below the title (e.g. "0:32"). Hidden when the
    /// clip's duration is unknown (0) so we never show a misleading "0:00".
    subtitle: gtk::Label,
    kebab: gtk::MenuButton,
}

fn build_clip_card() -> (gtk::Overlay, CardWidgets) {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();

    let image = gtk::Picture::builder()
        .height_request(180)
        .width_request(320)
        .build();
    image.add_css_class("clip-thumb");
    // Clip the Picture's contents to its (rounded) allocation so the
    // `.clip-thumb { border-radius }` CSS actually rounds the image corners
    // rather than just the widget's invisible box. Without Hidden overflow a
    // gtk::Picture paints its paintable past the rounded border.
    image.set_overflow(gtk::Overflow::Hidden);

    // Title + subtitle stack, left-aligned beneath the thumbnail.
    let text_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .build();

    let title = gtk::Label::builder()
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(30)
        .xalign(0.0)
        .build();
    title.add_css_class("clip-title");

    let subtitle = gtk::Label::builder()
        .xalign(0.0)
        .visible(false)
        .build();
    subtitle.add_css_class("clip-subtitle");

    text_box.append(&title);
    text_box.append(&subtitle);

    content.append(&image);
    content.append(&text_box);

    // Outer card is a gtk::Overlay so the kebab MenuButton can float over
    // the thumbnail without resizing the layout when shown/hidden.
    let card = gtk::Overlay::builder().build();
    card.add_css_class("clip-card");
    card.set_child(Some(&content));

    // Per-card menu model. The action names are scoped under the "clip"
    // prefix — connect_bind will install a SimpleActionGroup under that
    // prefix on this overlay, capturing the bound clip's filename and
    // storage_dir into each action's callback.
    //
    // Note: there is no "Remix" entry. The whole card is now click-to-
    // edit (the editor IS the remix panel) — see `bind_clip_card`'s
    // GestureClick on `content`.
    let menu = gio::Menu::new();
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

    (card.clone(), CardWidgets { card, content, image, title, subtitle, kebab })
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
    // Card title: the clip's human-readable display name. This is the
    // creation-time label by default (set during reconcile) or the user's
    // free-text rename. Fall back to the default creation-time label if the
    // stored name is somehow empty (mid-backfill), then to "Untitled".
    let title = if !meta.display_name.is_empty() {
        meta.display_name.clone()
    } else {
        let fallback = crate::clips::library::default_display_name(meta.created_unix as i64);
        if fallback.is_empty() {
            "Untitled".to_string()
        } else {
            fallback
        }
    };
    widgets.title.set_label(&title);

    // Duration subtitle: only shown when we actually know the duration.
    // A 0/unknown duration leaves the subtitle hidden rather than showing
    // a misleading "0:00".
    if meta.duration_ms > 0 {
        widgets.subtitle.set_label(&format_duration_ms(meta.duration_ms));
        widgets.subtitle.set_visible(true);
    } else {
        widgets.subtitle.set_label("");
        widgets.subtitle.set_visible(false);
    }

    // Click-on-card → open editor (remix panel). We attach to the inner
    // `content` Box, NOT the outer `card` Overlay: the kebab MenuButton
    // sits on the Overlay layer and swallows its own clicks, so attaching
    // here keeps kebab activations from also triggering the editor.
    //
    // The gesture is re-wired on every bind because ListView recycles
    // CardWidgets across model items — without removing the previous
    // gesture, recycled widgets would fire `on_remix` for *every* clip
    // they were ever bound to. `observe_controllers()` returns a live
    // `gio::ListModel`; we snapshot via `into_iter()` so removing during
    // iteration is safe.
    let existing: Vec<_> = widgets
        .content
        .observe_controllers()
        .iter::<glib::Object>()
        .flatten()
        .filter_map(|c| c.downcast::<gtk::GestureClick>().ok())
        .collect();
    for c in existing {
        widgets.content.remove_controller(&c);
    }
    let gesture = gtk::GestureClick::builder().button(1).build();
    {
        let on_remix = on_remix.clone();
        let storage_dir = storage_dir.clone();
        let filename = meta.filename.clone();
        gesture.connect_released(move |g, _n_press, _x, _y| {
            // Left-click only (gesture is built with `.button(1)`); claim
            // the sequence so the click doesn't propagate up and trigger
            // any default activation on the card.
            g.set_state(gtk::EventSequenceState::Claimed);
            let dir = storage_dir.borrow().clone();
            on_remix(dir.join(&filename));
        });
    }
    widgets.content.add_controller(gesture);

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
/// for the storage_dir + model so that Rename / Delete / Open-in-Folder
/// can mutate disk state without walking widget trees or holding
/// ClipsPage refs. The Rc means a live `set_storage_dir()` change is
/// seen by any subsequent kebab activation. The Remix entry was removed
/// when the click-to-edit gesture landed on `CardWidgets.content` —
/// see `bind_clip_card`.
fn build_clip_actions(
    widgets: &CardWidgets,
    filename: String,
    storage_dir: Rc<RefCell<PathBuf>>,
    model: gio::ListStore,
) -> gio::SimpleActionGroup {
    let group = gio::SimpleActionGroup::new();

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

/// Maximum length (in chars) of a user-typed display name. Long enough for
/// a descriptive sentence, short enough to keep the card title sane.
const MAX_DISPLAY_NAME_CHARS: usize = 80;

/// Set the `display_name` of the index entry matching `filename`, then
/// persist. Returns `true` if an entry was found and updated. Best-effort
/// on the save (errors logged, not propagated) since the in-memory model
/// is refreshed by the caller from the same reconcile path regardless.
fn set_display_name_in_index(filename: &str, new_name: &str) -> bool {
    let mut idx = crate::clips::library::load_index();
    let mut changed = false;
    for m in idx.iter_mut() {
        if m.filename == filename {
            m.display_name = new_name.to_string();
            changed = true;
        }
    }
    if changed {
        if let Err(e) = crate::clips::library::save_index(&idx) {
            log::warn!("rename: failed to update clips index: {e}");
        }
    }
    changed
}

fn show_rename_dialog(
    anchor: &impl IsA<gtk::Widget>,
    filename: String,
    storage_dir: PathBuf,
    model: gio::ListStore,
) {
    use crate::clips::library;

    // Pre-fill the entry with the clip's current display name (free-text
    // label, NOT the on-disk filename). Renaming now edits only this label;
    // the .mp4 file on disk is never touched. If the index entry has no
    // display_name yet (e.g. mid-backfill), fall back to the default
    // creation-time label so the user starts from something meaningful.
    let current_display = library::load_index()
        .into_iter()
        .find(|m| m.filename == filename)
        .map(|m| {
            if m.display_name.is_empty() {
                library::default_display_name(m.created_unix as i64)
            } else {
                m.display_name
            }
        })
        .unwrap_or_default();

    let dialog = adw::AlertDialog::new(
        Some("Rename clip"),
        Some("Pick a name for this clip. This only changes the label shown here, not the file on disk."),
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
        .text(&current_display)
        // Hard cap typed input at the display-name limit. We still trim +
        // re-check below in case the platform IME bypasses max_length.
        .max_length(MAX_DISPLAY_NAME_CHARS as i32)
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
    dialog.connect_response(None, move |_dlg, response| {
        if response != "save" {
            // "cancel" or any non-save response: AlertDialog closes itself.
            return;
        }
        let new_name = entry.text().trim().to_string();
        if new_name.is_empty() {
            status.set_label("Name cannot be empty.");
            status.set_visible(true);
            return;
        }
        // max_length is in bytes-ish for the Entry; enforce a char cap too
        // (paste / IME can exceed it). Truncate defensively rather than
        // rejecting, so the user isn't blocked over a couple of stray chars.
        let new_name: String = new_name.chars().take(MAX_DISPLAY_NAME_CHARS).collect();
        if new_name == current_display {
            // No-op rename.
            return;
        }
        // Free-text label: no path validation needed, we never touch disk
        // filenames any more. Just persist the label and refresh.
        set_display_name_in_index(&filename, &new_name);
        refresh_model_in_place(&model, &storage_dir);
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
    let reconciled = library::reconcile(storage_dir);
    // Persist the reconciled index so mtime-stamped `created_unix` values
    // and backfilled `display_name`s for previously-unindexed / old-format
    // clips become durable. Without this the backfill would re-run on every
    // refresh and the user's first rename would load an empty display_name
    // from disk. Best-effort — a write failure just means the backfill
    // recomputes next time; the in-memory model below is unaffected.
    if let Err(e) = library::save_index(&reconciled) {
        log::warn!("[clip-lib] failed to persist reconciled index: {e}");
    }
    for meta in reconciled {
        model.append(&ClipObject::new(meta, storage_dir.clone()));
    }
    log::info!(
        "[clip-lib] refresh_clips_model ran for {}; GridView model now has {} item(s)",
        storage_dir.display(),
        model.n_items()
    );
}

/// Build the loaded-library view. Returns the root widget plus the header's
/// "N clips" count `gtk::Label` so `ClipsPage` can keep it in sync with the
/// model on every reconcile.
fn loaded_page(
    storage_dir: Rc<RefCell<PathBuf>>,
    model: &gio::ListStore,
    on_remix: Rc<dyn Fn(PathBuf)>,
) -> (gtk::Widget, gtk::Label) {
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        // Inset the grid from the sidebar / window edges so cards aren't
        // flush against the chrome. Top margin is small because the header
        // row above already supplies the breathing room from the HeaderBar.
        .margin_start(24)
        .margin_end(24)
        .margin_bottom(24)
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
        .max_columns(4)
        .build();
    // Scopes the `gridview.clips-grid > child { padding }` gutter and the
    // softened selection ring (see the CSS provider in window.rs) to this
    // grid only, so other GridViews/ListViews keep their default chrome.
    grid.add_css_class("clips-grid");

    scroll.set_child(Some(&grid));

    // Header row: a "Clips" title on the left and a dim count label.
    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .margin_top(20)
        .margin_start(24)
        .margin_end(24)
        .margin_bottom(12)
        .build();

    let heading = gtk::Label::builder()
        .label("Clips")
        .xalign(0.0)
        .build();
    heading.add_css_class("title-2");
    header.append(&heading);

    let count_label = gtk::Label::builder()
        .label("")
        .xalign(0.0)
        // Baseline-align the count with the title so it reads as a subtitle
        // sitting beside the heading rather than floating.
        .valign(gtk::Align::Baseline)
        .build();
    count_label.add_css_class("dim-label");
    header.append(&count_label);

    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .vexpand(true)
        .hexpand(true)
        .build();
    column.append(&header);
    column.append(&scroll);

    (column.upcast(), count_label)
}

/// Format the count label text with correct singular/plural. `0` reads
/// "0 clips" (plural is the natural English for zero).
fn format_clip_count(n: u32) -> String {
    if n == 1 {
        "1 clip".to_string()
    } else {
        format!("{n} clips")
    }
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
        self.count_label
            .set_label(&format_clip_count(self.model.n_items()));
    }

    /// True when the loaded clip-library GridView is the page the user is
    /// actually looking at: the inner stack's visible child is "loaded" AND
    /// the page widget is mapped (the Clips sidebar tab is the visible content
    /// stack child). Used by the poll-while-visible live-refresh in app.rs so
    /// it only reconciles the dir when the user can see the result, avoiding
    /// needless GridView rebuilds (which would reset scroll/selection) while
    /// the user is on another tab.
    pub fn is_loaded_view_visible(&self) -> bool {
        use gtk::prelude::WidgetExt;
        let on_loaded = self
            .root
            .visible_child_name()
            .map(|n| n == "loaded")
            .unwrap_or(false);
        on_loaded && self.root.is_mapped()
    }

    /// The current storage directory the browser is pointed at. Used by the
    /// poll-while-visible live-refresh to cheaply scan for add/remove changes.
    pub fn storage_dir(&self) -> PathBuf {
        self.storage_dir.borrow().clone()
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

    /// Sync the page-state / visible stack child to the user's
    /// onboarding status. Called from app.rs after `set_storage_dir`
    /// (so the model has been reconciled against the persisted clips
    /// folder) and from the wizard "Done" handler.
    ///
    /// - `onboarding_complete = false` → Onboarding (wizard).
    /// - `onboarding_complete = true`  → Loaded if any clips on disk,
    ///                                    otherwise Empty.
    ///
    /// The Remix child is intentionally not handled here — that path
    /// is only ever entered via a card click, which manages its own
    /// state transition (and the user has to explicitly Close out of
    /// it anyway).
    ///
    /// Reads `self.model.n_items()` rather than taking `has_clips`
    /// as an argument so callers don't have to remember to reconcile
    /// the model before calling. `refresh_clips_model()` already runs
    /// at construction and from `set_storage_dir`, so by the time
    /// this fires the model count is authoritative for what's on
    /// disk.
    pub fn sync_to_onboarding_state(&self, onboarding_complete: bool) {
        if !onboarding_complete {
            self.set_state(PageState::Onboarding);
            return;
        }
        let new_state = if self.model.n_items() == 0 {
            PageState::Empty
        } else {
            PageState::Loaded
        };
        self.set_state(new_state);
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
mod duration_fmt_tests {
    use super::*;

    #[test]
    fn formats_seconds_only() {
        assert_eq!(format_duration_ms(32_000), "0:32");
        assert_eq!(format_duration_ms(5_000), "0:05");
        assert_eq!(format_duration_ms(0), "0:00");
    }

    #[test]
    fn formats_minutes_and_seconds() {
        assert_eq!(format_duration_ms(65_000), "1:05");
        assert_eq!(format_duration_ms(60_000), "1:00");
        assert_eq!(format_duration_ms(125_000), "2:05");
    }

    #[test]
    fn rounds_down_sub_second_remainder() {
        // 32.9s -> 0:32 (we floor; the subtitle is approximate by design).
        assert_eq!(format_duration_ms(32_900), "0:32");
    }

    #[test]
    fn formats_over_ten_minutes() {
        assert_eq!(format_duration_ms(630_000), "10:30");
    }

    #[test]
    fn clip_count_singular_vs_plural() {
        assert_eq!(format_clip_count(0), "0 clips");
        assert_eq!(format_clip_count(1), "1 clip");
        assert_eq!(format_clip_count(2), "2 clips");
        assert_eq!(format_clip_count(42), "42 clips");
    }
}
