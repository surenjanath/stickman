//! Prints whether the Mac is playing audio, once a second for 5 s.
fn main() {
    for _ in 0..5 {
        println!("audio playing: {}", stickman::music::output_in_use());
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
