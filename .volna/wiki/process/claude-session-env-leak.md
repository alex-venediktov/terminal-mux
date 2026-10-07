# Claude Code, унаследовавший окружение другой сессии, не сохраняет транскрипт

**тип:** побочный эффект · **предмет:** окружение дочернего claude · **этапы:** implement

**вывод:** процесс, запущенный из сессии Claude Code (через инструмент Bash или из приложения,
запущенного оттуда), наследует `CLAUDECODE`, `CLAUDE_CODE_CHILD_SESSION`, `CLAUDE_CODE_SESSION_ID`,
`CLAUDE_CODE_MESSAGING_SOCKET` и другие переменные. Дочерний claude с ними считает себя вложенной
сессией и пишет «Transcript saving is off». Хост, запускающий claude, снимает эти переменные;
список привязан к версии Claude Code и проверяется по `env` новой версии.

**источник:**
- `src-tauri/src/pty.rs:42` — `"CLAUDE_CODE_CHILD_SESSION",`
