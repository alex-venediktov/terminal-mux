use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Serialize;
use serde_json::Value;

#[derive(Serialize, Clone, Debug)]
pub struct ProjectInfo {
    pub cwd: String,
    pub modified: u64,
    pub history: HashMap<String, u64>,
    pub exists: bool,
}

// Сколько строк начала журнала просматривать в поисках cwd
const HEAD_LINES: usize = 60;

// Проекты из хранилищ claude и pi, включая каталоги без сессий; ключ - путь без учета регистра
pub fn list_projects() -> Vec<ProjectInfo> {
    let mut map: HashMap<String, ProjectInfo> = HashMap::new();
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    let stores: [(PathBuf, &str, fn(&Path) -> Option<String>); 2] = [
        (home.join(".claude").join("projects"), "claude", claude_cwd),
        (home.join(".pi").join("agent").join("sessions"), "pi", pi_cwd),
    ];
    for (root, kind, read_cwd) in stores {
        let Ok(dirs) = std::fs::read_dir(&root) else { continue };
        for dir in dirs.flatten().filter(|d| d.path().is_dir()) {
            let newest = newest_session(&dir.path());
            let from_session = newest.as_ref().and_then(|(_, p)| read_cwd(p)).filter(|c| !c.is_empty());
            let Some(cwd) = resolve_root(&dir.file_name().to_string_lossy(), from_session) else {
                continue;
            };
            let entry = map.entry(cwd.trim_end_matches(['\\', '/']).to_lowercase()).or_insert(ProjectInfo {
                cwd,
                modified: 0,
                history: HashMap::new(),
                exists: false,
            });
            entry.modified = entry.modified.max(modified(&dir.path()));
            if let Some((m, _)) = newest {
                entry.modified = entry.modified.max(m);
                let h = entry.history.entry(kind.to_string()).or_insert(0);
                *h = (*h).max(m);
            }
        }
    }
    let mut out: Vec<ProjectInfo> = map.into_values().collect();
    for p in &mut out {
        p.exists = Path::new(&p.cwd).is_dir();
    }
    out.sort_by(|a, b| b.modified.cmp(&a.modified));
    out
}

// Самый свежий журнал сессии *.jsonl в каталоге проекта хранилища
fn newest_session(dir: &Path) -> Option<(u64, PathBuf)> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|f| f.path())
        .filter(|p| p.extension().map(|e| e == "jsonl").unwrap_or(false))
        .map(|p| (modified(&p), p))
        .max_by_key(|(m, _)| *m)
}

// Кодирование имени как в хранилищах: все, кроме букв и цифр ASCII, заменяется на "-"
fn encode(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .to_lowercase()
}

// Корень проекта по имени каталога хранилища: cwd сессии, если он совпадает с именем и существует, иначе сверка имени с диском
fn resolve_root(store_name: &str, session_cwd: Option<String>) -> Option<String> {
    let target = encode(store_name.trim_matches('-'));
    if let Some(cwd) = session_cwd.as_ref() {
        if encode(cwd.trim_end_matches(['\\', '/'])) == target && Path::new(cwd).is_dir() {
            return Some(cwd.clone());
        }
    }
    decode_dir_name(store_name).map(|p| p.to_string_lossy().into_owned()).or(session_cwd)
}

// Восстановление пути по имени каталога хранилища сверкой с реальными каталогами диска
pub fn decode_dir_name(name: &str) -> Option<PathBuf> {
    let n = encode(name.trim_matches('-'));
    let drive = n.chars().next().filter(|c| c.is_ascii_alphabetic())?;
    let rest = n.get(1..)?.strip_prefix("--")?;
    probe(&PathBuf::from(format!("{}:\\", drive.to_ascii_uppercase())), rest)
}

fn probe(dir: &Path, rest: &str) -> Option<PathBuf> {
    if rest.is_empty() {
        return Some(dir.to_path_buf());
    }
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let e = encode(&entry.file_name().to_string_lossy());
        if e.is_empty() {
            continue;
        }
        if rest == e {
            return Some(entry.path());
        }
        if let Some(tail) = rest.strip_prefix(&format!("{e}-")) {
            if let Some(found) = probe(&entry.path(), tail) {
                return Some(found);
            }
        }
    }
    None
}

fn modified(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn head(p: &Path) -> Vec<Value> {
    let Ok(f) = File::open(p) else { return Vec::new() };
    BufReader::new(f)
        .lines()
        .take(HEAD_LINES)
        .flatten()
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect()
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(str::to_owned)
}

// Каталог сессии Claude Code: поле cwd первых записей журнала
fn claude_cwd(p: &Path) -> Option<String> {
    head(p).iter().find_map(|v| str_field(v, "cwd"))
}

// Каталог сессии pi: заголовок первой строкой {type: session, id, cwd}
fn pi_cwd(p: &Path) -> Option<String> {
    let lines = head(p);
    let header = lines.first().filter(|v| str_field(v, "type").as_deref() == Some("session"))?;
    str_field(header, "cwd")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Индекс проектов отдает уникальные пути, свежие первыми, и признак существования каталога.
    #[test]
    fn projects_are_unique_sorted_and_marked_existing() {
        let list = list_projects();
        let mut keys: Vec<String> = list.iter().map(|p| p.cwd.trim_end_matches('\\').to_lowercase()).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), list.len());
        assert!(list.windows(2).all(|w| w[0].modified >= w[1].modified));
        for p in &list {
            assert_eq!(p.exists, Path::new(&p.cwd).is_dir());
        }
        eprintln!(
            "проектов: {}, существуют: {}, без сессий: {}",
            list.len(),
            list.iter().filter(|p| p.exists).count(),
            list.iter().filter(|p| p.history.is_empty()).count()
        );
    }

    /// Имя каталога хранилища раскодируется в реальный путь, включая точки и дефисы в именах.
    #[test]
    fn store_dir_name_decodes_to_real_path() {
        let base = std::env::temp_dir().join("mux decode").join("my-proj.v2").join(".volna");
        std::fs::create_dir_all(&base).unwrap();
        let s = base.to_string_lossy().to_string();
        let claude = encode(&s);
        assert_eq!(decode_dir_name(&claude).unwrap().to_string_lossy().to_lowercase(), s.to_lowercase());
        let pi = format!("--{}--", s.replace([':', '\\'], "-"));
        assert_eq!(decode_dir_name(&pi).unwrap().to_string_lossy().to_lowercase(), s.to_lowercase());
        assert!(decode_dir_name("Q--no-such-dir-anywhere").is_none());
    }

    /// Корень проекта берется из имени каталога хранилища, когда cwd сессии устарел или указывает на подкаталог.
    #[test]
    fn project_root_follows_store_name_not_stale_session_cwd() {
        let root = std::env::temp_dir().join("mux root").join("terminal-mux");
        std::fs::create_dir_all(root.join("src")).unwrap();
        let r = root.to_string_lossy().to_string();
        let store = encode(&r);
        let stale = r.replace("terminal-mux", "Terminal");
        let got = resolve_root(&store, Some(stale)).unwrap();
        assert_eq!(got.to_lowercase(), r.to_lowercase());
        let sub = root.join("src").to_string_lossy().to_string();
        assert_eq!(resolve_root(&store, Some(sub)).unwrap().to_lowercase(), r.to_lowercase());
        assert_eq!(resolve_root(&store, Some(r.clone())).unwrap(), r);
    }
}
