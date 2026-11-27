// The app icon: the splash's crystal with its speed bolt, drawn in code.
// Shared by the app (window icon) and build.rs (the .exe's icon resource), so the shape is defined once.
// No `//!` docs and no `use` here: build.rs pulls this file in with `include!`.

/// The icon as `n`×`n` RGBA pixels (row by row from the top), 4×4 supersampled.
pub fn crystal_rgba(n: u32) -> Vec<u8> {
    // shapes in the splash SVG's 120×120 coordinates, back to front, with top and bottom colors
    type Shape = (&'static [(f64, f64)], [f64; 3], [f64; 3]);
    let shapes: [Shape; 4] = [
        (&[(60.0, 6.0), (26.0, 38.0), (36.0, 92.0), (60.0, 106.0)], [169.0, 155.0, 255.0], [91.0, 69.0, 240.0]),
        (&[(60.0, 6.0), (94.0, 38.0), (84.0, 92.0), (60.0, 106.0)], [111.0, 216.0, 255.0], [47.0, 107.0, 245.0]),
        (&[(60.0, 6.0), (46.0, 40.0), (26.0, 38.0)], [235.0, 232.0, 255.0], [235.0, 232.0, 255.0]),
        (
            &[(66.0, 30.0), (50.0, 62.0), (61.0, 62.0), (55.0, 88.0), (73.0, 54.0), (62.0, 54.0), (70.0, 30.0)],
            [255.0, 247.0, 194.0],
            [251.0, 191.0, 36.0],
        ),
    ];
    let inside = |poly: &[(f64, f64)], x: f64, y: f64| {
        let mut c = false;
        for i in 0..poly.len() {
            let ((x1, y1), (x2, y2)) = (poly[i], poly[(i + 1) % poly.len()]);
            if (y1 > y) != (y2 > y) && x < x1 + (y - y1) * (x2 - x1) / (y2 - y1) {
                c = !c;
            }
        }
        c
    };
    let mut rgba = Vec::with_capacity((n * n * 4) as usize);
    for py in 0..n {
        for px in 0..n {
            let (mut acc, mut alpha) = ([0.0; 3], 0.0);
            for sy in 0..4 {
                for sx in 0..4 {
                    // map the icon onto the crystal's box (x 26..94, y 4..108) with a small margin
                    let x = 8.0 + (px as f64 + (sx as f64 + 0.5) / 4.0) / n as f64 * 104.0;
                    let y = 4.0 + (py as f64 + (sy as f64 + 0.5) / 4.0) / n as f64 * 104.0;
                    if let Some((_, top, bot)) = shapes.iter().rev().find(|(p, _, _)| inside(p, x, y)) {
                        let t = ((y - 6.0) / 100.0).clamp(0.0, 1.0);
                        for k in 0..3 {
                            acc[k] += top[k] + (bot[k] - top[k]) * t;
                        }
                        alpha += 1.0;
                    }
                }
            }
            let c = |k: usize| if alpha > 0.0 { (acc[k] / alpha) as u8 } else { 0 };
            rgba.extend_from_slice(&[c(0), c(1), c(2), (alpha / 16.0 * 255.0) as u8]);
        }
    }
    rgba
}
