//! Debug aid: `HM_GUI_SNAPSHOT=out.png` saves a screenshot of the window a few
//! seconds after startup and quits. Needs the `snapshot` feature.

#[cfg(feature = "snapshot")]
pub fn schedule(win: &crate::MainWindow, path: String) {
    use slint::ComponentHandle;
    let weak = win.as_weak();
    let delay = std::env::var("HM_GUI_SNAPSHOT_DELAY")
        .ok()
        .and_then(|d| d.parse().ok())
        .unwrap_or(3);
    slint::Timer::single_shot(std::time::Duration::from_secs(delay), move || {
        let Some(win) = weak.upgrade() else { return };
        match win.window().take_snapshot() {
            Ok(buf) => {
                let file = std::fs::File::create(&path).expect("create snapshot file");
                let mut enc =
                    png::Encoder::new(std::io::BufWriter::new(file), buf.width(), buf.height());
                enc.set_color(png::ColorType::Rgba);
                enc.set_depth(png::BitDepth::Eight);
                enc.write_header()
                    .unwrap()
                    .write_image_data(buf.as_bytes())
                    .unwrap();
            }
            Err(e) => eprintln!("snapshot failed: {e}"),
        }
        let _ = slint::quit_event_loop();
    });
}

#[cfg(not(feature = "snapshot"))]
pub fn schedule(_win: &crate::MainWindow, _path: String) {
    eprintln!("HM_GUI_SNAPSHOT needs the `snapshot` feature");
}
