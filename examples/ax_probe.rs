//! Prints the Dock's icons and the frontmost window's biggest scrollbar
//! (needs Accessibility for the terminal running it).
fn main() {
    let Some(dock) = stickman::macos::dock_pid() else {
        println!("no Dock");
        return;
    };
    let icons = stickman::macos::dock_icons(dock);
    println!("{} dock icons", icons.len());
    for i in icons.iter().take(5) {
        println!(
            "  {:?} at {:.0},{:.0} {:.0}x{:.0}",
            i.title, i.rect.x, i.rect.y, i.rect.width, i.rect.height
        );
    }
    let front = stickman::windows::snapshot(
        i64::from(std::process::id()),
        &[stickman::geom::Rect {
            x: -1e5,
            y: -1e5,
            width: 2e5,
            height: 2e5,
        }],
    );
    if let Some(w) = front.active() {
        println!(
            "front: {} -> scrollbar {:?}",
            w.owner,
            stickman::macos::find_scrollbar(w.pid)
        );
    }
}
