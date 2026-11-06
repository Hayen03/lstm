//! Turtle execution and deferred rasterization of lines, disks, and filled polygons.
//! World coordinates are centered on the canvas with positive Y pointing upward.

use crate::err::{Error, Result};
use crate::syntax::Token;
use tiny_skia::{Color, FillRule, LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};

type Point = (f64, f64);
#[derive(Clone, Copy)]
struct Pose {
    p: Point,
    heading: f64,
    color: u32,
}
enum Shape {
    Line(Point, Point, f64, u32),
    Disk(Point, f64, u32),
    Polygon(Vec<Point>, u32),
}

/// Maintains the turtle even when no image is being captured for this generation.
pub struct Dessin {
    pose: Pose,
    stack: Vec<Pose>,
    shapes: Vec<Shape>,
    // The insertion index places a completed fill beneath its own outline strokes.
    fill: Option<(usize, Vec<Point>)>,
    pub width: u32,
    pub height: u32,
    pub background: u32,
    /// Collect shapes for rasterization; movement and color changes always execute.
    pub capture: bool,
}
impl Dessin {
    pub fn new(width: u32, height: u32) -> Self {
        let mut d = Self {
            pose: Pose {
                p: (0., 0.),
                heading: 90.,
                color: 0,
            },
            stack: vec![],
            shapes: vec![],
            fill: None,
            width,
            height,
            background: 0xffffff,
            capture: true,
        };
        d.reset();
        d
    }
    /// Clears geometry and branches, returning to bottom center facing upward.
    /// The current pen color is retained across resets.
    pub fn reset(&mut self) {
        self.pose.p = (0., -(self.height as f64) / 2.);
        self.pose.heading = 90.;
        self.stack.clear();
        self.shapes.clear();
        self.fill = None;
    }
    pub fn pose(&self) -> (Point, f64) {
        (self.pose.p, self.pose.heading)
    }
    fn forward(&self, length: f64) -> Point {
        let a = self.pose.heading.to_radians();
        (
            self.pose.p.0 + length * a.cos(),
            self.pose.p.1 + length * a.sin(),
        )
    }
    fn go(&mut self, p: Point) {
        if let Some((_, points)) = &mut self.fill {
            points.push(p);
        }
        self.pose.p = p;
    }
    /// Executes one primitive; symbols without a primitive action are ignored.
    pub fn apply(&mut self, t: &Token) -> Result<()> {
        let n = |name: &str, default: f64| -> Result<f64> {
            Ok(match t.args.get(name) {
                Some(v) => v.to_f64()?,
                None => default,
            })
        };
        let p = self.pose.p;
        match t.ch.as_ref() {
            "F" | "M" | "D" | "P" => {
                // F/M move relative to the heading; D/P use absolute coordinates.
                // Only F and D leave a stroke, but all four extend an active fill.
                let next = if matches!(t.ch.as_ref(), "F" | "M") {
                    self.forward(n("length", 1.)?)
                } else {
                    (n("x", p.0)?, n("y", p.1)?)
                };
                if self.capture && matches!(t.ch.as_ref(), "F" | "D") {
                    self.shapes
                        .push(Shape::Line(p, next, n("width", 1.)?, self.pose.color))
                }
                self.go(next);
            }
            "L" => {
                // A leaf disk is centered one radius ahead without moving the turtle.
                let r = n("size", 1.)?;
                if self.capture {
                    self.shapes
                        .push(Shape::Disk(self.forward(r), r, self.pose.color))
                }
            }
            "C" => {
                self.pose.color = t
                    .args
                    .get("color")
                    .map(|v| v.to_i64())
                    .transpose()?
                    .unwrap_or(0) as u32
            }
            "R" => self.pose.heading = (self.pose.heading - n("deg", 0.)?).rem_euclid(360.),
            "H" => self.pose.heading = n("rot", self.pose.heading)?.rem_euclid(360.),
            "[" => self.stack.push(self.pose),
            "]" => {
                if let Some(state) = self.stack.pop() {
                    self.go(state.p);
                    self.pose = state
                }
            }
            "(" if self.capture => self.fill = Some((self.shapes.len(), vec![p])),
            ")" => {
                if let Some((i, pts)) = self.fill.take() {
                    self.shapes.insert(i, Shape::Polygon(pts, self.pose.color))
                }
            }
            _ => (),
        }
        Ok(())
    }
    /// Paints captured shapes in order onto an opaque background.
    pub fn raster(&self) -> Result<Pixmap> {
        let mut canvas =
            Pixmap::new(self.width, self.height).ok_or(Error::InvalidCanvasDimensions {
                width: self.width,
                height: self.height,
            })?;
        let color = |c: u32| Color::from_rgba8((c >> 16) as u8, (c >> 8) as u8, c as u8, 255);
        // Raster coordinates start at top left and increase downward.
        let xy = |(x, y): Point| {
            (
                (x + self.width as f64 / 2.) as f32,
                (self.height as f64 / 2. - y) as f32,
            )
        };
        canvas.fill(color(self.background));
        for shape in &self.shapes {
            let mut path = PathBuilder::new();
            let mut paint = Paint::default();
            paint.anti_alias = true;
            let (c, stroke) = match shape {
                Shape::Line(a, b, width, c) => {
                    let (x, y) = xy(*a);
                    path.move_to(x, y);
                    let (x, y) = xy(*b);
                    path.line_to(x, y);
                    (*c, Some(*width))
                }
                Shape::Disk(p, r, c) => {
                    let (x, y) = xy(*p);
                    if *r <= 0. {
                        continue;
                    }
                    path.push_circle(x, y, *r as f32);
                    (*c, None)
                }
                Shape::Polygon(points, c) => {
                    if points.len() < 3 {
                        continue;
                    }
                    let (x, y) = xy(points[0]);
                    path.move_to(x, y);
                    for &p in &points[1..] {
                        let (x, y) = xy(p);
                        path.line_to(x, y)
                    }
                    path.close();
                    (*c, None)
                }
            };
            paint.set_color(color(c));
            if let Some(path) = path.finish() {
                if let Some(w) = stroke {
                    let stroke = Stroke {
                        width: (w as f32).max(0.1),
                        line_cap: LineCap::Round,
                        ..Default::default()
                    };
                    canvas.stroke_path(&path, &paint, &stroke, Transform::identity(), None)
                } else {
                    canvas.fill_path(
                        &path,
                        &paint,
                        FillRule::Winding,
                        Transform::identity(),
                        None,
                    )
                }
            }
        }
        Ok(canvas)
    }
}
