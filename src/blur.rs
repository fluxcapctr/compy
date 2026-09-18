//! Gaussian blur, as three box blurs of the sizes that best approximate it (Kovesi), done with running sums
//! so the cost does not grow with the radius. Used for the Blur tool's softened sample; a blur is asked for
//! by region so a stroke only pays for the tiles it touches.

/// The three box radii whose combined blur approximates a Gaussian of `sigma`.
pub fn box_radii(sigma: f64) -> [usize; 3] {
    let n = 3.0;
    let ideal = (12.0 * sigma * sigma / n + 1.0).sqrt();
    let mut wl = ideal.floor() as i64;
    if wl % 2 == 0 { wl -= 1; }
    let wu = wl + 2;
    let m = ((12.0 * sigma * sigma - n * (wl * wl) as f64 - 4.0 * n * wl as f64 - 3.0 * n) / (-4.0 * wl as f64 - 4.0)).round() as i64;
    let mut radii = [0usize; 3];
    for (i, r) in radii.iter_mut().enumerate() { *r = (((if (i as i64) < m { wl } else { wu }) - 1) / 2).max(0) as usize; }
    radii
}

/// One box pass along x over an interleaved buffer (`channels` bytes per pixel), edges clamped, rows
/// split across threads.
fn box_h(src: &[u8], dst: &mut [u8], w: usize, h: usize, channels: usize, r: usize) {
    if r == 0 { dst.copy_from_slice(src); return; }
    let span = (2 * r + 1) as u32;
    let row_bytes = w * channels;
    let chunk_rows = (h / threads()).max(1);
    std::thread::scope(|scope| {
        for (chunk, out) in dst.chunks_mut(chunk_rows * row_bytes).enumerate() {
            let y0 = chunk * chunk_rows;
            scope.spawn(move || {
                for (i, orow) in out.chunks_mut(row_bytes).enumerate() {
                    let row = &src[(y0 + i) * row_bytes..(y0 + i + 1) * row_bytes];
                    for ch in 0..channels {
                        let px = |x: isize| row[(x.clamp(0, w as isize - 1) as usize) * channels + ch] as u32;
                        let mut sum: u32 = (-(r as isize)..=r as isize).map(px).sum();
                        for x in 0..w as isize {
                            orow[x as usize * channels + ch] = ((sum + span / 2) / span) as u8;
                            sum += px(x + r as isize + 1);
                            sum -= px(x - r as isize);
                        }
                    }
                }
            });
        }
    });
}

fn threads() -> usize { std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16) }

/// One box pass along y, columns split across threads; each thread writes its own column band.
fn box_v(src: &[u8], dst: &mut [u8], w: usize, h: usize, channels: usize, r: usize) {
    if r == 0 { dst.copy_from_slice(src); return; }
    let span = (2 * r + 1) as u32;
    let bands = threads().min(w).max(1);
    let band_width = w.div_ceil(bands);
    // Each thread produces its band as a separate buffer, then bands are interleaved back into dst.
    let results: Vec<Vec<u8>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..bands).map(|b| {
            let x0 = b * band_width;
            let x1 = ((b + 1) * band_width).min(w);
            scope.spawn(move || {
                let bw = x1.saturating_sub(x0);
                let mut out = vec![0u8; bw * h * channels];
                for (bx, x) in (x0..x1).enumerate() {
                    for ch in 0..channels {
                        let px = |y: isize| src[(y.clamp(0, h as isize - 1) as usize) * w * channels + x * channels + ch] as u32;
                        let mut sum: u32 = (-(r as isize)..=r as isize).map(px).sum();
                        for y in 0..h as isize {
                            out[y as usize * bw * channels + bx * channels + ch] = ((sum + span / 2) / span) as u8;
                            sum += px(y + r as isize + 1);
                            sum -= px(y - r as isize);
                        }
                    }
                }
                out
            })
        }).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for (b, out) in results.iter().enumerate() {
        let x0 = b * band_width;
        let bw = ((b + 1) * band_width).min(w).saturating_sub(x0);
        for y in 0..h { dst[(y * w + x0) * channels..(y * w + x0 + bw) * channels].copy_from_slice(&out[y * bw * channels..(y + 1) * bw * channels]); }
    }
}

/// Blurs a packed buffer in place (premultiplied, any channel count) by about `sigma` pixels.
pub fn gaussian(pixels: &mut [u8], w: usize, h: usize, channels: usize, sigma: f64) {
    if sigma <= 0.0 || w == 0 || h == 0 { return; }
    let mut scratch = vec![0u8; pixels.len()];
    for r in box_radii(sigma) {
        box_h(pixels, &mut scratch, w, h, channels, r);
        box_v(&scratch, pixels, w, h, channels, r);
    }
}

/// How far a blur of `sigma` reaches: the sum of its box radii.
pub fn reach(sigma: f64) -> usize { box_radii(sigma).iter().sum() }

/// A document-size image blurred on demand, one region at a time, each computed with enough margin that its
/// edges match a blur of the whole.
pub struct LazyBlur {
    pub sharp: Vec<u8>,
    pub width: usize,
    pub height: usize,
    pub sigma: f64,
    /// Blurred 256-pixel tiles by (column, row), kept once made.
    tiles: std::cell::RefCell<std::collections::HashMap<(usize, usize), Vec<u8>>>,
}

const BLUR_TILE: usize = 256;

impl LazyBlur {
    pub fn new(sharp: Vec<u8>, width: usize, height: usize, sigma: f64) -> LazyBlur {
        LazyBlur { sharp, width, height, sigma, tiles: Default::default() }
    }

    /// The blurred pixels of the rectangle (x, y, w, h), packed 4 bytes per pixel, assembled from cached tiles.
    pub fn region(&self, x: usize, y: usize, w: usize, h: usize) -> Vec<u8> {
        let mut out = vec![0u8; w * h * 4];
        for ty in y / BLUR_TILE..=(y + h - 1) / BLUR_TILE {
            for tx in x / BLUR_TILE..=(x + w - 1) / BLUR_TILE {
                let (tx0, ty0) = (tx * BLUR_TILE, ty * BLUR_TILE);
                let (tw, th) = (BLUR_TILE.min(self.width - tx0), BLUR_TILE.min(self.height - ty0));
                let mut tiles = self.tiles.borrow_mut();
                let tile = tiles.entry((tx, ty)).or_insert_with(|| self.blurred(tx0, ty0, tw, th));
                let (cx0, cy0) = (x.max(tx0), y.max(ty0));
                let (cx1, cy1) = ((x + w).min(tx0 + tw), (y + h).min(ty0 + th));
                for row in cy0..cy1 {
                    out[((row - y) * w + (cx0 - x)) * 4..((row - y) * w + (cx1 - x)) * 4].copy_from_slice(&tile[((row - ty0) * tw + (cx0 - tx0)) * 4..((row - ty0) * tw + (cx1 - tx0)) * 4]);
                }
            }
        }
        out
    }

    /// One rectangle blurred with enough margin that its edges match a blur of the whole.
    fn blurred(&self, x: usize, y: usize, w: usize, h: usize) -> Vec<u8> {
        let margin = reach(self.sigma);
        let (bx0, by0) = (x.saturating_sub(margin), y.saturating_sub(margin));
        let (bx1, by1) = ((x + w + margin).min(self.width), (y + h + margin).min(self.height));
        let (bw, bh) = (bx1 - bx0, by1 - by0);
        let mut block = vec![0u8; bw * bh * 4];
        for r in 0..bh {
            block[r * bw * 4..(r + 1) * bw * 4].copy_from_slice(&self.sharp[((by0 + r) * self.width + bx0) * 4..((by0 + r) * self.width + bx1) * 4]);
        }
        gaussian(&mut block, bw, bh, 4, self.sigma);
        let mut out = vec![0u8; w * h * 4];
        for r in 0..h {
            let sy = y + r - by0;
            out[r * w * 4..(r + 1) * w * 4].copy_from_slice(&block[(sy * bw + (x - bx0)) * 4..(sy * bw + (x - bx0) + w) * 4]);
        }
        out
    }
}
