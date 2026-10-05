//! Stitches screenshots together, side by side or one above the other.
//! Images shorter (or narrower) than the rest are centred and the space
//! around them is filled with the colour of their own edge, so they look
//! extended rather than boxed in.

use std::collections::HashMap;
use std::path::PathBuf;

use image::{Rgba, RgbaImage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Side by side, left to right.
    Horizontal,
    /// One above the other, top to bottom.
    Vertical,
}

/// Opens the files and combines them in order.
pub fn files(paths: &[PathBuf], dir: Direction) -> Result<RgbaImage, String> {
    let images = paths
        .iter()
        .map(|p| {
            image::open(p)
                .map(|i| i.to_rgba8())
                .map_err(|e| format!("couldn't open {}: {e}", p.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(combine(&images, dir))
}

pub fn combine(images: &[RgbaImage], dir: Direction) -> RgbaImage {
    let horizontal = dir == Direction::Horizontal;
    // Length along the direction, and the size across it.
    let along = |i: &RgbaImage| if horizontal { i.width() } else { i.height() };
    let across = |i: &RgbaImage| if horizontal { i.height() } else { i.width() };
    let total: u32 = images.iter().map(along).sum();
    let span = images.iter().map(across).max().unwrap_or(0);
    let mut out = if horizontal {
        RgbaImage::new(total, span)
    } else {
        RgbaImage::new(span, total)
    };
    let mut offset = 0;
    for img in images {
        let (len, size) = (along(img), across(img));
        let before = (span - size) / 2;
        let after = span - size - before;
        if before > 0 || after > 0 {
            // The rows (or columns) the padding sits against.
            let (first, last) = if horizontal {
                (edge(img, Side::Top), edge(img, Side::Bottom))
            } else {
                (edge(img, Side::Left), edge(img, Side::Right))
            };
            for a in 0..len {
                for b in 0..before {
                    put(&mut out, horizontal, offset + a, b, first);
                }
                for b in span - after..span {
                    put(&mut out, horizontal, offset + a, b, last);
                }
            }
        }
        let (x, y) = if horizontal {
            (offset, before)
        } else {
            (before, offset)
        };
        image::imageops::replace(&mut out, img, x.into(), y.into());
        offset += len;
    }
    out
}

fn put(out: &mut RgbaImage, horizontal: bool, a: u32, b: u32, c: Rgba<u8>) {
    if horizontal {
        out.put_pixel(a, b, c);
    } else {
        out.put_pixel(b, a, c);
    }
}

#[derive(Clone, Copy)]
enum Side {
    Top,
    Bottom,
    Left,
    Right,
}

/// The most common colour along one edge of the image.
fn edge(img: &RgbaImage, side: Side) -> Rgba<u8> {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Rgba([0, 0, 0, 0]);
    }
    let pixels: Box<dyn Iterator<Item = &Rgba<u8>>> = match side {
        Side::Top => Box::new((0..w).map(|x| img.get_pixel(x, 0))),
        Side::Bottom => Box::new((0..w).map(|x| img.get_pixel(x, h - 1))),
        Side::Left => Box::new((0..h).map(|y| img.get_pixel(0, y))),
        Side::Right => Box::new((0..h).map(|y| img.get_pixel(w - 1, y))),
    };
    let mut counts: HashMap<Rgba<u8>, usize> = HashMap::new();
    for p in pixels {
        *counts.entry(*p).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by_key(|&(c, n)| (n, c.0))
        .map_or(Rgba([0, 0, 0, 0]), |(c, _)| c)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Rgba<u8> = Rgba([255, 0, 0, 255]);
    const BLUE: Rgba<u8> = Rgba([0, 0, 255, 255]);
    const WHITE: Rgba<u8> = Rgba([255, 255, 255, 255]);

    #[test]
    fn horizontal_pads_the_shorter_image_with_its_edge() {
        let tall = RgbaImage::from_pixel(2, 6, RED);
        let mut short = RgbaImage::from_pixel(3, 2, WHITE);
        short.put_pixel(1, 1, BLUE); // a stray pixel on the edge doesn't win
        let out = combine(&[tall, short], Direction::Horizontal);
        assert_eq!(out.dimensions(), (5, 6));
        assert_eq!(*out.get_pixel(0, 0), RED);
        assert_eq!(*out.get_pixel(3, 0), WHITE); // padding above
        assert_eq!(*out.get_pixel(3, 2), WHITE); // the image itself
        assert_eq!(*out.get_pixel(3, 3), BLUE);
        assert_eq!(*out.get_pixel(4, 5), WHITE); // padding below
    }

    #[test]
    fn vertical_stacks_and_centres() {
        let wide = RgbaImage::from_pixel(5, 1, RED);
        let narrow = RgbaImage::from_pixel(1, 2, BLUE);
        let out = combine(&[wide, narrow], Direction::Vertical);
        assert_eq!(out.dimensions(), (5, 3));
        for x in 0..5 {
            assert_eq!(*out.get_pixel(x, 0), RED);
            assert_eq!(*out.get_pixel(x, 1), BLUE);
            assert_eq!(*out.get_pixel(x, 2), BLUE);
        }
    }
}
