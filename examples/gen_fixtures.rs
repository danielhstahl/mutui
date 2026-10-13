//! Write a small synthetic music library (used for manual demos and by the
//! integration tests): `cargo run --example gen_fixtures -- /tmp/library`.

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| {
        std::env::temp_dir()
            .join("music-tui-demo")
            .display()
            .to_string()
    });
    let path = std::path::Path::new(&dir);
    std::fs::create_dir_all(path).expect("create folder");
    let files = music_tui::testing::write_synthetic_library(path).expect("write fixtures");
    println!("wrote {} files under {}", files.len(), path.display());
}
