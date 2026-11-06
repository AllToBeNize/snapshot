use std::{
    cell::Cell,
    env, fs, io,
    path::{Path, PathBuf},
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Shortcut {
    pub modifiers: u32,
    pub key: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum Binding {
    Capture,
    Rectangle,
    Text,
    Brush,
    Mosaic,
    Undo,
    Redo,
    LongCapture,
    Copy,
    Save,
    Pin,
}

pub const BINDINGS: [Binding; 11] = [
    Binding::Capture,
    Binding::Rectangle,
    Binding::Text,
    Binding::Brush,
    Binding::Mosaic,
    Binding::Undo,
    Binding::Redo,
    Binding::LongCapture,
    Binding::Copy,
    Binding::Save,
    Binding::Pin,
];
const NAMES: [&str; BINDINGS.len()] = [
    "capture",
    "rectangle",
    "text",
    "brush",
    "mosaic",
    "undo",
    "redo",
    "long_capture",
    "copy",
    "save",
    "pin",
];
const LABELS: [&str; BINDINGS.len()] = [
    "开始截图（全局）",
    "矩形",
    "文本",
    "画笔",
    "马赛克",
    "撤回",
    "重做",
    "长截图",
    "复制",
    "保存",
    "贴图",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Bindings([Shortcut; BINDINGS.len()]);

pub const DEFAULT_OPACITY_STEP: u8 = 5;
const OPACITY_STEP_KEY: &str = "pin_opacity_step";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Preferences {
    pub bindings: Bindings,
    pub opacity_step: u8,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            bindings: DEFAULT,
            opacity_step: DEFAULT_OPACITY_STEP,
        }
    }
}

pub fn parse_opacity_step(value: &str) -> Result<u8, String> {
    value
        .trim()
        .parse::<u8>()
        .ok()
        .filter(|step| (1..=100).contains(step))
        .ok_or_else(|| "贴图不透明度步长必须是 1 到 100 的整数。".into())
}

const fn ctrl(key: u8) -> Shortcut {
    Shortcut {
        modifiers: MOD_CONTROL,
        key: key as u32,
    }
}

pub const DEFAULT: Bindings = Bindings([
    ctrl(b'9'),
    ctrl(b'1'),
    ctrl(b'2'),
    ctrl(b'3'),
    ctrl(b'4'),
    ctrl(b'5'),
    ctrl(b'6'),
    ctrl(b'7'),
    ctrl(b'C'),
    ctrl(b'S'),
    ctrl(b'T'),
]);

thread_local! {
    static CURRENT: Cell<Bindings> = const { Cell::new(DEFAULT) };
}

pub fn current() -> Bindings {
    CURRENT.get()
}

pub fn activate(bindings: Bindings) {
    CURRENT.set(bindings);
}

impl Binding {
    pub fn label(self) -> &'static str {
        LABELS[self as usize]
    }
}

impl Bindings {
    pub fn get(self, binding: Binding) -> Shortcut {
        self.0[binding as usize]
    }

    pub fn set(&mut self, binding: Binding, shortcut: Shortcut) {
        self.0[binding as usize] = shortcut;
    }

    pub fn resolve(self, shortcut: Shortcut) -> Option<Binding> {
        if let Some(binding) = BINDINGS
            .into_iter()
            .find(|&binding| self.get(binding) == shortcut)
        {
            return (binding != Binding::Capture).then_some(binding);
        }
        match shortcut {
            Shortcut { modifiers: 0, key } if key == u32::from(VK_RETURN) => Some(Binding::Copy),
            Shortcut {
                modifiers: MOD_CONTROL,
                key: 0x5a,
            } if self.get(Binding::Undo) == DEFAULT.get(Binding::Undo) => Some(Binding::Undo),
            Shortcut { modifiers, key }
                if self.get(Binding::Redo) == DEFAULT.get(Binding::Redo)
                    && ((modifiers == MOD_CONTROL && key == u32::from(b'Y'))
                        || (modifiers == MOD_CONTROL | MOD_SHIFT && key == u32::from(b'Z'))) =>
            {
                Some(Binding::Redo)
            }
            _ => None,
        }
    }

    pub fn validate(self) -> Result<Self, String> {
        for (index, binding) in BINDINGS.into_iter().enumerate() {
            let shortcut = self.get(binding);
            if let Some(other) = BINDINGS[..index]
                .iter()
                .find(|&&other| self.get(other) == shortcut)
            {
                return Err(format!(
                    "「{}」和「{}」都绑定了 {}。",
                    other.label(),
                    binding.label(),
                    shortcut.label(),
                ));
            }
        }
        Ok(self)
    }
}

impl Shortcut {
    pub fn from_key(modifiers: u32, key: u32) -> Option<Self> {
        let function_key =
            (u32::from(VK_F1)..=u32::from(VK_F24)).contains(&key) && key != u32::from(VK_F12);
        let alphanumeric =
            u8::try_from(key).is_ok_and(|key| key.is_ascii_uppercase() || key.is_ascii_digit());
        (function_key || (modifiers != 0 && alphanumeric)).then_some(Self { modifiers, key })
    }

    pub fn parse(value: &str) -> Option<Self> {
        let normalized = value.to_ascii_uppercase();
        let mut parts = normalized.split('+').map(str::trim);
        let key = parts.next_back()?;
        let mut modifiers = 0;
        for part in parts {
            let modifier = match part {
                "CTRL" => MOD_CONTROL,
                "ALT" => MOD_ALT,
                "SHIFT" => MOD_SHIFT,
                "WIN" => MOD_WIN,
                _ => return None,
            };
            if modifiers & modifier != 0 {
                return None;
            }
            modifiers |= modifier;
        }
        let key = match key.as_bytes() {
            [key @ (b'A'..=b'Z' | b'0'..=b'9')] => u32::from(*key),
            _ => {
                let number = key.strip_prefix('F')?.parse::<u32>().ok()?;
                if !(1..=24).contains(&number) {
                    return None;
                }
                u32::from(VK_F1) + number - 1
            }
        };
        Self::from_key(modifiers, key)
    }

    pub fn label(self) -> String {
        let mut label = String::new();
        for (mask, text) in [
            (MOD_CONTROL, "Ctrl+"),
            (MOD_ALT, "Alt+"),
            (MOD_SHIFT, "Shift+"),
            (MOD_WIN, "Win+"),
        ] {
            if self.modifiers & mask != 0 {
                label.push_str(text);
            }
        }
        if self.key >= u32::from(VK_F1) {
            label.push_str(&format!("F{}", self.key - u32::from(VK_F1) + 1));
        } else {
            label.push(self.key as u8 as char);
        }
        label
    }
}

pub unsafe fn modifiers() -> u32 {
    let pressed = |key| GetKeyState(i32::from(key)) < 0;
    u32::from(pressed(VK_CONTROL)) * MOD_CONTROL
        | u32::from(pressed(VK_MENU)) * MOD_ALT
        | u32::from(pressed(VK_SHIFT)) * MOD_SHIFT
        | u32::from(pressed(VK_LWIN) || pressed(VK_RWIN)) * MOD_WIN
}

pub fn path() -> Result<PathBuf, String> {
    let app_data = env::var_os("LOCALAPPDATA").ok_or("找不到 LOCALAPPDATA。")?;
    Ok(PathBuf::from(app_data)
        .join("Snapshot")
        .join("shortcuts.txt"))
}

pub fn load() -> Result<Preferences, String> {
    load_from(&path()?)
}

fn load_from(path: &Path) -> Result<Preferences, String> {
    let mut bindings = DEFAULT;
    let mut opacity_step = DEFAULT_OPACITY_STEP;
    match fs::read_to_string(path) {
        Ok(contents) => {
            for line in contents
                .trim_start_matches('\u{feff}')
                .lines()
                .filter(|line| !line.trim().is_empty())
            {
                let (name, value) = line
                    .split_once('=')
                    .ok_or("快捷键配置格式应为 名称=组合键。")?;
                let name = name.trim();
                if name == OPACITY_STEP_KEY {
                    opacity_step = parse_opacity_step(value)?;
                } else if let Some(index) = NAMES.iter().position(|&key| key == name) {
                    bindings.set(
                        BINDINGS[index],
                        Shortcut::parse(value).ok_or_else(|| format!("快捷键格式有误：{line}"))?,
                    );
                }
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let legacy = path.with_file_name("hotkey.txt");
            match fs::read_to_string(&legacy) {
                Ok(value) => {
                    let capture = Shortcut::parse(value.trim_start_matches('\u{feff}'))
                        .ok_or("原截图快捷键格式有误，请检查 hotkey.txt。")?;
                    bindings.set(Binding::Capture, capture);
                    // Legacy files could use an in-app default as the global trigger.
                    if let Some(binding) = BINDINGS[1..]
                        .iter()
                        .find(|&&binding| bindings.get(binding) == capture)
                    {
                        if let Some(replacement) = (b'0'..=b'9')
                            .map(ctrl)
                            .find(|&shortcut| !bindings.0.contains(&shortcut))
                        {
                            bindings.set(*binding, replacement);
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        Err(error) => return Err(format!("无法读取 {}。\n{error}", path.display())),
    }
    Ok(Preferences {
        bindings: bindings.validate()?,
        opacity_step,
    })
}

pub fn save(preferences: Preferences) -> Result<(), String> {
    save_to(&path()?, preferences)
}

fn save_to(path: &Path, preferences: Preferences) -> Result<(), String> {
    let temporary = path.with_extension("tmp");
    let mut contents: String = BINDINGS
        .into_iter()
        .map(|binding| {
            format!(
                "{}={}\r\n",
                NAMES[binding as usize],
                preferences.bindings.get(binding).label()
            )
        })
        .collect();
    contents.push_str(&format!(
        "{OPACITY_STEP_KEY}={}\r\n",
        preferences.opacity_step
    ));
    fs::create_dir_all(path.parent().expect("settings directory"))
        .map_err(|error| error.to_string())?;
    fs::write(&temporary, contents).map_err(|error| error.to_string())?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(temporary);
        return Err(format!("无法保存设置。\n{error}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bindings_and_rejects_reserved_or_ambiguous_values() {
        assert_eq!(
            Shortcut::parse("Ctrl+9"),
            Some(DEFAULT.get(Binding::Capture))
        );
        assert_eq!(
            Shortcut::parse("ctrl + Shift + f10").unwrap().label(),
            "Ctrl+Shift+F10"
        );
        assert_eq!(Shortcut::parse("F1").unwrap().label(), "F1");
        for value in [
            "9",
            "Ctrl+Ctrl+9",
            "Ctrl+",
            "Ctrl+9+X",
            "Ctrl+F12",
            "Ctrl+F25",
        ] {
            assert_eq!(Shortcut::parse(value), None, "{value}");
        }
    }

    #[test]
    fn explicit_bindings_take_priority_and_duplicates_are_rejected() {
        let mut bindings = DEFAULT;
        bindings.set(Binding::Copy, ctrl(b'Y'));
        assert_eq!(bindings.resolve(ctrl(b'Y')), Some(Binding::Copy));
        assert!(bindings.validate().is_ok());
        bindings.set(Binding::Save, ctrl(b'Y'));
        assert!(bindings.validate().is_err());
    }

    #[test]
    fn reads_legacy_capture_and_round_trips_settings_on_repeated_saves() {
        let folder =
            env::temp_dir().join(format!("snapshot-shortcuts-test-{}", std::process::id()));
        fs::create_dir_all(&folder).unwrap();
        let legacy = folder.join("hotkey.txt");
        let file = folder.join("shortcuts.txt");
        fs::write(&legacy, "Ctrl+F8\r\n").unwrap();
        let mut preferences = load_from(&file).unwrap();
        assert_eq!(preferences.opacity_step, 5);
        assert_eq!(
            preferences.bindings.get(Binding::Capture),
            Shortcut::parse("Ctrl+F8").unwrap()
        );
        preferences
            .bindings
            .set(Binding::Copy, Shortcut::parse("Ctrl+Shift+C").unwrap());
        preferences.opacity_step = 10;
        save_to(&file, preferences).unwrap();
        assert_eq!(load_from(&file).unwrap(), preferences);
        save_to(&file, Preferences::default()).unwrap();
        assert_eq!(load_from(&file).unwrap(), Preferences::default());
        fs::remove_file(legacy).unwrap();
        fs::remove_file(file).unwrap();
        fs::remove_dir(folder).unwrap();
    }
}
