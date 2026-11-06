use std::{fs, path::Path, process::Command};
use tempfile::tempdir;

fn render(model: &Path, out: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let result = Command::new(env!("CARGO_BIN_EXE_lstm"))
        .arg(model)
        .args([
            "-n", "2", "--seed", "fixed", "--width", "96", "--height", "64", "-o",
        ])
        .arg(out)
        .output()?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(())
}
#[test]
fn png_and_gif_render_headlessly() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let model = dir.path().join("model.lstm");
    fs::write(
        &model,
        r##"{bg:"#ffffff",axiom:"A",symbols:["A"],drawing:[{from:"A",locals:{len:"12"},to:"C{color:#dd0000}F{length:len,width:3}"}],rules:[{from:"A",to:"AA"}]}"##,
    )?;
    let png = dir.path().join("frame.png");
    render(&model, &png)?;
    let pixels = tiny_skia::Pixmap::load_png(&png)?;
    assert_eq!((pixels.width(), pixels.height()), (96, 64));
    assert!(
        pixels
            .pixels()
            .iter()
            .any(|p| p.red() > 180 && p.green() < 80)
    );
    let video = dir.path().join("frames.gif");
    render(&model, &video)?;
    let mut reader = gif::DecodeOptions::new().read_info(fs::File::open(&video)?)?;
    let mut count = 0;
    while let Some(frame) = reader.read_next_frame()? {
        assert_eq!((frame.width, frame.height, frame.delay), (96, 64, 50));
        count += 1;
    }
    assert_eq!(count, 3);
    Ok(())
}
