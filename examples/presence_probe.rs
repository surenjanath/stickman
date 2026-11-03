//! Prints whether the Mac is playing audio and whether a camera is in use.
fn main() {
    println!("audio playing: {}", stickman::music::output_in_use());
    println!("camera in use: {}", stickman::music::camera_in_use());
}
