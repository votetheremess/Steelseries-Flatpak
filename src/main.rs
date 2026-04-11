mod app;
mod audio;
mod hid;
mod window;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    app::run();
}
