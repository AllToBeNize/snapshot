use std::{
    fs::File,
    io::{self, BufWriter},
    mem::size_of,
    path::Path,
    ptr::{null_mut, NonNull},
    slice,
};
use windows_sys::Win32::{
    Foundation::{GlobalFree, HWND, RECT},
    Graphics::Gdi::*,
    System::{
        DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
        Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
    },
    UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    },
};

const BYTES_PER_PIXEL: usize = 4;
const CF_DIB: u32 = 8;

pub struct Desktop {
    pub bounds: RECT,
    pub surface: Surface,
}

pub struct Surface {
    pub dc: HDC,
    bitmap: HBITMAP,
    previous_bitmap: HGDIOBJ,
    pixels: NonNull<u8>,
}

impl Desktop {
    pub fn capture() -> io::Result<Self> {
        unsafe {
            let left = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let top = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
            let height = GetSystemMetrics(SM_CYVIRTUALSCREEN);
            pixel_len(width, height)?;

            let screen = GetDC(null_mut());
            if screen.is_null() {
                return Err(io::Error::last_os_error());
            }
            let result = Surface::new(screen, width, height).and_then(|surface| {
                if BitBlt(
                    surface.dc,
                    0,
                    0,
                    width,
                    height,
                    screen,
                    left,
                    top,
                    SRCCOPY | CAPTUREBLT,
                ) == 0
                {
                    return Err(io::Error::last_os_error());
                }
                GdiFlush();
                Ok(Self {
                    bounds: RECT {
                        left,
                        top,
                        right: left + width,
                        bottom: top + height,
                    },
                    surface,
                })
            });
            ReleaseDC(null_mut(), screen);
            result
        }
    }

    pub fn crop(&self, rect: RECT) -> io::Result<Image> {
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        let source_width = self.bounds.right - self.bounds.left;
        let source_height = self.bounds.bottom - self.bounds.top;
        if rect.left < 0 || rect.top < 0 || rect.right > source_width || rect.bottom > source_height
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "选区超出桌面范围",
            ));
        }
        let mut pixels = Vec::with_capacity(pixel_len(width, height)?);
        let source = unsafe {
            // The top-down DIB belongs to this capture and BitBlt was flushed before access.
            slice::from_raw_parts(
                self.surface.pixels.as_ptr(),
                pixel_len(source_width, source_height)?,
            )
        };
        let row_bytes = width as usize * BYTES_PER_PIXEL;
        for y in rect.top..rect.bottom {
            let start = (y as usize * source_width as usize + rect.left as usize) * BYTES_PER_PIXEL;
            pixels.extend_from_slice(&source[start..start + row_bytes]);
        }
        Ok(Image {
            width,
            height,
            pixels,
        })
    }
}

impl Surface {
    pub unsafe fn new(reference: HDC, width: i32, height: i32) -> io::Result<Self> {
        let dc = CreateCompatibleDC(reference);
        if dc.is_null() {
            return Err(io::Error::last_os_error());
        }
        let info = BITMAPINFO {
            bmiHeader: bitmap_header(width, height),
            ..Default::default()
        };
        let mut bits = null_mut();
        let bitmap = CreateDIBSection(reference, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        let Some(pixels) = NonNull::new(bits.cast()) else {
            let error = io::Error::last_os_error();
            if !bitmap.is_null() {
                DeleteObject(bitmap);
            }
            DeleteDC(dc);
            return Err(error);
        };
        let previous_bitmap = SelectObject(dc, bitmap);
        if previous_bitmap.is_null() || previous_bitmap as isize == -1 {
            let error = io::Error::last_os_error();
            DeleteObject(bitmap);
            DeleteDC(dc);
            return Err(error);
        }
        Ok(Self {
            dc,
            bitmap,
            previous_bitmap,
            pixels,
        })
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous_bitmap);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

pub struct Image {
    width: i32,
    height: i32,
    pixels: Vec<u8>,
}

impl Image {
    pub fn copy_to_clipboard(&self, owner: HWND) -> io::Result<()> {
        let header = bitmap_header(self.width, self.height);
        let header_size = size_of::<BITMAPINFOHEADER>();
        unsafe {
            let memory = GlobalAlloc(GMEM_MOVEABLE, header_size + self.pixels.len());
            if memory.is_null() {
                return Err(io::Error::last_os_error());
            }
            let target = GlobalLock(memory).cast::<u8>();
            if target.is_null() {
                let error = io::Error::last_os_error();
                GlobalFree(memory);
                return Err(error);
            }
            target.cast::<BITMAPINFOHEADER>().write_unaligned(header);
            target
                .add(header_size)
                .copy_from_nonoverlapping(self.pixels.as_ptr(), self.pixels.len());
            GlobalUnlock(memory);

            if OpenClipboard(owner) == 0 {
                let error = io::Error::last_os_error();
                GlobalFree(memory);
                return Err(error);
            }
            if EmptyClipboard() == 0 || SetClipboardData(CF_DIB, memory).is_null() {
                let error = io::Error::last_os_error();
                CloseClipboard();
                GlobalFree(memory);
                return Err(error);
            }
            // SetClipboardData transfers ownership to Windows.
            CloseClipboard();
        }
        Ok(())
    }

    pub fn save_png(mut self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        for pixel in self.pixels.as_chunks_mut::<BYTES_PER_PIXEL>().0 {
            pixel.swap(0, 2);
            pixel[3] = 255;
        }
        let file = BufWriter::new(File::create(path)?);
        let mut encoder = png::Encoder::new(file, self.width as u32, self.height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&self.pixels)?;
        writer.finish()?;
        Ok(())
    }
}

fn bitmap_header(width: i32, height: i32) -> BITMAPINFOHEADER {
    BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width,
        biHeight: -height,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        ..Default::default()
    }
}

fn pixel_len(width: i32, height: i32) -> io::Result<usize> {
    let length = i64::from(width) * i64::from(height);
    if width > 0 && height > 0 && length <= isize::MAX as i64 / BYTES_PER_PIXEL as i64 {
        return Ok(length as usize * BYTES_PER_PIXEL);
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "桌面或选区尺寸无效",
    ))
}
