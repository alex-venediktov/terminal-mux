// Подстановка каталога проекта вместо {cwd} в аргументах команды
pub fn expand_args(args: &[String], cwd: &str) -> Vec<String> {
    args.iter().map(|a| a.replace("{cwd}", cwd)).collect()
}

// Склейка аргументов в командную строку Windows по правилам разбора CommandLineToArgvW
pub fn join_args(args: &[String]) -> String {
    args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")
}

fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut out = String::from('"');
    let mut slashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('"');
    out
}

// Запуск программы с повышением прав через диалог UAC в отдельном окне, как из Проводника
#[cfg(windows)]
pub fn launch(program: &str, args: &[String], cwd: Option<&str>) -> Result<(), String> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let verb = wide("runas");
    let file = wide(program);
    let params = wide(&join_args(args));
    let dir = cwd.map(wide);
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            params.as_ptr(),
            dir.as_ref().map(|d| d.as_ptr()).unwrap_or(std::ptr::null()),
            SW_SHOWNORMAL,
        )
    };
    if result as isize > 32 {
        return Ok(());
    }
    // ERROR_CANCELLED: пользователь отказал в диалоге UAC
    match unsafe { GetLastError() } {
        1223 => Err("повышение прав отменено".into()),
        code => Err(format!("ShellExecute runas: код {}, ошибка {code}", result as isize)),
    }
}

#[cfg(not(windows))]
pub fn launch(_program: &str, _args: &[String], _cwd: Option<&str>) -> Result<(), String> {
    Err("повышение прав поддерживается только в Windows".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Склеенная командная строка разбирается обратно в те же аргументы по правилам Windows.
    #[test]
    fn joined_args_follow_windows_quoting_rules() {
        let args: Vec<String> = ["-NoExit", "", "a b", "say \"hi\"", r"C:\dir with space\", r"x\\"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            join_args(&args),
            r#"-NoExit "" "a b" "say \"hi\"" "C:\dir with space\\" x\\"#
        );
    }

    /// Каталог подставляется во все вхождения {cwd}, остальные аргументы не меняются.
    #[test]
    fn cwd_placeholder_is_expanded() {
        let args = vec!["-Command".to_string(), "Set-Location -LiteralPath '{cwd}'".to_string()];
        assert_eq!(expand_args(&args, r"D:\p"), ["-Command", r"Set-Location -LiteralPath 'D:\p'"]);
    }
}
