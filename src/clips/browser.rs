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
    /// Identical to the path used at model-build time, so a `reconcile()`
    /// against this dir reproduces what's on disk now. Stored on the page
    /// so external callers (and the kebab handlers) don't have to re-derive
    /// `~/Videos/Clips` every time.
    pub storage_dir: PathBuf,
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

    // Storage dir + model are owned by `ClipsPage` so the per-card kebab
    // handlers (Rename / Delete) can call `refresh_clips_model()` after
    // mutating disk state. `loaded_page()` builds the GridView against
    // these — `refresh_clips_model()` then keeps the same model alive,
    // just clearing+repopulating it from a fresh `library::reconcile()`.
    let storage_dir = PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join("Videos/Clips");
    let _ = std::fs::create_dir_all(&storage_dir);
    let model = gio::ListStore::new::<ClipObject>();

    let wizard = Rc::new(build_wizard());
    stack.add_named(&wizard.stack, Some("onboarding"));
    stack.add_named(&empty_page(), Some("empty"));
    stack.add_named(&loaded_page(&storage_dir, &model), Some("loaded"));

    let state = Rc::new(RefCell::new(PageState::Onboarding));
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
    storage_dir: &PathBuf,
    model: &gio::ListStore,
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
/// bound clip's filename + the page-level storage_dir + model so that
/// Rename / Delete / Open-in-Folder can mutate disk state and trigger a
/// model rebuild without walking widget trees or holding ClipsPage refs.
fn build_clip_actions(
    widgets: &CardWidgets,
    filename: String,
    storage_dir: PathBuf,
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
                storage_dir.clone(),
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
                storage_dir.clone(),
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
            if let Err(e) = std::process::Command::new("xdg-open")
                .arg(&storage_dir)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                log::warn!("xdg-open failed for {}: {e}", storage_dir.display());
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
    dialog.connect_response(None, move |_dlg, response| {
        if response != "save" {
            // "cancel" or any non-save response: AlertDialog closes itself.
            return;
        }
        let new_stem = entry.text().trim().to_string();
        // AlertDialog closes itself on response; on any validation
        // failure below we simply log + return. The user can re-open the
        // kebab menu and try again. (The `status` label is wired up but
        // currently only displays for the dialog's *initial* present —
        // future improvement: switch to keep-open by intercepting the
        // response signal with a separate Save button in extra_child.)
        if new_stem.is_empty() {
            status.set_label("Name cannot be empty.");
            status.set_visible(true);
            log::warn!("rename: empty name rejected");
            return;
        }
        if new_stem == old_stem {
            // No-op rename.
            return;
        }
        // Sanitize: disallow path separators and NUL so the user can't
        // escape the storage dir.
        if new_stem.contains('/') || new_stem.contains('\\') || new_stem.contains('\0') {
            log::warn!("rename: rejecting name with path separator: {new_stem}");
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
    dialog.connect_response(None, move |_dlg, response| {
        if response != "delete" {
            return;
        }
        let clip_path = storage_dir.join(&filename);
        if let Err(e) = std::fs::remove_file(&clip_path) {
            log::warn!("delete failed: {}: {e}", clip_path.display());
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

fn loaded_page(storage_dir: &PathBuf, model: &gio::ListStore) -> gtk::Widget {
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
    let storage_dir_for_bind = storage_dir.clone();
    let model_for_bind = model.clone();
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
        bind_clip_card(&widgets, &clip, &storage_dir_for_bind, &model_for_bind);
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
        refresh_model_in_place(&self.model, &self.storage_dir);
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
        let storage_for_backfill = self.storage_dir.clone();
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
