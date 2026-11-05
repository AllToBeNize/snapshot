use windows_sys::Win32::Foundation::{POINT, RECT};

const MIN_WIDTH: i32 = 132;
const MIN_HEIGHT: i32 = 100;
const SAMPLES: i32 = 24;

pub fn find_panel(pixels: &[u8], stride: usize, window: RECT, pointer: POINT) -> Option<RECT> {
    let pixel = |x: i32, y: i32| {
        let index = (y as usize * stride + x as usize) * 4;
        [pixels[index], pixels[index + 1], pixels[index + 2]]
    };
    let changes = |first: [u8; 3], second: [u8; 3]| {
        first
            .into_iter()
            .zip(second)
            .map(|(a, b)| a.abs_diff(b))
            .max()
            .unwrap_or(0)
            >= 4
    };
    let line = |start: i32, end: i32, differs: &dyn Fn(i32) -> bool| {
        if end - start < 48 {
            return false;
        }
        let hits = (0..SAMPLES)
            .filter(|&sample| differs(start + (end - start - 1) * sample / (SAMPLES - 1)))
            .count();
        hits >= 19
    };
    let scan_top = (pointer.y - 230).max(window.top);
    let scan_bottom = (pointer.y + 231).min(window.bottom);
    let vertical = |x| {
        line(scan_top, scan_bottom, &|y| {
            changes(pixel(x - 1, y), pixel(x, y))
        })
    };
    let left = (window.left + 1..=pointer.x - 5)
        .rev()
        .find(|&x| vertical(x))
        .unwrap_or(window.left);
    let right = (pointer.x + 6..window.right)
        .find(|&x| vertical(x))
        .unwrap_or(window.right);
    if right - left < MIN_WIDTH || (left - window.left) + (window.right - right) < 32 {
        return None;
    }

    // Inset samples keep the column's own vertical separator out of the row scan.
    let inset = ((right - left) / 12).min(24);
    let horizontal = |y| {
        let differs = |x| changes(pixel(x, y - 1), pixel(x, y));
        differs(left + inset)
            && differs(right - inset - 1)
            && line(left + inset, right - inset, &differs)
    };
    let top = (window.top + 1..=pointer.y - 5)
        .rev()
        .find(|&y| horizontal(y))
        .unwrap_or(window.top);
    let bottom = (pointer.y + 6..window.bottom)
        .find(|&y| horizontal(y))
        .unwrap_or(window.bottom);
    (bottom - top >= MIN_HEIGHT).then_some(RECT {
        left,
        top,
        right,
        bottom,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_low_contrast_content_column_without_including_the_composer() {
        let width = 1100usize;
        let height = 720;
        let mut pixels = vec![0u8; width * height * 4];
        for y in 0..height {
            for x in 0..width {
                let panel = (245..875).contains(&x) && y < 640;
                let message = panel && (390..840).contains(&x) && y % 132 < 21;
                let color = if message {
                    55
                } else if panel {
                    26
                } else {
                    20
                };
                pixels[(y * width + x) * 4..(y * width + x) * 4 + 3].fill(color);
            }
        }
        let window = RECT {
            right: width as i32,
            bottom: height as i32,
            ..Default::default()
        };
        let result = find_panel(&pixels, width, window, POINT { x: 540, y: 330 }).unwrap();
        assert_eq!(
            (result.left, result.top, result.right, result.bottom),
            (245, 0, 875, 640)
        );
        pixels.fill(26);
        assert!(find_panel(&pixels, width, window, POINT { x: 540, y: 330 }).is_none());
    }
}
