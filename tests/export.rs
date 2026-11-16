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
    render_with_args(model, out, &[])
}

fn render_with_args(
    model: &Path,
    out: &Path,
    args: &[&str],
) -> Result<(), Box<dyn std::error::Error>> {
    let result = Command::new(env!("CARGO_BIN_EXE_lstm"))
        .arg(model)
        .args(args)
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
fn transparent_exports_preserve_shapes_and_clear_each_generation()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let model = dir.path().join("transparent.lstm");
    // A red shape on a red background must survive, as must opaque black.
    fs::write(
        &model,
        r#"
        bg: #ff0000;
        axiom: A;
        draw: {A => P{x:gen()*12-24,y:20}L{size:5,color:#ff0000}
                    P{x:24,y:20}L{size:5,color:#000000}};
        "#,
    )?;
    for flag in ["--transparent", "-T"] {
        let png = dir.path().join("transparent.png");
        render_with_args(&model, &png, &[flag])?;
        let pixels = tiny_skia::Pixmap::load_png(&png)?;
        assert_eq!(pixels.pixel(0, 0).unwrap().alpha(), 0);
        assert_eq!(pixels.pixel(48, 39).unwrap().demultiply().red(), 255);
        assert_eq!(pixels.pixel(48, 39).unwrap().alpha(), 255);
        assert_eq!(pixels.pixel(72, 39).unwrap().red(), 0);
        assert_eq!(pixels.pixel(72, 39).unwrap().alpha(), 255);
        assert!(pixels.pixels().iter().any(|p| p.alpha() > 0 && p.alpha() < 255));

        let gif = dir.path().join("transparent.gif");
        render_with_args(&model, &gif, &[flag])?;
        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::RGBA);
        let mut reader = options.read_info(fs::File::open(gif)?)?;
        let mut generation = 0;
        while let Some(frame) = reader.read_next_frame()? {
            assert_eq!((frame.width, frame.height, frame.delay), (96, 64, 50));
            assert!(frame.transparent.is_some());
            assert_eq!(frame.dispose, gif::DisposalMethod::Background);
            let pixel = |x: usize, y: usize| &frame.buffer[(y * 96 + x) * 4..][..4];
            assert_eq!(pixel(0, 0)[3], 0);
            assert_eq!(pixel(24 + generation * 12, 39), [255, 0, 0, 255]);
            assert_eq!(pixel(72, 39), [0, 0, 0, 255]);
            if generation > 0 {
                assert_eq!(pixel(24 + (generation - 1) * 12, 39)[3], 0);
            }
            // Partially covered red edges must not turn dark during GIF encoding.
            for p in frame.buffer.chunks_exact(4).filter(|p| p[3] != 0) {
                assert!(p == [255, 0, 0, 255] || p == [0, 0, 0, 255]);
            }
            generation += 1;
        }
        assert_eq!(generation, 3);
    }
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

#[test]
fn native_backgrounds_use_the_effective_target_and_final_pose_in_png_and_gif()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let model = dir.path().join("background.lstm");
    fs::write(
        &model,
        r#"
        globals: {target:mxgen()}; recommended_iterations:9;
        name: 'Target '+str(target); width:8*2; height:16;
        axiom: A;
        draw: {A weight gen()+1 => P{x:gen()+1,y:0}};
        bg: target*65536+gen()*256+int(pos()[0])+screen()[0];
    "#,
    )?;
    let png = dir.path().join("background.png");
    render(&model, &png)?;
    let pixels = tiny_skia::Pixmap::load_png(&png)?;
    let pixel = pixels.pixel(0, 0).unwrap();
    assert_eq!((pixel.red(), pixel.green(), pixel.blue()), (2, 2, 99));

    let gif = dir.path().join("background.gif");
    render(&model, &gif)?;
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    let mut reader = options.read_info(fs::File::open(gif)?)?;
    let mut colors = Vec::new();
    while let Some(frame) = reader.read_next_frame()? {
        colors.push(frame.buffer[..4].to_vec());
    }
    assert_eq!(
        colors,
        vec![
            vec![2, 0, 97, 255],
            vec![2, 1, 98, 255],
            vec![2, 2, 99, 255]
        ]
    );
    Ok(())
}

#[test]
fn tracing_dynamic_weights_and_backgrounds_preserves_seeded_rendering()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let model = dir.path().join("random.lstm");
    fs::write(
        &model,
        r#"
        axiom: A;
        growth: {grow:A weight random()+1 => AA, stay:A weight random()+1 => A};
        draw: {line:A weight random()+1 => F{length:8,color:#ff0000}};
        bg: int(random()*65535);
    "#,
    )?;
    let plain = dir.path().join("plain.png");
    render(&model, &plain)?;
    let traced = dir.path().join("traced.png");
    let result = Command::new(env!("CARGO_BIN_EXE_lstm"))
        .arg(&model)
        .args([
            "-n",
            "2",
            "--seed",
            "fixed",
            "--width",
            "96",
            "--height",
            "64",
            "--trace-draw",
            "--trace-format",
            "jsonl",
            "-o",
        ])
        .arg(&traced)
        .output()?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fs::read(plain)?, fs::read(traced)?);
    let events = String::from_utf8(result.stderr)?
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let backgrounds: Vec<_> = events
        .iter()
        .filter(|e| e["event"] == "background_evaluated")
        .collect();
    assert_eq!(backgrounds.len(), 3);
    for (generation, event) in backgrounds.iter().enumerate() {
        assert_eq!(event["generation"], generation);
        assert!(event["value"].is_number());
        assert!(event.get("rule").is_none());
    }
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "rule_checked" && e["weight"].is_number())
    );
    Ok(())
}
