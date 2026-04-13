mod app;
mod audio;
mod autostart;
mod eq;
mod hid;
mod icons;
mod tray;
mod window;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let hidden = std::env::args().any(|a| a == "--hidden");
    app::run(hidden);
}
