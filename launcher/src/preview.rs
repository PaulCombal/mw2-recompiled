// What a field of view looks like: a street drawn by hand, from where a player stands,
// with the game's own projection -- the angle is the width of a 4:3 picture and a wider
// one, 16:9 or 16:10, is as tall. It is a drawing, not the game's picture: it shows what
// an angle does, more to the sides and everything ahead smaller.

use crate::draw::{DrawList, Point, int, point, rgba};
use crate::ui::{std_clamp, std_max, std_min};

const RADIANS: f32 = 0.01745329;
const EYE: f32 = 60.0; // the game's units, an inch each: where a standing player's eyes are
const NEAR: f32 = 6.0;

// The game's axes: x ahead, y to the left, z up.
#[derive(Clone, Copy)]
struct P3 {
    x: f32,
    y: f32,
    z: f32,
}

const fn p3(x: f32, y: f32, z: f32) -> P3 {
    P3 { x, y, z }
}

#[derive(Clone, Copy)]
struct Colour {
    r: f32,
    g: f32,
    b: f32,
}

const fn colour(r: f32, g: f32, b: f32) -> Colour {
    Colour { r, g, b }
}

struct Face {
    corner: [P3; 4],
    normal: P3,
    colour: Colour,
    flat: bool, // painted on the ground: under everything else
    held: bool, // in the player's hands: over everything else
}

const HAZE: Colour = colour(168.0, 170.0, 160.0);

fn face(corner: [P3; 4], normal: P3, colour: Colour) -> Face {
    Face { corner, normal, colour, flat: false, held: false }
}

fn add_box(faces: &mut Vec<Face>, low: P3, high: P3, colour: Colour) {
    let (x0, y0, z0, x1, y1, z1) = (low.x, low.y, low.z, high.x, high.y, high.z);
    faces.push(face([p3(x0, y0, z0), p3(x0, y1, z0), p3(x0, y1, z1), p3(x0, y0, z1)], p3(-1.0, 0.0, 0.0), colour));
    faces.push(face([p3(x1, y0, z0), p3(x1, y1, z0), p3(x1, y1, z1), p3(x1, y0, z1)], p3(1.0, 0.0, 0.0), colour));
    faces.push(face([p3(x0, y0, z0), p3(x1, y0, z0), p3(x1, y0, z1), p3(x0, y0, z1)], p3(0.0, -1.0, 0.0), colour));
    faces.push(face([p3(x0, y1, z0), p3(x1, y1, z0), p3(x1, y1, z1), p3(x0, y1, z1)], p3(0.0, 1.0, 0.0), colour));
    faces.push(face([p3(x0, y0, z1), p3(x1, y0, z1), p3(x1, y1, z1), p3(x0, y1, z1)], p3(0.0, 0.0, 1.0), colour));
}

fn paint(faces: &mut Vec<Face>, x0: f32, x1: f32, y0: f32, y1: f32, colour: Colour) {
    let mut painted =
        face([p3(x0, y0, 0.0), p3(x1, y0, 0.0), p3(x1, y1, 0.0), p3(x0, y1, 0.0)], p3(0.0, 0.0, 1.0), colour);
    painted.flat = true;
    faces.push(painted);
}

// A figure the height of a soldier: something whose size on the picture says how far it
// is.
fn soldier(faces: &mut Vec<Face>, x: f32, y: f32) {
    let (cloth, skin) = (colour(82.0, 88.0, 62.0), colour(150.0, 120.0, 96.0));
    add_box(faces, p3(x - 5.0, y - 9.0, 0.0), p3(x + 5.0, y - 1.0, 32.0), cloth);
    add_box(faces, p3(x - 5.0, y + 1.0, 0.0), p3(x + 5.0, y + 9.0, 32.0), cloth);
    add_box(faces, p3(x - 6.0, y - 11.0, 32.0), p3(x + 6.0, y + 11.0, 58.0), cloth);
    add_box(faces, p3(x - 5.0, y - 5.0, 59.0), p3(x + 5.0, y + 5.0, 70.0), skin);
}

// The weapon and the hands on it, a few inches ahead of the eye and below it to the
// right. The game draws its own with the view's angle like everything else, so a wider
// view makes it smaller and longer.
fn weapon(faces: &mut Vec<Face>) {
    let first = faces.len();
    let (steel, dark, glove, sleeve) =
        (colour(46.0, 47.0, 50.0), colour(30.0, 31.0, 33.0), colour(58.0, 60.0, 50.0), colour(82.0, 88.0, 62.0));
    let e = EYE;
    add_box(faces, p3(6.5, -6.4, e - 7.6), p3(10.0, -4.6, e - 4.8), dark); // the stock
    add_box(faces, p3(10.0, -6.6, e - 8.0), p3(22.0, -4.4, e - 4.5), steel); // the receiver
    add_box(faces, p3(22.0, -6.3, e - 7.2), p3(34.0, -4.7, e - 5.0), dark); // the handguard
    add_box(faces, p3(34.0, -5.8, e - 6.3), p3(41.0, -5.2, e - 5.7), steel); // the barrel
    add_box(faces, p3(11.0, -5.8, e - 4.5), p3(12.0, -5.2, e - 3.7), dark); // the sights
    add_box(faces, p3(33.0, -5.7, e - 5.0), p3(33.6, -5.3, e - 3.8), dark);
    add_box(faces, p3(17.0, -6.0, e - 12.5), p3(19.5, -5.0, e - 8.0), dark); // the magazine
    add_box(faces, p3(9.0, -7.4, e - 11.5), p3(14.0, -4.2, e - 8.0), glove); // the hand on the grip
    add_box(faces, p3(6.5, -10.5, e - 16.0), p3(10.5, -6.2, e - 10.0), sleeve);
    add_box(faces, p3(24.0, -7.0, e - 9.6), p3(29.0, -4.2, e - 7.2), glove); // the hand under the handguard
    add_box(faces, p3(15.0, -4.6, e - 14.0), p3(25.0, 0.5, e - 9.6), sleeve);
    for f in &mut faces[first..] {
        f.held = true;
    }
}

fn street() -> Vec<Face> {
    let mut faces = Vec::new();
    // The road, its kerbs and the line down its middle.
    paint(&mut faces, NEAR, 6000.0, -210.0, 210.0, colour(62.0, 62.0, 60.0));
    paint(&mut faces, NEAR, 6000.0, 200.0, 210.0, colour(120.0, 120.0, 112.0));
    paint(&mut faces, NEAR, 6000.0, -210.0, -200.0, colour(120.0, 120.0, 112.0));
    let mut x = 60.0f32;
    while x < 4000.0 {
        paint(&mut faces, x, x + 110.0, -4.0, 4.0, colour(176.0, 170.0, 130.0));
        x += 220.0;
    }

    // Buildings down both sides, with a gap for a side street: from, to, height, colour.
    let wall =
        [colour(128.0, 112.0, 94.0), colour(112.0, 104.0, 96.0), colour(140.0, 126.0, 104.0), colour(98.0, 96.0, 92.0)];
    let left: [(f32, f32, f32, usize); 5] = [
        (-200.0, 520.0, 300.0, 0),
        (560.0, 1300.0, 430.0, 1),
        (1560.0, 2400.0, 340.0, 2),
        (2440.0, 3600.0, 520.0, 3),
        (3640.0, 5200.0, 380.0, 0),
    ];
    let right: [(f32, f32, f32, usize); 5] = [
        (-200.0, 300.0, 360.0, 2),
        (620.0, 1500.0, 280.0, 3),
        (1540.0, 2100.0, 460.0, 0),
        (2380.0, 3400.0, 330.0, 1),
        (3440.0, 5200.0, 480.0, 2),
    ];
    for (from, to, height, c) in left {
        add_box(&mut faces, p3(from, 260.0, 0.0), p3(to, 700.0, height), wall[c]);
    }
    for (from, to, height, c) in right {
        add_box(&mut faces, p3(from, -700.0, 0.0), p3(to, -260.0, height), wall[c]);
    }
    add_box(&mut faces, p3(5200.0, -900.0, 0.0), p3(5600.0, 900.0, 600.0), wall[3]);

    // Beside the player, where only a wide view reaches: a wall to the left, crates to
    // the right.
    add_box(&mut faces, p3(20.0, 150.0, 0.0), p3(150.0, 162.0, 96.0), colour(150.0, 144.0, 128.0));
    add_box(&mut faces, p3(70.0, -150.0, 0.0), p3(110.0, -110.0, 40.0), colour(122.0, 96.0, 58.0));
    add_box(&mut faces, p3(78.0, -144.0, 40.0), p3(104.0, -118.0, 66.0), colour(134.0, 106.0, 64.0));

    // Ahead: a car's worth of box, a barrier, and soldiers at three distances.
    add_box(&mut faces, p3(620.0, 60.0, 0.0), p3(800.0, 140.0, 52.0), colour(70.0, 84.0, 104.0));
    add_box(&mut faces, p3(650.0, 68.0, 52.0), p3(760.0, 132.0, 78.0), colour(58.0, 70.0, 88.0));
    add_box(&mut faces, p3(1150.0, -190.0, 0.0), p3(1170.0, -40.0, 44.0), colour(150.0, 150.0, 142.0));
    soldier(&mut faces, 330.0, -60.0);
    soldier(&mut faces, 900.0, 30.0);
    soldier(&mut faces, 1900.0, -50.0);
    weapon(&mut faces);
    faces
}

// Right, up, ahead of the eye.
#[derive(Clone, Copy, Default)]
struct Seen {
    x: f32,
    y: f32,
    z: f32,
}

fn from_eye(p: P3) -> Seen {
    Seen { x: -p.y, y: p.z - EYE, z: p.x }
}

// IM_COL32 of C ints, as the macro casts them.
fn im_col32(r: i32, g: i32, b: i32, a: i32) -> u32 {
    ((a as u32) << 24) | ((b as u32) << 16) | ((g as u32) << 8) | (r as u32)
}

fn shade(c: Colour, light: f32, distance: f32) -> u32 {
    let haze = 1.0 - (-distance / 3200.0).exp();
    let channel = |c: f32, h: f32| int(std_clamp(c * light + (h - c * light) * haze, 0.0, 255.0));
    im_col32(channel(c.r, HAZE.r), channel(c.g, HAZE.g), channel(c.b, HAZE.b), 255)
}

/// Degrees across a picture of `aspect`, its width over its height, for the game's angle.
pub fn across(fov: f32, aspect: f32) -> f32 {
    2.0 * ((fov * 0.5 * RADIANS).tan() * 0.75 * aspect).atan() / RADIANS
}

/// The street, and the angle shown, which moves to a new one over a moment so that the
/// change is seen happening.
pub struct Preview {
    street: Vec<Face>,
    shown: Option<f32>,
}

struct Drawn {
    corner: [Point; 8],
    corners: usize,
    distance: f32,
    colour: u32,
    layer: i32,
}

impl Preview {
    pub fn new() -> Preview {
        Preview { street: street(), shown: None }
    }

    /// Draws the street in the rectangle, at the angle, `delta_time` seconds after the
    /// last frame.
    #[allow(clippy::needless_range_loop)] // the corners counted by index
    pub fn draw(&mut self, draw: &mut DrawList, min: Point, max: Point, fov: f32, aspect: f32, delta_time: f32) {
        let shown = self.shown.get_or_insert(fov);
        *shown += (fov - *shown) * std_min(1.0, delta_time * 9.0);
        if (fov - *shown).abs() < 0.02 {
            *shown = fov;
        }
        let shown = *shown;

        // The game's own: the angle's tangent is the 4:3 picture's half width, three
        // quarters of it the half height, and the width follows the shape.
        let tan_y = (shown * 0.5 * RADIANS).tan() * 0.75;
        let tan_x = tan_y * aspect;
        let middle = point((min.x + max.x) * 0.5, (min.y + max.y) * 0.5);
        let (half_width, half_height) = ((max.x - min.x) * 0.5, (max.y - min.y) * 0.5);
        let on_picture =
            |s: Seen| point(middle.x + s.x / s.z / tan_x * half_width, middle.y - s.y / s.z / tan_y * half_height);

        draw.push_clip(min, max);
        // The sky, and the ground to the horizon, which a level view has across the middle.
        let (sky, horizon) = (rgba(104, 122, 138, 255), rgba(168, 170, 160, 255));
        draw.add_rect_filled_multi_color(min, point(max.x, middle.y), sky, sky, horizon, horizon);
        let (near, far) = (rgba(150, 150, 140, 255), rgba(84, 80, 70, 255));
        draw.add_rect_filled_multi_color(point(min.x, middle.y), max, near, near, far, far);

        let mut drawn: Vec<Drawn> = Vec::with_capacity(self.street.len());
        let sun = p3(-0.45, -0.35, 0.82);
        for face in &self.street {
            // Faces turned away from the eye are behind their box.
            let c = face.corner[0];
            if face.normal.x * c.x + face.normal.y * c.y + face.normal.z * (c.z - EYE) >= 0.0 {
                continue;
            }

            // Cut at the near plane: what is behind the eye has no place on the picture.
            let mut cut = [Seen::default(); 8];
            let mut count = 0;
            for i in 0..4 {
                let (a, b) = (from_eye(face.corner[i]), from_eye(face.corner[(i + 1) % 4]));
                if a.z >= NEAR {
                    cut[count] = a;
                    count += 1;
                }
                if (a.z >= NEAR) != (b.z >= NEAR) {
                    let t = (NEAR - a.z) / (b.z - a.z);
                    cut[count] = Seen { x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t, z: NEAR };
                    count += 1;
                }
            }
            if count < 3 {
                continue;
            }

            let mut d = Drawn {
                corner: [Point::default(); 8],
                corners: count,
                distance: 0.0,
                colour: 0,
                layer: if face.flat {
                    0
                } else if face.held {
                    2
                } else {
                    1
                },
            };
            let mut sum = 0.0f32;
            for i in 0..count {
                d.corner[i] = on_picture(cut[i]);
                sum += (cut[i].x * cut[i].x + cut[i].y * cut[i].y + cut[i].z * cut[i].z).sqrt();
            }
            d.distance = sum / count as f32;
            // The draw list smooths the edge of a shape whose corners go clockwise.
            let mut turn = 0.0f32;
            for i in 0..count {
                let (a, b) = (d.corner[i], d.corner[(i + 1) % count]);
                turn += a.x * b.y - b.x * a.y;
            }
            if turn < 0.0 {
                d.corner[..count].reverse();
            }
            let n = face.normal;
            let lit = std_max(0.0, n.x * sun.x + n.y * sun.y + n.z * sun.z);
            // The ground runs from the feet to the horizon: its haze is the far end's, not
            // the middle's.
            d.colour = shade(face.colour, 0.58 + 0.42 * lit, if face.flat { 500.0 } else { d.distance });
            drawn.push(d);
        }
        // The ground, the street, then what the player holds; in each the farthest first.
        // The boxes do not cross, so nearer ones cover them rightly.
        drawn.sort_by(|a, b| {
            use std::cmp::Ordering;
            if a.layer != b.layer {
                a.layer.cmp(&b.layer)
            } else if a.distance > b.distance {
                Ordering::Less
            } else if b.distance > a.distance {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        });
        for d in &drawn {
            draw.add_convex_poly_filled(&d.corner[..d.corners], d.colour);
        }

        // Where the player aims.
        let tick = std_max(2.0, half_height * 0.035);
        let white = rgba(236, 236, 232, 220);
        for side in [-1.0f32, 1.0] {
            let (near, far) = (side * tick, side * tick * 2.6);
            draw.add_line(point(middle.x + near, middle.y), point(middle.x + far, middle.y), white, 1.5);
            draw.add_line(point(middle.x, middle.y + near), point(middle.x, middle.y + far), white, 1.5);
        }
        draw.pop_clip();
        draw.add_rect(min, max, rgba(0, 0, 0, 200), 0.0, 1.0);
    }
}
