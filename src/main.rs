//! CLI orchestration for live viewing and headless PNG/GIF export.
//! Generation zero is the axiom, so an export with N rewrites has N + 1 GIF frames.

mod context;
mod dessin;
pub mod err;
mod model;
mod runtime;
mod syntax;
mod trace;

use crate::err::{Error, Result, ResultContext, ensure};
use clap::Parser;
use dessin::Dessin;
use gif::{Encoder, Frame, Repeat};
use minifb::{Key, Window, WindowOptions};
use model::Model;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tempfile::NamedTempFile;

#[derive(Parser)]
#[command(
    version,
    about = "Render a JSON5 L-system, interactively or to a PNG or GIF."
)]
struct Cli {
    file: PathBuf,
    /// Number of rewrites after the axiom. Use -1 for endless live animation.
    #[arg(short = 'n', allow_hyphen_values = true)]
    iterations: Option<i64>,
    #[arg(short, long)]
    animate: bool,
    #[arg(short = 'm', long)]
    axiom: Option<String>,
    #[arg(short = 'f', long, default_value_t = 2.)]
    fps: f64,
    #[arg(short = 'g', long, default_value = "")]
    globals: String,
    #[arg(short = 's', long, allow_hyphen_values = true)]
    seed: Option<String>,
    /// A PNG contains the final frame. A GIF contains generation 0 through N.
    #[arg(short, long)]
    output: Option<PathBuf>,
    #[arg(short = 'W', long)]
    width: Option<u32>,
    #[arg(short = 'H', long)]
    height: Option<u32>,
    #[command(flatten)]
    trace: trace::Options,
}
fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}
// Compare existing paths canonically as well as literally to catch symlink aliases.
fn same(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(),b.canonicalize()),(Ok(x),Ok(y)) if x==y)
}
fn run(c: Cli) -> Result<()> {
    let src =
        fs::read_to_string(&c.file).with_context(|| format!("Reading {}", c.file.display()))?;
    let mut model = Model::load(&src, c.axiom.as_deref(), &c.globals, c.seed.as_deref())
        .with_context(|| format!("Loading {}", c.file.display()))?;

    let width = c.width.unwrap_or(model.width);
    let height = c.height.unwrap_or(model.height);
    // Dimensions must fit GIF's u16 fields and keep raster allocations bounded.
    ensure!(
        width > 0
            && height > 0
            && width <= 65535
            && height <= 65535
            && u64::from(width) * u64::from(height) <= 64_000_000,
        Error::InvalidCanvasDimensions { width, height }
    );
    ensure!(
        c.fps.is_finite(),
        Error::InvalidFrameRate {
            fps: c.fps,
            reason: "must be finite"
        }
    );
    let ext = c.output.as_deref().map(extension);
    if let Some(e) = &ext {
        ensure!(
            e == "gif" || e == "png",
            Error::UnsupportedOutput {
                extension: e.clone()
            }
        )
    }
    let gif = ext.as_deref() == Some("gif");
    // GIFs need each generation's drawing, including the axiom's animated semantics.
    let animate = c.animate || gif;
    if gif {
        ensure!(
            c.fps > 0.,
            Error::InvalidFrameRate {
                fps: c.fps,
                reason: "GIF requires a positive FPS"
            }
        )
    }
    if let Some(out) = &c.output {
        ensure!(
            !same(out, &c.file),
            Error::OutputOverwritesModel { path: out.clone() }
        )
    }
    if let Some(t) = &c.trace.trace_output {
        ensure!(
            !same(t, &c.file) && !c.output.as_ref().is_some_and(|p| same(p, t)),
            Error::TracePathConflict { path: t.clone() }
        )
    }
    let mut d = Dessin::new(width, height);
    let n = c.iterations.unwrap_or(model.iterations);
    ensure!(
        n >= 0 || (animate && c.output.is_none()),
        Error::UnboundedIteration
    );
    let mut trace = trace::Trace::new(c.trace)?;
    let mut window = if c.output.is_none() {
        Some(
            Window::new(
                &model.name,
                width as usize,
                height as usize,
                WindowOptions::default(),
            )
            .context("Opening display (use -o for headless rendering)")?,
        )
    } else {
        None
    };
    // Encode beside the destination and publish only after successful completion.
    // Keeping both files on the same filesystem allows persist() to rename it.
    let mut tmp = if let Some(out) = &c.output {
        Some(NamedTempFile::new_in(
            out.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?)
    } else {
        None
    };
    let mut encoder = if gif {
        let mut enc = Encoder::new(
            tmp.as_ref().unwrap().reopen()?,
            width as u16,
            height as u16,
            &[],
        )?;
        enc.set_repeat(Repeat::Infinite)?;
        Some(enc)
    } else {
        None
    };
    let mut seq: Arc<[syntax::Token]> = Vec::new().into();
    let mut generation = 0i64;
    let delay = if animate && c.fps > 0. {
        Duration::try_from_secs_f64(1. / c.fps).map_err(|_| Error::InvalidFrameRate {
            fps: c.fps,
            reason: "FPS out of range",
        })?
    } else {
        Duration::ZERO
    };
    d.background = model.bg;
    loop {
        if window
            .as_ref()
            .is_some_and(|w| !w.is_open() || w.is_key_down(Key::Escape))
        {
            return Ok(());
        }
        let start = Instant::now();
        let final_frame = generation == n;
        let show = final_frame || gif || (animate && window.is_some());
        // Capture the same pass that evaluates drawing locals and weighted rules.
        d.capture = show;
        seq = model
            .frame(seq, generation, &mut d, &mut trace, animate)
            .with_context(|| format!("Generation {generation}"))?;
        if show {
            let raster = d.raster()?;
            if let Some(enc) = &mut encoder {
                let mut rgba = raster.data().to_vec();
                let mut f = Frame::from_rgba_speed(width as u16, height as u16, &mut rgba, 10);
                // Difference rounded timestamps to distribute centisecond rounding
                // across frames instead of accumulating a fixed per-frame error.
                let tick = |i: i64| (i as f64 * 100. / c.fps).round();
                f.delay =
                    (tick(generation + 1) - tick(generation)).clamp(1., u16::MAX as f64) as u16;
                enc.write_frame(&f)?;
            } else if let Some(w) = &mut window {
                // minifb expects packed RGB words rather than tiny-skia's RGBA bytes.
                let buf: Vec<u32> = raster
                    .data()
                    .chunks_exact(4)
                    .map(|p| u32::from_be_bytes([0, p[0], p[1], p[2]]))
                    .collect();
                w.update_with_buffer(&buf, width as usize, height as usize)?;
            } else if final_frame {
                tmp.as_mut().unwrap().write_all(&raster.encode_png()?)?;
                break;
            }
        }
        if final_frame {
            break;
        }
        if let Some(w) = &mut window {
            // Pump window events during the wait so slow animations remain responsive.
            w.update();
            while w.is_open() && !w.is_key_down(Key::Escape) && start.elapsed() < delay {
                std::thread::sleep(
                    (delay.saturating_sub(start.elapsed())).min(Duration::from_millis(12)),
                );
                w.update()
            }
        }
        generation = generation.checked_add(1).ok_or(Error::GenerationOverflow)?;
    }
    if let Some(enc) = encoder {
        // Finish the GIF stream before making the temporary file the final output.
        enc.into_inner()?;
    }
    if let (Some(tmp), Some(out)) = (tmp, c.output) {
        tmp.persist(out)?;
    }
    if let Some(mut w) = window {
        // A finite live run keeps its final image visible until the user closes it.
        while w.is_open() && !w.is_key_down(Key::Escape) {
            w.update();
            std::thread::sleep(Duration::from_millis(16))
        }
    }
    Ok(())
}
fn main() -> std::process::ExitCode {
    match run(Cli::parse()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
