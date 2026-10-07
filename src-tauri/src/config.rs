use std::path::PathBuf;

use serde_json::{json, Map, Value};

// Настройки по умолчанию: размер шрифта, порядок проектов (date или name) и набор команд запуска
pub fn defaults() -> Value {
    json!({
        "fontSize": 14,
        "sort": "date",
        "commands": [
            {
                "id": "claude",
                "title": "Claude Code",
                "icon": "\u{2733}",
                "color": "#d97757",
                "program": "claude",
                "args": [],
                "continueArgs": ["--continue"],
                "history": "claude",
                "shiftEnter": "\u{1b}\r",
                "pasteImage": "\u{1b}v"
            },
            {
                "id": "pi",
                "title": "pi",
                "icon": "\u{3c0}",
                "color": "#6a9955",
                "program": "pi",
                "args": [],
                "continueArgs": ["--continue"],
                "history": "pi",
                "shiftEnter": "\u{1b}[13;2u",
                "pasteImage": "\u{1b}v"
            },
            {
                "id": "powershell",
                "title": "Windows PowerShell",
                "icon": ">_",
                "color": "#4f8fd6",
                "program": "powershell.exe",
                "args": ["-NoLogo"]
            },
            {
                "id": "powershell-admin",
                "title": "Windows PowerShell (администратор, отдельное окно)",
                "icon": "#>",
                "color": "#e06c6c",
                "program": "powershell.exe",
                "args": ["-NoLogo", "-NoExit", "-Command", "Set-Location -LiteralPath '{cwd}'"],
                "elevate": true
            },
            {
                "id": "ssh",
                "title": "SSH",
                "icon": "ssh",
                "color": "#c586c0",
                "program": "%SystemRoot%\\System32\\OpenSSH\\ssh.exe",
                "args": ["{host}"],
                "pick": "ssh-hosts"
            },
            {
                "id": "cmd",
                "title": "Командная строка",
                "icon": "C:\\",
                "color": "#bbbbbb",
                "program": "cmd.exe",
                "args": []
            }
        ]
    })
}

// Путь к config.json: рядом с exe (портативный режим), иначе ~/.terminal-mux; прежний файл переносится туда
pub fn locate(exe_dir: Option<PathBuf>, home: Option<PathBuf>, legacy: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(portable) = exe_dir.map(|d| d.join("config.json")).filter(|p| p.is_file()) {
        return Some(portable);
    }
    let path = home?.join(".terminal-mux").join("config.json");
    if !path.exists() {
        if let Some(old) = legacy.filter(|p| p.is_file()) {
            let _ = std::fs::create_dir_all(path.parent()?);
            let _ = std::fs::rename(&old, &path).or_else(|_| std::fs::copy(&old, &path).map(|_| ()));
        }
    }
    Some(path)
}

// Чтение config.json с дополнением недостающих ключей умолчаниями; файла нет - создается
pub fn load(path: &PathBuf) -> Value {
    let mut cfg = defaults();
    match std::fs::read_to_string(path) {
        Ok(text) => {
            if let Ok(Value::Object(mut user)) = serde_json::from_str::<Value>(&text) {
                // Ключ прежнего формата: Shift+Enter теперь задается в каждой команде
                let legacy = user.remove("shiftEnter").is_some();
                let incomplete = !user.contains_key("commands");
                merge(cfg.as_object_mut().unwrap(), user);
                if legacy || incomplete {
                    let _ = save(path, &cfg);
                }
            }
        }
        Err(_) => {
            let _ = save(path, &cfg);
        }
    }
    cfg
}

pub fn save(path: &PathBuf, cfg: &Value) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(path, text + "\n").map_err(|e| e.to_string())
}

// Слияние объектов по ключам; массив (список команд) заменяется целиком
fn merge(base: &mut Map<String, Value>, user: Map<String, Value>) {
    for (k, v) in user {
        match (base.get_mut(&k), v) {
            (Some(Value::Object(b)), Value::Object(u)) => merge(b, u),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Пользовательские значения перекрывают умолчания, список команд заменяется целиком.
    #[test]
    fn user_config_overrides_defaults_and_replaces_commands() {
        let path = std::env::temp_dir().join("mux-config-test").join("config.json");
        let _ = std::fs::remove_file(&path);
        assert_eq!(load(&path), defaults());
        assert!(path.is_file());
        std::fs::write(&path, r#"{"fontSize": 18, "commands": [{"id": "cmd", "program": "cmd.exe"}]}"#).unwrap();
        let cfg = load(&path);
        assert_eq!(cfg["fontSize"], 18);
        assert_eq!(cfg["commands"].as_array().unwrap().len(), 1);
    }

    /// Конфиг прежнего формата теряет ключ shiftEnter и дописывается списком команд в файл.
    #[test]
    fn legacy_config_is_migrated_and_saved() {
        let path = std::env::temp_dir().join("mux-config-legacy").join("config.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"fontSize": 18, "shiftEnter": {"pi": "\n"}}"#).unwrap();
        let cfg = load(&path);
        assert!(cfg.get("shiftEnter").is_none());
        assert_eq!(cfg["fontSize"], 18);
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved, cfg);
    }

    /// Кодирование base64 совпадает с эталонными значениями RFC 4648.
    #[test]
    fn base64_matches_rfc4648_vectors() {
        for (src, enc) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foobar", "Zm9vYmFy")] {
            assert_eq!(base64(src.as_bytes()), enc);
        }
    }

    /// Конфиг рядом с exe имеет приоритет, иначе ~/.terminal-mux, куда переносится прежний файл.
    #[test]
    fn config_location_prefers_portable_then_home_with_migration() {
        let root = std::env::temp_dir().join("mux-locate");
        let _ = std::fs::remove_dir_all(&root);
        let (exe, home, old) = (root.join("exe"), root.join("home"), root.join("appdata").join("config.json"));
        std::fs::create_dir_all(&exe).unwrap();
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, "{}").unwrap();
        let target = home.join(".terminal-mux").join("config.json");
        assert_eq!(locate(Some(exe.clone()), Some(home.clone()), Some(old.clone())), Some(target.clone()));
        assert!(target.is_file() && !old.exists());
        std::fs::write(exe.join("config.json"), "{}").unwrap();
        assert_eq!(locate(Some(exe.clone()), Some(home), None), Some(exe.join("config.json")));
    }
}
