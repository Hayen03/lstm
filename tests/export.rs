use std::{fs, path::Path, process::Command};
use tempfile::tempdir;

#[test]
fn absolute_coordinates_use_the_bottom_center_origin() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let model = dir.path().join("origin.lstm");
    let output = dir.path().join("origin.png");
    fs::write(
        &model,
        r##"{axiom:"P{x:0,y:6}H{rot:0}C{color:#ff0000}F{length:8,width:2}"}"##,
    )?;
    render(&model, &output)?;
    let pixels = tiny_skia::Pixmap::load_png(output)?;
    // Canvas is 96x64: x=0 is its horizontal center, y=6 is six pixels up.
    let bottom = pixels.pixel(52, 58).unwrap();
    assert!(bottom.red() > 240 && bottom.green() < 10);
    let former_center = pixels.pixel(52, 26).unwrap();
    assert_eq!(former_center.green(), 255);
    Ok(())
}

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

#[test]
fn native_and_json5_render_identically_regardless_of_extension()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    // Existing JSON5 .lstm files and native files with other extensions both work.
    let json5 = dir.path().join("legacy.lstm");
    let native = dir.path().join("native.txt");
    fs::write(
        &json5,
        r##"{axiom:'A',symbols:['A'],rules:[{from:'A',to:'AA'}],drawing:[{from:'A',to:'F{length:12,width:3,color:#dd0000}'}]}"##,
    )?;
    fs::write(
        &native,
        "\u{feff}// native model\nsymbols: {A}; axiom: A; growth: {A => AA}; draw: {A => F{length:12,width:3,color:#dd0000}};",
    )?;
    for extension in ["png", "gif"] {
        let a = dir.path().join(format!("legacy.{extension}"));
        let b = dir.path().join(format!("native.{extension}"));
        render(&json5, &a)?;
        render(&native, &b)?;
        assert_eq!(fs::read(a)?, fs::read(b)?);
    }
    Ok(())
}
