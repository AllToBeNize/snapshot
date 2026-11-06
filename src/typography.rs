use std::cell::LazyCell;
use windows_sys::{core::w, Win32::Graphics::Gdi::*};

#[derive(Clone, Copy)]
pub enum TextStyle {
    Ui,
    Compact,
    Annotation,
    HintDescription,
    Dimensions,
}

struct Font(HFONT);

impl Font {
    fn new(height: i32, face: *const u16) -> Self {
        let font = unsafe {
            CreateFontW(
                -height,
                0,
                0,
                0,
                FW_NORMAL as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                ANTIALIASED_QUALITY as u32,
                DEFAULT_PITCH as u32,
                face,
            )
        };
        Self(font)
    }
}

impl Drop for Font {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { DeleteObject(self.0) };
        }
    }
}

thread_local! {
    static UI: LazyCell<Font> = const { LazyCell::new(|| Font::new(15, w!("Segoe UI"))) };
    static COMPACT: LazyCell<Font> = const { LazyCell::new(|| Font::new(13, w!("Segoe UI"))) };
    static ANNOTATION: LazyCell<Font> = const { LazyCell::new(|| Font::new(16, w!("Segoe UI"))) };
    static HINT_DESCRIPTION: LazyCell<Font> = const { LazyCell::new(|| Font::new(15, w!("Microsoft YaHei"))) };
    static DIMENSIONS: LazyCell<Font> = const { LazyCell::new(|| Font::new(16, w!("Segoe UI"))) };
}

pub fn font(style: TextStyle) -> HFONT {
    let font = match style {
        TextStyle::Ui => UI.with(|font| font.0),
        TextStyle::Compact => COMPACT.with(|font| font.0),
        TextStyle::Annotation => ANNOTATION.with(|font| font.0),
        TextStyle::HintDescription => HINT_DESCRIPTION.with(|font| font.0),
        TextStyle::Dimensions => DIMENSIONS.with(|font| font.0),
    };
    if font.is_null() {
        unsafe { GetStockObject(DEFAULT_GUI_FONT) }
    } else {
        font
    }
}
