use std::{env, fs, io, path::PathBuf};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, VK_F1,
};

const DEFAULT_HOTKEY: &str = "Ctrl+9";

pub struct Config {
    pub path: PathBuf,
    pub label: String,
    pub modifiers: u32,
    pub key: u32,
}

impl Config {
    pub fn load() -> Result<Self, String> {
        let app_data = env::var_os("LOCALAPPDATA").ok_or("找不到 LOCALAPPDATA。")?;
        let folder = PathBuf::from(app_data).join("Snapshot");
        let path = folder.join("hotkey.txt");
        let contents = match fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
                fs::write(&path, format!("{DEFAULT_HOTKEY}\r\n"))
                    .map_err(|error| error.to_string())?;
                DEFAULT_HOTKEY.to_owned()
            }
            Err(error) => return Err(format!("无法读取 {}\n{error}", path.display())),
        };
        let label = contents.trim_start_matches('\u{feff}').trim().to_owned();
        let (modifiers, key) = parse_hotkey(&label).ok_or_else(|| {
            format!(
                "快捷键格式有误。请编辑 {}，例如 Ctrl+9 或 Ctrl+Shift+F10。",
                path.display()
            )
        })?;
        Ok(Self {
            path,
            label,
            modifiers,
            key,
        })
    }
}

fn parse_hotkey(value: &str) -> Option<(u32, u32)> {
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
    if modifiers == 0 {
        return None;
    }

    let key = match key.as_bytes() {
        [key @ (b'A'..=b'Z' | b'0'..=b'9')] => u32::from(*key),
        _ => {
            let number = key.strip_prefix('F')?.parse::<u32>().ok()?;
            if !(1..=24).contains(&number) || number == 12 {
                return None;
            }
            u32::from(VK_F1) + number - 1
        }
    };
    Some((modifiers | MOD_NOREPEAT, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_and_custom_combinations() {
        assert_eq!(
            parse_hotkey(DEFAULT_HOTKEY),
            Some((MOD_CONTROL | MOD_NOREPEAT, 0x39))
        );
        assert_eq!(
            parse_hotkey("ctrl + Shift + f10"),
            Some((MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT, u32::from(VK_F1) + 9)),
        );
    }

    #[test]
    fn rejects_ambiguous_and_reserved_combinations() {
        for value in [
            "9",
            "Ctrl+Ctrl+9",
            "Ctrl+",
            "Ctrl+9+X",
            "Ctrl+F12",
            "Ctrl+F25",
        ] {
            assert_eq!(parse_hotkey(value), None, "{value}");
        }
    }
}
