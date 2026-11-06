use std::collections::HashMap;

use crate::capture::Image;

pub const MIN_HEIGHT: i32 = 64;
const MAX_HEIGHT: usize = 32_768;
const MAX_BYTES: usize = 128 * 1024 * 1024;
const MIN_ANCHORS: usize = 6;
const MIN_RUN: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Update {
    Ready,
    NoMatch,
    Limit,
}

pub struct Stitcher {
    pub image: Image,
    rows: Fingerprints,
    next_rows: Fingerprints,
    rejected_rows: Vec<u64>,
    aligner: Aligner,
    strip_aligner: Aligner,
    viewport_top: i32,
}

impl Stitcher {
    pub fn new(image: Image) -> Option<Self> {
        if image.height as usize > MAX_HEIGHT || image.pixels.len() > MAX_BYTES {
            return None;
        }
        let mut rows = Fingerprints::default();
        rows.update(&image.pixels, image.width as usize);
        let mut aligner = Aligner::default();
        aligner.set_reference(&rows.full);
        Some(Self {
            next_rows: Fingerprints::default(),
            rejected_rows: Vec::new(),
            aligner,
            strip_aligner: Aligner::default(),
            rows,
            image,
            viewport_top: 0,
        })
    }

    pub fn observe(&mut self, frame: &[u8]) -> Update {
        let width = self.image.width as usize;
        self.next_rows.update(frame, width);
        if self.next_rows.full == self.rows.full {
            return Update::Ready;
        }
        if self.next_rows.full == self.rejected_rows {
            return Update::NoMatch;
        }
        let alignment = self
            .aligner
            .find(&self.rows.full, &self.next_rows.full)
            .or_else(|| {
                self.strip_aligner
                    .agree_on_shift(&self.rows, &self.next_rows)
            });
        let Some((shift, seam)) = alignment else {
            let noise_limit = (self.rows.full.len() / 100).max(2);
            if self
                .rows
                .full
                .iter()
                .zip(&self.next_rows.full)
                .filter(|(old, new)| old != new)
                .take(noise_limit + 1)
                .count()
                <= noise_limit
            {
                // A blinking caret or a few animated rows don't move the viewport.
                return Update::Ready;
            }
            std::mem::swap(&mut self.rejected_rows, &mut self.next_rows.full);
            return Update::NoMatch;
        };
        let top = self.viewport_top + shift;
        let bottom = top + self.rows.full.len() as i32;
        let height = (self.image.height.max(bottom) - top.min(0)) as usize;
        let row_bytes = width * 4;
        let Some(bytes) = height.checked_mul(row_bytes) else {
            return Update::Limit;
        };
        if height > MAX_HEIGHT || bytes > MAX_BYTES {
            return Update::Limit;
        }
        let pixels = &mut self.image.pixels;
        if bytes > pixels.capacity() {
            let capacity = (pixels.capacity() * 2).clamp(bytes, MAX_BYTES);
            if pixels.try_reserve_exact(capacity - pixels.len()).is_err() {
                return Update::Limit;
            }
        }

        // Replace the edge through the seam, keeping fixed chrome only at the extremes.
        let frame_seam = seam * row_bytes;
        let image_seam = (top + seam as i32) as usize * row_bytes;
        if top < 0 {
            let old_bytes = pixels.len();
            pixels.resize(bytes, 0);
            pixels.copy_within(image_seam..old_bytes, frame_seam);
            pixels[..frame_seam].copy_from_slice(&frame[..frame_seam]);
        } else if bottom > self.image.height {
            pixels.truncate(image_seam);
            pixels.extend_from_slice(&frame[frame_seam..]);
        }
        self.viewport_top = top.max(0);
        self.image.height = height as i32;
        self.rejected_rows.clear();
        std::mem::swap(&mut self.rows, &mut self.next_rows);
        self.aligner.set_reference(&self.rows.full);
        Update::Ready
    }
}

#[derive(Default)]
struct Fingerprints {
    full: Vec<u64>,
    strips: [Vec<u64>; 3],
}

impl Fingerprints {
    fn update(&mut self, frame: &[u8], width: usize) {
        self.full.clear();
        self.strips.iter_mut().for_each(Vec::clear);
        // Leave out the scrollbar strip and the unused fourth byte of the screen DIB.
        let right = width - (width / 20).min(24);
        let step = right.div_ceil(128);
        for row in frame.chunks_exact(width * 4) {
            let mut full = 0;
            for (index, strip) in self.strips.iter_mut().enumerate() {
                let left = index * right / 3 * 4;
                let end = (index + 1) * right / 3 * 4;
                let mut hash = 0xcbf2_9ce4_8422_2325u64;
                for pixel in row[left..end].as_chunks::<4>().0.iter().step_by(step) {
                    let rgb = u32::from_le_bytes([pixel[0], pixel[1], pixel[2], 0]);
                    hash = (hash ^ u64::from(rgb)).wrapping_mul(0x0100_0000_01b3);
                }
                strip.push(hash);
                full = (full ^ hash).wrapping_mul(0x0100_0000_01b3);
            }
            self.full.push(full);
        }
    }
}

#[derive(Default)]
struct Aligner {
    anchors: HashMap<u64, Option<usize>>,
    votes: Vec<usize>,
}

impl Aligner {
    fn agree_on_shift(
        &mut self,
        previous: &Fingerprints,
        next: &Fingerprints,
    ) -> Option<(i32, usize)> {
        let mut candidates = [None; 3];
        for (index, (old, new)) in previous.strips.iter().zip(&next.strips).enumerate() {
            if old == new {
                continue;
            }
            self.set_reference(old);
            let Some((shift, seam)) = self.find(old, new) else {
                continue;
            };
            // Two independently matching strips must agree on how far the content moved.
            for (other_shift, other_seam) in candidates[..index].iter().flatten() {
                if *other_shift == shift {
                    return Some((shift, (seam + other_seam) / 2));
                }
            }
            candidates[index] = Some((shift, seam));
        }
        None
    }

    fn set_reference(&mut self, previous: &[u64]) {
        self.anchors.clear();
        for (y, &hash) in previous.iter().enumerate() {
            self.anchors
                .entry(hash)
                .and_modify(|anchor| *anchor = None)
                .or_insert(Some(y));
        }
    }

    fn find(&mut self, previous: &[u64], next: &[u64]) -> Option<(i32, usize)> {
        let height = previous.len();
        let max_shift = height - (height / 4).max(32);
        self.votes.clear();
        self.votes.resize(2 * max_shift + 1, 0);
        for (y, hash) in next.iter().enumerate() {
            if let Some(Some(old_y)) = self.anchors.get(hash) {
                let shift = *old_y as i32 - y as i32;
                if shift != 0 && shift.unsigned_abs() as usize <= max_shift {
                    self.votes[(shift + max_shift as i32) as usize] += 1;
                }
            }
        }

        // The same screen rows at both edges can be pinned headers or footers.
        // Confirm the offset using only the overlap where both frames can scroll.
        let same_rows = previous.iter().zip(next);
        let top = same_rows
            .clone()
            .take(height / 3)
            .take_while(|(old, new)| old == new)
            .count();
        let bottom = same_rows
            .rev()
            .take(height / 3)
            .take_while(|(old, new)| old == new)
            .count();
        let content_end = height - bottom;

        let mut found = None;
        for (index, &votes) in self.votes.iter().enumerate() {
            if votes < MIN_ANCHORS {
                continue;
            }
            let shift = index as i32 - max_shift as i32;
            let old_start = top + shift.max(0) as usize;
            let new_start = top + (-shift).max(0) as usize;
            let overlap = content_end.saturating_sub(old_start.max(new_start));
            if overlap < MIN_RUN * 2 {
                continue;
            }
            let previous_rows = &previous[old_start..old_start + overlap];
            let next_rows = &next[new_start..new_start + overlap];
            let unique_matches = previous_rows
                .iter()
                .zip(next_rows)
                .enumerate()
                .filter(|(index, (old, new))| {
                    old == new && self.anchors.get(old) == Some(&Some(old_start + index))
                })
                .take(MIN_ANCHORS)
                .count();
            if unique_matches < MIN_ANCHORS {
                continue;
            }
            if let Some(seam) = match_overlap(previous_rows, next_rows) {
                if found.is_some() {
                    return None;
                }
                found = Some((shift, new_start + seam));
            }
        }
        found
    }
}

fn match_overlap(previous: &[u64], next: &[u64]) -> Option<usize> {
    let mut matches = 0;
    let mut run = 0;
    let mut best_run = 0;
    let mut seam = 0;
    for (y, (old, new)) in previous.iter().zip(next).enumerate() {
        if old == new {
            matches += 1;
            run += 1;
            if run > best_run {
                best_run = run;
                seam = y + 1 - run / 2;
            }
        } else {
            run = 0;
        }
    }
    (best_run >= MIN_RUN && matches * 4 >= previous.len() * 3).then_some(seam)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTH: i32 = 40;
    const HEIGHT: i32 = 128;
    const CHROME: usize = 8;
    const ROW_BYTES: usize = WIDTH as usize * 4;

    fn frame(offset: u8) -> Image {
        frame_with_chrome(offset, CHROME)
    }

    fn frame_with_chrome(offset: u8, chrome: usize) -> Image {
        let mut pixels = Vec::new();
        for y in 0..HEIGHT as usize {
            let color: u16 = if y < chrome {
                250
            } else if y >= HEIGHT as usize - chrome {
                251
            } else {
                u16::from(offset) + (y - chrome) as u16
            };
            let mut row = [color as u8; ROW_BYTES];
            for pixel in row.chunks_exact_mut(4) {
                pixel[1] = (color >> 8) as u8;
            }
            row[ROW_BYTES - 4..].fill(offset);
            pixels.extend_from_slice(&row);
        }
        Image {
            width: WIDTH,
            height: HEIGHT,
            pixels,
        }
    }

    #[test]
    fn aligns_short_content_overlap_between_fixed_bars() {
        let chrome = 25;
        let mut stitcher = Stitcher::new(frame_with_chrome(45, chrome)).unwrap();
        assert!(stitcher.observe(&frame_with_chrome(0, chrome).pixels) == Update::Ready);
        assert!(stitcher.observe(&frame_with_chrome(60, chrome).pixels) == Update::Ready);
        let rows: Vec<_> = stitcher
            .image
            .pixels
            .chunks_exact(ROW_BYTES)
            .map(|row| row[0])
            .collect();
        assert_eq!(rows.len(), HEIGHT as usize + 60);
        assert_eq!(&rows[..chrome], &[250; 25]);
        assert_eq!(&rows[rows.len() - chrome..], &[251; 25]);
        assert!(rows[chrome..rows.len() - chrome].iter().copied().eq(0..138));
    }

    #[test]
    fn ignores_small_changes_at_the_same_scroll_position() {
        let mut stitcher = Stitcher::new(frame(0)).unwrap();
        let mut blinking = frame(0);
        blinking.pixels[42 * ROW_BYTES..43 * ROW_BYTES].fill(247);
        assert!(stitcher.observe(&blinking.pixels) == Update::Ready);
        assert_eq!(stitcher.image.height, HEIGHT);
        assert!(stitcher.observe(&frame(22).pixels) == Update::Ready);
        assert_image(&stitcher, 22);
    }

    #[test]
    fn adds_only_new_rows_and_keeps_fixed_chrome_once() {
        let mut stitcher = Stitcher::new(frame(0)).unwrap();
        assert!(stitcher.observe(&frame(24).pixels) == Update::Ready);
        assert!(stitcher.observe(&frame(52).pixels) == Update::Ready);
        assert_image(&stitcher, 52);
    }

    fn assert_image(stitcher: &Stitcher, extension: u8) {
        let image = &stitcher.image;
        assert_eq!(image.height, HEIGHT + i32::from(extension));
        let rows: Vec<_> = image
            .pixels
            .chunks_exact(ROW_BYTES)
            .map(|row| row[0])
            .collect();
        assert_eq!(&rows[..CHROME], &[250; CHROME]);
        assert_eq!(&rows[rows.len() - CHROME..], &[251; CHROME]);
        assert!(rows[CHROME..rows.len() - CHROME]
            .iter()
            .copied()
            .eq(0..(HEIGHT as u8 + extension - 2 * CHROME as u8)));
    }

    #[test]
    fn extends_both_ends_without_duplicating_revisited_content() {
        let mut stitcher = Stitcher::new(frame(36)).unwrap();
        assert!(stitcher.observe(&frame(60).pixels) == Update::Ready);
        assert!(stitcher.observe(&frame(12).pixels) == Update::Ready);
        assert!(stitcher.observe(&frame(190).pixels) == Update::NoMatch);
        assert!(stitcher.observe(&frame(0).pixels) == Update::Ready);
        assert_image(&stitcher, 60);
        assert!(stitcher.observe(&frame(36).pixels) == Update::Ready);
        assert!(stitcher.observe(&frame(12).pixels) == Update::Ready);
        assert_image(&stitcher, 60);
        assert!(stitcher.observe(&frame(47).pixels) == Update::Ready);
        assert!(stitcher.observe(&frame(79).pixels) == Update::Ready);
        assert_image(&stitcher, 79);
    }

    #[test]
    fn stationary_sidebar_does_not_break_scrolling_content_alignment() {
        let with_sidebar = |offset| {
            let mut image = frame(offset);
            for (y, row) in image.pixels.chunks_exact_mut(ROW_BYTES).enumerate() {
                for pixel in row[30 * 4..36 * 4].chunks_exact_mut(4) {
                    pixel.copy_from_slice(&[y as u8, 211, 220, 0]);
                }
            }
            image
        };
        let mut stitcher = Stitcher::new(with_sidebar(0)).unwrap();
        assert!(stitcher.observe(&with_sidebar(32).pixels) == Update::Ready);
        assert_image(&stitcher, 32);
    }

    #[test]
    fn repeated_rows_alone_do_not_identify_a_scroll_offset() {
        let previous: Vec<_> = (0..128).map(|y| y % 4).collect();
        let next: Vec<_> = (0..128).map(|y| (y + 3) % 4).collect();
        let mut aligner = Aligner::default();
        aligner.set_reference(&previous);
        assert!(aligner.find(&previous, &next).is_none());
    }
}
