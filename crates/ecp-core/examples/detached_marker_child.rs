use std::path::PathBuf;
use std::time::Duration;

fn main() {
    let marker = PathBuf::from(std::env::args().nth(1).expect("arg 1: marker path"));
    std::thread::sleep(Duration::from_millis(200));
    std::fs::write(marker, b"ok").expect("write marker");
}
