//! Time OCR on a window: `cargo run --release --example ocr_probe -- <window-id>`
fn main() {
    let id: u32 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .expect("window id");
    let t = std::time::Instant::now();
    match stykanimate_overlay::ocr::read_window_now(id) {
        Some((lines, words)) => println!(
            "{} lines, {} words in {:?}",
            lines.len(),
            words.len(),
            t.elapsed()
        ),
        None => println!("capture failed after {:?}", t.elapsed()),
    }
}
