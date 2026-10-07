use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;

// Получатель вывода экземпляра: (id, текст) или (id, код завершения)
pub trait Sink: Send + Sync + 'static {
    fn output(&self, id: u32, data: String);
    fn exit(&self, id: u32, code: Option<u32>);
}

struct Instance {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
}

#[derive(Default)]
pub struct Registry {
    next: AtomicU32,
    items: Mutex<HashMap<u32, Instance>>,
}

#[derive(Serialize, Clone)]
pub struct Spawned {
    pub id: u32,
    pub program: String,
}

// Переменные сессии Claude Code, которые хост не передает дочерним процессам
const SESSION_ENV: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "AI_AGENT",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
];

// Программа и аргументы запуска: поиск в PATH, обертка npm (.cmd) раскрывается в node со скриптом пакета
pub fn build_command(program: &str, args: &[String]) -> Result<(PathBuf, Vec<String>), String> {
    let mut full: Vec<String> = args.to_vec();
    let given = Path::new(program);
    let found = if given.is_absolute() {
        given.to_path_buf()
    } else {
        find_in_path(program).ok_or(format!("{program} не найден в PATH"))?
    };
    if let Some(script) = npm_shim_script(&found) {
        full.insert(0, script.to_string_lossy().into_owned());
        let node = find_in_path("node").ok_or("node не найден в PATH")?;
        return Ok((node, full));
    }
    Ok((found, full))
}

// Поиск исполняемого файла в PATH с учетом PATHEXT
fn find_in_path(name: &str) -> Option<PathBuf> {
    let exts: Vec<String> = std::env::var("PATHEXT")
        .unwrap_or(".EXE;.CMD;.BAT".into())
        .split(';')
        .map(|s| s.to_ascii_lowercase())
        .collect();
    for dir in std::env::split_paths(&std::env::var_os("PATH")?) {
        if Path::new(name).extension().is_some() && dir.join(name).is_file() {
            return Some(dir.join(name));
        }
        for ext in &exts {
            let p = dir.join(format!("{name}{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

// Путь к js-скрипту из обертки npm (.cmd), чтобы запускать node без cmd /c
fn npm_shim_script(shim: &Path) -> Option<PathBuf> {
    if !shim.extension()?.eq_ignore_ascii_case("cmd") {
        return None;
    }
    let text = std::fs::read_to_string(shim).ok()?;
    let start = text.find("\"%dp0%\\")? + "\"%dp0%\\".len();
    let end = start + text[start..].find('"')?;
    let script = shim.parent()?.join(&text[start..end]);
    script.is_file().then_some(script)
}

impl Registry {
    pub fn spawn(
        &self,
        sink: Arc<dyn Sink>,
        program: &str,
        args: &[String],
        cwd: Option<&str>,
        cols: u16,
        rows: u16,
    ) -> Result<Spawned, String> {
        let (program, args) = build_command(program, args)?;
        let pair = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| e.to_string())?;
        let mut cmd = CommandBuilder::new(&program);
        cmd.args(&args);
        if let Some(dir) = cwd.filter(|d| Path::new(d).is_dir()) {
            cmd.cwd(dir);
        }
        // Признаки сессии родителя снимаются, иначе дочерний claude считает себя вложенным и не пишет журнал
        for name in SESSION_ENV {
            cmd.env_remove(name);
        }
        cmd.env("COLORTERM", "truecolor");
        let child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
        drop(pair.slave);

        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let child = Arc::new(Mutex::new(child));

        // Чтение вывода с переносом неполного символа UTF-8 в следующую порцию
        let out_sink = sink.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            let mut carry: Vec<u8> = Vec::new();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        carry.extend_from_slice(&buf[..n]);
                        let valid = match std::str::from_utf8(&carry) {
                            Ok(_) => carry.len(),
                            Err(e) if e.error_len().is_none() => e.valid_up_to(),
                            Err(_) => carry.len(),
                        };
                        let rest = carry.split_off(valid);
                        out_sink.output(id, String::from_utf8_lossy(&carry).into_owned());
                        carry = rest;
                    }
                }
            }
        });

        // Ожидание завершения процесса опросом, EOF от ConPTY приходит не на всех сборках
        let wait_child = child.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(300));
            let Ok(mut guard) = wait_child.lock() else { break };
            let status = guard.try_wait();
            drop(guard);
            match status {
                Ok(Some(st)) => {
                    sink.exit(id, Some(st.exit_code()));
                    break;
                }
                Ok(None) => continue,
                Err(_) => {
                    sink.exit(id, None);
                    break;
                }
            }
        });

        self.items.lock().unwrap().insert(id, Instance { master: pair.master, writer, child });
        Ok(Spawned { id, program: program.to_string_lossy().into_owned() })
    }

    pub fn write(&self, id: u32, data: &[u8]) -> Result<(), String> {
        let mut items = self.items.lock().unwrap();
        let inst = items.get_mut(&id).ok_or("нет экземпляра")?;
        inst.writer.write_all(data).map_err(|e| e.to_string())?;
        inst.writer.flush().map_err(|e| e.to_string())
    }

    pub fn resize(&self, id: u32, cols: u16, rows: u16) -> Result<(), String> {
        let items = self.items.lock().unwrap();
        let inst = items.get(&id).ok_or("нет экземпляра")?;
        inst.master
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| e.to_string())
    }

    pub fn kill(&self, id: u32) -> Result<(), String> {
        let inst = self.items.lock().unwrap().remove(&id).ok_or("нет экземпляра")?;
        let _ = inst.child.lock().unwrap().kill();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Collect {
        out: Mutex<String>,
        exited: Mutex<Option<Option<u32>>>,
    }
    impl Sink for Collect {
        fn output(&self, _id: u32, data: String) {
            self.out.lock().unwrap().push_str(&data);
        }
        fn exit(&self, _id: u32, code: Option<u32>) {
            *self.exited.lock().unwrap() = Some(code);
        }
    }

    fn wait_for(sink: &Collect, needle: &str) -> bool {
        for _ in 0..100 {
            if sink.out.lock().unwrap().contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    /// Команда, введенная в cmd через ConPTY, исполняется, и ее вывод доходит до получателя.
    #[test]
    fn cmd_in_conpty_echoes_typed_command_output() {
        let reg = Registry::default();
        let sink = Arc::new(Collect::default());
        std::env::set_var("CLAUDE_CODE_CHILD_SESSION", "1");
        let s = reg.spawn(sink.clone(), "cmd", &[], None, 100, 30).unwrap();
        // ConPTY ждет ответа на запрос позиции курсора, прежде чем отдавать вывод
        assert!(wait_for(&sink, "\x1b[6n"), "нет запроса DSR");
        reg.write(s.id, b"\x1b[1;1R").unwrap();
        reg.write(s.id, b"echo mux-%USERNAME:~0,0%probe\r").unwrap();
        assert!(wait_for(&sink, "mux-probe"), "вывод: {}", sink.out.lock().unwrap());
        reg.write(
            s.id,
            b"if defined CLAUDE_CODE_CHILD_SESSION (echo env-%USERNAME:~0,0%leaked) else (echo env-%USERNAME:~0,0%clean)\r",
        )
        .unwrap();
        assert!(wait_for(&sink, "env-clean"), "вывод: {}", sink.out.lock().unwrap());
        reg.resize(s.id, 120, 40).unwrap();
        reg.write(s.id, b"exit\r").unwrap();
        for _ in 0..50 {
            if sink.exited.lock().unwrap().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("процесс не завершился");
    }

    /// Обертка npm для pi раскрывается в запуск node со скриптом пакета.
    #[test]
    fn pi_npm_shim_resolves_to_node_and_script() {
        let Ok((program, args)) = build_command("pi", &["--continue".into()]) else { return };
        if program.file_stem().map(|s| s == "node").unwrap_or(false) {
            assert!(args[0].ends_with(".js"));
            assert_eq!(&args[1..], ["--continue"]);
        }
    }
}
